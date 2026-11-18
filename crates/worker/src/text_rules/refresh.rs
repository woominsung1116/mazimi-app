//! The versioned rule_v1 measurement step: stores each published programme's
//! facts in `program_rule_extractions` and nothing else.
//!
//! Measurement only: this never writes `eligibility_rules` or any other
//! served table, so it cannot change ranking, labels, explanations or
//! "조건 확인됨". A programme is (re)extracted when it has no stored row, when
//! the stored `extractor_version` is not [`EXTRACTOR_VERSION`], or when the
//! SQL hash of the fields the rules read ([`input_hash_sql`]) changed.
//! Unchanged rows are skipped. Separate from the sources' `content_hash`: a
//! rule change never makes a source re-sync.
//!
//! Every run is bounded ([`Budget`]): at most `max_rows` pending programmes
//! and no new batch after `max_duration`. A run reads only pending rows
//! (stale version or hash first, then missing ones), so the next run goes on
//! where the last one stopped. The worker runs it as its own scheduled job
//! every ten minutes, never on the startup path: the first backfill of
//! ~13k programmes is spread over many runs instead of blocking the
//! heartbeat (incident 2026-10-22). `text-rules-refresh` loops bounded runs
//! until nothing is pending.
//!
//! Writes go [`BATCH`] rows per statement. A batch that fails is retried row
//! by row, each in its own transaction, so one bad row is logged and counted
//! while the rest of its batch is still written.
//!
//! A row whose write fails is counted in `program_rule_extraction_failures`
//! for its current input hash and extractor version. Pending rows are read
//! fewest failures first, so failing rows go behind the others, and a row
//! with [`MAX_FAILURES`] is parked until its input or the extractor version
//! changes (a parked row keeps `job:text_rules_refresh` open). A successful
//! write clears the count.
//!
//! When the previous scheduled run left nothing pending, a run first checks
//! a cheap watermark (the latest ingestion run, whether one is running, and
//! the count of published programmes of the read sources). If it is the
//! same, the run skips the full hash recompute. A full recompute still runs
//! at least every [`FULL_CHECK_EVERY`], for writers that change a payload
//! outside an ingestion run.

use anyhow::{Context, Result};
use mazimi_core::text_rules::{hashed_keys, input_hash_sql, EXTRACTOR, EXTRACTOR_VERSION, SOURCES};
use serde_json::Value;
use sqlx::PgPool;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};
use tracing::warn;
use uuid::Uuid;

use super::compile::{compile, Compiled};
use super::coverage::uncovered_codes;
use super::extract::extract;

/// Rows per write statement.
pub const BATCH: usize = 200;
/// Failed writes after which a row is parked until its input changes.
pub const MAX_FAILURES: i32 = 5;
/// The longest a run may skip the full recompute on an unchanged watermark.
pub const FULL_CHECK_EVERY: Duration = Duration::from_secs(60 * 60);

/// The bound of one run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    /// Pending programmes read per run.
    pub max_rows: usize,
    /// No batch starts after this much time since the run began.
    pub max_duration: Duration,
}

