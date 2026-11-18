//! Extraction batch: select programs that lack structured facts (nightly)
//! or whose source text changed (the ingestion queue, first in every run),
//! skip unchanged sources, call the provider once per (program source,
//! provider, model, prompt, schema), store validated per-fact candidates with
//! their rule cross-check and supersede candidates of an outdated source.
//! Public program data changes only through [`super::apply`]: by an admin,
//! or by [`super::auto`] for a rule-verified fact when auto-apply is on. A
//! provider failure leaves the rule result in place.
use super::auto::{auto_apply_program, AutoApplyConfig, AutoCounters};
use super::crosscheck::cross_check;
use super::facts::{Confidence, FactType};
use super::provider::{
    build_request, ExtractionProvider, OllamaExtractionProvider, ProviderFailure,
};
use super::source::{build_source, ExtractionSource, ProgramText, RuleCoverage};
use super::validate::{process_output, ProcessedFact};
use super::{DEFAULT_MODEL, DEFAULT_URL, PROMPT_VERSION, SCHEMA_VERSION};
use crate::ollama::{default_num_threads, MAX_NUM_THREADS};
use crate::operations::{self, EventCommand, Severity};
use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use futures_util::FutureExt;
use serde::Serialize;
use serde_json::{json, Value};
use sqlx::{FromRow, PgPool, Postgres, Transaction};
use std::{panic::AssertUnwindSafe, time::Duration, time::Instant};
use uuid::Uuid;

/// Program types with their own curation flows (brand verification, FSS
/// disclosures); the eligibility-fact contract does not apply to them.
const EXCLUDED_PROGRAM_TYPES: &[&str] = &["corporate_benefit", "financial_product"];
const PRIORITY_PROGRAM_TYPES: &str = "'policy', 'scholarship', 'support'";
/// Consecutive transport failures that open the breaker for the rest of
/// the run.
pub const BREAKER_UNAVAILABLE: usize = 3;
/// Consecutive contract-violating answers that open the breaker.
pub const BREAKER_INVALID: usize = 5;
const EVENT_FAILURES: &str = "ai_extraction:provider_failures";
const EVENT_BREAKER: &str = "ai_extraction:circuit_open";
const EVENT_AUTO_SPIKE: &str = "ai_extraction:auto_apply_spike";

/// What started a run (stored on the run row).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    /// The nightly sweep: queued programs first, then programs with gaps.
    Nightly,
    /// The hourly drain of programs whose source text changed.
    Queue,
    Manual,
}

impl Trigger {
    fn as_str(self) -> &'static str {
        match self {
            Self::Nightly => "nightly",
            Self::Queue => "queue",
            Self::Manual => "manual",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Call the provider for programs without a cached answer.
    Extract,
    /// Re-validate and re-store cached answers only. No model call
    /// (`NORMALIZE_ONLY` replay after a validator or guard change).
    NormalizeOnly,
}

impl Mode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Extract => "extract",
            Self::NormalizeOnly => "normalize_only",
        }
    }
}

#[derive(Debug, Clone)]
pub struct JobConfig {
    pub mode: Mode,
    /// Programs processed (provider calls, cache hits, failures) per run.
    pub max_programs: i64,
    /// A failed extraction is not retried before this has passed.
    pub failure_cooldown: chrono::Duration,
    /// A program is started only if `elapsed + call_timeout` still fits in
    /// this budget, so the run ends before the daily job's 30-minute cap.
    pub time_budget: Duration,
    /// The provider's request timeout (worst case of one call).
    pub call_timeout: Duration,
    /// Restrict the batch to these programs (targeted re-extraction, tests).
    pub only_programs: Option<Vec<Uuid>>,
    /// Only programs in the ingestion queue (the hourly drain).
    pub queue_only: bool,
    pub trigger: Trigger,
    /// Rule-verified auto-apply; off by default.
    pub auto_apply: AutoApplyConfig,
}

impl Default for JobConfig {
    fn default() -> Self {
        Self {
            mode: Mode::Extract,
            max_programs: 8,
            failure_cooldown: chrono::Duration::hours(6),
            time_budget: Duration::from_secs(25 * 60),
            call_timeout: Duration::from_secs(180),
            only_programs: None,
            queue_only: false,
            trigger: Trigger::Manual,
            auto_apply: AutoApplyConfig::default(),
        }
    }
}

fn env_flag(name: &str) -> bool {
    matches!(
        std::env::var(name)
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "true" | "1" | "yes" | "on"
    )
}

fn bounded_env(name: &str, default: u64, min: u64, max: u64) -> Result<u64> {
    let value = match std::env::var(name) {
        Ok(value) if !value.trim().is_empty() => value
            .trim()
            .parse::<u64>()
            .with_context(|| format!("{name} must be an integer"))?,
        _ => default,
    };
    if !(min..=max).contains(&value) {
        bail!("{name} must be between {min} and {max}");
    }
    Ok(value)
}

