//! Process-local agent state: flags, per-user admission, the model planner
//! behind the shared single model slot and a breaker, the run concurrency
//! cap, and the progress-event registry that streams a run's steps.
//!
//! Multi-instance note: limits, the breaker and the registry live in this
//! process (like the v1 assistant's). A client that loses its stream polls
//! `GET /assistant/runs/{id}`, which reads the database.
use super::{
    classifier::{self, ArtefactPaths, IntentClassifier},
    goals::{Extras, GoalContext},
    official_page::{LiveCheck, OfficialPages},
    planner::{self, ModelFailure, ModelOutcome},
};
use crate::assistant_provider::{
    env_flag, AssistantRuntime, OllamaOptions, OllamaProvider, PlanProvider,
};
use mazimi_core::agent::{PlanError, MAX_MODEL_PLAN_STEPS};
use serde_json::Value;
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::{Duration, Instant},
};
use tokio::sync::{broadcast, Notify, OwnedSemaphorePermit, Semaphore};
use uuid::Uuid;

/// Agent runs started per user per minute.
pub const MESSAGES_PER_MINUTE: usize = 6;
/// Approve/reject decisions per user per minute.
pub const DECISIONS_PER_MINUTE: usize = 30;
/// Model-planned runs per user per hour: one user cannot hold the single
/// server-wide model slot (~80 s per plan) for everyone else.
pub const MODEL_PLANS_PER_HOUR: usize = 6;
const USER_MAP_CAP: usize = 20_000;
const BREAKER_THRESHOLD: u8 = 3;
const BREAKER_OPEN: Duration = Duration::from_secs(120);
/// Finished runs stay streamable this long for late subscribers.
const FINISHED_TTL: Duration = Duration::from_secs(120);

#[derive(Debug, Clone)]
pub struct AgentConfig {
    /// `ASSISTANT_AGENT_ENABLED` (default off).
    pub enabled: bool,
    /// `ASSISTANT_RETENTION_DAYS` (1..=90, default 30). The worker purges.
    pub retention_days: u32,
    /// `ASSISTANT_AGENT_PLAN_TIMEOUT_SECS` (10..=300, default 180): one
    /// model planning call. qwen3.5:9b measured ~80 s p50 on the host.
    pub plan_timeout: Duration,
    /// `ASSISTANT_AGENT_RUN_TIMEOUT_SECS` (30..=600, default 300).
    pub run_timeout: Duration,
    /// After this a stream says `backgrounded`; the run continues and a push
    /// is queued when it ends. `ASSISTANT_AGENT_BACKGROUND_AFTER_SECS`
    /// (5..=60, default 20).
    pub background_after: Duration,
    /// Confirmation lifetime (spec §8: 10 minutes).
    pub confirmation_ttl: Duration,
    /// Runs executing at once, server-wide (`ASSISTANT_AGENT_MAX_RUNS`,
    /// 1..=64, default 8). Read tools are DB-bound.
    pub max_concurrent_runs: usize,
    /// Per read tool.
    pub tool_timeout: Duration,
    /// agent-tools-v3 tools, each behind its own flag (default off).
    pub tools: ToolFlags,
    /// `ASSISTANT_AGENT_PLANNER_REFUSALS` (default off): whether a model
    /// planner refusal is answered as one. Off, refusal belongs to the guard,
    /// the rules and the classifier's bar; a goal the model refuses gets the
    /// rules' answer, or a not-understood reply (the tuned planner refused 3
    /// of 16 honest goals naming fraud terms on dev).
    pub planner_refusals: bool,
}

/// The agent-tools-v3 tools a deployment offers. Off: the fast path routes
/// those goals as before and model plans lose those steps.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ToolFlags {
    /// `ASSISTANT_AGENT_OFFICIAL_PAGE_ENABLED` (+ the fetch service:
    /// `OFFICIAL_PAGE_MCP_URL`, `OFFICIAL_PAGE_MCP_TOKEN`).
    pub official_page: bool,
    /// `ASSISTANT_AGENT_DOCUMENT_LINKS_ENABLED`.
    pub document_links: bool,
    /// `ASSISTANT_AGENT_CALENDAR_ENABLED`.
    pub calendar: bool,
    /// `ASSISTANT_AGENT_JOURNEY_ENABLED=false` turns the 신청 준비 journey's
    /// tools off (vault match, official-page handoff). On by default, so
    /// the field is the kill switch.
    pub journey_off: bool,
}

impl ToolFlags {
    pub const ALL: Self = Self {
        official_page: true,
        document_links: true,
        calendar: true,
        journey_off: false,
    };

    pub fn from_env() -> Self {
        Self {
            official_page: env_flag("ASSISTANT_AGENT_OFFICIAL_PAGE_ENABLED"),
            document_links: env_flag("ASSISTANT_AGENT_DOCUMENT_LINKS_ENABLED"),
            calendar: env_flag("ASSISTANT_AGENT_CALENDAR_ENABLED"),
            journey_off: std::env::var("ASSISTANT_AGENT_JOURNEY_ENABLED")
                .is_ok_and(|v| matches!(v.trim(), "0" | "false" | "off" | "no")),
        }
    }

    /// Whether a tool may run. Tools before v3 are always allowed.
    pub fn allows(self, tool: mazimi_core::agent::ToolName) -> bool {
        use mazimi_core::agent::ToolName;
        match tool {
            ToolName::CheckOfficialPage => self.official_page,
            ToolName::MatchVaultDocuments
            | ToolName::RequestSignature
            | ToolName::OpenOfficialApplication => !self.journey_off,
            ToolName::DocumentIssueLink => self.document_links,
            ToolName::AddToCalendar => self.calendar,
            _ => true,
        }
    }

    /// The v3 tools offered, in catalog order.
    pub fn v3_tools(self) -> Vec<mazimi_core::agent::ToolName> {
        mazimi_core::agent::ToolName::V3
            .into_iter()
            .filter(|t| self.allows(*t))
            .collect()
    }

    /// The fast path's view of the same flags.
    pub fn extras(self) -> Extras {
        Extras {
            official_page: self.official_page,
            document_links: self.document_links,
            calendar: self.calendar,
        }
    }
}