impl Budget {
    /// The scheduled job's bound: 1,000 rows or 60 s, whichever comes first.
    pub const SCHEDULED: Budget = Budget {
        max_rows: 1_000,
        max_duration: Duration::from_secs(60),
    };
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RefreshSummary {
    /// Pending programmes of the read sources before this run (parked rows
    /// not included).
    pub pending: usize,
    /// Pending programmes parked after [`MAX_FAILURES`] failed writes.
    pub parked: usize,
    /// The watermark was unchanged since a run that left nothing pending,
    /// so nothing was recomputed.
    pub skipped_unchanged: bool,
    /// Pending programmes this run compiled (at most `Budget::max_rows`).
    pub examined: usize,
    /// Extractions inserted or replaced.
    pub written: usize,
    /// Rows whose write failed (logged; the run continued).
    pub failed: usize,
    /// Over the rows written this run: fully structured texts and evaluable ones.
    pub fully_structured: usize,
    pub evaluable: usize,
}

impl RefreshSummary {
    /// Programmes still pending after this run.
    pub fn remaining(&self) -> usize {
        self.pending.saturating_sub(self.written)
    }
}

#[derive(sqlx::FromRow)]
struct InputRow {
    id: Uuid,
    source_type: String,
    raw: Value,
    input_hash: String,
    failures: i32,
    pending: i64,
    parked: i64,
}

/// One extraction to write.
struct Pending {
    program_id: Uuid,
    input_hash: String,
    compiled: Compiled,
}

/// `jsonb_build_object` over every source's [`hashed_keys`]: only the fields
/// the rules read leave the database.
fn raw_keys_sql() -> String {
    let mut keys: Vec<&str> = SOURCES
        .iter()
        .flat_map(|s| hashed_keys(s).iter().copied())
        .collect();
    keys.sort();
    keys.dedup();
    let pairs: Vec<String> = keys
        .iter()
        .map(|key| format!("'{key}', p.raw_payload->'{key}'"))
        .collect();
    format!(
        "jsonb_strip_nulls(jsonb_build_object({}))",
        pairs.join(", ")
    )
}

/// The text fields of a projected payload (strings only).
fn texts(raw: &Value) -> BTreeMap<String, String> {
    raw.as_object()
        .into_iter()
        .flatten()
        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
        .collect()
}

/// rule_v1 over one projected payload.
pub fn compile_row(source_type: &str, raw: &Value) -> Compiled {
    let extraction = extract(source_type, &texts(raw));
    let uncovered = uncovered_codes(source_type, raw);
    compile(&extraction, &uncovered)
}

/// What changes when a payload of a read source may have changed: the
/// latest ingestion run, the number still running, and the count of
/// published programmes of the read sources.
type Watermark = (Option<chrono::DateTime<chrono::Utc>>, i64, i64);

/// The watermark of the last scheduled run that left nothing pending, and
/// when the full recompute behind it ran.
static CLEAN_AT: std::sync::Mutex<Option<(Watermark, Instant)>> = std::sync::Mutex::new(None);

async fn watermark(pool: &PgPool) -> Result<Watermark> {
    sqlx::query_as(
        "SELECT (SELECT max(started_at) FROM ingestion_runs),
                (SELECT count(*) FROM ingestion_runs
                 WHERE status = 'running' AND started_at > now() - interval '2 hours'),
                (SELECT count(*) FROM current_programs WHERE source_type = ANY($1))",
    )
    .bind(SOURCES.as_slice())
    .fetch_one(pool)
    .await
    .context("rule_v1 watermark query failed")
}

/// One bounded run over every pending programme ([`Budget::SCHEDULED`]).
/// Skips the recompute when the watermark is unchanged since a clean run
/// less than [`FULL_CHECK_EVERY`] ago.
pub async fn refresh(pool: &PgPool) -> Result<RefreshSummary> {
    let mark = watermark(pool).await?;
    let clean = *CLEAN_AT.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((previous, at)) = clean {
        if previous == mark && mark.1 == 0 && at.elapsed() < FULL_CHECK_EVERY {
            return Ok(RefreshSummary {
                skipped_unchanged: true,
                ..RefreshSummary::default()
            });
        }
    }
    let summary = refresh_programs(pool, None, Budget::SCHEDULED).await?;
    *CLEAN_AT.lock().unwrap_or_else(|e| e.into_inner()) =
        (summary.remaining() == 0 && summary.parked == 0 && summary.failed == 0)
            .then(|| (mark, Instant::now()));
    Ok(summary)
}

/// [`refresh`] limited to `only` (tests and targeted reruns) under `budget`.
pub async fn refresh_programs(
    pool: &PgPool,
    only: Option<&[Uuid]>,
    budget: Budget,
) -> Result<RefreshSummary> {
    let started = Instant::now();
    // Only pending rows leave the database: no row, another extractor
    // version, or a changed input hash. Rows with fewer failed writes (for
    // this input and version) go first, parked rows last; then stale rows
    // (their stored facts are wrong now), then missing ones. Both counts are
    // taken before the LIMIT.
    let rows = sqlx::query_as::<_, InputRow>(&format!(
        "WITH c AS (
             SELECT p.id, p.source_type, {raw} AS raw, {hash} AS input_hash,
                    e.program_id IS NOT NULL AS stored,
                    e.extractor_version AS stored_version, e.input_hash AS stored_hash,
                    f.input_hash AS failed_hash, f.extractor_version AS failed_version,
                    f.failures AS failed_count
             FROM current_programs p
             LEFT JOIN program_rule_extractions e ON e.program_id = p.id
             LEFT JOIN program_rule_extraction_failures f ON f.program_id = p.id
             WHERE p.source_type = ANY($1) AND ($2::uuid[] IS NULL OR p.id = ANY($2))),
         d AS (
             SELECT id, source_type, raw, input_hash, stored,
                    CASE WHEN failed_hash = input_hash AND failed_version = $3
                         THEN failed_count ELSE 0 END AS failures
             FROM c
             WHERE NOT stored OR stored_version <> $3 OR stored_hash <> input_hash)
         SELECT id, source_type, raw, input_hash, failures,
                count(*) FILTER (WHERE failures < $5) OVER () AS pending,
                count(*) FILTER (WHERE failures >= $5) OVER () AS parked
         FROM d
         ORDER BY failures >= $5, failures, stored DESC, id
         LIMIT $4",
        raw = raw_keys_sql(),
        hash = input_hash_sql("p."),
    ))
    .bind(SOURCES.as_slice())
    .bind(only)
    .bind(EXTRACTOR_VERSION)
    .bind(budget.max_rows as i64)
    .bind(MAX_FAILURES)
    .fetch_all(pool)
    .await
    .context("rule_v1 inputs query failed")?;