/// `None` when `EXTRACTION_AI_ENABLED` is not set: the safe default.
pub fn config_from_env() -> Result<Option<(OllamaExtractionProvider, JobConfig)>> {
    if !env_flag("EXTRACTION_AI_ENABLED") {
        return Ok(None);
    }
    let url = std::env::var("EXTRACTION_AI_URL").unwrap_or_else(|_| DEFAULT_URL.into());
    let model = std::env::var("EXTRACTION_AI_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.into());
    let keep_alive = std::env::var("EXTRACTION_AI_KEEP_ALIVE").unwrap_or_else(|_| "5m".into());
    let timeout = bounded_env("EXTRACTION_AI_TIMEOUT_SECS", 180, 5, 600)?;
    // Match the Ollama container's cpuset; this process's CPU count is only
    // the fallback (see `ollama::valid_num_threads`).
    let num_threads = bounded_env(
        "EXTRACTION_AI_NUM_THREADS",
        u64::from(default_num_threads()),
        1,
        u64::from(MAX_NUM_THREADS),
    )? as u32;
    let provider = OllamaExtractionProvider::new(
        &url,
        &model,
        Duration::from_secs(timeout),
        &keep_alive,
        env_flag("EXTRACTION_AI_ALLOW_ANY_PORT"),
        num_threads,
    )
    .map_err(|_| {
        anyhow::anyhow!(
            "EXTRACTION_AI_URL must be a private Ollama endpoint (loopback, private IP or `ollama`, port 11434) and EXTRACTION_AI_MODEL/KEEP_ALIVE must be valid"
        )
    })?;
    let mode = match std::env::var("EXTRACTION_AI_MODE")
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "" | "extract" => Mode::Extract,
        "normalize_only" => Mode::NormalizeOnly,
        _ => bail!("EXTRACTION_AI_MODE must be extract or normalize_only"),
    };
    let config = JobConfig {
        mode,
        max_programs: bounded_env("EXTRACTION_AI_MAX_PROGRAMS", 8, 1, 200)? as i64,
        failure_cooldown: chrono::Duration::hours(bounded_env(
            "EXTRACTION_AI_FAILURE_COOLDOWN_HOURS",
            6,
            1,
            168,
        )? as i64),
        time_budget: Duration::from_secs(
            bounded_env("EXTRACTION_AI_TIME_BUDGET_MINUTES", 25, 1, 28)? * 60,
        ),
        call_timeout: Duration::from_secs(timeout),
        only_programs: None,
        queue_only: false,
        trigger: Trigger::Nightly,
        auto_apply: AutoApplyConfig::from_env()?,
    };
    Ok(Some((provider, config)))
}

/// The hourly drain of the ingestion queue: at most
/// `EXTRACTION_AI_QUEUE_MAX_PROGRAMS` (default 4, 0 = off) changed programs
/// within a 10-minute budget. `None` when extraction or the drain is off.
pub fn queue_config_from_env() -> Result<Option<(OllamaExtractionProvider, JobConfig)>> {
    let Some((provider, config)) = config_from_env()? else {
        return Ok(None);
    };
    let max = bounded_env("EXTRACTION_AI_QUEUE_MAX_PROGRAMS", 4, 0, 50)? as i64;
    if max == 0 {
        return Ok(None);
    }
    Ok(Some((
        provider,
        JobConfig {
            mode: Mode::Extract,
            max_programs: max,
            time_budget: Duration::from_secs(10 * 60).max(config.call_timeout),
            queue_only: true,
            trigger: Trigger::Queue,
            ..config
        },
    )))
}

/// Telemetry of one run; also persisted on the run row.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct RunSummary {
    pub run_id: Uuid,
    pub status: String,
    pub selected: usize,
    pub cache_hits: usize,
    pub calls: usize,
    pub failures: usize,
    pub cooldown_skips: usize,
    pub breaker_skips: usize,
    pub budget_skips: usize,
    pub unchanged_skips: usize,
    pub rejected_by_guard: usize,
    pub invalid_facts: usize,
    pub excluded_by_rules: usize,
    pub candidates_written: usize,
    pub superseded: usize,
    pub breaker_opened: bool,
    pub auto_apply_enabled: bool,
    pub auto: AutoCounters,
    /// Queued programs processed and removed from the ingestion queue.
    pub queue_drained: usize,
}

pub async fn run_from_env(pool: &PgPool) -> Result<Option<RunSummary>> {
    let Some((provider, config)) = config_from_env()? else {
        return Ok(None);
    };
    run_batch(pool, &provider, &config).await.map(Some)
}

#[derive(Debug, FromRow)]
struct ProgramRow {
    id: Uuid,
    source_type: String,
    source_id: Option<String>,
    #[sqlx(flatten)]
    text: ProgramText,
    min_age: Option<i32>,
    max_age: Option<i32>,
    application_start_at: Option<DateTime<Utc>>,
    application_end_at: Option<DateTime<Utc>>,
    program_status: String,
    regions: Option<Vec<String>>,
    normalized_payload: Option<Value>,
    has_documents: bool,
    /// Set when the program is in the ingestion queue.
    queued_at: Option<DateTime<Utc>>,
}

impl ProgramRow {
    fn coverage(&self) -> RuleCoverage {
        RuleCoverage {
            age: self.min_age.is_some() || self.max_age.is_some(),
            region: self
                .regions
                .as_ref()
                .is_some_and(|r| !r.is_empty() && !r.iter().any(|code| code == "unknown")),
            enrollment: self
                .normalized_payload
                .as_ref()
                .and_then(|p| p.get("enrollment_statuses"))
                .and_then(Value::as_array)
                .is_some_and(|a| !a.is_empty()),
            employment: false,
            application_window: self.application_start_at.is_some()
                || self.application_end_at.is_some()
                || self.program_status == "always_open",
            documents: self.has_documents,
        }
    }
}

