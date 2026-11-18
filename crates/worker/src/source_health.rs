//! "Success but empty" guard: a source run that completes while the
//! catalogue it feeds has gone empty, collapsed, or entirely closed is
//! reported as degraded instead of a plain success.
//!
//! The scholarship case motivated it: 1,646 records, all closed, latest
//! deadline 2025-12-31, because the dataset URL pointed at a fixed yearly
//! file. Every run "succeeded".

use crate::operations::{self, EventCommand, Severity};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use sqlx::PgPool;

/// Day-over-day drop of currently served programmes that is treated as a
/// collapse (percent).
const DEFAULT_MAX_DROP_PCT: i64 = 30;
/// Below this previous count a percentage drop is noise (a few programmes
/// closing), so it is only a warning.
pub const MIN_BASE_FOR_DROP_RULE: i64 = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputPolicy {
    /// Minimum currently served programmes for the source.
    pub floor: i64,
    pub max_drop_pct: i64,
}

impl OutputPolicy {
    /// `SOURCE_ACTIVE_FLOOR_<SOURCE>` (for example
    /// `SOURCE_ACTIVE_FLOOR_YOUTH_CENTER=500`, default 1: an empty source is
    /// always degraded) and `SOURCE_ACTIVE_MAX_DROP_PCT` (default 30).
    pub fn from_env(source: &str) -> Result<Self> {
        let floor_var = format!("SOURCE_ACTIVE_FLOOR_{}", source.to_ascii_uppercase());
        let floor = read_i64(&floor_var, 1, 0, 1_000_000)?;
        let max_drop_pct = read_i64("SOURCE_ACTIVE_MAX_DROP_PCT", DEFAULT_MAX_DROP_PCT, 1, 100)?;
        Ok(Self {
            floor,
            max_drop_pct,
        })
    }
}

fn read_i64(name: &str, default: i64, min: i64, max: i64) -> Result<i64> {
    let value = match std::env::var(name) {
        Ok(value) if !value.trim().is_empty() => value
            .trim()
            .parse::<i64>()
            .with_context(|| format!("{name} must be an integer"))?,
        _ => default,
    };
    anyhow::ensure!(
        (min..=max).contains(&value),
        "{name} must be between {min} and {max}"
    );
    Ok(value)
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SourceOutput {
    /// Every programme row of the source, closed or not.
    pub total: i64,
    /// Rows the public catalogue serves (`current_programs`).
    pub current: i64,
    pub previous_current: Option<i64>,
    pub max_application_end_at: Option<DateTime<Utc>>,
    pub reasons: Vec<&'static str>,
    /// Non-degrading observations (a drop from a small base).
    pub warnings: Vec<&'static str>,
}

impl SourceOutput {
    pub fn degraded(&self) -> bool {
        !self.reasons.is_empty()
    }
}

/// Pure decision, separated for tests: `(degrading reasons, warnings)`.
/// The day-over-day drop degrades only from a previous count of at least
/// [`MIN_BASE_FOR_DROP_RULE`]; from a smaller base it is a warning.
pub fn assess(
    total: i64,
    current: i64,
    previous: Option<i64>,
    policy: &OutputPolicy,
) -> (Vec<&'static str>, Vec<&'static str>) {
    let mut reasons = Vec::new();
    let mut warnings = Vec::new();
    if total > 0 && current == 0 {
        reasons.push("all_closed");
    }
    if current < policy.floor {
        reasons.push("below_floor");
    }
    if let Some(previous) = previous.filter(|previous| *previous > 0) {
        if current * 100 < previous * (100 - policy.max_drop_pct) {
            if previous >= MIN_BASE_FOR_DROP_RULE {
                reasons.push("dropped_day_over_day");
            } else {
                warnings.push("dropped_day_over_day_small_base");
            }
        }
    }
    (reasons, warnings)
}

pub fn reasons(
    total: i64,
    current: i64,
    previous: Option<i64>,
    policy: &OutputPolicy,
) -> Vec<&'static str> {
    assess(total, current, previous, policy).0
}

/// Measures the source after a successful run, compares with the previous
/// day's reading and opens (or resolves) `source_output:<source>`.
pub async fn check(
    pool: &PgPool,
    source: &str,
    previous_current: Option<i64>,
    policy: &OutputPolicy,
) -> Result<SourceOutput> {
    let (total, max_end): (i64, Option<DateTime<Utc>>) = sqlx::query_as(
        "SELECT count(*), max(application_end_at) FROM programs WHERE source_type = $1",
    )
    .bind(source)
    .fetch_one(pool)
    .await?;
    let current: i64 =
        sqlx::query_scalar("SELECT count(*) FROM current_programs WHERE source_type = $1")
            .bind(source)
            .fetch_one(pool)
            .await?;
    let (reasons, warnings) = assess(total, current, previous_current, policy);
    let output = SourceOutput {
        total,
        current,
        previous_current,
        max_application_end_at: max_end,
        reasons,
        warnings,
    };
    let fingerprint = format!("source_output:{source}");
    let small_drop_fingerprint = format!("source_output_small_drop:{source}");
    if output.degraded() {
        tracing::error!(
            event = "source_output_degraded",
            source,
            total,
            current,
            previous = ?previous_current,
            reasons = ?output.reasons,
            "source run succeeded but its served catalogue is empty, collapsed or closed"
        );
        operations::record_event(
            pool,
            &EventCommand {
                fingerprint: &fingerprint,
                severity: Severity::Error,
                component: "ingestion",
                event_type: "source_output_degraded",
                subject_type: Some("program_source"),
                subject_id: Some(source),
                message:
                    "수집은 성공했지만 서비스 중인 공고가 비었거나 급감했거나 모두 마감되었습니다.",
                details: serde_json::json!({
                    "source": source,
                    "total": total,
                    "current": current,
                    "previous_current": previous_current,
                    "floor": policy.floor,
                    "max_drop_pct": policy.max_drop_pct,
                    "max_application_end_at": max_end,
                    "reasons": output.reasons,
                }),
            },
        )
        .await?;
    } else if !output.warnings.is_empty() {
        tracing::warn!(
            event = "source_output_small_drop",
            source,
            current,
            previous = ?previous_current,
            "served programmes dropped from a small base"
        );
        operations::record_event(
            pool,
            &EventCommand {
                // Its own fingerprint: a small-base warning must never
                // downgrade an open degraded-output error.
                fingerprint: &small_drop_fingerprint,
                severity: Severity::Warning,
                component: "ingestion",
                event_type: "source_output_small_drop",
                subject_type: Some("program_source"),
                subject_id: Some(source),
                message:
                    "서비스 중인 공고가 전날보다 크게 줄었습니다(기준 수가 작아 경고로만 기록).",
                details: serde_json::json!({
                    "source": source,
                    "total": total,
                    "current": current,
                    "previous_current": previous_current,
                    "max_drop_pct": policy.max_drop_pct,
                    "warnings": output.warnings,
                }),
            },
        )
        .await?;
    } else {
        operations::resolve_event(
            pool,
            &fingerprint,
            None,
            Some("source output back above its floor"),
        )
        .await?;
        operations::resolve_event(
            pool,
            &small_drop_fingerprint,
            None,
            Some("source output steady again"),
        )
        .await?;
    }
    Ok(output)
}