    let mut summary = RefreshSummary {
        pending: rows.first().map_or(0, |r| r.pending as usize),
        parked: rows.first().map_or(0, |r| r.parked as usize),
        ..RefreshSummary::default()
    };
    let rows: Vec<&InputRow> = rows.iter().filter(|r| r.failures < MAX_FAILURES).collect();
    for chunk in rows.chunks(BATCH) {
        if started.elapsed() >= budget.max_duration {
            break;
        }
        let batch: Vec<Pending> = chunk
            .iter()
            .map(|row| Pending {
                program_id: row.id,
                input_hash: row.input_hash.clone(),
                compiled: compile_row(&row.source_type, &row.raw),
            })
            .collect();
        summary.examined += batch.len();
        let outcome = store_batch(pool, &batch).await;
        record_attempts(pool, &batch, &outcome.failed_ids).await;
        summary.written += outcome.written;
        summary.failed += outcome.failed;
        for p in batch
            .iter()
            .filter(|p| !outcome.failed_ids.contains(&p.program_id))
        {
            summary.fully_structured += p.compiled.fully_structured as usize;
            summary.evaluable += p.compiled.evaluable as usize;
        }
    }
    Ok(summary)
}

/// Count a failure for each failed row (for its input hash and version;
/// another input starts again at one) and clear the count of the others.
/// Never fails the run: a lost count only keeps the old order.
async fn record_attempts(pool: &PgPool, batch: &[Pending], failed: &[Uuid]) {
    let (bad, good): (Vec<&Pending>, Vec<&Pending>) =
        batch.iter().partition(|p| failed.contains(&p.program_id));
    let good: Vec<Uuid> = good.iter().map(|p| p.program_id).collect();
    if let Err(error) =
        sqlx::query("DELETE FROM program_rule_extraction_failures WHERE program_id = ANY($1)")
            .bind(&good)
            .execute(pool)
            .await
    {
        warn!(error = %crate::error_safety::persisted_error(&error), "rule_v1 failure counts not cleared");
    }
    for p in bad {
        if let Err(error) = sqlx::query(
            "INSERT INTO program_rule_extraction_failures AS f
               (program_id, extractor_version, input_hash, failures, last_failed_at)
             SELECT p.id, $2, $3, 1, now() FROM programs p WHERE p.id = $1
             ON CONFLICT (program_id) DO UPDATE SET
               failures = CASE WHEN f.input_hash = EXCLUDED.input_hash
                                AND f.extractor_version = EXCLUDED.extractor_version
                               THEN f.failures + 1 ELSE 1 END,
               input_hash = EXCLUDED.input_hash,
               extractor_version = EXCLUDED.extractor_version,
               last_failed_at = EXCLUDED.last_failed_at",
        )
        .bind(p.program_id)
        .bind(EXTRACTOR_VERSION)
        .bind(&p.input_hash)
        .execute(pool)
        .await
        {
            warn!(program_id = %p.program_id, error = %crate::error_safety::persisted_error(&error),
                "rule_v1 failure count not recorded");
        }
    }
}

#[derive(Debug, Default)]
struct BatchOutcome {
    written: usize,
    failed: usize,
    failed_ids: Vec<Uuid>,
}