/// Columns of [`ProgramText`]; the API rebuilds the current source with the
/// same list before applying an approval.
pub const PROGRAM_TEXT_COLUMNS: &str =
    "p.title, p.summary, p.application_method, p.submission_documents, p.screening_method, p.raw_payload";

/// The program's current evidence source, or `None` for an unknown id.
pub async fn current_source<'e, E: sqlx::PgExecutor<'e>>(
    executor: E,
    program_id: Uuid,
) -> Result<Option<ExtractionSource>> {
    let text = sqlx::query_as::<_, ProgramText>(&format!(
        "SELECT {PROGRAM_TEXT_COLUMNS} FROM programs p WHERE p.id = $1"
    ))
    .bind(program_id)
    .fetch_optional(executor)
    .await?;
    Ok(text.as_ref().map(build_source))
}

async fn select_programs(
    pool: &PgPool,
    provider: &str,
    model: &str,
    config: &JobConfig,
) -> Result<Vec<ProgramRow>> {
    // A queued program (source text changed since the last sync) is always
    // eligible and comes first; the cache skips it when the masked source
    // turns out unchanged.
    let lacking = match (config.mode, config.queue_only) {
        (_, true) => "AND q.program_id IS NOT NULL",
        (Mode::Extract, false) => {
            "AND (q.program_id IS NOT NULL
                  OR (p.min_age IS NULL AND p.max_age IS NULL)
                  OR (p.application_start_at IS NULL AND p.application_end_at IS NULL
                      AND p.program_status NOT IN ('always_open', 'closed'))
                  OR NOT EXISTS (SELECT 1 FROM program_documents d WHERE d.program_id = p.id)
                  OR NOT EXISTS (SELECT 1 FROM eligibility_rules r WHERE r.program_id = p.id))"
        }
        (Mode::NormalizeOnly, false) => "AND last.checked_at IS NOT NULL",
    };
    let sql = format!(
        "SELECT p.id, p.source_type, p.source_id, {PROGRAM_TEXT_COLUMNS},
                p.min_age, p.max_age, p.application_start_at, p.application_end_at,
                p.program_status, p.regions, p.normalized_payload,
                EXISTS (SELECT 1 FROM program_documents d WHERE d.program_id = p.id) AS has_documents,
                q.enqueued_at AS queued_at
         FROM programs p
         LEFT JOIN program_ai_extraction_queue q ON q.program_id = p.id
         LEFT JOIN LATERAL (
             SELECT max(c.checked_at) AS checked_at
             FROM program_ai_extraction_cache c
             WHERE c.program_id = p.id AND c.provider = $1 AND c.model = $2
               AND c.prompt_version = $3 AND c.schema_version = $4
         ) last ON true
         WHERE p.is_active AND NOT (p.program_type = ANY($5))
           AND ($7::uuid[] IS NULL OR p.id = ANY($7))
           {lacking}
         ORDER BY (q.program_id IS NOT NULL) DESC, q.enqueued_at ASC NULLS LAST,
                  (last.checked_at IS NULL) DESC,
                  ({available}) DESC,
                  (p.program_type IN ({PRIORITY_PROGRAM_TYPES})) DESC,
                  last.checked_at ASC NULLS FIRST,
                  p.updated_at DESC, p.id
         LIMIT $6",
        available = mazimi_core::currently_available_sql("p.", "now()"),
    );
    let excluded: Vec<String> = EXCLUDED_PROGRAM_TYPES
        .iter()
        .map(|s| s.to_string())
        .collect();
    Ok(sqlx::query_as::<_, ProgramRow>(&sql)
        .bind(provider)
        .bind(model)
        .bind(PROMPT_VERSION)
        .bind(SCHEMA_VERSION)
        .bind(&excluded)
        // A window larger than the budget: unchanged programs are skipped
        // without counting toward it.
        .bind(config.max_programs.saturating_mul(3))
        .bind(config.only_programs.as_deref())
        .fetch_all(pool)
        .await?)
}

/// Stops calling a provider that keeps failing within one run.
#[derive(Debug, Default)]
struct Breaker {
    unavailable: usize,
    invalid: usize,
}

impl Breaker {
    fn open(&self) -> bool {
        self.unavailable >= BREAKER_UNAVAILABLE || self.invalid >= BREAKER_INVALID
    }
    fn record(&mut self, result: &Result<(), ProviderFailure>) {
        match result {
            Ok(()) => *self = Self::default(),
            Err(ProviderFailure::Unavailable) => {
                self.unavailable += 1;
                self.invalid = 0;
            }
            Err(ProviderFailure::InvalidOutput(_)) => {
                self.invalid += 1;
                self.unavailable = 0;
            }
        }
    }
}

#[derive(Debug, FromRow)]
struct CacheRow {
    id: Uuid,
    status: String,
    request_hash: String,
    response_json: Option<Value>,
    last_failed_at: Option<DateTime<Utc>>,
}

enum Outcome {
    CacheHit { written: usize },
    Called { written: usize },
    Failed(ProviderFailure),
    CooldownSkip,
    BreakerSkip,
    Unchanged,
}

struct RunCtx<'a, P: ?Sized> {
    pool: &'a PgPool,
    provider: &'a P,
    config: &'a JobConfig,
    run_id: Uuid,
}

