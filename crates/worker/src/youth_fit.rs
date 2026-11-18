//! Stores the [`mazimi_core::youth_fit`] verdict of every published programme
//! in `program_youth_fit`.
//!
//! A row is (re)classified when it has no verdict yet, when its stored
//! `classifier_version` is not the current one, or when the hash of the
//! classifier's inputs changed (new raw payload, curated youth flag, ages).
//! Unchanged rows are skipped, so a run after the first only writes what
//! ingestion changed. Runs inside data maintenance (worker start, every six
//! hours, after the daily source sweep); `youth-fit-refresh` runs it once.
//!
//! A row without a stored verdict is not demoted anywhere, so a failed or
//! late run shows rows rather than hiding them. Human overrides
//! (`reviewed_*`) are never written here.

use anyhow::{Context, Result};
use mazimi_core::youth_fit::{self, ProgramFacts, Verdict, RAW_KEYS};
use serde_json::Value;
use sqlx::PgPool;
use std::collections::BTreeMap;
use uuid::Uuid;

/// Rows written per statement.
const BATCH: usize = 500;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RefreshSummary {
    /// Published programmes read.
    pub examined: usize,
    /// Verdicts inserted or replaced.
    pub written: usize,
    /// Current verdict counts over every examined row, by verdict code.
    pub verdicts: BTreeMap<&'static str, usize>,
}

#[derive(sqlx::FromRow)]
struct InputRow {
    id: Uuid,
    source_type: String,
    title: String,
    summary: Option<String>,
    min_age: Option<i32>,
    max_age: Option<i32>,
    curated_youth: bool,
    raw: Value,
    stored_version: Option<String>,
    stored_hash: Option<String>,
}

/// `jsonb_build_object('k', p.raw_payload->'k', …)` over [`RAW_KEYS`]: only
/// the fields the classifier reads leave the database.
fn raw_keys_sql() -> String {
    let pairs: Vec<String> = RAW_KEYS
        .iter()
        .map(|key| format!("'{key}', p.raw_payload->'{key}'"))
        .collect();
    format!(
        "jsonb_strip_nulls(jsonb_build_object({}))",
        pairs.join(", ")
    )
}

/// One verdict row to write.
struct Write {
    program_id: Uuid,
    verdict: &'static str,
    target: Option<&'static str>,
    rule: &'static str,
    evidence: Option<String>,
    input_hash: String,
}

pub async fn refresh(pool: &PgPool) -> Result<RefreshSummary> {
    let rows = sqlx::query_as::<_, InputRow>(&format!(
        "SELECT p.id, p.source_type, p.title, left(p.summary, 600) AS summary,
                p.min_age, p.max_age,
                COALESCE(p.normalized_payload->'youth_relevant' = 'true'::jsonb, false) AS curated_youth,
                {raw} AS raw,
                f.classifier_version AS stored_version, f.input_hash AS stored_hash
         FROM current_programs p
         LEFT JOIN program_youth_fit f ON f.program_id = p.id",
        raw = raw_keys_sql()
    ))
    .fetch_all(pool)
    .await
    .context("youth_fit inputs query failed")?;

    let mut summary = RefreshSummary {
        examined: rows.len(),
        ..RefreshSummary::default()
    };
    let mut writes = Vec::new();
    for row in &rows {
        let facts = ProgramFacts {
            source_type: &row.source_type,
            title: &row.title,
            summary: row.summary.as_deref(),
            min_age: row.min_age,
            max_age: row.max_age,
            curated_youth: row.curated_youth,
            raw: &row.raw,
        };
        let hash = youth_fit::input_hash(&facts);
        let decision = youth_fit::classify(&facts);
        *summary
            .verdicts
            .entry(decision.verdict.as_str())
            .or_default() += 1;
        let current = row.stored_version.as_deref() == Some(youth_fit::classifier_version())
            && row.stored_hash.as_deref() == Some(hash.as_str());
        if current {
            continue;
        }
        writes.push(Write {
            program_id: row.id,
            verdict: decision.verdict.as_str(),
            target: decision.target.map(youth_fit::Target::as_str),
            rule: decision.rule,
            evidence: decision
                .evidence
                .map(|e| e.chars().take(300).collect::<String>())
                .filter(|e| !e.is_empty()),
            input_hash: hash,
        });
    }
    for chunk in writes.chunks(BATCH) {
        write(pool, chunk).await?;
        summary.written += chunk.len();
    }
    Ok(summary)
}

