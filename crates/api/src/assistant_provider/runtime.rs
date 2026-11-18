//! Request admission, consent gating, concurrency and circuit breaking around
//! the optional [`AnswerProvider`]. Retrieval answers never wait on it: the
//! grounded answer is served first and generation is a separate request
//! that is refused (not queued) when the single slot is busy.
use super::{
    ollama::{OllamaOptions, OllamaProvider},
    AnswerProvider, AnswerRequest, ProviderError,
};
use serde::Serialize;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, OwnedSemaphorePermit, Semaphore};
use uuid::Uuid;

/// Consecutive failures of one kind before the breaker opens.
const BREAKER_THRESHOLD: u8 = 3;
/// Timeouts and transport errors usually mean a cold or busy model.
const UNAVAILABLE_OPEN: Duration = Duration::from_secs(60);
/// Contract violations do not fix themselves; back off longer.
const INVALID_OPEN: Duration = Duration::from_secs(300);
/// Minimum spacing between one user's generations.
const AI_USER_SPACING: Duration = Duration::from_secs(10);
const AI_USER_CAP: usize = 2048;
/// Whole-generation bound (first token to last); CPU generation of a
/// 160-token answer measured 30-100 s on the target host.
pub const DEFAULT_TIMEOUT_SECS: u64 = 120;
const MAX_TIMEOUT_SECS: u64 = 300;
/// Per-user admission for POST /programs/{id}/assistant (and its stream).
pub const ASK_PER_MINUTE: usize = 10;
/// Per-user admission for POST /assistant/search (a whole-catalogue read).
pub const SEARCH_PER_MINUTE: usize = 6;
const USER_MAP_CAP: usize = 20_000;

/// `true`, `1`, `yes` or `on` (any case) enable a flag; anything else is off.
pub fn env_flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|v| {
        matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "true" | "1" | "yes" | "on"
        )
    })
}

/// The one kill switch for every model call (answer model, agent planner,
/// intent classifier, reranker): `ASSISTANT_MODELS_ENABLED` set to `false`,
/// `0`, `no` or `off`. Unset or anything else leaves each model to its own
/// flag (code review M).
pub fn models_allowed_from(value: Option<&str>) -> bool {
    !value.is_some_and(|v| {
        matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "false" | "0" | "no" | "off"
        )
    })
}

/// [`models_allowed_from`] on the process environment.
pub fn models_allowed() -> bool {
    models_allowed_from(std::env::var("ASSISTANT_MODELS_ENABLED").ok().as_deref())
}

pub struct AssistantRuntime {
    provider: Option<Arc<dyn AnswerProvider>>,
    timeout: Duration,
    /// One generation at a time, server-wide.
    gate: Arc<Semaphore>,
    limits: Arc<Mutex<Limits>>,
    asks: Mutex<HashMap<Uuid, Vec<Instant>>>,
    searches: Mutex<HashMap<Uuid, Vec<Instant>>>,
}

#[derive(Default)]
struct Limits {
    users: HashMap<Uuid, Instant>,
    /// Timeouts and transport errors.
    unavailable_failures: u8,
    /// Malformed, out-of-contract or ungrounded model output.
    invalid_failures: u8,
    open_until: Option<Instant>,
    /// When the last answer stream ended. It shares the model's single KV
    /// slot with the agent planner and evicts the planner's cached prefix.
    last_finished: Option<Instant>,
}