/// Queue rows of programmes the batch never selects (inactive, or a type
/// extraction skips) would keep the hourly drain waking up for nothing.
/// They are dropped; a programme that becomes active again is re-enqueued by
/// the `is_active` branch of the queue trigger. The programme rows are read
/// under a share lock: a reactivation committing meanwhile is re-read (and
/// kept), and one still in flight is skipped until the next prune, so the
/// row its trigger enqueued is never deleted on a stale `is_active`.
pub async fn prune_queue(pool: &PgPool) -> Result<u64> {
    let excluded: Vec<String> = EXCLUDED_PROGRAM_TYPES
        .iter()
        .map(|s| s.to_string())
        .collect();
    Ok(sqlx::query(
        "WITH stale AS (
             SELECT p.id FROM programs p
             WHERE (NOT p.is_active OR p.program_type = ANY($1))
               AND EXISTS (SELECT 1 FROM program_ai_extraction_queue q WHERE q.program_id = p.id)
             FOR SHARE OF p SKIP LOCKED
         )
         DELETE FROM program_ai_extraction_queue q USING stale WHERE q.program_id = stale.id",
    )
    .bind(&excluded)
    .execute(pool)
    .await?
    .rows_affected())
}

pub async fn run_batch<P: ExtractionProvider + ?Sized>(
    pool: &PgPool,
    provider: &P,
    config: &JobConfig,
) -> Result<RunSummary> {
    if !(1..=200).contains(&config.max_programs) {
        bail!("max_programs must be between 1 and 200");
    }
    let run_id: Uuid = sqlx::query_scalar(
        "INSERT INTO program_ai_extraction_runs (mode, provider, model, prompt_version, schema_version,
             trigger, auto_apply_enabled)
         VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id",
    )
    .bind(config.mode.as_str())
    .bind(provider.provider_name())
    .bind(provider.model_name())
    .bind(PROMPT_VERSION)
    .bind(SCHEMA_VERSION)
    .bind(config.trigger.as_str())
    .bind(config.auto_apply.enabled)
    .fetch_one(pool)
    .await?;
    let mut summary = RunSummary {
        run_id,
        auto_apply_enabled: config.auto_apply.enabled,
        ..RunSummary::default()
    };
    let result = run_programs(
        &RunCtx {
            pool,
            provider,
            config,
            run_id,
        },
        &mut summary,
    )
    .await;
    summary.status = match &result {
        Err(_) => "failed",
        Ok(()) if summary.failures == 0 && !summary.breaker_opened => "succeeded",
        Ok(()) if summary.calls > 0 && summary.failures >= summary.calls => "failed",
        Ok(()) => "partial",
    }
    .to_string();
    let error = result
        .as_ref()
        .err()
        .map(crate::error_safety::persisted_error);
    persist_summary(pool, &summary, error.as_deref()).await?;
    report_events(pool, &summary).await;
    tracing::info!(
        run_id = %summary.run_id,
        status = summary.status,
        selected = summary.selected,
        cache_hits = summary.cache_hits,
        calls = summary.calls,
        failures = summary.failures,
        cooldown_skips = summary.cooldown_skips,
        breaker_skips = summary.breaker_skips,
        rejected_by_guard = summary.rejected_by_guard,
        invalid_facts = summary.invalid_facts,
        candidates_written = summary.candidates_written,
        superseded = summary.superseded,
        "ai extraction run complete"
    );
    result.map(|()| summary)
}

async fn run_programs<P: ExtractionProvider + ?Sized>(
    ctx: &RunCtx<'_, P>,
    summary: &mut RunSummary,
) -> Result<()> {
    let programs = select_programs(
        ctx.pool,
        ctx.provider.provider_name(),
        ctx.provider.model_name(),
        ctx.config,
    )
    .await?;
    let started = Instant::now();
    let mut breaker = Breaker::default();
    let mut processed = 0i64;
    for program in &programs {
        if processed >= ctx.config.max_programs {
            break;
        }
        if started.elapsed() + ctx.config.call_timeout > ctx.config.time_budget {
            summary.budget_skips += 1;
            continue;
        }
        summary.selected += 1;
        let outcome = AssertUnwindSafe(process_program(ctx, program, &mut breaker, summary))
            .catch_unwind()
            .await;
        // A queued program leaves the queue once it was handled (served,
        // called, failed into the cooldown, or skipped as unchanged). A
        // breaker skip, a storage error or a panic keeps it for the next run,
        // and so does a re-enqueue during the run (newer enqueued_at).
        let attempted = matches!(&outcome, Ok(Ok(o)) if !matches!(o, Outcome::BreakerSkip));
        if let (Some(queued_at), true) = (program.queued_at, attempted) {
            match sqlx::query(
                "DELETE FROM program_ai_extraction_queue WHERE program_id = $1 AND enqueued_at <= $2",
            )
            .bind(program.id)
            .bind(queued_at)
            .execute(ctx.pool)
            .await
            {
                Ok(done) => summary.queue_drained += done.rows_affected() as usize,
                Err(error) => {
                    tracing::warn!(program_id = %program.id, error = %error, "ai extraction queue entry not removed")
                }
            }
        }
        match outcome {
            Ok(Ok(Outcome::CacheHit { written })) => {
                processed += 1;
                summary.cache_hits += 1;
                summary.candidates_written += written;
            }
            Ok(Ok(Outcome::Called { written })) => {
                processed += 1;
                summary.calls += 1;
                summary.candidates_written += written;
            }
            Ok(Ok(Outcome::Failed(failure))) => {
                processed += 1;
                summary.calls += 1;
                summary.failures += 1;
                tracing::warn!(program_id = %program.id, error = %failure, "ai extraction failed; rule result kept");
            }
            Ok(Ok(Outcome::CooldownSkip)) => summary.cooldown_skips += 1,
            Ok(Ok(Outcome::BreakerSkip)) => summary.breaker_skips += 1,
            Ok(Ok(Outcome::Unchanged)) => summary.unchanged_skips += 1,
            Ok(Err(error)) => {
                // One program's storage error must not block the nightly
                // batch; it is counted and the rest continue.
                processed += 1;
                summary.failures += 1;
                tracing::error!(program_id = %program.id, error = %crate::error_safety::persisted_error(&error), "ai extraction storage failed; the remaining programs continue");
            }
            Err(_panic) => {
                processed += 1;
                summary.failures += 1;
                tracing::error!(program_id = %program.id, "ai extraction panicked; the remaining programs continue");
            }
        }
        if breaker.open() {
            summary.breaker_opened = true;
        }
    }
    Ok(())
}