/// One statement for the whole batch; when it fails, each row in its own
/// transaction so only the bad rows are lost.
async fn store_batch(pool: &PgPool, batch: &[Pending]) -> BatchOutcome {
    match upsert_batch(pool, batch).await {
        Ok(written) => BatchOutcome {
            written,
            ..BatchOutcome::default()
        },
        Err(error) => {
            warn!(
                rows = batch.len(),
                error = %crate::error_safety::persisted_error(&error),
                "rule_v1 batch write failed; retrying row by row"
            );
            let mut outcome = BatchOutcome::default();
            for p in batch {
                match store(pool, p.program_id, &p.input_hash, &p.compiled).await {
                    Ok(true) => outcome.written += 1,
                    Ok(false) => {}
                    Err(error) => {
                        outcome.failed += 1;
                        outcome.failed_ids.push(p.program_id);
                        warn!(
                            program_id = %p.program_id,
                            error = %crate::error_safety::persisted_error(&error),
                            "rule_v1 extraction write failed; row skipped"
                        );
                    }
                }
            }
            outcome
        }
    }
}

/// Upsert a batch in one statement. A programme deleted since the read is
/// skipped by the join; the count is the rows written.
async fn upsert_batch(pool: &PgPool, batch: &[Pending]) -> Result<usize> {
    let rows: Vec<Value> = batch
        .iter()
        .map(|p| {
            serde_json::json!({
                "program_id": p.program_id,
                "input_hash": p.input_hash,
                "has_text": p.compiled.has_text,
                "fully_structured": p.compiled.fully_structured,
                "evaluable": p.compiled.evaluable,
                "facts": p.compiled.facts,
                "leaves": p.compiled.leaves,
                "blockers": p.compiled.blockers,
            })
        })
        .collect();
    let done = sqlx::query(
        "INSERT INTO program_rule_extractions
           (program_id, extractor, extractor_version, input_hash, has_text, fully_structured,
            evaluable, facts, leaves, blockers, computed_at)
         SELECT p.id, $2, $3, r.input_hash, r.has_text, r.fully_structured, r.evaluable,
                r.facts, r.leaves, ARRAY(SELECT jsonb_array_elements_text(r.blockers)), now()
         FROM jsonb_to_recordset($1) AS r(program_id uuid, input_hash text, has_text boolean,
                fully_structured boolean, evaluable boolean, facts jsonb, leaves jsonb,
                blockers jsonb)
         JOIN programs p ON p.id = r.program_id
         ON CONFLICT (program_id) DO UPDATE SET
           extractor = EXCLUDED.extractor, extractor_version = EXCLUDED.extractor_version,
           input_hash = EXCLUDED.input_hash, has_text = EXCLUDED.has_text,
           fully_structured = EXCLUDED.fully_structured, evaluable = EXCLUDED.evaluable,
           facts = EXCLUDED.facts, leaves = EXCLUDED.leaves, blockers = EXCLUDED.blockers,
           computed_at = EXCLUDED.computed_at",
    )
    .bind(Value::Array(rows))
    .bind(EXTRACTOR)
    .bind(EXTRACTOR_VERSION)
    .execute(pool)
    .await
    .context("rule_v1 extraction batch upsert failed")?;
    Ok(done.rows_affected() as usize)
}

/// Upsert one extraction in its own transaction. `false` when the programme
/// was deleted since the read.
async fn store(pool: &PgPool, program_id: Uuid, input_hash: &str, c: &Compiled) -> Result<bool> {
    let mut tx = pool.begin().await?;
    let done = sqlx::query(
        "INSERT INTO program_rule_extractions
           (program_id, extractor, extractor_version, input_hash, has_text, fully_structured,
            evaluable, facts, leaves, blockers, computed_at)
         SELECT p.id, $2, $3, $4, $5, $6, $7, $8, $9, $10, now() FROM programs p WHERE p.id = $1
         ON CONFLICT (program_id) DO UPDATE SET
           extractor = EXCLUDED.extractor, extractor_version = EXCLUDED.extractor_version,
           input_hash = EXCLUDED.input_hash, has_text = EXCLUDED.has_text,
           fully_structured = EXCLUDED.fully_structured, evaluable = EXCLUDED.evaluable,
           facts = EXCLUDED.facts, leaves = EXCLUDED.leaves, blockers = EXCLUDED.blockers,
           computed_at = EXCLUDED.computed_at",
    )
    .bind(program_id)
    .bind(EXTRACTOR)
    .bind(EXTRACTOR_VERSION)
    .bind(input_hash)
    .bind(c.has_text)
    .bind(c.fully_structured)
    .bind(c.evaluable)
    .bind(Value::Array(c.facts.clone()))
    .bind(Value::Array(c.leaves.clone()))
    .bind(&c.blockers)
    .execute(&mut *tx)
    .await
    .context("rule_v1 extraction upsert failed")?;
    tx.commit().await?;
    Ok(done.rows_affected() > 0)
}