/// Load the understanding model on a background thread and install it
/// for the intent layer. Until it is installed (or if it fails) the lexical
/// rules read the writes: the AI-off fallback.
fn spawn_understanding_load() {
    use super::classifier::understand::{Understanding, UnderstandingPaths};
    let dir = std::env::var("ASSISTANT_UNDERSTANDING_DIR")
        .or_else(|_| std::env::var("ASSISTANT_INTENT_CLASSIFIER_DIR"))
        .unwrap_or_else(|_| classifier::DEFAULT_DIR.into());
    let cores = std::thread::available_parallelism().map_or(1, usize::from);
    let threads = bounded("ASSISTANT_UNDERSTANDING_THREADS", 1, 16, 2).min(cores);
    let spawned = std::thread::Builder::new()
        .name("understanding-load".into())
        .spawn(move || {
            let started = Instant::now();
            match Understanding::load(&UnderstandingPaths::in_dir(dir.as_ref()), threads) {
                Ok(model) => {
                    // One warm-up read before the first run's (delta review
                    // HIGH): the first read pays the pool and page-in costs.
                    let _ = model.read("이 혜택 마감 알림 켜줘");
                    tracing::info!(
                        version = model.version(),
                        threads,
                        load_ms = started.elapsed().as_millis() as u64,
                        "Agent understanding model loaded"
                    );
                    super::intent::install_understanding(Arc::new(model));
                }
                Err(error) => tracing::warn!(
                    %error,
                    "Agent understanding model disabled; the rules read writes (AI-off fallback)"
                ),
            }
        });
    if spawned.is_err() {
        tracing::warn!("Agent understanding loader could not start; the rules read writes");
    }
}

/// The largest run timeout a deployment may set
/// (`ASSISTANT_AGENT_RUN_TIMEOUT_SECS`): the stale-run sweep's cutoff.
pub const MAX_RUN_TIMEOUT: Duration = Duration::from_secs(600);

/// The tuned planner's default plan timeout: its arm64 p95 is about 1 s, and
/// one hung call holds the planner's single slot (code review M).
pub const TUNED_PLAN_TIMEOUT: Duration = Duration::from_secs(12);

fn flag_of(env_value: &dyn Fn(&str) -> Option<String>, name: &str) -> bool {
    env_value(name).is_some_and(|v| {
        matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "true" | "1" | "yes" | "on"
        )
    })
}

/// The plan timeout: `ASSISTANT_AGENT_PLAN_TIMEOUT_SECS` when set, else
/// [`TUNED_PLAN_TIMEOUT`] for the tuned planner, else the configured
/// default; always inside the run timeout.
fn plan_timeout_for(
    env_value: &dyn Fn(&str) -> Option<String>,
    configured: Duration,
    run_timeout: Duration,
) -> Duration {
    let tuned = env_value("ASSISTANT_AGENT_PLANNER_PROMPT")
        .as_deref()
        .map(str::trim)
        == Some("tuned");
    if env_value("ASSISTANT_AGENT_PLAN_TIMEOUT_SECS").is_none() && tuned {
        plan_within_run(TUNED_PLAN_TIMEOUT, run_timeout)
    } else {
        configured
    }
}

/// The model planner from the environment, or `None`:
/// - every model is off (`ASSISTANT_MODELS_ENABLED=false`, the one kill
///   switch);
/// - neither `ASSISTANT_AI_ENABLED` (the answer model's Ollama) nor
///   `ASSISTANT_AGENT_PLANNER_ENABLED` (the dedicated planner service) is on;
/// - `ASSISTANT_AGENT_PLANNER_PROMPT` is not `few-shot` or `tuned`;
/// - the Ollama settings are invalid.
fn planner_from(
    env_value: &dyn Fn(&str) -> Option<String>,
    config: &AgentConfig,
) -> Option<Arc<dyn PlanProvider>> {
    let killed = !crate::assistant_provider::models_allowed_from(
        env_value("ASSISTANT_MODELS_ENABLED").as_deref(),
    );
    let wanted = flag_of(env_value, "ASSISTANT_AI_ENABLED")
        || flag_of(env_value, "ASSISTANT_AGENT_PLANNER_ENABLED");
    if !config.enabled || killed || !wanted {
        return None;
    }
    let endpoint = env_value("ASSISTANT_AGENT_PLANNER_URL")
        .or_else(|| env_value("ASSISTANT_OLLAMA_URL"))
        .unwrap_or_else(|| "http://127.0.0.1:11434".into());
    let model = env_value("ASSISTANT_AGENT_PLANNER_MODEL")
        .or_else(|| env_value("ASSISTANT_AI_MODEL"))
        .unwrap_or_else(|| crate::assistant_provider::GENERATION_MODEL.into());
    let threads = env_value("ASSISTANT_AGENT_PLANNER_NUM_THREADS")
        .or_else(|| env_value("ASSISTANT_AI_NUM_THREADS"));
    let Some(plan_prompt) = crate::assistant_provider::PlanPrompt::parse(
        &env_value("ASSISTANT_AGENT_PLANNER_PROMPT").unwrap_or_default(),
    ) else {
        tracing::warn!("ASSISTANT_AGENT_PLANNER_PROMPT is not few-shot or tuned; planner off");
        return None;
    };
    let options = OllamaOptions {
        keep_alive: keep_alive_env(),
        allow_any_port: flag_of(env_value, "ASSISTANT_OLLAMA_ALLOW_ANY_PORT"),
        num_threads: mazimi_worker::ollama::parse_num_threads(threads.as_deref()).unwrap_or(0),
        plan_prompt,
    };
    match OllamaProvider::with_options(&endpoint, &model, config.plan_timeout, options) {
        Ok(provider) => {
            Some(Arc::new(provider.with_v3_tools(config.tools.v3_tools())) as Arc<dyn PlanProvider>)
        }
        Err(_) => {
            tracing::warn!("Agent model planner configuration invalid; fast paths only");
            None
        }
    }
}

fn bounded<T: std::str::FromStr + PartialOrd>(name: &str, min: T, max: T, default: T) -> T {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<T>().ok())
        .filter(|v| *v >= min && *v <= max)
        .unwrap_or(default)
}

/// Per read tool.
const TOOL_TIMEOUT: Duration = Duration::from_secs(15);
/// Time a run keeps after the model plan for its tools and message: one
/// tool timeout per step a model plan may have.
const PLAN_HEADROOM: Duration =
    Duration::from_secs(TOOL_TIMEOUT.as_secs() * MAX_MODEL_PLAN_STEPS as u64);

/// The model call ends before the run does, so a slow plan becomes the
/// planner's fallback message, not a run timeout with no message.
fn plan_within_run(plan: Duration, run: Duration) -> Duration {
    plan.min(run.saturating_sub(PLAN_HEADROOM))
        .max(Duration::from_secs(10))
}