async fn write(pool: &PgPool, chunk: &[Write]) -> Result<()> {
    let ids: Vec<Uuid> = chunk.iter().map(|w| w.program_id).collect();
    let verdicts: Vec<&str> = chunk.iter().map(|w| w.verdict).collect();
    let targets: Vec<Option<&str>> = chunk.iter().map(|w| w.target).collect();
    let rules: Vec<&str> = chunk.iter().map(|w| w.rule).collect();
    let evidence: Vec<Option<&str>> = chunk.iter().map(|w| w.evidence.as_deref()).collect();
    let hashes: Vec<&str> = chunk.iter().map(|w| w.input_hash.as_str()).collect();
    // A programme deleted since the read is skipped by the join, not an error.
    sqlx::query(
        "INSERT INTO program_youth_fit
           (program_id, verdict, target, rule, evidence, classifier_version, input_hash, computed_at)
         SELECT u.id, u.verdict, u.target, u.rule, u.evidence, $7, u.hash, now()
         FROM UNNEST($1::uuid[], $2::text[], $3::text[], $4::text[], $5::text[], $6::text[])
              AS u(id, verdict, target, rule, evidence, hash)
         JOIN programs p ON p.id = u.id
         ON CONFLICT (program_id) DO UPDATE SET
           verdict = EXCLUDED.verdict, target = EXCLUDED.target, rule = EXCLUDED.rule,
           evidence = EXCLUDED.evidence, classifier_version = EXCLUDED.classifier_version,
           input_hash = EXCLUDED.input_hash, computed_at = EXCLUDED.computed_at",
    )
    .bind(&ids)
    .bind(&verdicts)
    .bind(&targets)
    .bind(&rules)
    .bind(&evidence)
    .bind(&hashes)
    .bind(youth_fit::classifier_version())
    .execute(pool)
    .await
    .context("youth_fit verdict upsert failed")?;
    Ok(())
}

/// Operational event of a failed refresh; resolved by the next success.
pub const REFRESH_FINGERPRINT: &str = "job:youth_fit_refresh";
/// Operational event of a demoted share that left its baseline.
pub const SHARE_FINGERPRINT: &str = "youth_fit:demoted_share";
/// A share more than this many percentage points from the baseline opens
/// [`SHARE_FINGERPRINT`]; only a share back within it resolves the event.
pub const SHARE_JUMP_POINTS: f64 = 5.0;
/// The baseline is the median share of the runs of this many past days. While
/// the event is open it stays the baseline stored in the event, so a
/// sustained jump never becomes its own baseline.
pub const SHARE_BASELINE_DAYS: i32 = 7;

/// [`refresh`] with its outcome recorded: a failure opens
/// [`REFRESH_FINGERPRINT`] (error), a success resolves it, stores the run's
/// verdict counts and checks the demoted share. Never fails the caller.
pub async fn refresh_monitored(pool: &PgPool) -> Option<RefreshSummary> {
    let outcome = refresh(pool).await;
    record_outcome(pool, outcome).await
}

