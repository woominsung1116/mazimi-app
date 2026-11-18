//! Bounded catalogue data maintenance: corporate-benefit matching and
//! expiry, residence-scope monitoring, the youth_fit verdicts and the
//! stored-rule health ([`crate::rule_health`]).
//!
//! Matching and expiry rules live in the database migration so one
//! transaction observes a consistent catalogue. This wrapper keeps scheduler
//! concerns, logging and error handling in the worker.

use anyhow::{Context, Result};
use sqlx::{FromRow, PgPool};

#[derive(Debug, Clone, Copy, Default, FromRow, PartialEq, Eq)]
pub struct MaintenanceSummary {
    pub duplicate_candidates_upserted: i64,
    pub quality_issues_observed: i64,
    pub programs_expired: i64,
}

pub async fn run(pool: &PgPool) -> Result<MaintenanceSummary> {
    sqlx::query("SELECT expire_benefit_feeds()")
        .execute(pool)
        .await
        .context("structured benefit feed expiry failed")?;
    let mut summary = sqlx::query_as::<_, MaintenanceSummary>(
        "SELECT duplicate_candidates_upserted, quality_issues_observed,
                programs_expired
         FROM run_corporate_benefit_maintenance(now())",
    )
    .fetch_one(pool)
    .await
    .context("corporate benefit data maintenance failed")?;
    // Before expiry: a row whose stored round ended moves to its next round
    // instead of closing (audit M4).
    let advanced = advance_application_rounds(pool)
        .await
        .context("advancing application rounds failed")?;
    if advanced > 0 {
        tracing::info!(advanced, "programmes moved to their next application round");
    }
    summary.programs_expired += sqlx::query_scalar::<_, i64>("SELECT expire_all_programs()")
        .fetch_one(pool)
        .await
        .context("all-category expiry failed")?;
    sqlx::query("UPDATE programs p SET program_status='active',updated_at=now() FROM current_programs c WHERE p.id=c.id AND p.program_status='upcoming' AND p.application_start_at<=now()")
        .execute(pool).await.context("opening scheduled programs failed")?;
    monitor_residence_scope(pool).await?;
    // Last, and never fatal: the steps above have committed, and a failed
    // refresh only keeps the stored verdicts (a missing one demotes nothing).
    // A failure opens its own operational event (job:youth_fit_refresh),
    // separate from this job's incident.
    if let Some(fit) = crate::youth_fit::refresh_monitored(pool).await {
        tracing::info!(
            examined = fit.examined,
            written = fit.written,
            demoted = crate::youth_fit::demoted(&fit),
            verdicts = ?fit.verdicts,
            "youth_fit verdicts refreshed"
        );
    }
    // Never fatal either: it only reads, and its event says what it found.
    if let Err(error) = crate::rule_health::monitor(pool).await {
        tracing::warn!(error = %error, "rule health check failed");
    }
    // The rule_v1 text-rule refresh is not here: its first backfill is
    // proportional to the catalogue, so it runs as its own bounded scheduled
    // job (worker main, every ten minutes).
    Ok(summary)
}

/// Unknown geography is never silently treated as nationwide. The catalogue can
/// retain those rows, while this source-level incident remains open until a
/// later normalization resolves them or the rows expire. Only counts are stored.
/// Moves every programme whose stored application round has ended to the
/// next round its source states (`normalized_payload.application_rounds`,
/// written by the normalizer when a notice states several): the window
/// columns take the round's days and the status is `upcoming` or `active`.
/// A row closed by expiry (`retirement_reason = 'expired'`) reopens the same
/// way a fresh fetch would reopen it. Left alone: rows with an approved
/// window override, a source-declared intake status, an operator hold or any
/// other retirement, and rows with no later round (expiry closes them).
/// Static SQL, no bound input; reads and writes `programs` only.
pub async fn advance_application_rounds<'e, E: sqlx::PgExecutor<'e>>(executor: E) -> Result<u64> {
    Ok(sqlx::query(
        "WITH next AS ( \
             SELECT p.id, r.start_at, r.end_at FROM programs p \
             CROSS JOIN LATERAL ( \
                 SELECT (x->>'start')::timestamptz AS start_at, (x->>'end')::timestamptz AS end_at \
                 FROM jsonb_array_elements(p.normalized_payload->'application_rounds') x \
                 WHERE (x->>'end')::timestamptz > now() \
                 ORDER BY (x->>'end')::timestamptz LIMIT 1) r \
             WHERE jsonb_typeof(p.normalized_payload->'application_rounds') = 'array' \
               AND p.application_end_at IS NOT NULL AND p.application_end_at <= now() \
               AND (p.is_active OR p.retirement_reason = 'expired') \
               AND (p.retirement_reason IS NULL OR p.retirement_reason = 'expired') \
               AND p.program_status IN ('active', 'upcoming', 'closed') \
               AND NOT (p.normalized_payload ? 'source_status') \
               AND NOT EXISTS (SELECT 1 FROM program_ai_field_overrides o \
                   WHERE o.program_id = p.id \
                     AND o.field IN ('application_start_at', 'application_end_at'))) \
         UPDATE programs p SET application_start_at = n.start_at, application_end_at = n.end_at, \
             program_status = CASE WHEN n.start_at > now() THEN 'upcoming' ELSE 'active' END, \
             is_active = true, retired_at = NULL, retirement_reason = NULL, updated_at = now() \
         FROM next n WHERE p.id = n.id",
    )
    .execute(executor)
    .await?
    .rows_affected())
}