impl Limits {
    fn record(&mut self, outcome: Result<(), ProviderError>, now: Instant) {
        match outcome {
            Ok(()) => {
                self.unavailable_failures = 0;
                self.invalid_failures = 0;
                self.open_until = None;
            }
            Err(ProviderError::Unavailable) => {
                self.unavailable_failures = self.unavailable_failures.saturating_add(1);
                if self.unavailable_failures >= BREAKER_THRESHOLD {
                    self.open_until = Some(now + UNAVAILABLE_OPEN);
                }
            }
            Err(ProviderError::InvalidOutput) => {
                self.invalid_failures = self.invalid_failures.saturating_add(1);
                if self.invalid_failures >= BREAKER_THRESHOLD {
                    self.open_until = Some(now + INVALID_OPEN);
                }
            }
            Err(ProviderError::Cancelled) => {}
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AnswerMode {
    Rules,
    AiAssisted,
    Fallback,
}

/// Why a generation was not started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// `ASSISTANT_AI_ENABLED` is off or misconfigured.
    Disabled,
    /// Another generation holds the single slot.
    Busy,
    /// The breaker is open after repeated failures.
    Unavailable,
    /// This user started one less than [`AI_USER_SPACING`] ago.
    TooSoon,
}

/// An admitted generation. Holds the server-wide slot until dropped.
pub struct Generation {
    _permit: OwnedSemaphorePermit,
    provider: Arc<dyn AnswerProvider>,
    timeout: Duration,
    limits: Arc<Mutex<Limits>>,
    user_id: Uuid,
}

impl Generation {
    /// Run the provider under the timeout, check the finished text with
    /// `check`, and record exactly one outcome for the breaker: a text that
    /// fails `check` counts as out-of-contract output, a cancelled run
    /// (client left) counts as nothing. The user's spacing restarts when
    /// the run ends, so one account cannot hold the slot back to back.
    pub async fn run<V>(
        self,
        request: &AnswerRequest,
        deltas: mpsc::Sender<String>,
        check: impl FnOnce(&str) -> Result<(), V>,
    ) -> Result<Result<String, V>, ProviderError> {
        // Marks the end of the stream even when the caller drops this future
        // (client gone): the answer still evicted the planner's prefix.
        struct Finished(Arc<Mutex<Limits>>);
        impl Drop for Finished {
            fn drop(&mut self) {
                if let Ok(mut limits) = self.0.lock() {
                    limits.last_finished = Some(Instant::now());
                }
            }
        }
        let _finished = Finished(self.limits.clone());
        let result =
            match tokio::time::timeout(self.timeout, self.provider.generate(request, deltas)).await
            {
                Ok(result) => result,
                Err(_) => Err(ProviderError::Unavailable),
            };
        let result = result.map(|text| check(&text).map(|()| text));
        let outcome = match &result {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(_)) => Err(ProviderError::InvalidOutput),
            Err(error) => Err(error.clone()),
        };
        if matches!(
            outcome,
            Err(ProviderError::Unavailable | ProviderError::InvalidOutput)
        ) {
            tracing::warn!("Assistant generation failed or was ungrounded; quotes stay available");
        }
        if let Ok(mut limits) = self.limits.lock() {
            let now = Instant::now();
            limits.record(outcome, now);
            limits.users.insert(self.user_id, now);
        }
        result
    }
}

fn admit(map: &Mutex<HashMap<Uuid, Vec<Instant>>>, user: Uuid, limit: usize) -> bool {
    let now = Instant::now();
    let window = Duration::from_secs(60);
    // A poisoned map still holds valid timestamps; failing open would lift
    // the limit for everyone (security L6).
    let mut map = map
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if map.len() >= USER_MAP_CAP {
        map.retain(|_, times| {
            times.retain(|at| now.duration_since(*at) < window);
            !times.is_empty()
        });
    }
    let times = map.entry(user).or_default();
    times.retain(|at| now.duration_since(*at) < window);
    if times.len() >= limit {
        return false;
    }
    times.push(now);
    true
}

impl AssistantRuntime {
    pub fn disabled() -> Self {
        Self::new(None, Duration::from_secs(DEFAULT_TIMEOUT_SECS))
    }

    pub fn new(provider: Option<Arc<dyn AnswerProvider>>, timeout: Duration) -> Self {
        Self {
            provider,
            timeout,
            gate: Arc::new(Semaphore::new(1)),
            limits: Arc::new(Mutex::new(Limits::default())),
            asks: Mutex::new(HashMap::new()),
            searches: Mutex::new(HashMap::new()),
        }
    }