pub(crate) async fn record_outcome(
    pool: &PgPool,
    outcome: Result<RefreshSummary>,
) -> Option<RefreshSummary> {
    use crate::operations::{self, EventCommand, Severity};
    match outcome {
        Err(error) => {
            tracing::error!(
                error = %crate::error_safety::persisted_error(&error),
                "youth_fit refresh failed; stored verdicts kept"
            );
            if let Err(error) = operations::record_event(
                pool,
                &EventCommand {
                    fingerprint: REFRESH_FINGERPRINT,
                    severity: Severity::Error,
                    component: "youth_fit",
                    event_type: "youth_fit_refresh_failed",
                    subject_type: None,
                    subject_id: None,
                    message: "청년 적합도(youth_fit) 갱신이 실패했습니다. 저장된 판정을 그대로 쓰고 다음 정비 때 다시 시도합니다.",
                    details: serde_json::json!({}),
                },
            )
            .await
            {
                tracing::error!(error = %error, "failed to persist youth_fit refresh incident");
            }
            None
        }
        Ok(summary) => {
            if let Err(error) = operations::resolve_event(
                pool,
                REFRESH_FINGERPRINT,
                None,
                Some("youth_fit refresh recovered"),
            )
            .await
            {
                tracing::error!(error = %error, "failed to resolve youth_fit refresh incident");
            }
            if let Err(error) = record_run(pool, &summary).await {
                tracing::error!(
                    error = %crate::error_safety::persisted_error(&error),
                    "youth_fit run counts not recorded"
                );
            }
            Some(summary)
        }
    }
}

/// Stores the run's counts and opens or resolves [`SHARE_FINGERPRINT`].
async fn record_run(pool: &PgPool, summary: &RefreshSummary) -> Result<()> {
    use crate::operations::{self, EventCommand, Severity};
    let share = if summary.examined == 0 {
        0.0
    } else {
        100.0 * demoted(summary) as f64 / summary.examined as f64
    };
    let share = (share * 100.0).round() / 100.0;
    let version = youth_fit::classifier_version();
    let baseline = share_baseline(pool, version).await?;
    sqlx::query(
        "INSERT INTO program_youth_fit_runs (classifier_version, examined, written, verdicts, demoted_share)
         VALUES ($1, $2, $3, $4, $5::numeric)",
    )
    .bind(version)
    .bind(i32::try_from(summary.examined).unwrap_or(i32::MAX))
    .bind(i32::try_from(summary.written).unwrap_or(i32::MAX))
    .bind(serde_json::to_value(&summary.verdicts)?)
    .bind(share)
    .execute(pool)
    .await?;
    let Some(baseline) = baseline else {
        return Ok(());
    };
    if (share - baseline).abs() > SHARE_JUMP_POINTS {
        operations::record_event(
            pool,
            &EventCommand {
                fingerprint: SHARE_FINGERPRINT,
                severity: Severity::Warning,
                component: "youth_fit",
                event_type: "youth_fit_demoted_share_jump",
                subject_type: None,
                subject_id: None,
                message: "청년 적합도에서 기본 목록에서 빠지는 공고 비율이 최근 7일 기준보다 5%p 넘게 바뀌었습니다. 원천 데이터나 분류 규칙 변경을 확인하세요.",
                details: serde_json::json!({
                    "baseline_share": baseline,
                    "baseline_version": version,
                    "demoted_share": share,
                    "verdicts": summary.verdicts,
                }),
            },
        )
        .await?;
    } else {
        operations::resolve_event(
            pool,
            SHARE_FINGERPRINT,
            None,
            Some("demoted share back within its baseline"),
        )
        .await?;
    }
    Ok(())
}

/// Resolution note of a share event closed because the rules changed.
pub const VERSION_CHANGED_NOTE: &str = "rule version changed: baseline restarted";