/// The previous reading stored by the daily job of this source (its details
/// carry `{"source": <name>, "output": {"current": n}}`).
pub async fn previous_current(pool: &PgPool, source: &str) -> Result<Option<i64>> {
    let value: Option<Value> = sqlx::query_scalar(
        "SELECT details->'output'->'current' FROM worker_daily_jobs
         WHERE job_name LIKE 'daily:%' AND details->>'source' = $1
         ORDER BY last_success_at DESC NULLS LAST LIMIT 1",
    )
    .bind(source)
    .fetch_optional(pool)
    .await?
    .flatten();
    Ok(value.and_then(|value| value.as_i64()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    const POLICY: OutputPolicy = OutputPolicy {
        floor: 1,
        max_drop_pct: 30,
    };

    #[test]
    fn empty_closed_and_collapsed_outputs_are_degraded() {
        // The scholarship state of 2026-10-16: 1,646 rows, none served.
        assert_eq!(
            reasons(1_646, 0, Some(0), &POLICY),
            vec!["all_closed", "below_floor"]
        );
        assert_eq!(reasons(0, 0, None, &POLICY), vec!["below_floor"]);
        assert_eq!(
            reasons(2_000, 690, Some(1_000), &POLICY),
            vec!["dropped_day_over_day"]
        );
        assert!(reasons(2_000, 700, Some(1_000), &POLICY).is_empty());
        assert!(reasons(2_000, 1_200, None, &POLICY).is_empty());
        // From a base under 50 a collapse is only a warning.
        assert_eq!(
            assess(40, 10, Some(49), &POLICY),
            (vec![], vec!["dropped_day_over_day_small_base"])
        );
        assert_eq!(
            assess(400, 30, Some(50), &POLICY),
            (vec!["dropped_day_over_day"], vec![])
        );
        let floor = OutputPolicy {
            floor: 500,
            ..POLICY
        };
        assert_eq!(reasons(2_000, 499, None, &floor), vec!["below_floor"]);
    }

    #[tokio::test]
    async fn all_closed_source_opens_an_error_and_recovery_resolves_it() {
        let url = std::env::var("TEST_DATABASE_URL")
            .unwrap_or_else(|_| "postgres://wello:wello@localhost:5432/wello".into());
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect(&url)
            .await
            .expect("isolated test database is required");
        let source = format!("health_{}", Uuid::new_v4().simple());
        for n in 0..3 {
            sqlx::query(
                "INSERT INTO programs(program_type,source_type,source_id,title,program_status,is_active,application_end_at)
                 VALUES('scholarship',$1,$2,'지난 장학금','closed',false,'2025-12-31T14:59:59Z')",
            )
            .bind(&source)
            .bind(format!("s{n}"))
            .execute(&pool)
            .await
            .unwrap();
        }
        let output = check(&pool, &source, Some(0), &POLICY).await.unwrap();
        assert!(output.degraded());
        assert_eq!((output.total, output.current), (3, 0));
        let fingerprint = format!("source_output:{source}");
        let (status, severity, details): (String, String, Value) = sqlx::query_as(
            "SELECT status, severity, details FROM operational_events WHERE fingerprint=$1",
        )
        .bind(&fingerprint)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!((status.as_str(), severity.as_str()), ("open", "error"));
        assert_eq!(
            details["reasons"],
            serde_json::json!(["all_closed", "below_floor"])
        );

        sqlx::query(
            "UPDATE programs SET program_status='active', is_active=true, application_end_at=now()+interval '30 days'
             WHERE source_type=$1 AND source_id='s0'",
        )
        .bind(&source)
        .execute(&pool)
        .await
        .unwrap();
        let output = check(&pool, &source, Some(0), &POLICY).await.unwrap();
        assert!(!output.degraded(), "{output:?}");
        let status: String =
            sqlx::query_scalar("SELECT status FROM operational_events WHERE fingerprint=$1")
                .bind(&fingerprint)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, "resolved");
        sqlx::query("DELETE FROM programs WHERE source_type=$1")
            .bind(&source)
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
    }
}