async fn process_program<P: ExtractionProvider + ?Sized>(
    ctx: &RunCtx<'_, P>,
    program: &ProgramRow,
    breaker: &mut Breaker,
    summary: &mut RunSummary,
) -> Result<Outcome> {
    let source = build_source(&program.text);
    let coverage = program.coverage();
    let requested: Vec<FactType> = coverage.requested();
    let provider_name = ctx.provider.provider_name();
    let model = ctx.provider.model_name();
    summary.superseded += sqlx::query(
        "UPDATE program_ai_extraction_candidates
         SET status = 'superseded', updated_at = now()
         WHERE program_id = $1 AND status IN ('pending', 'flagged', 'invalid')
           AND (source_hash <> $2 OR prompt_version <> $3 OR model <> $4)",
    )
    .bind(program.id)
    .bind(&source.hash)
    .bind(PROMPT_VERSION)
    .bind(model)
    .execute(ctx.pool)
    .await?
    .rows_affected() as usize;

    let request = build_request(&source, &requested, provider_name, model);
    let cache = sqlx::query_as::<_, CacheRow>(
        "SELECT id, status, request_hash, response_json, last_failed_at
         FROM program_ai_extraction_cache
         WHERE program_id = $1 AND source_hash = $2 AND provider = $3 AND model = $4
           AND prompt_version = $5 AND schema_version = $6",
    )
    .bind(program.id)
    .bind(&source.hash)
    .bind(provider_name)
    .bind(model)
    .bind(PROMPT_VERSION)
    .bind(SCHEMA_VERSION)
    .fetch_optional(ctx.pool)
    .await?;

    let touch = |id: Uuid| {
        sqlx::query("UPDATE program_ai_extraction_cache SET checked_at = now() WHERE id = $1")
            .bind(id)
            .execute(ctx.pool)
    };
    if let Some(row) = &cache {
        if row.status == "succeeded" && row.request_hash == request.request_hash {
            let response = row.response_json.clone().unwrap_or(Value::Null);
            let written = match process_output(&response, &source, &coverage) {
                Ok(processed) => {
                    tally(summary, &processed.counters);
                    let mut tx = ctx.pool.begin().await?;
                    let written = write_candidates(
                        &mut tx,
                        ctx,
                        program,
                        row.id,
                        &source,
                        &requested,
                        &processed.facts,
                    )
                    .await?;
                    tx.commit().await?;
                    written
                }
                Err(error) => {
                    tracing::warn!(program_id = %program.id, error, "cached answer no longer parses");
                    0
                }
            };
            touch(row.id).await?;
            auto_step(ctx, program.id, &source.hash, summary).await?;
            return Ok(if written == 0 {
                Outcome::Unchanged
            } else {
                Outcome::CacheHit { written }
            });
        }
        if row.status == "failed"
            && row
                .last_failed_at
                .is_some_and(|at| at + ctx.config.failure_cooldown > Utc::now())
        {
            touch(row.id).await?;
            return Ok(Outcome::CooldownSkip);
        }
    }
    if ctx.config.mode == Mode::NormalizeOnly {
        if let Some(row) = &cache {
            touch(row.id).await?;
        }
        return Ok(Outcome::Unchanged);
    }
    if breaker.open() {
        return Ok(Outcome::BreakerSkip);
    }

    let call_started = Instant::now();
    let response = ctx.provider.extract(&request).await;
    let latency_ms = i32::try_from(call_started.elapsed().as_millis()).unwrap_or(i32::MAX);
    let processed = response.and_then(|value| {
        process_output(&value, &source, &coverage)
            .map(|processed| (value, processed))
            .map_err(ProviderFailure::InvalidOutput)
    });
    breaker.record(&processed.as_ref().map(|_| ()).map_err(Clone::clone));
    let coverage_json = serde_json::to_value(coverage)?;
    match processed {
        Ok((value, processed)) => {
            tally(summary, &processed.counters);
            let mut tx = ctx.pool.begin().await?;
            let cache_id: Uuid = sqlx::query_scalar(
                "INSERT INTO program_ai_extraction_cache (
                     program_id, source_type, source_id, source_hash, provider, model,
                     prompt_version, schema_version, request_hash, source_text, rule_coverage,
                     status, response_json, error, latency_ms
                 ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, 'succeeded', $12, NULL, $13)
                 ON CONFLICT (program_id, source_hash, provider, model, prompt_version, schema_version)
                 DO UPDATE SET request_hash = EXCLUDED.request_hash, source_text = EXCLUDED.source_text,
                     rule_coverage = EXCLUDED.rule_coverage, status = 'succeeded',
                     response_json = EXCLUDED.response_json, error = NULL,
                     latency_ms = EXCLUDED.latency_ms, checked_at = now(), updated_at = now()
                 RETURNING id",
            )
            .bind(program.id)
            .bind(&program.source_type)
            .bind(&program.source_id)
            .bind(&source.hash)
            .bind(provider_name)
            .bind(model)
            .bind(PROMPT_VERSION)
            .bind(SCHEMA_VERSION)
            .bind(&request.request_hash)
            .bind(&source.text)
            .bind(&coverage_json)
            .bind(&value)
            .bind(latency_ms)
            .fetch_one(&mut *tx)
            .await?;
            let written = write_candidates(
                &mut tx,
                ctx,
                program,
                cache_id,
                &source,
                &requested,
                &processed.facts,
            )
            .await?;
            tx.commit().await?;
            auto_step(ctx, program.id, &source.hash, summary).await?;
            Ok(Outcome::Called { written })
        }
        Err(failure) => {
            sqlx::query(
                "INSERT INTO program_ai_extraction_cache (
                     program_id, source_type, source_id, source_hash, provider, model,
                     prompt_version, schema_version, request_hash, source_text, rule_coverage,
                     status, error, failure_count, last_failed_at, latency_ms
                 ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, 'failed', $12, 1, now(), $13)
                 ON CONFLICT (program_id, source_hash, provider, model, prompt_version, schema_version)
                 DO UPDATE SET request_hash = EXCLUDED.request_hash, status = 'failed',
                     response_json = NULL, error = EXCLUDED.error,
                     failure_count = LEAST(program_ai_extraction_cache.failure_count + 1, 1000),
                     last_failed_at = now(), latency_ms = EXCLUDED.latency_ms,
                     checked_at = now(), updated_at = now()",
            )
            .bind(program.id)
            .bind(&program.source_type)
            .bind(&program.source_id)
            .bind(&source.hash)
            .bind(provider_name)
            .bind(model)
            .bind(PROMPT_VERSION)
            .bind(SCHEMA_VERSION)
            .bind(&request.request_hash)
            .bind(&source.text)
            .bind(&coverage_json)
            .bind(crate::error_safety::persisted_error(&failure))
            .bind(latency_ms)
            .execute(ctx.pool)
            .await?;
            Ok(Outcome::Failed(failure))
        }
    }
}