/// The baseline share for this rule `version`: the one stored in the open
/// event, else the median of this version's runs of the last
/// [`SHARE_BASELINE_DAYS`] days (None before its first run). An open event
/// whose baseline belongs to another version is resolved with
/// [`VERSION_CHANGED_NOTE`]: an intended rule change restarts the baseline
/// instead of holding the alert open. Read before the current run is stored.
async fn share_baseline(pool: &PgPool, version: &str) -> Result<Option<f64>> {
    let open: Option<(Option<f64>, Option<String>)> = sqlx::query_as(
        "SELECT CASE WHEN details->>'baseline_share' ~ '^[0-9]+(\\.[0-9]+)?$'
                     THEN (details->>'baseline_share')::float8 END,
                details->>'baseline_version'
         FROM operational_events WHERE fingerprint = $1 AND status <> 'resolved'",
    )
    .bind(SHARE_FINGERPRINT)
    .fetch_optional(pool)
    .await?;
    match open {
        Some((Some(share), Some(of))) if of == version => return Ok(Some(share)),
        Some(_) => {
            crate::operations::resolve_event(
                pool,
                SHARE_FINGERPRINT,
                None,
                Some(VERSION_CHANGED_NOTE),
            )
            .await?;
        }
        None => {}
    }
    let median: Option<f64> = sqlx::query_scalar(
        "SELECT percentile_cont(0.5) WITHIN GROUP (ORDER BY demoted_share::float8)
         FROM program_youth_fit_runs
         WHERE classifier_version = $1 AND ran_at > now() - make_interval(days => $2)",
    )
    .bind(version)
    .bind(SHARE_BASELINE_DAYS)
    .fetch_one(pool)
    .await?;
    Ok(median)
}