impl AgentConfig {
    pub fn from_env() -> Self {
        let run_timeout = Duration::from_secs(bounded(
            "ASSISTANT_AGENT_RUN_TIMEOUT_SECS",
            30,
            MAX_RUN_TIMEOUT.as_secs(),
            300,
        ));
        Self {
            enabled: env_flag("ASSISTANT_AGENT_ENABLED"),
            retention_days: bounded("ASSISTANT_RETENTION_DAYS", 1, 90, 30),
            plan_timeout: plan_within_run(
                Duration::from_secs(bounded("ASSISTANT_AGENT_PLAN_TIMEOUT_SECS", 10, 300, 180)),
                run_timeout,
            ),
            run_timeout,
            background_after: Duration::from_secs(bounded(
                "ASSISTANT_AGENT_BACKGROUND_AFTER_SECS",
                5,
                60,
                20,
            )),
            confirmation_ttl: Duration::from_secs(600),
            max_concurrent_runs: bounded("ASSISTANT_AGENT_MAX_RUNS", 1, 64, 8),
            tool_timeout: TOOL_TIMEOUT,
            tools: ToolFlags::from_env(),
            planner_refusals: env_flag("ASSISTANT_AGENT_PLANNER_REFUSALS"),
        }
    }

    /// Enabled with defaults; for tests and the eval harness.
    pub fn enabled() -> Self {
        Self {
            enabled: true,
            ..Self::disabled()
        }
    }

    pub fn disabled() -> Self {
        Self {
            enabled: false,
            retention_days: 30,
            plan_timeout: Duration::from_secs(180),
            run_timeout: Duration::from_secs(300),
            background_after: Duration::from_secs(20),
            confirmation_ttl: Duration::from_secs(600),
            max_concurrent_runs: 8,
            tool_timeout: TOOL_TIMEOUT,
            tools: ToolFlags::default(),
            planner_refusals: false,
        }
    }
}

#[derive(Default)]
struct Breaker {
    failures: u8,
    open_until: Option<Instant>,
}

/// Why the model planner did not return a plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlannerUnavailable {
    /// No model planner (`ASSISTANT_AI_ENABLED` off or misconfigured).
    Disabled,
    /// The single model slot is in use.
    Busy,
    /// Breaker open after repeated failures.
    Tripped,
    /// Transport failure or timeout.
    Failed,
    /// This user's hourly model-planning budget is spent.
    UserLimit,
}

/// One run's progress events: a replayable log plus live subscribers.
pub struct RunChannel {
    log: Mutex<Vec<Value>>,
    live: broadcast::Sender<Value>,
    cancelled: AtomicBool,
    cancel_signal: Notify,
    finished: Mutex<Option<Instant>>,
    pub started: Instant,
}

impl RunChannel {
    /// Log and broadcast under one lock, so a subscriber sees each event
    /// once: either in the replayed log or live.
    pub fn emit(&self, event: Value) {
        let mut log = self
            .log
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if log.len() < 256 {
            log.push(event.clone());
        }
        let _ = self.live.send(event);
    }

    /// Events so far and a receiver for the rest. Subscribing under the log
    /// lock means no event falls between the two.
    pub fn subscribe(&self) -> (Vec<Value>, broadcast::Receiver<Value>) {
        let log = self
            .log
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (log.clone(), self.live.subscribe())
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.cancel_signal.notify_waiters();
    }