/// Rule-verified facts of this program's current source go through the
/// shared apply path (or are held with a reason).
async fn auto_step<P: ExtractionProvider + ?Sized>(
    ctx: &RunCtx<'_, P>,
    program_id: Uuid,
    source_hash: &str,
    summary: &mut RunSummary,
) -> Result<()> {
    auto_apply_program(
        ctx.pool,
        program_id,
        source_hash,
        ctx.run_id,
        &ctx.config.auto_apply,
        &mut summary.auto,
    )
    .await
}

fn tally(summary: &mut RunSummary, counters: &super::validate::ProcessCounters) {
    summary.rejected_by_guard += counters.rejected_by_guard;
    summary.invalid_facts += counters.invalid;
    summary.excluded_by_rules += counters.excluded_by_rules;
}

/// Insert or refresh the run's facts. Reviewed rows (approved, rejected)
/// are never touched; unreviewed rows of this extraction that the current
/// answer no longer contains are superseded. Returns the rows written.
async fn write_candidates<P: ExtractionProvider + ?Sized>(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &RunCtx<'_, P>,
    program: &ProgramRow,
    cache_id: Uuid,
    source: &ExtractionSource,
    requested: &[FactType],
    facts: &[ProcessedFact],
) -> Result<usize> {
    let model = ctx.provider.model_name();
    let mut written = 0;
    for fact in facts {
        // The rule parser's independent reading of the same quote; shown to
        // reviewers for every fact and the gate for auto-apply.
        let auto_check = fact
            .fact_value()
            .zip(fact.evidence.as_ref())
            .map(|(value, evidence)| serde_json::to_value(cross_check(&value, &evidence.quote)))
            .transpose()?;
        // `hold` is set by the auto-apply step after this write; it is not
        // part of the extraction, so it neither counts as a change nor
        // survives one (the next auto-apply step sets it again).
        let changed = sqlx::query(
            "INSERT INTO program_ai_extraction_candidates (
                 run_id, cache_id, program_id, source_hash, provider, model, prompt_version,
                 schema_version, fact_key, fact_type, value, evidence, confidence, status, validation,
                 auto_check
             ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)
             ON CONFLICT (program_id, source_hash, prompt_version, model, fact_key)
             DO UPDATE SET run_id = EXCLUDED.run_id, cache_id = EXCLUDED.cache_id,
                 value = EXCLUDED.value, evidence = EXCLUDED.evidence,
                 confidence = EXCLUDED.confidence, status = EXCLUDED.status,
                 validation = EXCLUDED.validation, auto_check = EXCLUDED.auto_check,
                 updated_at = now()
             WHERE program_ai_extraction_candidates.status IN ('pending', 'flagged', 'invalid', 'superseded')
               AND (program_ai_extraction_candidates.value, program_ai_extraction_candidates.evidence,
                    program_ai_extraction_candidates.confidence, program_ai_extraction_candidates.status,
                    program_ai_extraction_candidates.validation,
                    program_ai_extraction_candidates.auto_check - 'hold')
                   IS DISTINCT FROM
                   (EXCLUDED.value, EXCLUDED.evidence, EXCLUDED.confidence, EXCLUDED.status,
                    EXCLUDED.validation, EXCLUDED.auto_check)",
        )
        .bind(ctx.run_id)
        .bind(cache_id)
        .bind(program.id)
        .bind(&source.hash)
        .bind(ctx.provider.provider_name())
        .bind(model)
        .bind(PROMPT_VERSION)
        .bind(SCHEMA_VERSION)
        .bind(&fact.key)
        .bind(fact.fact_type.as_str())
        .bind(&fact.value)
        .bind(fact.evidence.as_ref().map(serde_json::to_value).transpose()?)
        .bind(fact.confidence.map(Confidence::as_str))
        .bind(fact.state.as_str())
        .bind(serde_json::to_value(&fact.validation)?)
        .bind(auto_check)
        .execute(&mut **tx)
        .await?
        .rows_affected();
        written += changed as usize;
    }
    let keys: Vec<&str> = facts.iter().map(|f| f.key.as_str()).collect();
    // Only types this request asked for: a type the rules now cover (for
    // example documents after one was approved) keeps its open candidates.
    let requested: Vec<&str> = requested.iter().map(|t| t.as_str()).collect();
    sqlx::query(
        "UPDATE program_ai_extraction_candidates
         SET status = 'superseded', updated_at = now()
         WHERE program_id = $1 AND source_hash = $2 AND prompt_version = $3 AND model = $4
           AND status IN ('pending', 'flagged', 'invalid') AND NOT (fact_key = ANY($5))
           AND fact_type = ANY($6)",
    )
    .bind(program.id)
    .bind(&source.hash)
    .bind(PROMPT_VERSION)
    .bind(model)
    .bind(&keys)
    .bind(&requested)
    .execute(&mut **tx)
    .await?;
    Ok(written)
}