pub async fn monitor_residence_scope(pool: &PgPool) -> Result<()> {
    let mut tx = pool.begin().await?;
    monitor_residence_scope_on(&mut tx).await?;
    tx.commit().await?;
    Ok(())
}

async fn monitor_residence_scope_on(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>) -> Result<()> {
    use crate::operations::{self, EventCommand, Severity};
    let rows: Vec<(String, i64)> = sqlx::query_as(
        "WITH unresolved AS (
          SELECT source_type, count(*) AS count FROM programs
          WHERE is_active AND 'unknown'=ANY(regions) GROUP BY source_type
        ), sources AS (
          SELECT source_type FROM unresolved UNION
          SELECT subject_id FROM operational_events WHERE component='residence_scope'
            AND subject_type='program_source' AND subject_id IS NOT NULL
            AND (status<>'resolved'
                 OR COALESCE((details->>'active_programs_with_unknown_region')::bigint,1)>0)
        ) SELECT s.source_type,COALESCE(u.count,0) FROM sources s
          LEFT JOIN unresolved u USING(source_type)",
    )
    .fetch_all(&mut **tx)
    .await
    .context("residence scope monitoring query failed")?;
    for (source, count) in rows {
        let fingerprint = format!("residence_scope:{source}");
        let previous: Option<(String, Option<i64>)> = sqlx::query_as(
            "SELECT status,(details->>'active_programs_with_unknown_region')::bigint
             FROM operational_events WHERE fingerprint=$1",
        )
        .bind(&fingerprint)
        .fetch_optional(&mut **tx)
        .await?;
        let previous_count = previous.as_ref().and_then(|(_, c)| *c);
        let resolved = previous.as_ref().is_some_and(|(s, _)| s == "resolved");
        // A drop (a mapping fix or expiry) resolves the incident; the remaining
        // count keeps being reported, and only a rise reopens it.
        let dropped = previous_count.is_some_and(|p| count > 0 && count < p);
        if dropped || (resolved && previous_count.is_some_and(|p| count > 0 && count <= p)) {
            if dropped {
                let note = format!(
                    "지역 미확인 공고가 {}건에서 {count}건으로 줄었습니다. 남은 건수는 계속 집계하며, 늘어나면 다시 알립니다.",
                    previous_count.unwrap_or_default()
                );
                operations::resolve_event_tx(tx, &fingerprint, None, Some(&note)).await?;
            }
            sqlx::query(
                "UPDATE operational_events SET details=$2,updated_at=now() WHERE fingerprint=$1",
            )
            .bind(&fingerprint)
            .bind(serde_json::json!({"active_programs_with_unknown_region":count}))
            .execute(&mut **tx)
            .await?;
        } else if count > 0 {
            operations::record_event_tx(tx, &EventCommand {
                fingerprint: &fingerprint, severity: Severity::Warning,
                component: "residence_scope", event_type: "unknown_residence_scope",
                subject_type: Some("program_source"), subject_id: Some(&source),
                message: "지역 조건을 확인할 수 없는 공고는 내 지역과 맞춤 추천에서 제외했습니다. 수집 정보가 보완되면 자동으로 다시 판정합니다.",
                details: serde_json::json!({"active_programs_with_unknown_region":count}),
            }).await?;
        } else {
            operations::resolve_event_tx(
                tx,
                &fingerprint,
                None,
                Some("지역 미확인 공고가 모두 해소되었거나 만료되었습니다."),
            )
            .await?;
            sqlx::query(
                "UPDATE operational_events SET details=$2,updated_at=now() WHERE fingerprint=$1",
            )
            .bind(&fingerprint)
            .bind(serde_json::json!({"active_programs_with_unknown_region":0}))
            .execute(&mut **tx)
            .await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Audit M4: a programme whose stored round ended takes the next stated
    /// round; one with no later round, a window override or a source
    /// status is left for expiry.
    #[tokio::test]
    async fn an_ended_round_moves_to_the_next_one() {
        let database_url = std::env::var("TEST_DATABASE_URL")
            .unwrap_or_else(|_| "postgres://wello:wello@localhost:5432/wello".into());
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .expect("isolated test database required");
        let mut tx = pool.begin().await.unwrap();
        let now = chrono::Utc::now();
        let round = |from: i64, to: i64| {
            serde_json::json!({"start": now + chrono::Duration::days(from),
                "end": now + chrono::Duration::days(to)})
        };
        let rounds = serde_json::json!({"application_rounds": [round(-40, -10), round(20, 40)]});
        let insert = |payload: serde_json::Value, active: bool, reason: Option<&'static str>| {
            let id = uuid::Uuid::new_v4();
            let end = now - chrono::Duration::days(10);
            (id, payload, end, active, reason)
        };
        let mut ids = Vec::new();
        let last_only =
            serde_json::json!({"application_rounds": [round(-60, -40), round(-30, -10)]});
        let with_status = serde_json::json!({"application_rounds": rounds["application_rounds"], "source_status": "closed"});
        for (id, payload, end, active, reason) in [
            insert(rounds.clone(), true, None),
            insert(rounds.clone(), false, Some("expired")),
            insert(rounds.clone(), false, Some("duplicate")),
            insert(last_only, true, None),
            insert(with_status, true, None),
        ] {
            sqlx::query("INSERT INTO programs(id,program_type,source_type,title,regions,normalized_payload,application_start_at,application_end_at,program_status,is_active,retirement_reason) VALUES($1,'welfare','gov_benefits','라운드',ARRAY[]::text[],$2,$3,$3,'active',$4,$5)")
                .bind(id).bind(&payload).bind(end).bind(active).bind(reason)
                .execute(&mut *tx).await.unwrap();
            ids.push(id);
        }
        let advanced = advance_application_rounds(&mut *tx).await.unwrap();
        assert_eq!(advanced, 2);
        for (i, id) in ids.iter().enumerate() {
            let (status, active, end): (String, bool, chrono::DateTime<chrono::Utc>) = sqlx::query_as(
                "SELECT program_status, is_active, application_end_at FROM programs WHERE id = $1",
            )
            .bind(id)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
            if i < 2 {
                assert_eq!((status.as_str(), active), ("upcoming", true), "row {i}");
                assert!(end > now, "row {i}");
            } else {
                assert!(end < now, "row {i} untouched");
            }
        }
        tx.rollback().await.unwrap();
    }

    #[tokio::test]
    async fn residence_incident_opens_with_counts_and_resolves_without_deleting_history() {
        let database_url = std::env::var("TEST_DATABASE_URL")
            .unwrap_or_else(|_| "postgres://wello:wello@localhost:5432/wello".into());
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .expect("isolated test database required");
        let mut tx = pool.begin().await.unwrap();
        let program_id = uuid::Uuid::new_v4();
        let source = format!("scope_test_{}", program_id.simple());
        sqlx::query("INSERT INTO programs(id,program_type,source_type,title,regions) VALUES($1,'welfare',$2,'원문은 운영이벤트에 저장하지 않음',ARRAY['unknown'])")
            .bind(program_id).bind(&source).execute(&mut *tx).await.unwrap();
        monitor_residence_scope_on(&mut tx).await.unwrap();
        let fingerprint = format!("residence_scope:{source}");
        let (status, details): (String, serde_json::Value) =
            sqlx::query_as("SELECT status,details FROM operational_events WHERE fingerprint=$1")
                .bind(&fingerprint)
                .fetch_one(&mut *tx)
                .await
                .unwrap();
        assert_eq!(status, "open");
        assert_eq!(
            details,
            serde_json::json!({"active_programs_with_unknown_region":1})
        );
        async fn read(
            tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
            fingerprint: &str,
        ) -> (String, serde_json::Value) {
            sqlx::query_as("SELECT status,details FROM operational_events WHERE fingerprint=$1")
                .bind(fingerprint)
                .fetch_one(&mut **tx)
                .await
                .unwrap()
        }
        // Two more unknown rows: the count rises and the incident stays open.
        let mut extra = Vec::new();
        for _ in 0..2 {
            let id = uuid::Uuid::new_v4();
            sqlx::query("INSERT INTO programs(id,program_type,source_type,title,regions) VALUES($1,'welfare',$2,'지역 미확인',ARRAY['unknown'])")
                .bind(id).bind(&source).execute(&mut *tx).await.unwrap();
            extra.push(id);
        }
        monitor_residence_scope_on(&mut tx).await.unwrap();
        let (status, details) = read(&mut tx, &fingerprint).await;
        assert_eq!(status, "open");
        assert_eq!(details["active_programs_with_unknown_region"], 3);
        // A drop resolves it while the remaining count keeps being reported.
        sqlx::query("UPDATE programs SET regions=ARRAY['busan'] WHERE id=$1")
            .bind(program_id)
            .execute(&mut *tx)
            .await
            .unwrap();
        monitor_residence_scope_on(&mut tx).await.unwrap();
        let (status, details) = read(&mut tx, &fingerprint).await;
        assert_eq!(status, "resolved");
        assert_eq!(details["active_programs_with_unknown_region"], 2);
        let note: String = sqlx::query_scalar(
            "SELECT resolution_note FROM operational_events WHERE fingerprint=$1",
        )
        .bind(&fingerprint)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        assert!(note.contains("3건에서 2건"), "{note}");
        // The same remainder stays resolved; a further drop is still counted.
        monitor_residence_scope_on(&mut tx).await.unwrap();
        assert_eq!(read(&mut tx, &fingerprint).await.0, "resolved");
        sqlx::query("UPDATE programs SET regions=ARRAY['busan'] WHERE id=$1")
            .bind(extra[0])
            .execute(&mut *tx)
            .await
            .unwrap();
        monitor_residence_scope_on(&mut tx).await.unwrap();
        let (status, details) = read(&mut tx, &fingerprint).await;
        assert_eq!(status, "resolved");
        assert_eq!(details["active_programs_with_unknown_region"], 1);
        // A rise above the last reported count reopens it.
        sqlx::query("UPDATE programs SET regions=ARRAY['unknown'] WHERE id=$1")
            .bind(extra[0])
            .execute(&mut *tx)
            .await
            .unwrap();
        monitor_residence_scope_on(&mut tx).await.unwrap();
        let (status, details) = read(&mut tx, &fingerprint).await;
        assert_eq!(status, "open");
        assert_eq!(details["active_programs_with_unknown_region"], 2);
        // None left: resolved with a zero count.
        sqlx::query("UPDATE programs SET regions=ARRAY['busan'] WHERE id=ANY($1)")
            .bind(&extra)
            .execute(&mut *tx)
            .await
            .unwrap();
        monitor_residence_scope_on(&mut tx).await.unwrap();
        let (status, details) = read(&mut tx, &fingerprint).await;
        assert_eq!(status, "resolved");
        assert_eq!(details["active_programs_with_unknown_region"], 0);

        // The stale path: resolved by a drop with rows left (2 → 1), then the
        // last one goes. The resolved event must still record 0, so that one
        // new unknown row reopens it instead of comparing against a stale 1.
        sqlx::query("UPDATE programs SET regions=ARRAY['unknown'] WHERE id=ANY($1)")
            .bind(&extra)
            .execute(&mut *tx)
            .await
            .unwrap();
        monitor_residence_scope_on(&mut tx).await.unwrap();
        assert_eq!(read(&mut tx, &fingerprint).await.0, "open");
        sqlx::query("UPDATE programs SET regions=ARRAY['busan'] WHERE id=$1")
            .bind(extra[0])
            .execute(&mut *tx)
            .await
            .unwrap();
        monitor_residence_scope_on(&mut tx).await.unwrap();
        let (status, details) = read(&mut tx, &fingerprint).await;
        assert_eq!(status, "resolved");
        assert_eq!(details["active_programs_with_unknown_region"], 1);
        sqlx::query("UPDATE programs SET regions=ARRAY['busan'] WHERE id=$1")
            .bind(extra[1])
            .execute(&mut *tx)
            .await
            .unwrap();
        monitor_residence_scope_on(&mut tx).await.unwrap();
        let (status, details) = read(&mut tx, &fingerprint).await;
        assert_eq!(status, "resolved");
        assert_eq!(details["active_programs_with_unknown_region"], 0);
        sqlx::query("UPDATE programs SET regions=ARRAY['unknown'] WHERE id=$1")
            .bind(program_id)
            .execute(&mut *tx)
            .await
            .unwrap();
        monitor_residence_scope_on(&mut tx).await.unwrap();
        let (status, details) = read(&mut tx, &fingerprint).await;
        assert_eq!(status, "open");
        assert_eq!(details["active_programs_with_unknown_region"], 1);
        let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM programs WHERE id=$1)")
            .bind(program_id)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        assert!(exists);
        tx.rollback().await.unwrap();
        pool.close().await;
    }
}