/// Demoted share of the examined rows, for logs.
pub fn demoted(summary: &RefreshSummary) -> usize {
    summary
        .verdicts
        .iter()
        .filter(|(code, _)| Verdict::parse(code).is_some_and(Verdict::demoted))
        .map(|(_, count)| count)
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn database_url() -> String {
        std::env::var("TEST_DATABASE_URL")
            .unwrap_or_else(|_| "postgres://wello:wello@localhost:5432/wello".into())
    }

    #[test]
    fn raw_keys_sql_reads_only_classifier_keys() {
        let sql = raw_keys_sql();
        for key in RAW_KEYS {
            assert!(
                sql.contains(&format!("'{key}', p.raw_payload->'{key}'")),
                "{key}"
            );
        }
        assert!(!sql.contains("지원내용"));
    }

    async fn event_status(pool: &PgPool, fingerprint: &str) -> Option<String> {
        sqlx::query_scalar("SELECT status FROM operational_events WHERE fingerprint = $1")
            .bind(fingerprint)
            .fetch_optional(pool)
            .await
            .unwrap()
    }

    fn summary(examined: usize, demoted_rows: usize) -> RefreshSummary {
        let mut verdicts = BTreeMap::new();
        verdicts.insert("fit", examined - demoted_rows);
        verdicts.insert("off_target", demoted_rows);
        RefreshSummary {
            examined,
            written: 0,
            verdicts,
        }
    }

    /// A failed refresh opens an error event, the next success resolves
    /// it; a jump of more than 5 points in the demoted share opens a
    /// warning that a calmer run resolves.
    #[tokio::test]
    async fn refresh_outcomes_open_and_resolve_operational_events() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect(&database_url())
            .await
            .expect("isolated test database required");
        assert!(record_outcome(&pool, Err(anyhow::anyhow!("boom")))
            .await
            .is_none());
        assert_eq!(
            event_status(&pool, REFRESH_FINGERPRINT).await.as_deref(),
            Some("open")
        );

        assert!(record_outcome(&pool, Ok(summary(1000, 300)))
            .await
            .is_some());
        assert_eq!(
            event_status(&pool, REFRESH_FINGERPRINT).await.as_deref(),
            Some("resolved")
        );

        let runs: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM program_youth_fit_runs WHERE demoted_share = 30.00",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(runs >= 1);
    }

    /// The share is compared with a baseline (the 7-day median, kept in the
    /// open event), not the previous run: a sustained jump stays open and
    /// only a share back near the baseline resolves it.
    #[tokio::test]
    async fn a_sustained_share_jump_stays_open_until_the_share_returns() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect(&database_url())
            .await
            .expect("isolated test database required");
        sqlx::query("DELETE FROM program_youth_fit_runs")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM operational_events WHERE fingerprint = $1")
            .bind(SHARE_FINGERPRINT)
            .execute(&pool)
            .await
            .unwrap();
        let status = || event_status(&pool, SHARE_FINGERPRINT);
        for _ in 0..3 {
            record_outcome(&pool, Ok(summary(1000, 300))).await;
        }
        assert_eq!(status().await, None);
        // 30% -> 40%: opens, and five more 40% runs (the 7-day median is
        // now 40%) keep it open against the stored 30% baseline.
        for _ in 0..6 {
            record_outcome(&pool, Ok(summary(1000, 400))).await;
            assert_eq!(status().await.as_deref(), Some("open"));
        }
        // 36% is still more than 5 points away; 34% is back.
        record_outcome(&pool, Ok(summary(1000, 360))).await;
        assert_eq!(status().await.as_deref(), Some("open"));
        record_outcome(&pool, Ok(summary(1000, 340))).await;
        assert_eq!(status().await.as_deref(), Some("resolved"));
    }

    /// A rule-version change restarts the baseline: the event opened
    /// against the old version's share is resolved with a note, and the new
    /// version's runs alone form the next baseline, so an intended rule
    /// change never leaves the alert open.
    #[tokio::test]
    async fn a_rule_version_change_restarts_the_share_baseline() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect(&database_url())
            .await
            .expect("isolated test database required");
        sqlx::query("DELETE FROM program_youth_fit_runs")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM operational_events WHERE fingerprint = $1")
            .bind(SHARE_FINGERPRINT)
            .execute(&pool)
            .await
            .unwrap();
        // Runs of an older rule version at 30%, and the alert they opened.
        sqlx::query(
            "INSERT INTO program_youth_fit_runs (classifier_version, examined, written, verdicts, demoted_share)
             SELECT 'youth_fit_v1_000000000000', 1000, 0, '{}'::jsonb, 30.00 FROM generate_series(1, 3)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO operational_events (fingerprint, severity, component, event_type, message, details)
             VALUES ($1, 'warning', 'youth_fit', 'youth_fit_demoted_share_jump', 'old',
                     jsonb_build_object('baseline_share', 30.0,
                                        'baseline_version', 'youth_fit_v1_000000000000'))",
        )
        .bind(SHARE_FINGERPRINT)
        .execute(&pool)
        .await
        .unwrap();
        let event = || async {
            sqlx::query_as::<_, (String, Option<String>)>(
                "SELECT status, resolution_note FROM operational_events WHERE fingerprint = $1",
            )
            .bind(SHARE_FINGERPRINT)
            .fetch_one(&pool)
            .await
            .unwrap()
        };
        // The new version moves the share to 40%: the old event resolves
        // with a note and no new one opens (no run of this version yet).
        record_outcome(&pool, Ok(summary(1000, 400))).await;
        assert_eq!(
            event().await,
            ("resolved".to_owned(), Some(VERSION_CHANGED_NOTE.to_owned()))
        );
        // Later runs compare with the new version's 40%, not the old 30%.
        record_outcome(&pool, Ok(summary(1000, 410))).await;
        assert_eq!(event().await.0, "resolved");
        record_outcome(&pool, Ok(summary(1000, 500))).await;
        assert_eq!(event().await.0, "open");
    }

    /// The table's CHECK lists must accept every code the classifier writes.
    #[test]
    fn migration_accepts_every_verdict_and_target_code() {
        let sql = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../infra/migrations/20261021000020_program_youth_fit.sql"
        ))
        .unwrap();
        for v in Verdict::ALL {
            assert!(sql.contains(&format!("'{}'", v.as_str())), "{}", v.as_str());
        }
        for t in youth_fit::Target::ALL {
            assert!(sql.contains(&format!("'{}'", t.as_str())), "{}", t.as_str());
        }
        assert!(sql.contains("program_youth_fit_runs"));
    }

    async fn insert_program(pool: &PgPool, title: &str, raw: Value) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO programs (id, program_type, source_type, source_id, title, program_status,
                                   is_active, raw_payload, normalized_payload)
             VALUES ($1, 'benefit', 'gov_benefits', $2, $3, 'active', true, $4, '{}'::jsonb)",
        )
        .bind(id)
        .bind(format!("youth-fit-test-{}", id.simple()))
        .bind(title)
        .bind(raw)
        .execute(pool)
        .await
        .unwrap();
        id
    }

    async fn stored(pool: &PgPool, id: Uuid) -> Option<(String, String, Option<String>, String)> {
        sqlx::query_as(
            "SELECT verdict, rule, evidence, classifier_version FROM program_youth_fit
             WHERE program_id = $1",
        )
        .bind(id)
        .fetch_optional(pool)
        .await
        .unwrap()
    }

    /// Classifies new rows, skips unchanged ones, recomputes on a content or
    /// version change and leaves a human override alone.
    #[tokio::test]
    async fn refresh_writes_changed_rows_only_and_keeps_reviews() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect(&database_url())
            .await
            .expect("isolated test database required");
        let farm = insert_program(
            &pool,
            "한우 사료 지원",
            json!({"사용자구분": "개인", "지원대상": "○ 한우 사육농가",
                   "supportConditions": {"JA0315": "Y", "JA0110": 18}}),
        )
        .await;
        let youth = insert_program(
            &pool,
            "청년 월세 지원",
            json!({"사용자구분": "개인", "지원대상": "○ 만 19~34세 청년"}),
        )
        .await;

        let first = refresh(&pool).await.unwrap();
        assert!(first.written >= 2, "{first:?}");
        assert_eq!(
            stored(&pool, farm).await,
            Some((
                "off_target".into(),
                "S2".into(),
                Some("flags=JA0315".into()),
                youth_fit::classifier_version().into()
            ))
        );
        assert_eq!(stored(&pool, youth).await.unwrap().0, "fit");

        // Nothing changed: neither row is written again.
        let computed_at = |id: Uuid| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, chrono::DateTime<chrono::Utc>>(
                    "SELECT computed_at FROM program_youth_fit WHERE program_id = $1",
                )
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap()
            }
        };
        let before = computed_at(farm).await;
        refresh(&pool).await.unwrap();
        assert_eq!(computed_at(farm).await, before);

        // A content change is reclassified without any version change.
        sqlx::query("UPDATE programs SET raw_payload = $2 WHERE id = $1")
            .bind(farm)
            .bind(
                json!({"사용자구분": "개인", "지원대상": "○ 귀촌인 및 한우 사육농가",
                     "supportConditions": {"JA0315": "Y"}}),
            )
            .execute(&pool)
            .await
            .unwrap();
        // A stale classifier version is recomputed even when the inputs
        // did not change.
        sqlx::query("UPDATE program_youth_fit SET classifier_version = 'youth_fit_v0' WHERE program_id = $1")
            .bind(youth)
            .execute(&pool)
            .await
            .unwrap();
        // A reviewed override survives every recompute.
        sqlx::query(
            "UPDATE program_youth_fit SET reviewed_verdict = 'off_target', reviewed_target = 'elder',
                    reviewed_by = 'test', reviewed_at = now() WHERE program_id = $1",
        )
        .bind(youth)
        .execute(&pool)
        .await
        .unwrap();
        refresh(&pool).await.unwrap();
        assert_eq!(stored(&pool, farm).await.unwrap().0, "fit");
        assert_eq!(
            stored(&pool, youth).await.unwrap().3,
            youth_fit::classifier_version().to_owned()
        );
        let review: (Option<String>, Option<String>) = sqlx::query_as(
            "SELECT reviewed_verdict, reviewed_by FROM program_youth_fit WHERE program_id = $1",
        )
        .bind(youth)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(review, (Some("off_target".into()), Some("test".into())));

        sqlx::query("DELETE FROM programs WHERE id = ANY($1)")
            .bind(vec![farm, youth])
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            stored(&pool, farm).await,
            None,
            "verdicts cascade with programmes"
        );
    }
}