async fn persist_summary(pool: &PgPool, s: &RunSummary, error: Option<&str>) -> Result<()> {
    let n = |v: usize| i32::try_from(v).unwrap_or(i32::MAX);
    sqlx::query(
        "UPDATE program_ai_extraction_runs SET status = $2, selected = $3, cache_hits = $4,
             calls = $5, failures = $6, cooldown_skips = $7, breaker_skips = $8,
             budget_skips = $9, unchanged_skips = $10, rejected_by_guard = $11,
             invalid_facts = $12, excluded_by_rules = $13, candidates_written = $14,
             superseded = $15, error = $16, auto_verified = $17, auto_applied = $18,
             auto_held = $19, auto_cap_skips = $20, audit_sampled = $21, queue_drained = $22,
             finished_at = now()
         WHERE id = $1",
    )
    .bind(s.run_id)
    .bind(&s.status)
    .bind(n(s.selected))
    .bind(n(s.cache_hits))
    .bind(n(s.calls))
    .bind(n(s.failures))
    .bind(n(s.cooldown_skips))
    .bind(n(s.breaker_skips))
    .bind(n(s.budget_skips))
    .bind(n(s.unchanged_skips))
    .bind(n(s.rejected_by_guard))
    .bind(n(s.invalid_facts))
    .bind(n(s.excluded_by_rules))
    .bind(n(s.candidates_written))
    .bind(n(s.superseded))
    .bind(error)
    .bind(n(s.auto.verified))
    .bind(n(s.auto.applied))
    .bind(n(s.auto.held))
    .bind(n(s.auto.cap_skips))
    .bind(n(s.auto.audit_sampled))
    .bind(n(s.queue_drained))
    .execute(pool)
    .await?;
    Ok(())
}