/// Operational event of a failed refresh; resolved by the next clean run.
pub const REFRESH_FINGERPRINT: &str = "job:text_rules_refresh";

/// [`refresh`] with its outcome recorded: a failed run or failed rows open
/// [`REFRESH_FINGERPRINT`], a clean run resolves it. Never fails the caller.
pub async fn refresh_monitored(pool: &PgPool) -> Option<RefreshSummary> {
    use crate::operations::{self, EventCommand, Severity};
    let outcome = refresh(pool).await;
    let problem = match &outcome {
        Ok(summary) if summary.failed == 0 && summary.parked == 0 => None,
        Ok(summary) => Some(format!(
            "{} rows failed, {} parked after {MAX_FAILURES} failures",
            summary.failed, summary.parked
        )),
        Err(error) => Some(crate::error_safety::persisted_error(error)),
    };
    match problem {
        None => {
            if let Err(error) =
                operations::resolve_event(pool, REFRESH_FINGERPRINT, None, None).await
            {
                warn!(error = %crate::error_safety::persisted_error(&error), "text_rules refresh event resolve failed");
            }
        }
        Some(problem) => {
            tracing::error!(error = %problem, "text_rules refresh incomplete; stored extractions kept");
            if let Err(error) = operations::record_event(
                pool,
                &EventCommand {
                    fingerprint: REFRESH_FINGERPRINT,
                    severity: Severity::Error,
                    component: "text_rules",
                    event_type: "text_rules_refresh_failed",
                    subject_type: None,
                    subject_id: None,
                    message:
                        "rule_v1 text-rule measurement refresh failed; stored extractions kept",
                    details: serde_json::json!({
                        "extractor_version": EXTRACTOR_VERSION,
                        "failed_rows": outcome.as_ref().map(|s| s.failed).ok(),
                        "parked_rows": outcome.as_ref().map(|s| s.parked).ok(),
                    }),
                },
            )
            .await
            {
                warn!(error = %crate::error_safety::persisted_error(&error), "text_rules refresh event record failed");
            }
        }
    }
    outcome.ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    async fn pool() -> PgPool {
        let url = std::env::var("TEST_DATABASE_URL")
            .unwrap_or_else(|_| "postgres://wello:wello@localhost:5432/wello".into());
        sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect(&url)
            .await
            .expect("isolated test database required")
    }

    async fn programme(pool: &PgPool, payload: Value) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO programs (id, program_type, source_type, source_id, title, program_status,
                                   is_active, raw_payload, regions)
             VALUES ($1, 'benefit', 'gov_benefits', $2, '중위소득 테스트', 'active', true, $3, '{}')",
        )
        .bind(id)
        .bind(format!("t{}", id.simple()))
        .bind(payload)
        .execute(pool)
        .await
        .unwrap();
        id
    }

    async fn rules(pool: &PgPool, id: Uuid) -> Vec<(i32, Option<String>, Value)> {
        sqlx::query_as(
            "SELECT version, created_by, rule_json FROM eligibility_rules WHERE program_id = $1 ORDER BY version",
        )
        .bind(id)
        .fetch_all(pool)
        .await
        .unwrap()
    }

    /// The refresh stores facts by payload hash and version, never touches
    /// `eligibility_rules` (another writer's row stays byte for byte), and is a
    /// no-op on an unchanged payload.
    #[tokio::test]
    async fn refresh_is_measurement_only_and_versioned_by_payload_hash() {
        let pool = pool().await;
        let id = programme(
            &pool,
            json!({"지원대상": "기준 중위소득 100% 이하인 자", "사용자구분": "개인"}),
        )
        .await;
        sqlx::query(
            "INSERT INTO eligibility_rules (program_id, rule_json, hard_filter_json, version, created_by)
             VALUES ($1, $2, $2, 1, 'ingestion:source_codes')",
        )
        .bind(id)
        .bind(json!({"all": [{"field": "region_code", "op": "in", "value": ["busan"],
            "origin": "source_codes"}]}))
        .execute(&pool)
        .await
        .unwrap();
        let before = rules(&pool, id).await;
        let only = [id];
        let first = refresh_programs(&pool, Some(&only), Budget::SCHEDULED)
            .await
            .unwrap();
        assert_eq!((first.examined, first.written, first.failed), (1, 1, 0));
        assert_eq!((first.fully_structured, first.evaluable), (1, 1));
        let (evaluable, current, quote, leaf): (bool, bool, String, String) =
            sqlx::query_as(&format!(
                "SELECT e.evaluable, e.input_hash = {} AND e.extractor_version = $2,
                        e.facts->0->>'quote', e.leaves->0->>'field'
                 FROM program_rule_extractions e JOIN programs p ON p.id = e.program_id
                 WHERE e.program_id = $1",
                input_hash_sql("p.")
            ))
            .bind(id)
            .bind(EXTRACTOR_VERSION)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(evaluable && current);
        assert_eq!(quote, "기준 중위소득 100% 이하");
        assert_eq!(leaf, "income_median_pct");
        assert_eq!(
            rules(&pool, id).await,
            before,
            "eligibility_rules untouched"
        );

        let again = refresh_programs(&pool, Some(&only), Budget::SCHEDULED)
            .await
            .unwrap();
        assert_eq!(again.written, 0);

        sqlx::query("UPDATE programs SET raw_payload = $2 WHERE id = $1")
            .bind(id)
            .bind(json!({"지원대상": "관내 거주 청년", "사용자구분": "개인"}))
            .execute(&pool)
            .await
            .unwrap();
        let changed = refresh_programs(&pool, Some(&only), Budget::SCHEDULED)
            .await
            .unwrap();
        assert_eq!((changed.written, changed.evaluable), (1, 0));
        assert_eq!(
            rules(&pool, id).await,
            before,
            "eligibility_rules untouched"
        );

        sqlx::query("DELETE FROM programs WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
    }

    /// `n` published programmes with no extraction, as after the migration.
    async fn pending_programmes(pool: &PgPool, n: i32) -> Vec<Uuid> {
        sqlx::query_scalar(
            "INSERT INTO programs (id, program_type, source_type, source_id, title, program_status,
                                   is_active, raw_payload, regions)
             SELECT gen_random_uuid(), 'benefit', 'gov_benefits', 'bound-' || gen_random_uuid(),
                    '상한 테스트', 'active', true,
                    jsonb_build_object('지원대상', '기준 중위소득 ' || (i % 150 + 1) || '% 이하인 자',
                                       '사용자구분', '개인'),
                    '{}'
             FROM generate_series(1, $1) AS i
             RETURNING id",
        )
        .bind(n)
        .fetch_all(pool)
        .await
        .unwrap()
    }

    async fn stored(pool: &PgPool, ids: &[Uuid]) -> i64 {
        sqlx::query_scalar(
            "SELECT count(*) FROM program_rule_extractions WHERE program_id = ANY($1)",
        )
        .bind(ids)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    /// The 2026-10-22 incident: ~13k programmes pending at once. One run
    /// writes at most `max_rows`, ends within its budget, and the next run
    /// goes on with rows the first did not write.
    #[tokio::test]
    async fn a_run_over_thirteen_thousand_pending_rows_is_bounded_and_resumes() {
        let pool = pool().await;
        let ids = pending_programmes(&pool, 13_000).await;
        let only = ids.as_slice();

        let started = Instant::now();
        let first = refresh_programs(&pool, Some(only), Budget::SCHEDULED)
            .await
            .unwrap();
        let elapsed = started.elapsed();
        assert_eq!(first.pending, 13_000);
        assert_eq!(
            (first.examined, first.written, first.failed),
            (1_000, 1_000, 0)
        );
        assert_eq!(first.remaining(), 12_000);
        // The deadline only stops new batches, so a run may pass it by one
        // batch; locally the whole run takes a few seconds.
        assert!(
            elapsed < Budget::SCHEDULED.max_duration + Duration::from_secs(30),
            "{elapsed:?}"
        );
        assert_eq!(stored(&pool, only).await, 1_000);

        let second = refresh_programs(&pool, Some(only), Budget::SCHEDULED)
            .await
            .unwrap();
        assert_eq!((second.pending, second.written), (12_000, 1_000));
        assert_eq!(
            stored(&pool, only).await,
            2_000,
            "the second run wrote other rows"
        );

        // An exhausted deadline starts no batch and writes nothing.
        let none = refresh_programs(
            &pool,
            Some(only),
            Budget {
                max_rows: 13_000,
                max_duration: Duration::ZERO,
            },
        )
        .await
        .unwrap();
        assert_eq!((none.pending, none.examined, none.written), (11_000, 0, 0));

        // A version change makes stored rows pending again, ahead of the
        // missing ones.
        let stale_ids: Vec<Uuid> = sqlx::query_scalar(
            "UPDATE program_rule_extractions SET extractor_version = extractor_version + 1
             WHERE program_id IN (SELECT program_id FROM program_rule_extractions
                                  WHERE program_id = ANY($1) LIMIT 5)
             RETURNING program_id",
        )
        .bind(&ids)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(stale_ids.len(), 5);
        let stale = refresh_programs(
            &pool,
            Some(only),
            Budget {
                max_rows: 10,
                max_duration: Duration::from_secs(60),
            },
        )
        .await
        .unwrap();
        assert_eq!((stale.pending, stale.written), (11_005, 10));
        let rewritten: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM program_rule_extractions
             WHERE program_id = ANY($1) AND extractor_version = $2",
        )
        .bind(&stale_ids)
        .bind(EXTRACTOR_VERSION)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(rewritten, 5, "stale rows are rewritten first");
        assert_eq!(stored(&pool, only).await, 2_005, "then 5 missing rows");

        sqlx::query("DELETE FROM programs WHERE id = ANY($1)")
            .bind(&ids)
            .execute(&pool)
            .await
            .unwrap();
    }

    /// One bad row in a batch is counted as failed; the rest of the batch is
    /// still written.
    #[tokio::test]
    async fn a_failed_batch_falls_back_to_rows_and_keeps_the_good_ones() {
        let pool = pool().await;
        let ids = pending_programmes(&pool, 3).await;
        let compiled = compile_row(
            "gov_benefits",
            &serde_json::json!({"지원대상": "기준 중위소득 100% 이하인 자"}),
        );
        let batch: Vec<Pending> = ids
            .iter()
            .enumerate()
            .map(|(i, id)| Pending {
                program_id: *id,
                // The table's CHECK rejects a hash that is not 64 hex digits.
                input_hash: if i == 1 { "bad".into() } else { "0".repeat(64) },
                compiled: compiled.clone(),
            })
            .collect();
        let outcome = store_batch(&pool, &batch).await;
        assert_eq!((outcome.written, outcome.failed), (2, 1));
        assert_eq!(outcome.failed_ids, vec![ids[1]]);
        assert_eq!(stored(&pool, &ids).await, 2);

        sqlx::query("DELETE FROM programs WHERE id = ANY($1)")
            .bind(&ids)
            .execute(&pool)
            .await
            .unwrap();
    }

    /// Fails every write of the programmes listed in `rule_v1_test_fail`.
    async fn failing_writes(pool: &PgPool, on: bool) {
        let statements: &[&str] = if on {
            &[
                "CREATE TABLE IF NOT EXISTS rule_v1_test_fail (program_id uuid PRIMARY KEY)",
                "CREATE OR REPLACE FUNCTION rule_v1_test_fail() RETURNS trigger AS $$
                 BEGIN
                   IF EXISTS (SELECT 1 FROM rule_v1_test_fail WHERE program_id = NEW.program_id) THEN
                     RAISE EXCEPTION 'rule_v1 test failure';
                   END IF;
                   RETURN NEW;
                 END $$ LANGUAGE plpgsql",
                "DROP TRIGGER IF EXISTS rule_v1_test_fail ON program_rule_extractions",
                "CREATE TRIGGER rule_v1_test_fail BEFORE INSERT OR UPDATE ON program_rule_extractions
                 FOR EACH ROW EXECUTE FUNCTION rule_v1_test_fail()",
            ]
        } else {
            &[
                "DROP TRIGGER IF EXISTS rule_v1_test_fail ON program_rule_extractions",
                "DROP FUNCTION IF EXISTS rule_v1_test_fail()",
                "DROP TABLE IF EXISTS rule_v1_test_fail",
            ]
        };
        for statement in statements {
            sqlx::query(statement).execute(pool).await.unwrap();
        }
    }

    async fn failures(pool: &PgPool, id: Uuid) -> Option<i32> {
        sqlx::query_scalar(
            "SELECT failures FROM program_rule_extraction_failures WHERE program_id = $1",
        )
        .bind(id)
        .fetch_optional(pool)
        .await
        .unwrap()
    }

    /// A row that keeps failing goes behind the others, is parked after
    /// MAX_FAILURES failures, and comes back when its input changes.
    #[tokio::test]
    async fn a_row_that_keeps_failing_goes_last_and_is_parked_until_its_input_changes() {
        let pool = pool().await;
        let mut ids = pending_programmes(&pool, 4).await;
        ids.sort();
        let bad = ids[0];
        failing_writes(&pool, true).await;
        sqlx::query("INSERT INTO rule_v1_test_fail VALUES ($1)")
            .bind(bad)
            .execute(&pool)
            .await
            .unwrap();
        let only = ids.as_slice();
        let two = Budget {
            max_rows: 2,
            max_duration: Duration::from_secs(60),
        };
        // First by id, the bad row fails once.
        let first = refresh_programs(&pool, Some(only), two).await.unwrap();
        assert_eq!((first.written, first.failed), (1, 1), "{first:?}");
        assert_eq!(failures(&pool, bad).await, Some(1));
        // It now goes behind the two rows not written yet.
        let second = refresh_programs(&pool, Some(only), two).await.unwrap();
        assert_eq!((second.written, second.failed), (2, 0), "{second:?}");
        assert_eq!(stored(&pool, &ids[1..]).await, 3);
        assert_eq!(failures(&pool, bad).await, Some(1), "not tried again");
        // Retried alone until parked.
        for n in 2..=MAX_FAILURES {
            let run = refresh_programs(&pool, Some(only), two).await.unwrap();
            assert_eq!((run.failed, run.parked), (1, 0), "{run:?}");
            assert_eq!(failures(&pool, bad).await, Some(n));
        }
        let parked = refresh_programs(&pool, Some(only), two).await.unwrap();
        assert_eq!(
            (
                parked.pending,
                parked.parked,
                parked.examined,
                parked.remaining()
            ),
            (0, 1, 0, 0),
            "{parked:?}"
        );
        // A new input is pending again; once it writes, its count is gone.
        sqlx::query("DELETE FROM rule_v1_test_fail")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("UPDATE programs SET raw_payload = raw_payload || '{\"선정기준\":\"관내 거주\"}' WHERE id = $1")
            .bind(bad)
            .execute(&pool)
            .await
            .unwrap();
        let back = refresh_programs(&pool, Some(only), two).await.unwrap();
        assert_eq!(
            (back.pending, back.parked, back.written),
            (1, 0, 1),
            "{back:?}"
        );
        assert_eq!(failures(&pool, bad).await, None);

        failing_writes(&pool, false).await;
        sqlx::query("DELETE FROM programs WHERE id = ANY($1)")
            .bind(&ids)
            .execute(&pool)
            .await
            .unwrap();
    }

    /// After a run that left nothing pending, an unchanged watermark skips
    /// the full recompute; a new programme ends the skip.
    #[tokio::test]
    async fn an_unchanged_watermark_skips_the_recompute() {
        let pool = pool().await;
        if watermark(&pool).await.unwrap().1 > 0 {
            eprintln!("an ingestion run is running in the test database; skipping");
            return;
        }
        let mut clean = false;
        for _ in 0..30 {
            let run = refresh(&pool).await.unwrap();
            if run.skipped_unchanged || (run.remaining() == 0 && run.parked == 0 && run.failed == 0)
            {
                clean = true;
                break;
            }
        }
        assert!(clean, "the test database never came clean");
        let skipped = refresh(&pool).await.unwrap();
        assert!(skipped.skipped_unchanged, "{skipped:?}");

        let ids = pending_programmes(&pool, 1).await;
        let run = refresh(&pool).await.unwrap();
        assert!(!run.skipped_unchanged, "{run:?}");
        assert_eq!(stored(&pool, &ids).await, 1);

        sqlx::query("DELETE FROM programs WHERE id = ANY($1)")
            .bind(&ids)
            .execute(&pool)
            .await
            .unwrap();
    }
}