    /// Resolves once the run is cancelled (at once if it already is).
    pub async fn cancelled(&self) {
        loop {
            let signal = self.cancel_signal.notified();
            tokio::pin!(signal);
            // Registered before the check, so a cancel in between wakes it.
            signal.as_mut().enable();
            if self.is_cancelled() {
                return;
            }
            signal.await;
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    pub fn finish(&self) {
        if let Ok(mut finished) = self.finished.lock() {
            finished.get_or_insert_with(Instant::now);
        }
    }

    pub fn is_finished(&self) -> bool {
        self.finished.lock().is_ok_and(|f| f.is_some())
    }
}

pub struct AgentRuntime {
    pub config: AgentConfig,
    planner: Option<Arc<dyn PlanProvider>>,
    assistant: Arc<AssistantRuntime>,
    breaker: Mutex<Breaker>,
    runs: Mutex<HashMap<Uuid, Arc<RunChannel>>>,
    messages: Mutex<HashMap<Uuid, Vec<Instant>>>,
    decisions: Mutex<HashMap<Uuid, Vec<Instant>>>,
    model_plans: Mutex<HashMap<Uuid, Vec<Instant>>>,
    slots: Arc<Semaphore>,
    /// `check_official_page` client, cache and per-user budget.
    pub official_pages: OfficialPages,
    warm: Arc<Mutex<WarmState>>,
    /// Set once the intent classifier has loaded and verified its artefacts.
    classifier: Arc<OnceLock<Arc<IntentClassifier>>>,
    /// `ASSISTANT_AGENT_PLANNER_DIGEST`: the planner model's expected digest
    /// (security review F3). With one, the planner is off until the served
    /// model's digest matches ([`Self::verify_planner_digest`]).
    planner_digest: Option<String>,
    /// [`DigestCheck`] as u8: 0 unknown, 1 match, 2 mismatch.
    digest_state: Arc<std::sync::atomic::AtomicU8>,
}

/// The served planner model against its pin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DigestCheck {
    Match,
    Mismatch,
    /// Ollama did not answer: checked again later.
    Unknown,
}

/// [`check_digest`], stored in `state` (0 unknown, 1 match, 2 mismatch);
/// the first mismatch logs one warning.
async fn record_digest(
    planner: &dyn PlanProvider,
    pin: &str,
    state: &std::sync::atomic::AtomicU8,
) -> DigestCheck {
    let check = check_digest(planner, pin).await;
    let code = match check {
        DigestCheck::Match => 1,
        DigestCheck::Mismatch => 2,
        DigestCheck::Unknown => 0,
    };
    if state.swap(code, Ordering::SeqCst) != 2 && check == DigestCheck::Mismatch {
        tracing::warn!(
            "Agent planner model digest does not match ASSISTANT_AGENT_PLANNER_DIGEST; planner off"
        );
    }
    check
}

/// Compare the served model's digest with `pin` (`sha256:` optional on either).
pub async fn check_digest(planner: &dyn PlanProvider, pin: &str) -> DigestCheck {
    let bare = |d: &str| d.trim().trim_start_matches("sha256:").to_ascii_lowercase();
    match planner.model_digest().await {
        Ok(Some(served)) if bare(&served) == bare(pin) => DigestCheck::Match,
        Ok(_) => DigestCheck::Mismatch,
        Err(_) => DigestCheck::Unknown,
    }
}

/// Warm-up keeper tick.
const WARM_TICK: Duration = Duration::from_secs(30);
/// Refresh this long before Ollama's `keep_alive` would unload the model.
const WARM_REFRESH_MARGIN: Duration = Duration::from_secs(120);
/// Restore the prefix only after the answer slot has been quiet this long,
/// so a warm-up does not take the slot between a user's turns.
const WARM_AFTER_ANSWER: Duration = Duration::from_secs(30);
/// Retry a failed warm-up (Ollama still starting, model missing) after
/// this, doubling per consecutive failure up to [`WARM_RETRY_MAX`].
const WARM_RETRY: Duration = Duration::from_secs(120);
const WARM_RETRY_MAX: Duration = Duration::from_secs(3600);
/// One warm-up's own bound, shorter than a plan's: a stalled Ollama cannot
/// hold the shared model slot for the whole plan timeout. A cold load on
/// the production CPU host is ~50 s.
const WARM_TIMEOUT: Duration = Duration::from_secs(90);

/// When the planner's model and prefix were last known warm.
#[derive(Debug, Default, Clone, Copy)]
struct WarmState {
    /// The last plan or warm-up the model served.
    touched: Option<Instant>,
    /// After a failed warm-up, not before this.
    retry_at: Option<Instant>,
    /// Consecutive failed warm-ups (backoff exponent).
    failures: u32,
}

/// The wait after `failures` consecutive failed warm-ups: 2, 4, 8 … min,
/// capped at an hour.
fn warm_backoff(failures: u32) -> Duration {
    WARM_RETRY
        .saturating_mul(1u32 << failures.saturating_sub(1).min(16))
        .min(WARM_RETRY_MAX)
}

/// A `keep_alive` too short to refresh before it expires ("0s", "1m",
/// "2m"): the keeper would reload the model on every tick.
fn keeper_useful(keep_alive: Option<Duration>) -> bool {
    keep_alive.is_none_or(|k| k >= WARM_REFRESH_MARGIN + WARM_TICK)
}

/// `ASSISTANT_OLLAMA_KEEP_ALIVE`, shared with the answer stream.
fn keep_alive_env() -> String {
    std::env::var("ASSISTANT_OLLAMA_KEEP_ALIVE")
        .ok()
        .filter(|v| mazimi_worker::ollama::valid_keep_alive(v))
        .unwrap_or_else(|| crate::assistant_provider::DEFAULT_KEEP_ALIVE.into())
}

/// Ollama `keep_alive` as a duration; `None` for `-1` (never unloads).
fn keep_alive_duration(value: &str) -> Option<Duration> {
    let value = value.trim();
    let (digits, unit) = value.split_at(value.find(|c: char| !c.is_ascii_digit())?);
    let n: u64 = digits.parse().ok()?;
    Some(Duration::from_secs(match unit {
        "s" => n,
        "m" => n * 60,
        "h" => n * 3600,
        _ => return None,
    }))
}

/// Whether the keeper should warm the planner now: never warmed, about to
/// be unloaded by `keep_alive`, or an answer stream ran since the last plan
/// (it evicted the planner's cached prefix) and has been quiet a while.
fn warm_due(
    now: Instant,
    state: WarmState,
    last_answer: Option<Instant>,
    keep_alive: Option<Duration>,
) -> bool {
    if state.retry_at.is_some_and(|at| now < at) {
        return false;
    }
    let Some(touched) = state.touched else {
        return true;
    };
    let expiring = keep_alive.is_some_and(|k| {
        now.duration_since(touched) >= k.saturating_sub(WARM_REFRESH_MARGIN).max(WARM_TICK)
    });
    let evicted =
        last_answer.is_some_and(|at| at > touched && now.duration_since(at) >= WARM_AFTER_ANSWER);
    expiring || evicted
}

/// One warm-up, holding the shared model slot so it never runs beside a
/// user's plan or answer (a busy slot skips this tick). A failure is
/// logged and retried later; it does not count toward the breaker, so a
/// slow-starting Ollama cannot lock users out.
async fn warm_once(
    planner: &dyn PlanProvider,
    assistant: &AssistantRuntime,
    warm: &Mutex<WarmState>,
    timeout: Duration,
) -> bool {
    let Some(_slot) = assistant.try_model_slot() else {
        return false;
    };
    let ok = matches!(
        tokio::time::timeout(timeout, planner.warm_up()).await,
        Ok(Ok(()))
    );
    let now = Instant::now();
    if let Ok(mut state) = warm.lock() {
        if ok {
            *state = WarmState {
                touched: Some(now),
                retry_at: None,
                failures: 0,
            };
        } else {
            state.failures = state.failures.saturating_add(1);
            state.retry_at = Some(now + warm_backoff(state.failures));
        }
    }
    if !ok {
        tracing::warn!("Agent planner warm-up failed; retrying later");
    }
    ok
}

fn admit(map: &Mutex<HashMap<Uuid, Vec<Instant>>>, user: Uuid, limit: usize) -> bool {
    admit_within(map, user, limit, Duration::from_secs(60))
}

fn admit_within(
    map: &Mutex<HashMap<Uuid, Vec<Instant>>>,
    user: Uuid,
    limit: usize,
    window: Duration,
) -> bool {
    let now = Instant::now();
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

impl AgentRuntime {
    pub fn new(
        config: AgentConfig,
        planner: Option<Arc<dyn PlanProvider>>,
        assistant: Arc<AssistantRuntime>,
    ) -> Self {
        let slots = Arc::new(Semaphore::new(config.max_concurrent_runs));
        Self {
            config,
            planner,
            assistant,
            breaker: Mutex::new(Breaker::default()),
            runs: Mutex::new(HashMap::new()),
            messages: Mutex::new(HashMap::new()),
            decisions: Mutex::new(HashMap::new()),
            model_plans: Mutex::new(HashMap::new()),
            slots,
            official_pages: OfficialPages::new(None),
            warm: Arc::new(Mutex::new(WarmState::default())),
            classifier: Arc::new(OnceLock::new()),
            planner_digest: None,
            digest_state: Arc::new(std::sync::atomic::AtomicU8::new(0)),
        }
    }

    /// Pin the planner model's digest (security review F3).
    pub fn with_planner_digest(mut self, digest: Option<String>) -> Self {
        self.planner_digest = digest.filter(|d| !d.trim().is_empty());
        self
    }

    /// Check the served planner model against the pin once; a mismatch
    /// turns the planner off for good and logs one warning.
    pub async fn verify_planner_digest(&self) -> DigestCheck {
        match (&self.planner, &self.planner_digest) {
            (Some(planner), Some(pin)) => {
                record_digest(planner.as_ref(), pin, &self.digest_state).await
            }
            _ => DigestCheck::Match,
        }
    }

    /// Verify the planner digest in the background until it matches or
    /// mismatches (an unreachable Ollama is retried every 30 s).
    fn spawn_digest_check(&self) {
        let (Some(planner), Some(pin), Ok(handle)) = (
            self.planner.clone(),
            self.planner_digest.clone(),
            tokio::runtime::Handle::try_current(),
        ) else {
            return;
        };
        let state = self.digest_state.clone();
        handle.spawn(async move {
            while record_digest(planner.as_ref(), &pin, &state).await == DigestCheck::Unknown {
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
        });
    }

    /// Route with this intent classifier (tests and tools; production loads
    /// it from the environment in [`Self::from_env`]).
    pub fn with_classifier(self, classifier: Arc<IntentClassifier>) -> Self {
        let _ = self.classifier.set(classifier);
        self
    }

    /// The live official-page check through this client (tests, or the
    /// MCP client from the environment).
    pub fn with_live_check(mut self, checker: Option<Arc<dyn LiveCheck>>) -> Self {
        self.official_pages = OfficialPages::new(checker);
        self
    }

    /// The intent classifier, once loaded (`ASSISTANT_INTENT_CLASSIFIER_ENABLED`).
    pub fn classifier(&self) -> Option<Arc<IntentClassifier>> {
        self.classifier.get().cloned()
    }

    /// Load the classifier on a background thread: hashing ~490 MB and
    /// reading ~85 MB of weights takes a second or two, and the API serves
    /// meanwhile with the classifier off. A missing or mismatched artefact
    /// is logged once and leaves it off; routing is then what it was.
    fn spawn_classifier_load(&self) {
        let slot = self.classifier.clone();
        let dir = std::env::var("ASSISTANT_INTENT_CLASSIFIER_DIR")
            .unwrap_or_else(|_| classifier::DEFAULT_DIR.into());
        let cores = std::thread::available_parallelism().map_or(1, usize::from);
        let threads = bounded("ASSISTANT_INTENT_CLASSIFIER_THREADS", 1, 16, 2).min(cores);
        let threshold = std::env::var("ASSISTANT_INTENT_CLASSIFIER_THRESHOLD")
            .ok()
            .and_then(|v| v.trim().parse::<f64>().ok())
            .filter(|t| (0.5..=0.99).contains(t));
        let spawned = std::thread::Builder::new()
            .name("intent-classifier-load".into())
            .spawn(move || {
                let started = Instant::now();
                match IntentClassifier::load(&ArtefactPaths::in_dir(dir.as_ref()), threads) {
                    Ok(loaded) => {
                        let loaded = match threshold {
                            Some(t) => loaded.with_threshold(t),
                            None => loaded,
                        };
                        tracing::info!(
                            version = loaded.version(),
                            threshold = loaded.threshold(),
                            threads,
                            load_ms = started.elapsed().as_millis() as u64,
                            "Agent intent classifier loaded"
                        );
                        let _ = slot.set(Arc::new(loaded));
                    }
                    Err(error) => tracing::warn!(
                        %error,
                        "Agent intent classifier disabled; routing falls back to rules and model"
                    ),
                }
            });
        if spawned.is_err() {
            tracing::warn!("Agent intent classifier loader could not start; classifier off");
        }
    }

    /// Keep the planner's model loaded and its prompt prefix cached, so a
    /// user's first plan is not a cold load (~50 s on the production CPU
    /// host): warm on start, refresh before `keep_alive` unloads the model,
    /// and restore the prefix after an answer stream evicted it. With this
    /// on, the model stays resident while the API runs (see the contract's
    /// cold-start section for the RAM and CPU cost).
    pub fn spawn_warm_keeper(&self, keep_alive: Option<Duration>) {
        let (Some(planner), Ok(handle)) =
            (self.planner.clone(), tokio::runtime::Handle::try_current())
        else {
            return;
        };
        if !keeper_useful(keep_alive) {
            tracing::warn!(
                "ASSISTANT_OLLAMA_KEEP_ALIVE is shorter than the warm-up refresh window; planner warm-up off"
            );
            return;
        }
        let assistant = self.assistant.clone();
        let warm = self.warm.clone();
        let timeout = WARM_TIMEOUT.min(self.config.plan_timeout);
        handle.spawn(async move {
            let mut tick = tokio::time::interval(WARM_TICK);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                let state = warm.lock().map(|s| *s).unwrap_or_default();
                if warm_due(
                    Instant::now(),
                    state,
                    assistant.last_generation_at(),
                    keep_alive,
                ) {
                    warm_once(planner.as_ref(), &assistant, &warm, timeout).await;
                }
            }
        });
    }

    /// Flags from the environment. The model planner: [`planner_from`].
    pub fn from_env(assistant: Arc<AssistantRuntime>) -> Self {
        let mut config = AgentConfig::from_env();
        let env_value = |name: &str| std::env::var(name).ok();
        config.plan_timeout = plan_timeout_for(&env_value, config.plan_timeout, config.run_timeout);
        let planner = planner_from(&env_value, &config);
        let keep_alive = planner.is_some().then(keep_alive_env);
        let classifier_on = config.enabled
            && crate::assistant_provider::models_allowed()
            && env_flag("ASSISTANT_INTENT_CLASSIFIER_ENABLED");
        let mut runtime = Self::new(config, planner, assistant)
            .with_planner_digest(std::env::var("ASSISTANT_AGENT_PLANNER_DIGEST").ok());
        runtime.spawn_digest_check();
        runtime.official_pages = OfficialPages::from_env();
        if classifier_on {
            runtime.spawn_classifier_load();
        }
        if runtime.config.enabled && super::intent::understanding_on() {
            spawn_understanding_load();
        }
        // On by default with the model planner; `ASSISTANT_AGENT_WARMUP=false`
        // leaves loading to the first plan and unloading to `keep_alive`.
        let warm_up = std::env::var("ASSISTANT_AGENT_WARMUP").map_or(true, |v| {
            !matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "false" | "0" | "no" | "off"
            )
        });
        if let (Some(keep_alive), true) = (keep_alive, warm_up) {
            runtime.spawn_warm_keeper(keep_alive_duration(&keep_alive));
        }
        runtime
    }

    pub fn enabled(&self) -> bool {
        self.config.enabled
    }

    pub fn model_planner_available(&self) -> bool {
        let pinned_ok =
            self.planner_digest.is_none() || self.digest_state.load(Ordering::SeqCst) == 1;
        pinned_ok
            && self.planner.is_some()
            && self
                .breaker
                .lock()
                .is_ok_and(|b| b.open_until.is_none_or(|until| until <= Instant::now()))
    }

    pub fn planner_model(&self) -> Option<String> {
        self.planner.as_ref().map(|p| p.model().to_owned())
    }

    /// The prompt version the model planner sends (run records).
    pub fn planner_prompt_version(&self, v3_tools: &[mazimi_core::agent::ToolName]) -> String {
        self.planner.as_ref().map_or_else(
            || crate::assistant_provider::plan_prompt_version(v3_tools),
            |p| p.prompt_version(v3_tools),
        )
    }

    /// One model planning run ([`planner::plan`]: a call plus at most one
    /// repair call): per-user hourly budget, shared slot held for both
    /// calls, one timeout for both, breaker. Never waits for the slot.
    /// `goal` must already be masked.
    pub async fn plan_with_model(
        &self,
        user: Uuid,
        goal: &str,
        context: GoalContext,
    ) -> Result<Result<ModelOutcome, PlanError>, PlannerUnavailable> {
        let planner = self.planner.clone().ok_or(PlannerUnavailable::Disabled)?;
        if !self.model_planner_available() {
            return Err(PlannerUnavailable::Tripped);
        }
        // The slot first: a busy slot does not spend the hourly budget.
        let _slot = self
            .assistant
            .try_model_slot()
            .ok_or(PlannerUnavailable::Busy)?;
        if !admit_within(
            &self.model_plans,
            user,
            MODEL_PLANS_PER_HOUR,
            Duration::from_secs(3600),
        ) {
            return Err(PlannerUnavailable::UserLimit);
        }
        let run = tokio::time::timeout(
            self.config.plan_timeout,
            planner::plan_with_tools(
                planner.as_ref(),
                goal,
                context,
                &self.config.tools.v3_tools(),
            ),
        )
        .await;
        // A plan that reached the model refreshed its keep-alive and prefix.
        if matches!(&run, Ok(r) if !matches!(r.outcome, Err(ModelFailure::Provider(_)))) {
            if let Ok(mut state) = self.warm.lock() {
                state.touched = Some(Instant::now());
            }
        }
        match run.map(|r| r.outcome) {
            Ok(Ok(outcome)) => {
                self.record(true);
                Ok(Ok(outcome))
            }
            // The model answered: not an outage, not counted.
            Ok(Err(ModelFailure::Invalid(error))) => {
                self.record(true);
                Ok(Err(error))
            }
            Ok(Err(ModelFailure::Provider(_))) | Err(_) => {
                self.record(false);
                Err(PlannerUnavailable::Failed)
            }
        }
    }

    /// Record a transport outcome. Plans that parse but fail validation are
    /// not counted: one user's odd goals must not open the shared breaker.
    fn record(&self, ok: bool) {
        if let Ok(mut breaker) = self.breaker.lock() {
            if ok {
                *breaker = Breaker::default();
            } else {
                breaker.failures = breaker.failures.saturating_add(1);
                if breaker.failures >= BREAKER_THRESHOLD {
                    breaker.open_until = Some(Instant::now() + BREAKER_OPEN);
                }
            }
        }
    }

    pub fn admit_message(&self, user: Uuid) -> bool {
        admit(&self.messages, user, MESSAGES_PER_MINUTE)
    }

    pub fn admit_decision(&self, user: Uuid) -> bool {
        admit(&self.decisions, user, DECISIONS_PER_MINUTE)
    }

    pub fn try_run_slot(&self) -> Option<OwnedSemaphorePermit> {
        self.slots.clone().try_acquire_owned().ok()
    }

    pub fn register(&self, run: Uuid) -> Arc<RunChannel> {
        let (live, _) = broadcast::channel(64);
        let channel = Arc::new(RunChannel {
            log: Mutex::new(Vec::new()),
            live,
            cancelled: AtomicBool::new(false),
            cancel_signal: Notify::new(),
            finished: Mutex::new(None),
            started: Instant::now(),
        });
        let mut runs = self
            .runs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        runs.retain(|_, c| {
            c.finished
                .lock()
                .map(|f| f.is_none_or(|at| at.elapsed() < FINISHED_TTL))
                .unwrap_or(false)
        });
        runs.insert(run, channel.clone());
        channel
    }

    pub fn channel(&self, run: Uuid) -> Option<Arc<RunChannel>> {
        self.runs
            .lock()
            .ok()
            .and_then(|runs| runs.get(&run).cloned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: std::collections::HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k: &str| map.get(k).cloned()
    }

    /// Code review M: one lever stops every model call; an unknown prompt
    /// value is no planner, not a provider with zero threads.
    #[test]
    fn the_kill_switch_and_an_unknown_prompt_give_no_planner() {
        let config = AgentConfig::enabled();
        let tuned = [
            ("ASSISTANT_AGENT_PLANNER_ENABLED", "true"),
            ("ASSISTANT_AGENT_PLANNER_PROMPT", "tuned"),
        ];
        assert!(planner_from(&env(&tuned), &config).is_some());
        let mut killed = tuned.to_vec();
        killed.push(("ASSISTANT_MODELS_ENABLED", "false"));
        assert!(planner_from(&env(&killed), &config).is_none());
        let ai_killed = [
            ("ASSISTANT_AI_ENABLED", "true"),
            ("ASSISTANT_MODELS_ENABLED", "off"),
        ];
        assert!(planner_from(&env(&ai_killed), &config).is_none());
        let unknown = [
            ("ASSISTANT_AGENT_PLANNER_ENABLED", "true"),
            ("ASSISTANT_AGENT_PLANNER_PROMPT", "tunned"),
        ];
        assert!(planner_from(&env(&unknown), &config).is_none());
        assert!(planner_from(&env(&[]), &config).is_none());
    }

    /// Code review M: the tuned planner's plan timeout is 12 s unless set.
    #[test]
    fn the_tuned_planner_times_out_in_twelve_seconds() {
        let run = Duration::from_secs(300);
        let configured = Duration::from_secs(180);
        assert_eq!(
            plan_timeout_for(
                &env(&[("ASSISTANT_AGENT_PLANNER_PROMPT", "tuned")]),
                configured,
                run
            ),
            TUNED_PLAN_TIMEOUT
        );
        assert_eq!(
            plan_timeout_for(
                &env(&[
                    ("ASSISTANT_AGENT_PLANNER_PROMPT", "tuned"),
                    ("ASSISTANT_AGENT_PLAN_TIMEOUT_SECS", "30")
                ]),
                configured,
                run
            ),
            configured
        );
        assert_eq!(plan_timeout_for(&env(&[]), configured, run), configured);
    }
    use crate::assistant_provider::{PlanRequest, ProviderError};
    use async_trait::async_trait;

    struct Script(Result<&'static str, ProviderError>);
    #[async_trait]
    impl PlanProvider for Script {
        async fn plan(&self, _: &PlanRequest) -> Result<String, ProviderError> {
            self.0.clone().map(str::to_owned)
        }
        fn model(&self) -> &str {
            "script"
        }
    }

    const GOAL: &str = "목표";
    const NONE: GoalContext = GoalContext {
        has_program: false,
        selected: 0,
        saved: 0,
        suspect: false,
        handoffs: false,
    };

    #[tokio::test]
    async fn planner_is_off_without_a_provider_and_trips_after_failures() {
        let assistant = Arc::new(AssistantRuntime::disabled());
        let off = AgentRuntime::new(AgentConfig::enabled(), None, assistant.clone());
        assert!(!off.model_planner_available());
        assert_eq!(
            off.plan_with_model(Uuid::nil(), GOAL, NONE).await,
            Err(PlannerUnavailable::Disabled)
        );
        let failing = AgentRuntime::new(
            AgentConfig::enabled(),
            Some(Arc::new(Script(Err(ProviderError::Unavailable)))),
            assistant.clone(),
        );
        for _ in 0..3 {
            assert_eq!(
                failing.plan_with_model(Uuid::nil(), GOAL, NONE).await,
                Err(PlannerUnavailable::Failed)
            );
        }
        assert_eq!(
            failing.plan_with_model(Uuid::nil(), GOAL, NONE).await,
            Err(PlannerUnavailable::Tripped)
        );
        let ok = AgentRuntime::new(
            AgentConfig::enabled(),
            Some(Arc::new(Script(Ok("{\"steps\":[]}")))),
            assistant.clone(),
        );
        // The answer stream holds the slot: the planner refuses, not waits.
        let held = assistant.try_model_slot().unwrap();
        assert_eq!(
            ok.plan_with_model(Uuid::nil(), GOAL, NONE).await,
            Err(PlannerUnavailable::Busy)
        );
        drop(held);
        assert_eq!(
            ok.plan_with_model(Uuid::nil(), GOAL, NONE).await,
            Ok(Ok(ModelOutcome::Refuse))
        );
    }

    #[tokio::test]
    async fn invalid_or_cut_off_model_output_never_opens_the_breaker() {
        let assistant = Arc::new(AssistantRuntime::disabled());
        for reply in [Ok("not json"), Err(ProviderError::InvalidOutput)] {
            let runtime = AgentRuntime::new(
                AgentConfig::enabled(),
                Some(Arc::new(Script(reply))),
                assistant.clone(),
            );
            for user in 0..5u128 {
                assert_eq!(
                    runtime
                        .plan_with_model(Uuid::from_u128(user), GOAL, NONE)
                        .await,
                    Ok(Err(PlanError::InvalidJson))
                );
            }
            assert!(runtime.model_planner_available(), "breaker stays closed");
        }
    }

    #[test]
    fn the_model_plan_ends_inside_the_run() {
        let s = Duration::from_secs;
        assert_eq!(plan_within_run(s(180), s(300)), s(180), "defaults");
        assert_eq!(PLAN_HEADROOM, s(60), "4 steps x 15 s");
        assert_eq!(plan_within_run(s(300), s(120)), s(60));
        assert_eq!(plan_within_run(s(300), s(30)), s(10), "floor");
        let defaults = AgentConfig::disabled();
        assert!(defaults.plan_timeout + PLAN_HEADROOM <= defaults.run_timeout);
    }

    #[test]
    fn admission_is_per_user() {
        let runtime = AgentRuntime::new(
            AgentConfig::enabled(),
            None,
            Arc::new(AssistantRuntime::disabled()),
        );
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        for _ in 0..MESSAGES_PER_MINUTE {
            assert!(runtime.admit_message(a));
        }
        assert!(!runtime.admit_message(a));
        assert!(runtime.admit_message(b));
        assert!(runtime.admit_decision(a), "separate budget");
    }

    #[tokio::test]
    async fn late_subscribers_replay_the_log() {
        let runtime = AgentRuntime::new(
            AgentConfig::enabled(),
            None,
            Arc::new(AssistantRuntime::disabled()),
        );
        let run = Uuid::new_v4();
        let channel = runtime.register(run);
        channel.emit(serde_json::json!({"type":"status","status":"planning"}));
        let (log, mut live) = runtime.channel(run).unwrap().subscribe();
        assert_eq!(log.len(), 1);
        channel.emit(serde_json::json!({"type":"done"}));
        assert_eq!(live.recv().await.unwrap()["type"], "done");
        channel.cancel();
        assert!(channel.is_cancelled());
    }

    #[tokio::test]
    async fn cancelled_wakes_on_cancel_and_returns_at_once_after() {
        let runtime = AgentRuntime::new(
            AgentConfig::enabled(),
            None,
            Arc::new(AssistantRuntime::disabled()),
        );
        let channel = runtime.register(Uuid::new_v4());
        let waiter = {
            let channel = channel.clone();
            tokio::spawn(async move { channel.cancelled().await })
        };
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());
        channel.cancel();
        tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("woken by cancel")
            .unwrap();
        tokio::time::timeout(Duration::from_millis(50), channel.cancelled())
            .await
            .expect("already cancelled");
    }

    #[tokio::test]
    async fn a_busy_slot_does_not_spend_the_hourly_budget() {
        let assistant = Arc::new(AssistantRuntime::disabled());
        let runtime = AgentRuntime::new(
            AgentConfig::enabled(),
            Some(Arc::new(Script(Ok("{\"steps\":[]}")))),
            assistant.clone(),
        );
        let held = assistant.try_model_slot().unwrap();
        for _ in 0..MODEL_PLANS_PER_HOUR + 2 {
            assert_eq!(
                runtime.plan_with_model(Uuid::nil(), GOAL, NONE).await,
                Err(PlannerUnavailable::Busy)
            );
        }
        drop(held);
        assert_eq!(
            runtime.plan_with_model(Uuid::nil(), GOAL, NONE).await,
            Ok(Ok(ModelOutcome::Refuse))
        );
    }

    #[test]
    fn keep_alive_parses_to_a_refresh_horizon() {
        assert_eq!(keep_alive_duration("30m"), Some(Duration::from_secs(1800)));
        assert_eq!(keep_alive_duration("600s"), Some(Duration::from_secs(600)));
        assert_eq!(keep_alive_duration("2h"), Some(Duration::from_secs(7200)));
        assert_eq!(keep_alive_duration("-1"), None);
    }

    #[test]
    fn warm_up_is_due_cold_before_expiry_and_after_an_answer() {
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let thirty = Some(Duration::from_secs(1800));
        let warm = |touched: u64| WarmState {
            touched: Some(at(touched)),
            retry_at: None,
            failures: 0,
        };
        // Never warmed: due, unless a failure is waiting to retry.
        assert!(warm_due(at(0), WarmState::default(), None, thirty));
        assert!(!warm_due(
            at(10),
            WarmState {
                touched: None,
                retry_at: Some(at(60)),
                failures: 1,
            },
            None,
            thirty
        ));
        // Refreshed two minutes before keep_alive would unload it.
        assert!(!warm_due(at(1000), warm(0), None, thirty));
        assert!(warm_due(at(1680), warm(0), None, thirty));
        // "-1" never unloads.
        assert!(!warm_due(at(100_000), warm(0), None, None));
        // An answer stream after the last plan evicted the prefix: restore
        // it once the answer slot has been quiet a while.
        assert!(!warm_due(at(110), warm(0), Some(at(100)), thirty));
        assert!(warm_due(at(130), warm(0), Some(at(100)), thirty));
        assert!(!warm_due(at(130), warm(120), Some(at(100)), thirty));
    }

    struct Digest(Result<Option<String>, ProviderError>);
    #[async_trait]
    impl PlanProvider for Digest {
        async fn plan(&self, _: &PlanRequest) -> Result<String, ProviderError> {
            Err(ProviderError::Unavailable)
        }
        fn model(&self) -> &str {
            "digest"
        }
        async fn model_digest(&self) -> Result<Option<String>, ProviderError> {
            self.0.clone()
        }
    }

    /// Security review F3: with ASSISTANT_AGENT_PLANNER_DIGEST set the
    /// planner is off until the served model's digest matches; a mismatch
    /// keeps it off for good, an unreachable Ollama is checked again.
    #[tokio::test]
    async fn a_planner_digest_mismatch_keeps_the_planner_off() {
        let ok = Digest(Ok(Some("sha256:abc".into())));
        assert_eq!(check_digest(&ok, "abc").await, DigestCheck::Match);
        assert_eq!(check_digest(&ok, "sha256:abc").await, DigestCheck::Match);
        assert_eq!(check_digest(&ok, "def").await, DigestCheck::Mismatch);
        assert_eq!(
            check_digest(&Digest(Ok(None)), "abc").await,
            DigestCheck::Mismatch
        );
        assert_eq!(
            check_digest(&Digest(Err(ProviderError::Unavailable)), "abc").await,
            DigestCheck::Unknown
        );
        let planner: Arc<dyn PlanProvider> = Arc::new(Digest(Ok(Some("abc".into()))));
        let pinned = AgentRuntime::new(
            AgentConfig::enabled(),
            Some(planner.clone()),
            Arc::new(AssistantRuntime::disabled()),
        )
        .with_planner_digest(Some("abc".into()));
        assert!(!pinned.model_planner_available(), "unverified: off");
        pinned.verify_planner_digest().await;
        assert!(pinned.model_planner_available());
        let wrong = AgentRuntime::new(
            AgentConfig::enabled(),
            Some(planner),
            Arc::new(AssistantRuntime::disabled()),
        )
        .with_planner_digest(Some("zzz".into()));
        wrong.verify_planner_digest().await;
        assert!(!wrong.model_planner_available());
    }

    struct Warming(std::sync::atomic::AtomicUsize, bool);
    #[async_trait]
    impl PlanProvider for Warming {
        async fn plan(&self, _: &PlanRequest) -> Result<String, ProviderError> {
            Err(ProviderError::Unavailable)
        }
        fn model(&self) -> &str {
            "warming"
        }
        async fn warm_up(&self) -> Result<(), ProviderError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            if self.1 {
                Ok(())
            } else {
                Err(ProviderError::Unavailable)
            }
        }
    }

    #[tokio::test]
    async fn warm_up_takes_the_model_slot_and_records_its_outcome() {
        let assistant = AssistantRuntime::disabled();
        let warm = Mutex::new(WarmState::default());
        let ok = Warming(Default::default(), true);
        // A user holds the slot: the tick is skipped, nothing is sent.
        let held = assistant.try_model_slot().unwrap();
        assert!(!warm_once(&ok, &assistant, &warm, Duration::from_secs(1)).await);
        assert_eq!(ok.0.load(Ordering::SeqCst), 0);
        drop(held);
        assert!(warm_once(&ok, &assistant, &warm, Duration::from_secs(1)).await);
        assert_eq!(ok.0.load(Ordering::SeqCst), 1);
        assert!(warm.lock().unwrap().touched.is_some());
        assert!(assistant.try_model_slot().is_some(), "slot released");
        // A failure waits to retry and does not touch the breaker.
        let failing = Warming(Default::default(), false);
        let cold = Mutex::new(WarmState::default());
        assert!(!warm_once(&failing, &assistant, &cold, Duration::from_secs(1)).await);
        let state = *cold.lock().unwrap();
        assert!(state.touched.is_none() && state.retry_at.is_some());
        assert!(!warm_due(Instant::now(), state, None, None));
    }

    #[test]
    fn short_keep_alive_disables_the_keeper_and_failures_back_off() {
        for (value, useful) in [
            ("0s", false),
            ("1m", false),
            ("2m", false),
            ("149s", false),
            ("150s", true),
            ("30m", true),
            ("-1", true),
        ] {
            assert_eq!(keeper_useful(keep_alive_duration(value)), useful, "{value}");
        }
        assert_eq!(warm_backoff(1), Duration::from_secs(120));
        assert_eq!(warm_backoff(2), Duration::from_secs(240));
        assert_eq!(warm_backoff(3), Duration::from_secs(480));
        assert_eq!(warm_backoff(40), WARM_RETRY_MAX);
        assert!(WARM_TIMEOUT < AgentConfig::disabled().plan_timeout);
    }

    #[tokio::test]
    async fn repeated_warm_up_failures_wait_longer() {
        let assistant = AssistantRuntime::disabled();
        let failing = Warming(Default::default(), false);
        let warm = Mutex::new(WarmState::default());
        let mut waits = Vec::new();
        for _ in 0..3 {
            let before = Instant::now();
            warm_once(&failing, &assistant, &warm, Duration::from_secs(1)).await;
            let state = *warm.lock().unwrap();
            waits.push(state.retry_at.unwrap().duration_since(before));
        }
        assert!(waits[0] < waits[1] && waits[1] < waits[2], "{waits:?}");
        assert_eq!(warm.lock().unwrap().failures, 3);
    }
}