/// Operator incidents: provider failures (warning) and an open breaker
/// (error). Both resolve on the next clean run. Recording failures never
/// fail the job.
async fn report_events(pool: &PgPool, s: &RunSummary) {
    let details = json!({
        "run_id": s.run_id, "calls": s.calls, "failures": s.failures,
        "breaker_skips": s.breaker_skips, "cooldown_skips": s.cooldown_skips,
        "auto_applied": s.auto.applied, "auto_cap_skips": s.auto.cap_skips
    });
    // `open` raises the incident; `resolve` needs a run that actually
    // reached the provider and got answers (a run of skips proves nothing).
    for (fingerprint, open, resolve, severity, event_type, message) in [
        (
            EVENT_FAILURES,
            s.failures > 0,
            s.calls > s.failures,
            Severity::Warning,
            "ai_extraction_failures",
            "AI extraction completed with provider failures; rule results kept",
        ),
        (
            EVENT_BREAKER,
            s.breaker_opened,
            s.calls > s.failures,
            Severity::Error,
            "ai_extraction_circuit_open",
            "AI extraction stopped calling the model after repeated failures",
        ),
        // More rule-verified facts than one run may apply: an upstream
        // change hit many programs at once, or a parser accepts too much.
        // Resolved by a run with auto-apply on that stays under the cap.
        (
            EVENT_AUTO_SPIKE,
            s.auto.cap_skips > 0,
            s.auto_apply_enabled && s.auto.cap_skips == 0,
            Severity::Warning,
            "ai_extraction_auto_apply_spike",
            "AI extraction auto-apply reached its per-run cap; the rest wait for review",
        ),
    ] {
        if !open && !resolve {
            continue;
        }
        let result = if open {
            operations::record_event(
                pool,
                &EventCommand {
                    fingerprint,
                    severity,
                    component: "ai_extraction",
                    event_type,
                    subject_type: Some("ai_extraction_run"),
                    subject_id: Some(&s.run_id.to_string()),
                    message,
                    details: details.clone(),
                },
            )
            .await
            .map(|_| ())
        } else {
            operations::resolve_event(pool, fingerprint, None, Some("ai extraction recovered"))
                .await
                .map(|_| ())
        };
        if let Err(error) = result {
            tracing::error!(error = %error, fingerprint, "failed to persist ai extraction incident");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn breaker_opens_on_consecutive_failures_and_resets_on_success() {
        let mut breaker = Breaker::default();
        for _ in 0..BREAKER_UNAVAILABLE - 1 {
            breaker.record(&Err(ProviderFailure::Unavailable));
        }
        assert!(!breaker.open());
        breaker.record(&Ok(()));
        for _ in 0..BREAKER_UNAVAILABLE {
            breaker.record(&Err(ProviderFailure::Unavailable));
        }
        assert!(breaker.open());
        let mut invalid = Breaker::default();
        for _ in 0..BREAKER_INVALID {
            assert!(!invalid.open());
            invalid.record(&Err(ProviderFailure::InvalidOutput("x".into())));
        }
        assert!(invalid.open());
        // Alternating failure types are not consecutive failures of one kind.
        let mut mixed = Breaker::default();
        for _ in 0..4 {
            mixed.record(&Err(ProviderFailure::Unavailable));
            mixed.record(&Err(ProviderFailure::InvalidOutput("x".into())));
        }
        assert!(!mixed.open());
    }

    #[test]
    fn coverage_follows_normalized_columns() {
        let row = ProgramRow {
            id: Uuid::nil(),
            source_type: "youth_center".into(),
            source_id: None,
            text: ProgramText::default(),
            min_age: Some(19),
            max_age: None,
            application_start_at: None,
            application_end_at: None,
            program_status: "always_open".into(),
            regions: Some(vec!["unknown".into()]),
            normalized_payload: Some(json!({"enrollment_statuses": ["enrolled"]})),
            has_documents: false,
            queued_at: None,
        };
        let coverage = row.coverage();
        assert!(coverage.age && coverage.application_window && coverage.enrollment);
        assert!(!coverage.region, "unresolved region is a gap");
        assert!(!coverage.documents && !coverage.employment);
    }

    #[test]
    fn env_defaults_are_off_and_bounds_are_enforced() {
        // Process-wide env: only names this module owns are touched.
        std::env::remove_var("EXTRACTION_AI_ENABLED");
        assert!(config_from_env().unwrap().is_none());
        assert!(bounded_env("EXTRACTION_AI_TEST_UNSET_VALUE", 6, 1, 168).unwrap() == 6);
        std::env::set_var("EXTRACTION_AI_TEST_BOUND", "0");
        assert!(bounded_env("EXTRACTION_AI_TEST_BOUND", 6, 1, 168).is_err());
        std::env::set_var("EXTRACTION_AI_TEST_BOUND", "x");
        assert!(bounded_env("EXTRACTION_AI_TEST_BOUND", 6, 1, 168).is_err());
        std::env::remove_var("EXTRACTION_AI_TEST_BOUND");
        let defaults = JobConfig::default();
        assert_eq!(defaults.failure_cooldown, chrono::Duration::hours(6));
        assert_eq!(defaults.mode, Mode::Extract);

        // Same test so no parallel test sees EXTRACTION_AI_ENABLED flip.
        let request = build_request(
            &build_source(&ProgramText::default()),
            &[FactType::Income],
            "ollama",
            DEFAULT_MODEL,
        );
        let threads = |value: Option<&str>| {
            match value {
                Some(v) => std::env::set_var("EXTRACTION_AI_NUM_THREADS", v),
                None => std::env::remove_var("EXTRACTION_AI_NUM_THREADS"),
            }
            config_from_env()
                .map(|c| c.unwrap().0.payload(&request)["options"]["num_thread"].clone())
        };
        std::env::set_var("EXTRACTION_AI_ENABLED", "true");
        let results = [
            threads(None).ok(),
            threads(Some("3")).ok(),
            threads(Some("0")).ok(),
            threads(Some("65")).ok(),
            threads(Some("two")).ok(),
        ];
        std::env::remove_var("EXTRACTION_AI_NUM_THREADS");
        std::env::remove_var("EXTRACTION_AI_ENABLED");
        assert_eq!(results[0], Some(json!(default_num_threads())));
        assert_eq!(results[1], Some(json!(3)));
        assert_eq!(results[2..], [None, None, None]);
    }
}