    pub fn from_env() -> Self {
        if !env_flag("ASSISTANT_AI_ENABLED") || !models_allowed() {
            return Self::disabled();
        }
        let endpoint = std::env::var("ASSISTANT_OLLAMA_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:11434".into());
        let model = std::env::var("ASSISTANT_AI_MODEL")
            .unwrap_or_else(|_| super::ollama::GENERATION_MODEL.into());
        let timeout = std::env::var("ASSISTANT_AI_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|n| (1..=MAX_TIMEOUT_SECS).contains(n))
            .unwrap_or(DEFAULT_TIMEOUT_SECS);
        let options = OllamaOptions {
            keep_alive: std::env::var("ASSISTANT_OLLAMA_KEEP_ALIVE")
                .ok()
                .filter(|v| super::ollama::valid_keep_alive(v))
                .unwrap_or_else(|| super::ollama::DEFAULT_KEEP_ALIVE.into()),
            allow_any_port: env_flag("ASSISTANT_OLLAMA_ALLOW_ANY_PORT"),
            // Invalid (not 1..=64) -> 0 -> rejected below -> quotes only.
            num_threads: super::ollama::parse_num_threads(
                std::env::var("ASSISTANT_AI_NUM_THREADS").ok().as_deref(),
            )
            .unwrap_or(0),
            plan_prompt: Default::default(),
        };
        let provider = match OllamaProvider::with_options(
            &endpoint,
            &model,
            Duration::from_secs(timeout),
            options,
        ) {
            Ok(provider) => Arc::new(provider) as Arc<dyn AnswerProvider>,
            Err(_) => {
                tracing::warn!("Assistant AI configuration invalid; serving quotes only");
                return Self::disabled();
            }
        };
        let runtime = Self::new(Some(provider), Duration::from_secs(timeout));
        if env_flag("ASSISTANT_AI_WARMUP") {
            runtime.spawn_warm_up();
        }
        runtime
    }

    /// Preload the model in the background. A failed warm-up counts as an
    /// availability failure, so a missing model is reflected in
    /// [`Self::ai_available`] before users wait on it.
    pub fn spawn_warm_up(&self) {
        let (Some(provider), Ok(handle)) =
            (self.provider.clone(), tokio::runtime::Handle::try_current())
        else {
            return;
        };
        let limits = self.limits.clone();
        let timeout = self.timeout.max(Duration::from_secs(45));
        handle.spawn(async move {
            let outcome = match tokio::time::timeout(timeout, provider.warm_up()).await {
                Ok(result) => result,
                Err(_) => Err(ProviderError::Unavailable),
            };
            if outcome.is_err() {
                tracing::warn!("Assistant model warm-up failed; grounded quotes stay available");
            }
            if let Ok(mut limits) = limits.lock() {
                limits.record(outcome, Instant::now());
            }
        });
    }

    /// Configured AND not currently tripped by recent failures (lazy health:
    /// derived from real outcomes, no extra probe request).
    pub fn ai_available(&self) -> bool {
        self.provider.is_some()
            && self.limits.lock().is_ok_and(|limits| {
                limits
                    .open_until
                    .is_none_or(|until| until <= Instant::now())
            })
    }

    /// When the last answer stream ended, if one ran.
    pub fn last_generation_at(&self) -> Option<Instant> {
        self.limits
            .lock()
            .ok()
            .and_then(|limits| limits.last_finished)
    }

    /// The server-wide single model slot, for the agent's model planner. The
    /// planner and the answer stream share one private Ollama runner
    /// (`OLLAMA_NUM_PARALLEL=1`), so they take turns instead of queueing.
    /// `None` when busy; never waits.
    pub fn try_model_slot(&self) -> Option<OwnedSemaphorePermit> {
        self.gate.clone().try_acquire_owned().ok()
    }

    /// Per-user sliding window for questions (grounded answers and their
    /// generations). The IP governor alone lets one account behind many
    /// IPs, or many accounts behind one IP, starve others.
    pub fn admit_question(&self, user_id: Uuid) -> bool {
        admit(&self.asks, user_id, ASK_PER_MINUTE)
    }

    /// Per-user sliding window for cross-programme search.
    pub fn admit_search(&self, user_id: Uuid) -> bool {
        admit(&self.searches, user_id, SEARCH_PER_MINUTE)
    }

    /// Take the single generation slot for `user_id`, or say why not. Never
    /// waits: a busy slot is refused so the caller can keep the quotes.
    pub fn start_generation(&self, user_id: Uuid) -> Result<Generation, Refusal> {
        let provider = self.provider.clone().ok_or(Refusal::Disabled)?;
        let permit = self
            .gate
            .clone()
            .try_acquire_owned()
            .map_err(|_| Refusal::Busy)?;
        let now = Instant::now();
        let mut limits = self.limits.lock().map_err(|_| Refusal::Unavailable)?;
        if limits.open_until.is_some_and(|until| until > now) {
            return Err(Refusal::Unavailable);
        }
        limits
            .users
            .retain(|_, at| now.duration_since(*at) < AI_USER_SPACING);
        if limits.users.contains_key(&user_id) {
            return Err(Refusal::TooSoon);
        }
        // A full map degrades only new users, not everyone.
        if limits.users.len() >= AI_USER_CAP {
            return Err(Refusal::Busy);
        }
        limits.users.insert(user_id, now);
        Ok(Generation {
            _permit: permit,
            provider,
            timeout: self.timeout,
            limits: self.limits.clone(),
            user_id,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Fake {
        count: AtomicUsize,
        outcome: Result<(), ProviderError>,
    }
    impl Fake {
        fn new(outcome: Result<(), ProviderError>) -> Arc<Self> {
            Arc::new(Self {
                count: AtomicUsize::new(0),
                outcome,
            })
        }
    }
    #[async_trait]
    impl AnswerProvider for Fake {
        async fn generate(
            &self,
            _: &AnswerRequest,
            deltas: mpsc::Sender<String>,
        ) -> Result<String, ProviderError> {
            self.count.fetch_add(1, Ordering::Relaxed);
            self.outcome.clone()?;
            deltas
                .send("요약".into())
                .await
                .map_err(|_| ProviderError::Cancelled)?;
            Ok("요약".into())
        }
        async fn warm_up(&self) -> Result<(), ProviderError> {
            self.outcome.clone()
        }
    }

    fn request() -> AnswerRequest {
        AnswerRequest {
            question: "서류".into(),
            verdict: None,
            passages: vec!["신분증".into()],
        }
    }

    async fn run_checked(
        runtime: &AssistantRuntime,
        user: Uuid,
        grounded: bool,
    ) -> Result<Result<String, ()>, ProviderError> {
        let generation = runtime.start_generation(user).expect("admitted");
        let (tx, mut rx) = mpsc::channel(8);
        let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
        let result = generation
            .run(&request(), tx, |_| if grounded { Ok(()) } else { Err(()) })
            .await;
        drain.await.unwrap();
        result
    }

    async fn run(runtime: &AssistantRuntime, user: Uuid) -> Result<String, ProviderError> {
        run_checked(runtime, user, true)
            .await
            .map(|r| r.expect("grounded"))
    }

    #[tokio::test]
    async fn disabled_runtime_refuses_without_calling_anything() {
        let runtime = AssistantRuntime::disabled();
        assert!(!runtime.ai_available());
        assert_eq!(
            runtime.start_generation(Uuid::new_v4()).err(),
            Some(Refusal::Disabled)
        );
    }

    #[tokio::test]
    async fn one_generation_at_a_time_and_per_user_spacing() {
        let fake = Fake::new(Ok(()));
        let runtime = AssistantRuntime::new(Some(fake.clone()), Duration::from_secs(1));
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let held = runtime.start_generation(a).expect("first");
        assert_eq!(runtime.start_generation(b).err(), Some(Refusal::Busy));
        drop(held);
        assert_eq!(run(&runtime, b).await.unwrap(), "요약");
        assert_eq!(runtime.start_generation(b).err(), Some(Refusal::TooSoon));
        assert_eq!(fake.count.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn invalid_output_opens_circuit() {
        let fake = Fake::new(Err(ProviderError::InvalidOutput));
        let runtime = AssistantRuntime::new(Some(fake.clone()), Duration::from_secs(1));
        assert!(runtime.ai_available());
        for _ in 0..3 {
            assert_eq!(
                run(&runtime, Uuid::new_v4()).await,
                Err(ProviderError::InvalidOutput)
            );
        }
        assert!(
            !runtime.ai_available(),
            "tripped breaker reports unavailable"
        );
        assert_eq!(
            runtime.start_generation(Uuid::new_v4()).err(),
            Some(Refusal::Unavailable)
        );
        assert_eq!(fake.count.load(Ordering::Relaxed), 3);
    }

    #[tokio::test]
    async fn ungrounded_outputs_count_toward_the_breaker() {
        let runtime = AssistantRuntime::new(Some(Fake::new(Ok(()))), Duration::from_secs(1));
        for _ in 0..3 {
            assert_eq!(
                run_checked(&runtime, Uuid::new_v4(), false).await,
                Ok(Err(()))
            );
        }
        assert!(
            !runtime.ai_available(),
            "three ungrounded answers in a row trip the breaker"
        );
    }

    #[tokio::test]
    async fn spacing_restarts_when_a_generation_ends() {
        let runtime = AssistantRuntime::new(Some(Fake::new(Ok(()))), Duration::from_secs(1));
        let user = Uuid::new_v4();
        let generation = runtime.start_generation(user).unwrap();
        runtime.limits.lock().unwrap().users.clear();
        let (tx, _rx) = mpsc::channel(8);
        let _ = generation.run(&request(), tx, |_| Ok::<(), ()>(())).await;
        assert_eq!(runtime.start_generation(user).err(), Some(Refusal::TooSoon));
    }

    #[test]
    fn timeout_and_invalid_counters_are_separate_and_cancel_is_ignored() {
        let now = Instant::now();
        let mut limits = Limits::default();
        for outcome in [
            Err(ProviderError::Unavailable),
            Err(ProviderError::InvalidOutput),
            Err(ProviderError::Cancelled),
            Err(ProviderError::Unavailable),
            Err(ProviderError::InvalidOutput),
            Err(ProviderError::Cancelled),
        ] {
            limits.record(outcome, now);
        }
        assert!(limits.open_until.is_none(), "mixed failures do not trip");
        limits.record(Err(ProviderError::Unavailable), now);
        assert_eq!(limits.open_until, Some(now + UNAVAILABLE_OPEN));
        let mut limits = Limits::default();
        for _ in 0..3 {
            limits.record(Err(ProviderError::InvalidOutput), now);
        }
        assert_eq!(limits.open_until, Some(now + INVALID_OPEN));
        limits.record(Ok(()), now);
        assert!(limits.open_until.is_none());
        assert_eq!(
            (limits.unavailable_failures, limits.invalid_failures),
            (0, 0)
        );
    }

    #[tokio::test]
    async fn failed_warm_up_marks_ai_unavailable_lazily() {
        let runtime = AssistantRuntime::new(
            Some(Fake::new(Err(ProviderError::Unavailable))),
            Duration::from_secs(1),
        );
        for _ in 0..3 {
            runtime.spawn_warm_up();
        }
        for _ in 0..50 {
            if !runtime.ai_available() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("three failed warm-ups must trip availability");
    }

    #[test]
    fn question_and_search_admission_are_per_user_and_separate() {
        let runtime = AssistantRuntime::disabled();
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        for _ in 0..ASK_PER_MINUTE {
            assert!(runtime.admit_question(a));
        }
        assert!(!runtime.admit_question(a));
        assert!(runtime.admit_question(b), "other users are unaffected");
        for _ in 0..SEARCH_PER_MINUTE {
            assert!(runtime.admit_search(a), "separate budget");
        }
        assert!(!runtime.admit_search(a));
    }

    /// Security L6: a panic while holding the map must not lift the limit.
    #[test]
    fn question_admission_survives_a_poisoned_lock() {
        let runtime = Arc::new(AssistantRuntime::disabled());
        let user = Uuid::new_v4();
        for _ in 0..ASK_PER_MINUTE {
            assert!(runtime.admit_question(user));
        }
        let poisoner = Arc::clone(&runtime);
        let _ = std::thread::spawn(move || {
            let _guard = poisoner.asks.lock().unwrap();
            panic!("poison the per-user map");
        })
        .join();
        assert!(runtime.asks.is_poisoned());
        assert!(!runtime.admit_question(user));
    }

    /// Round 4: the generated summary is off unless `ASSISTANT_AI_ENABLED`
    /// is set true; unset is off (its guard is a frozen design).
    #[test]
    fn the_generated_summary_is_off_by_default() {
        assert!(
            std::env::var("ASSISTANT_AI_ENABLED").is_err() || !env_flag("ASSISTANT_AI_ENABLED"),
            "the test environment must not enable the summary"
        );
        assert!(!AssistantRuntime::from_env().ai_available());
    }

    #[test]
    fn env_flag_accepts_common_truthy_spellings() {
        for (value, expected) in [
            ("true", true),
            ("TRUE", true),
            (" True ", true),
            ("1", true),
            ("yes", true),
            ("on", true),
            ("false", false),
            ("0", false),
            ("", false),
            ("enabled", false),
        ] {
            let name = format!("MAZIMI_TEST_FLAG_{}", Uuid::new_v4().simple());
            std::env::set_var(&name, value);
            assert_eq!(env_flag(&name), expected, "{value:?}");
            std::env::remove_var(&name);
        }
        assert!(!env_flag("MAZIMI_TEST_FLAG_UNSET_NEVER"));
    }

    struct Slow {
        entered: tokio::sync::Notify,
    }
    #[async_trait]
    impl AnswerProvider for Slow {
        async fn generate(
            &self,
            _: &AnswerRequest,
            _: mpsc::Sender<String>,
        ) -> Result<String, ProviderError> {
            self.entered.notify_one();
            std::future::pending().await
        }
    }

    #[tokio::test]
    async fn timeout_releases_the_slot_and_counts_as_unavailable() {
        let slow = Arc::new(Slow {
            entered: tokio::sync::Notify::new(),
        });
        let runtime = Arc::new(AssistantRuntime::new(
            Some(slow.clone()),
            Duration::from_millis(30),
        ));
        let running = runtime.clone();
        let first = tokio::spawn(async move { run(&running, Uuid::new_v4()).await });
        slow.entered.notified().await;
        assert_eq!(
            runtime.start_generation(Uuid::new_v4()).err(),
            Some(Refusal::Busy)
        );
        assert_eq!(first.await.unwrap(), Err(ProviderError::Unavailable));
        assert!(runtime.gate.clone().try_acquire_owned().is_ok());
        let limits = runtime.limits.lock().unwrap();
        assert_eq!(
            (limits.unavailable_failures, limits.invalid_failures),
            (1, 0)
        );
    }

    struct Stalled;
    #[async_trait]
    impl AnswerProvider for Stalled {
        async fn generate(
            &self,
            _: &AnswerRequest,
            _: mpsc::Sender<String>,
        ) -> Result<String, ProviderError> {
            std::future::pending().await
        }
    }

    /// A stream the client drops still marks the end of an answer: it used
    /// the shared model slot, so the planner's prefix must be restored.
    #[tokio::test]
    async fn a_dropped_answer_stream_marks_its_end() {
        let runtime = AssistantRuntime::new(Some(Arc::new(Stalled)), Duration::from_secs(60));
        assert!(runtime.last_generation_at().is_none());
        let generation = runtime.start_generation(Uuid::new_v4()).expect("admitted");
        let (tx, _rx) = mpsc::channel(8);
        let task = tokio::spawn(async move {
            let _ = generation.run(&request(), tx, |_| Ok::<(), ()>(())).await;
        });
        tokio::task::yield_now().await;
        task.abort();
        let _ = task.await;
        assert!(runtime.last_generation_at().is_some());
        assert!(runtime.try_model_slot().is_some(), "slot released");
    }
}
