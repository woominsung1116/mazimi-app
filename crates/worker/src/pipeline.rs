//! Normalize pipeline: fetch → save raw → hash compare → normalize → upsert programs → log

use anyhow::Result;
use chrono::{DateTime, Duration, NaiveDate, NaiveTime, Utc};
use sqlx::PgPool;
use std::collections::{HashMap, HashSet};
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::sources::{DataSource, RawRecord};
use crate::{
    error_safety::persisted_error,
    operations::{self, EventCommand, Severity},
};

mod age_basis;
mod region_replay;
pub use region_replay::renormalize_stored_regions;
#[cfg(test)]
mod side_documents_tests;
#[cfg(test)]
mod volatile_hash_tests;

/// Source-specific versions are deliberately explicit. Bump only the source
/// whose mapping changed; its retained snapshots will then be replayed while
/// unrelated catalogues remain untouched.
pub fn normalizer_version(source_name: &str) -> i32 {
    match source_name {
        // v2 treats upstream sentinel ages (0/999) as unknown rather than
        // turning them into false eligibility boundaries.
        // v3/v2 align official financial, welfare and policy categories with
        // the public catalogue tabs; existing snapshots are replayable.
        // v4 restores residence scopes and quarantines unrecognized regions.
        // v5 (youth_center), v3 (scholarship, fss_financial), v2 (local_scraper)
        // anchor application windows to Korean calendar days: start 00:00 KST,
        // end 23:59:59 KST, instead of 00:00 UTC.
        // v6 (youth_center) applies the source's intake code aplyPrdSeCd:
        // 0057003 마감 → closed, 0057002 상시 → always_open.
        // v7 (youth_center) keeps 시·군·구 districts, maps the coded
        // conditions (jobCd, schoolCd, sbizCd, mrgSttsCd, earn*) and flags a
        // district registrar listing the whole country.
        // v8 (youth_center) compiles schoolCd sets without 기타 and not
        // topped by a 졸업 code into an education_stage rule, and plcyMajorCd
        // into a major_category label.
        // v9 recompiles those education_stage rules after
        // education::admitted_education_stages began admitting 졸업 예정
        // for a 대학/고교 졸업 code (audit M7): stored source rules keep the
        // old set until replayed.
        // v10 compiles every coded restriction into the source rule (audit
        // H1): sbizCd groups no profile answers become a label on
        // target_group (여성: gender), mrgSttsCd becomes marital_status;
        // aplyYmd is read by the 보조금24 window parser, every round kept
        // (audit M5); a stated military age extension widens max_age by up
        // to 6 and makes the stated upper age a label (audit M6); a zipCd
        // with an unknown 시·군·구 code is an unknown scope, not its whole
        // sido (audit M8). A stated age reference day is stored as
        // age_basis and a birth-date range becomes a birth_year rule (R-M1).
        "youth_center" => 10,
        // v3 preserves application guidance already present in official APIs.
        // v5 reads explicit dated windows out of the 신청기한 text.
        // v6 maps supportConditions (ages, target groups, applicant types),
        // keeps 시·군·구 districts, narrows the scope-unknown heuristic and
        // closes "신규신청불가"/"신청불필요".
        // v7 compiles supportConditions groups the profile answers (대학생,
        // 구직자, 장애인; 다문화, 1인 가구) into label conditions (named
        // checks that never hide).
        // v8 resolves residence scopes that v7 left unknown: 공공기관 are
        // nationwide unless the text names a place or vicinity, the merged
        // 전남광주통합특별시교육청 is read as its sido, and local bodies use
        // their parent government's 소관기관코드 (crate::agency_codes).
        // 구비서류 (from the detail row) needs no bump: it is side data that
        // every run writes to unchanged rows (`sync_side_documents`).
        // v9 compiles every coded restriction into the source rule (audit
        // H1): groups and household kinds no profile answers become a label
        // on target_group, JA0101/JA0102 a label on gender. 신청기한 words
        // that only say intake may continue (추가, 미정, 상시, 연장) no longer
        // void the dated window (audit M3); several rounds are kept and the
        // current one stored (audit M4). A child-only age band is dropped
        // only when it is a child's (pupil flag, parent-facing flag or under
        // 13) and then asks about children; a 13–18 band an individual
        // applies for is kept (audit M7). A 중앙행정기관 text naming an area
        // by its kind (농어촌, 인구감소지역, 주변) is unknown scope, and
        // "외국인근로자" is no longer read as 인근 (audit M9). A stated age
        // reference day is stored as age_basis and a birth-date range
        // becomes a birth_year rule (R-M1).
        "gov_benefits" => 9,
        // v4 (scholarship) keeps 제출서류 상세내용 as submission_documents.
        // v5 compiles 대학구분 (school_type) and 학년구분 (education_stage)
        // into the source rule and types a 상품구분 학자금대출 row as a
        // financial product (audit H2); a residence text naming exactly one
        // place is that place, with its districts (audit M2).
        "scholarship" => 5,
        "local_scraper" => 2,
        // v4 (fss_financial) reads "만19세~만34세" (만 repeated after the
        // separator) as 19–34 instead of no age bound.
        // v5 (fss_financial) reads every kind and sector: the summary adds
        // the kind's own lines (대출한도, 대출종류) and the rates from the
        // option rows, and 주택담보대출 is named as such instead of 대출.
        // v6 (fss_financial): a disclosure period is no application window
        // (no start/end; a passed `dcls_end_day` closes the row), rate lines
        // name the term of the top rate, names are whitespace-squashed, and
        // loans never carry an age band.
        // v7 (fss_financial): a 가입대상 clause that holds a rate or sets an
        // age as a topic ("만 34세 이하는 …") sets no age band.
        // v8 (fss_financial): alternative bounds in one clause are their
        // union, not the one written last (audit L4).
        "fss_financial" => 8,
        // v3 (national_welfare): output unchanged; the bump replayed every
        // retained snapshot once through the welfare ↔ 보조금24 duplicate
        // check. That check now runs on every record of every run.
        // v4 (local_welfare) was the reviewed regions-only upgrade.
        // v5 (local_welfare) keeps the 시·군·구 named by sggNm as districts;
        // applied through the full replay (renormalize-snapshots), which also
        // runs the welfare ↔ 보조금24 duplicate check on every stored row.
        "national_welfare" => 3,
        "local_welfare" => 5,
        _ => 1,
    }
}

/// Consecutive complete runs a program may be absent from its source before
/// it is closed. Three daily runs tolerate a one-off upstream listing glitch.
const SOURCE_MISSING_RUNS_TO_CLOSE: i32 = 3;

/// A fetch smaller than this share of the previous successful run is treated
/// as incomplete: no program is counted missing or closed from it.
const MIN_FETCH_RATIO_FOR_RECONCILIATION: f64 = 0.8;

#[derive(Debug, Default)]
struct ProcessingStats {
    total: usize,
    changed: usize,
    renormalized: usize,
    /// Transient failures (database, I/O). The run fails and is retried.
    failed: usize,
    /// Deterministic upstream defects. The same payload is rejected on every
    /// run, so these never fail the run or trigger a re-fetch.
    rejected: usize,
    /// Deterministic rejections of records that never produced a programme
    /// row (see [`quarantines_rejection`]). Counted, never alerted on.
    quarantined: usize,
    /// Records dropped because a later record of the same fetch had the
    /// same source id (`ingestion_runs.total_duplicate_ids`). A source must
    /// give every upstream row its own id; a non-zero count is logged.
    duplicate_ids: usize,
    /// Records the provider returned before client-side filtering
    /// (`SourceFetch::upstream_seen`).
    upstream_seen: usize,
    /// Every upstream identity present in this fetch, processed or not.
    seen_source_ids: Vec<String>,
    /// Whether the fetch covered the provider's whole catalogue
    /// (`SourceFetch::whole_settled`). An incomplete fetch is ingested but
    /// never counts a program as missing.
    fetch_complete: bool,
    /// The fetch's parts named by id prefix, each with the programmes
    /// shown under it when the run started (`SourceFetch::parts`).
    parts: Vec<PartReport>,
}

/// One part of a fetch as the run saw it.
#[derive(Debug, Clone)]
struct PartReport {
    id_prefix: String,
    outcome: crate::sources::PartOutcome,
    /// Active programmes whose id starts with the prefix.
    shown: i64,
}

impl PartReport {
    fn unsettled(&self) -> Option<&crate::sources::Unsettled> {
        match &self.outcome.state {
            crate::sources::PartState::Unsettled(unsettled) => Some(unsettled),
            _ => None,
        }
    }

    /// "{prefix}* {code}: {detail}".
    fn line(&self) -> Option<String> {
        self.unsettled()
            .map(|u| format!("{}* {}: {}", self.id_prefix, u.code, u.detail))
    }
}

/// Reason codes of a part that waiting cannot fix: the source's code must
/// change. Always error severity.
const NEEDS_CODE: &[&str] = &["schema_changed", "page_limit", "request_rejected"];

/// Code of an empty answer for a part whose programmes are still shown.
const ANSWERED_EMPTY: &str = "answered_empty";

/// Days a programme of a list-driven source stays shown without the
/// provider listing it again. Past that it is closed as `source_removed`
/// (reopened by the next run that lists it), whatever kept it: a part that
/// stays unsettled, an empty answer, a run that fails.
pub const LISTED_WITHIN_DAYS: &[(&str, i32)] = &[("fss_financial", 7)];

/// A record whose normalized form violates the storage contract. Retrying the
/// identical payload cannot succeed, so the pipeline records it as rejected
/// instead of failing the run.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct RecordRejection(String);

fn is_rejection(error: &anyhow::Error) -> bool {
    error.downcast_ref::<RecordRejection>().is_some()
}

/// `SOURCE_ALLOW_EMPTY_<SOURCE>` (for example
/// `SOURCE_ALLOW_EMPTY_LOCAL_SCRAPER=true`): the source may legitimately
/// return nothing (a seasonal catalogue between rounds). Default strict: a
/// provider returning zero records fails the run and is retried, and a fetch
/// whose records were all filtered out is recorded as `empty` with an open
/// `ingestion_empty:<source>` warning. An unreadable value stays strict.
pub fn allows_empty_fetch(source_name: &str) -> bool {
    let name = format!("SOURCE_ALLOW_EMPTY_{}", source_name.to_ascii_uppercase());
    match std::env::var(&name) {
        Ok(value) => match value.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" => true,
            "" | "0" | "false" | "no" => false,
            _ => {
                warn!(variable = %name, "unreadable allow-empty flag; the source stays strict");
                false
            }
        },
        Err(_) => false,
    }
}

/// Run status and stored message of a completed fetch. A run with records
/// that all failed or were rejected keeps those statuses; a run without a
/// single record is `empty`, never `success`.
fn completed_run_status(stats: &ProcessingStats) -> (&'static str, Option<String>) {
    let (status, message) = record_run_status(stats);
    // The parts a fetch could not read are named in the run itself, beside
    // whatever the records did.
    let parts: Vec<String> = stats.parts.iter().filter_map(PartReport::line).collect();
    if parts.is_empty() {
        return (status, message);
    }
    let unsettled = format!(
        "{} part(s) of the catalogue were not settled; their programmes were left as they were: {}",
        parts.len(),
        parts.join("; ")
    );
    let message = match message {
        Some(message) => format!("{message}. {unsettled}"),
        None => unsettled,
    };
    (status, Some(message))
}

fn record_run_status(stats: &ProcessingStats) -> (&'static str, Option<String>) {
    if stats.failed > 0 {
        (
            "failed",
            Some("One or more records failed; retry scheduled".into()),
        )
    } else if stats.rejected > 0 {
        (
            "success_with_rejections",
            Some("One or more records were rejected as invalid upstream data".into()),
        )
    } else if stats.total == 0 {
        (
            "empty",
            Some(format!(
                "The provider returned {} record(s); none passed the source filter",
                stats.upstream_seen
            )),
        )
    } else {
        ("success", None)
    }
}

/// What a completed ingestion run did, for the daily job's details.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestionOutcome {
    pub status: &'static str,
    /// Records kept after client-side filtering.
    pub fetched: usize,
    pub upstream_seen: usize,
    /// `empty` on a source that is not allowed to be empty.
    pub unexpectedly_empty: bool,
    /// Unsettled parts that need attention (programmes still shown under
    /// them, or a reason waiting cannot fix), as "{prefix}* {code}: {detail}".
    pub alerting_parts: Vec<String>,
    /// Unsettled parts with nothing shown and a reason that may heal.
    pub blocked_parts: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProcessOutcome {
    Unchanged,
    Changed,
    Renormalized,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecordOrigin {
    FreshFetch,
    StoredSnapshot,
}

/// Run the full ingestion pipeline for a single source.
pub async fn run_ingestion<S: DataSource>(pool: &PgPool, source: &S) -> Result<IngestionOutcome> {
    let source_name = source.name();
    let allow_empty = allows_empty_fetch(source_name);
    let current_normalizer_version = normalizer_version(source_name);
    let run_id = Uuid::new_v4();
    let started_at = Utc::now();

    info!(source = source_name, run_id = %run_id, "ingestion run started");

    // Insert ingestion_run row (status = running)
    sqlx::query(
        "INSERT INTO ingestion_runs ( \
             id, source_name, status, started_at, normalizer_version, run_kind \
         ) VALUES ($1, $2, 'running', $3, $4, 'ingestion')",
    )
    .bind(run_id)
    .bind(source_name)
    .bind(started_at)
    .bind(current_normalizer_version)
    .execute(pool)
    .await?;
    crate::daily::note_job_run(run_id);

    let result = process_source(pool, source, run_id, allow_empty).await;

    let finished_at = Utc::now();
    let mut run_status = "failed";
    match &result {
        Ok(stats) => {
            let (status, error_message) = completed_run_status(stats);
            run_status = status;
            sqlx::query(
                "UPDATE ingestion_runs \
                 SET status = $2, finished_at = $3, total_fetched = $4, total_changed = $5, \
                     total_renormalized = $6, total_failed = $7, total_rejected = $8, \
                     error_message = $9, total_quarantined = $10, total_duplicate_ids = $11 \
                 WHERE id = $1",
            )
            .bind(run_id)
            .bind(status)
            .bind(finished_at)
            .bind(stats.total as i32)
            .bind(stats.changed as i32)
            .bind(stats.renormalized as i32)
            .bind(stats.failed as i32)
            .bind(stats.rejected as i32)
            .bind(error_message.as_deref())
            .bind(stats.quarantined as i32)
            .bind(stats.duplicate_ids as i32)
            .execute(pool)
            .await?;
            info!(
                source = source_name,
                run_id = %run_id,
                status,
                upstream_seen = stats.upstream_seen,
                total = stats.total,
                changed = stats.changed,
                renormalized = stats.renormalized,
                failed = stats.failed,
                rejected = stats.rejected,
                quarantined = stats.quarantined,
                "ingestion run completed"
            );
            // Only a complete, failure-free fetch may count a program as
            // missing. A transient failure is retried within the hour and
            // must not double-count absences; an incomplete fetch (provider
            // total mismatch) is skipped inside the reconciliation.
            if stats.failed == 0 {
                let lifecycle = reconcile_source_lifecycle(pool, source_name, run_id, stats).await;
                report_lifecycle(pool, source_name, run_id, &lifecycle).await;
            }
            let incomplete_fingerprint = format!("ingestion_incomplete:{source_name}");
            if !stats.fetch_complete {
                let run_id_text = run_id.to_string();
                if let Err(event_error) = operations::record_event(
                    pool,
                    &EventCommand {
                        fingerprint: &incomplete_fingerprint,
                        severity: Severity::Warning,
                        component: "ingestion",
                        event_type: "ingestion_incomplete_fetch",
                        subject_type: Some("ingestion_run"),
                        subject_id: Some(&run_id_text),
                        message: "A source reported more records than its pages delivered; records were ingested, lifecycle reconciliation was skipped",
                        details: serde_json::json!({
                            "source": source_name,
                            "total": stats.total,
                            "skipped_reason": INCOMPLETE_FETCH_REASON
                        }),
                    },
                )
                .await
                {
                    warn!(source = source_name, error = %event_error, "failed to record incomplete fetch incident");
                }
            } else if let Err(event_error) = operations::resolve_event(
                pool,
                &incomplete_fingerprint,
                None,
                Some("fetch covered the provider total"),
            )
            .await
            {
                warn!(source = source_name, error = %event_error, "failed to resolve incomplete fetch incident");
            }
            record_or_resolve_empty(pool, source_name, run_id, stats, allow_empty).await;
            report_parts(pool, source_name, run_id, stats).await;
            if stats.failed == 0 {
                match settle_renormalization_partial(pool, source_name).await {
                    Ok(true) => info!(
                        source = source_name,
                        "renormalization incident resolved: the regular run settled every stored record it had failed on"
                    ),
                    Ok(false) => {}
                    Err(event_error) => {
                        warn!(source = source_name, error = %event_error, "failed to settle renormalization incident")
                    }
                }
            }
            let failed_fingerprint = format!("ingestion_failed:{source_name}");
            if let Err(event_error) = operations::resolve_event(
                pool,
                &failed_fingerprint,
                None,
                Some("ingestion recovered"),
            )
            .await
            {
                warn!(source = source_name, error = %event_error, "failed to resolve ingestion incident");
            }
            let partial_fingerprint = format!("ingestion_partial:{source_name}");
            if stats.failed > 0 || stats.rejected > 0 {
                let run_id_text = run_id.to_string();
                if let Err(event_error) = operations::record_event(
                    pool,
                    &EventCommand {
                        fingerprint: &partial_fingerprint,
                        severity: Severity::Warning,
                        component: "ingestion",
                        event_type: "ingestion_partial_failure",
                        subject_type: Some("ingestion_run"),
                        subject_id: Some(&run_id_text),
                        message: "An ingestion run completed with rejected or failed records",
                        details: serde_json::json!({
                            "source": source_name,
                            "total": stats.total,
                            "failed": stats.failed,
                            "rejected": stats.rejected
                        }),
                    },
                )
                .await
                {
                    warn!(source = source_name, error = %event_error, "failed to record partial ingestion incident");
                }
            } else if let Err(event_error) = operations::resolve_event(
                pool,
                &partial_fingerprint,
                None,
                Some(&format!(
                    "ingestion completed without rejected or failed records; \
                     {} deterministic upstream defect(s) without a programme row quarantined",
                    stats.quarantined
                )),
            )
            .await
            {
                warn!(source = source_name, error = %event_error, "failed to resolve partial ingestion incident");
            }
        }
        Err(e) => {
            sqlx::query(
                "UPDATE ingestion_runs \
                 SET status = 'failed', finished_at = $2, error_message = $3 \
                 WHERE id = $1",
            )
            .bind(run_id)
            .bind(finished_at)
            .bind(persisted_error(e))
            .execute(pool)
            .await?;
            error!(source = source_name, run_id = %run_id, error = %e, "ingestion run failed");
            let fingerprint = format!("ingestion_failed:{source_name}");
            let run_id_text = run_id.to_string();
            if let Err(event_error) = operations::record_event(
                pool,
                &EventCommand {
                    fingerprint: &fingerprint,
                    severity: Severity::Error,
                    component: "ingestion",
                    event_type: "ingestion_run_failed",
                    subject_type: Some("ingestion_run"),
                    subject_id: Some(&run_id_text),
                    message: "A scheduled source ingestion run failed",
                    details: serde_json::json!({ "source": source_name }),
                },
            )
            .await
            {
                warn!(source = source_name, error = %event_error, "failed to record ingestion incident");
            }
        }
    }

    result.and_then(|stats| {
        anyhow::ensure!(
            stats.failed == 0,
            "{} records failed; ingestion will retry",
            stats.failed
        );
        Ok(IngestionOutcome {
            status: run_status,
            fetched: stats.total,
            upstream_seen: stats.upstream_seen,
            unexpectedly_empty: run_status == "empty" && !allow_empty,
            alerting_parts: stats
                .parts
                .iter()
                .filter(|part| part_alerts(part))
                .filter_map(PartReport::line)
                .collect(),
            blocked_parts: stats
                .parts
                .iter()
                .filter(|part| part.unsettled().is_some() && !part_alerts(part))
                .filter_map(PartReport::line)
                .collect(),
        })
    })
}

/// A failed reconciliation changes no programme, so programmes removed
/// upstream stay active: `lifecycle_reconciliation_failed:<source>` opens at
/// error severity and resolves on the next reconciliation that completes
/// (a skip for an incomplete fetch included: it ran without an error).
async fn report_lifecycle(
    pool: &PgPool,
    source_name: &str,
    run_id: Uuid,
    lifecycle: &Result<LifecycleSummary>,
) {
    let fingerprint = format!("lifecycle_reconciliation_failed:{source_name}");
    let event = match lifecycle {
        Ok(summary) => {
            info!(
                source = source_name,
                run_id = %run_id,
                skipped = summary.skipped_reason,
                marked_missing = summary.marked_missing,
                closed = summary.closed,
                restored = summary.restored,
                "source lifecycle reconciliation complete"
            );
            operations::resolve_event(
                pool,
                &fingerprint,
                None,
                Some("source lifecycle reconciliation completed"),
            )
            .await
            .map(|_| ())
        }
        Err(lifecycle_error) => {
            error!(
                source = source_name,
                run_id = %run_id,
                error = %persisted_error(lifecycle_error),
                "source lifecycle reconciliation failed; no program state changed"
            );
            let run_id_text = run_id.to_string();
            operations::record_event(
                pool,
                &EventCommand {
                    fingerprint: &fingerprint,
                    severity: Severity::Error,
                    component: "ingestion",
                    event_type: "lifecycle_reconciliation_failed",
                    subject_type: Some("ingestion_run"),
                    subject_id: Some(&run_id_text),
                    message: "Source lifecycle reconciliation failed; programmes removed upstream stay active until it completes",
                    details: serde_json::json!({ "source": source_name }),
                },
            )
            .await
            .map(|_| ())
        }
    };
    if let Err(event_error) = event {
        warn!(source = source_name, error = %event_error, "failed to record lifecycle reconciliation incident");
    }
}

/// Resolves an open `renormalization_partial:<source>` once a regular run
/// has settled what the partial replay failed on. The replay runs only from
/// the operator CLI (`renormalize-snapshots`), so without this the incident
/// stays open even after the record it failed on is handled: youth_center
/// `20260422005400212868` (inverted window from upstream) is quarantined on
/// every regular run while its snapshot stays at normalizer version 0.
///
/// The replay can fail only on a stale snapshot that has a programme row:
/// one without a row is quarantined instead (`quarantines_rejection`), and
/// a successfully processed record is marked current. So the incident is
/// settled when none of the snapshots the replay failed on (its
/// `ingestion_items`; every stale snapshot of the source once retention has
/// pruned those) is still stale with a programme row. Nothing but the event
/// is written; the catalogue is never replayed automatically.
async fn settle_renormalization_partial(pool: &PgPool, source_name: &str) -> Result<bool> {
    let fingerprint = format!("renormalization_partial:{source_name}");
    let open: Option<Option<String>> = sqlx::query_scalar(
        "SELECT subject_id FROM operational_events WHERE fingerprint = $1 AND status <> 'resolved'",
    )
    .bind(&fingerprint)
    .fetch_optional(pool)
    .await?;
    let Some(subject_id) = open else {
        return Ok(false);
    };
    let partial_run = subject_id.and_then(|id| Uuid::parse_str(&id).ok());
    // The replay's failed items name what it failed on, unless retention
    // pruned them or an audit row was lost (fewer rows than its
    // total_failed): then every stale snapshot of the source is checked.
    let failed_ids: Vec<String> = match partial_run {
        Some(run_id) => {
            let ids: Vec<String> = sqlx::query_scalar(
                "SELECT source_id FROM ingestion_items WHERE run_id = $1 AND status = 'failed'",
            )
            .bind(run_id)
            .fetch_all(pool)
            .await?;
            let total_failed: Option<i32> =
                sqlx::query_scalar("SELECT total_failed FROM ingestion_runs WHERE id = $1")
                    .bind(run_id)
                    .fetch_optional(pool)
                    .await?
                    .flatten();
            if total_failed.is_some_and(|total| ids.len() >= total.max(0) as usize) {
                ids
            } else {
                Vec::new()
            }
        }
        None => Vec::new(),
    };
    let unsettled: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM source_snapshots s \
         WHERE s.source_name = $1 AND s.normalizer_version < $2 \
           AND (cardinality($3::text[]) = 0 OR s.source_id = ANY($3)) \
           AND EXISTS (SELECT 1 FROM programs p \
                       WHERE p.source_type = s.source_name AND p.source_id = s.source_id)",
    )
    .bind(source_name)
    .bind(normalizer_version(source_name))
    .bind(&failed_ids)
    .fetch_one(pool)
    .await?;
    if unsettled > 0 {
        return Ok(false);
    }
    let note = if failed_ids.is_empty() {
        "a regular run left no stale stored record with a programme row; \
         the replay cannot fail on what remains"
            .to_string()
    } else {
        format!(
            "a regular run settled all {} stored record(s) the replay failed on \
             (processed at the current normalizer version, or quarantined without a programme row)",
            failed_ids.len()
        )
    };
    operations::resolve_event(pool, &fingerprint, None, Some(&note)).await
}

/// `ingestion_empty:<source>`: open while a strict source's fetch keeps no
/// record (every upstream record filtered out), resolved by the next run
/// with records, or when the source is configured as allowed to be empty.
async fn record_or_resolve_empty(
    pool: &PgPool,
    source_name: &str,
    run_id: Uuid,
    stats: &ProcessingStats,
    allow_empty: bool,
) {
    let fingerprint = format!("ingestion_empty:{source_name}");
    let outcome = if stats.total == 0 && !allow_empty {
        let run_id_text = run_id.to_string();
        warn!(
            source = source_name,
            upstream_seen = stats.upstream_seen,
            "ingestion kept no record; every upstream record was filtered out"
        );
        operations::record_event(
            pool,
            &EventCommand {
                fingerprint: &fingerprint,
                severity: Severity::Warning,
                component: "ingestion",
                event_type: "ingestion_empty",
                subject_type: Some("ingestion_run"),
                subject_id: Some(&run_id_text),
                message: "A source run kept no record: the provider returned records but none passed the source filter",
                details: serde_json::json!({
                    "source": source_name,
                    "upstream_seen": stats.upstream_seen,
                    "kept": 0
                }),
            },
        )
        .await
        .map(|_| ())
    } else {
        let note = if stats.total > 0 {
            format!("the source kept {} record(s) again", stats.total)
        } else {
            "the source is configured as allowed to be empty".into()
        };
        operations::resolve_event(pool, &fingerprint, None, Some(&note))
            .await
            .map(|_| ())
    };
    if let Err(event_error) = outcome {
        warn!(source = source_name, error = %event_error, "failed to record or resolve empty ingestion incident");
    }
}

/// Whether an unsettled part needs a person: programmes are still shown
/// under it, or waiting cannot fix its reason ([`NEEDS_CODE`]).
fn part_alerts(part: &PartReport) -> bool {
    part.unsettled()
        .is_some_and(|u| part.shown > 0 || NEEDS_CODE.contains(&u.code))
}

/// Per-part state and alerts, one fingerprint per reason code
/// (`ingestion_part_<code>:<source>`). A code is an error while any part
/// with it still shows programmes or the code needs a code change; a part
/// that shows nothing and may heal is "blocked": its event is resolved with
/// the parts named, and the digest lists it from `ingestion_parts`. A code
/// no part has any more is resolved.
async fn report_parts(pool: &PgPool, source_name: &str, run_id: Uuid, stats: &ProcessingStats) {
    // A run without parts still resolves what an earlier run with parts left
    // open (a source that settled, or stopped reading in parts).
    if stats.parts.is_empty() {
        resolve_settled_part_events::<()>(pool, source_name, &std::collections::BTreeMap::new())
            .await;
        return;
    }
    if let Err(error) = persist_parts(pool, source_name, run_id, &stats.parts).await {
        warn!(source = source_name, error = %persisted_error(&error), "failed to persist part state");
    }
    let since: HashMap<String, (Option<DateTime<Utc>>, i32)> =
        sqlx::query_as::<_, (String, Option<DateTime<Utc>>, i32)>(
            "SELECT id_prefix, unsettled_since, consecutive_unsettled_runs FROM ingestion_parts \
         WHERE source_name = $1 AND state = 'unsettled'",
        )
        .bind(source_name)
        .fetch_all(pool)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|(prefix, since, runs)| (prefix, (since, runs)))
        .collect();
    let mut by_code: std::collections::BTreeMap<&'static str, Vec<&PartReport>> =
        std::collections::BTreeMap::new();
    for part in &stats.parts {
        if let Some(u) = part.unsettled() {
            by_code.entry(u.code).or_default().push(part);
        }
    }
    resolve_settled_part_events(pool, source_name, &by_code).await;
    let prefix = PART_EVENT_PREFIX;
    let suffix = format!(":{source_name}");
    let run_id_text = run_id.to_string();
    for (code, parts) in by_code {
        let fingerprint = format!("{prefix}{code}{suffix}");
        let details: Vec<serde_json::Value> = parts
            .iter()
            .map(|part| {
                let (since, runs) = since.get(&part.id_prefix).cloned().unwrap_or((None, 0));
                serde_json::json!({
                    "id_prefix": part.id_prefix,
                    "detail": part.unsettled().map(|u| u.detail.clone()),
                    "shown": part.shown,
                    "unsettled_since": since,
                    "consecutive_unsettled_runs": runs,
                })
            })
            .collect();
        let alerting = parts.iter().any(|part| part_alerts(part));
        let outcome = if alerting {
            warn!(
                source = source_name,
                event = "ingestion_part_alert",
                reason = code,
                parts = parts.len(),
                "unsettled parts need attention"
            );
            operations::record_event(
                pool,
                &EventCommand {
                    fingerprint: &fingerprint,
                    severity: Severity::Error,
                    component: "ingestion",
                    event_type: "ingestion_unsettled_parts",
                    subject_type: Some("ingestion_run"),
                    subject_id: Some(&run_id_text),
                    message: if NEEDS_CODE.contains(&code) {
                        "Parts of a source's catalogue no longer match what the collector reads; a code change is needed"
                    } else {
                        "Parts of a source's catalogue were not read while their programmes are still shown"
                    },
                    details: serde_json::json!({"source": source_name, "reason": code, "parts": details}),
                },
            )
            .await
            .map(|_| ())
        } else {
            let names: Vec<&str> = parts.iter().map(|part| part.id_prefix.as_str()).collect();
            operations::resolve_event(
                pool,
                &fingerprint,
                None,
                Some(&format!(
                    "blocked, nothing shown under it: {}",
                    names.join(", ")
                )),
            )
            .await
            .map(|_| ())
        };
        if let Err(error) = outcome {
            warn!(source = source_name, error = %error, "failed to record or resolve part incident");
        }
    }
}

const PART_EVENT_PREFIX: &str = "ingestion_part_";

/// Resolves every open `ingestion_part_<code>:<source>` whose code no part
/// of this run has.
async fn resolve_settled_part_events<V>(
    pool: &PgPool,
    source_name: &str,
    by_code: &std::collections::BTreeMap<&'static str, V>,
) {
    let prefix = PART_EVENT_PREFIX;
    let suffix = format!(":{source_name}");
    let open: Vec<String> = sqlx::query_scalar(
        "SELECT fingerprint FROM operational_events \
         WHERE status <> 'resolved' AND starts_with(fingerprint, $1) AND right(fingerprint, $2) = $3",
    )
    .bind(prefix)
    .bind(suffix.chars().count() as i32)
    .bind(&suffix)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    for fingerprint in open {
        let code = &fingerprint[prefix.len()..fingerprint.len() - suffix.len()];
        if !by_code.contains_key(code) {
            if let Err(error) = operations::resolve_event(
                pool,
                &fingerprint,
                None,
                Some("no part has this reason any more"),
            )
            .await
            {
                warn!(source = source_name, error = %error, "failed to resolve part incident");
            }
        }
    }
}

/// `ingestion_parts`: the last outcome of every part, since when it has been
/// unsettled and for how many runs in a row.
async fn persist_parts(
    pool: &PgPool,
    source_name: &str,
    run_id: Uuid,
    parts: &[PartReport],
) -> Result<()> {
    let mut prefixes = Vec::new();
    let mut states = Vec::new();
    let mut codes: Vec<Option<String>> = Vec::new();
    let mut details: Vec<Option<String>> = Vec::new();
    let (mut seen, mut kept, mut set_aside, mut shown) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for part in parts {
        prefixes.push(part.id_prefix.clone());
        states.push(match &part.outcome.state {
            crate::sources::PartState::Settled => "settled",
            crate::sources::PartState::Empty => "empty",
            crate::sources::PartState::Unsettled(_) => "unsettled",
        });
        codes.push(part.unsettled().map(|u| u.code.to_string()));
        details.push(
            part.unsettled()
                .map(|u| u.detail.chars().take(300).collect()),
        );
        seen.push(part.outcome.seen as i32);
        kept.push(part.outcome.kept as i32);
        set_aside.push(part.outcome.set_aside as i32);
        shown.push(part.shown as i32);
    }
    sqlx::query(
        "INSERT INTO ingestion_parts (source_name, id_prefix, state, reason_code, reason_detail, \
             unsettled_since, consecutive_unsettled_runs, last_settled_at, last_seen, last_kept, \
             last_set_aside, shown, last_run_id, updated_at) \
         SELECT $1, p.prefix, p.state, p.code, p.detail, \
             CASE WHEN p.state = 'unsettled' THEN now() END, \
             CASE WHEN p.state = 'unsettled' THEN 1 ELSE 0 END, \
             CASE WHEN p.state <> 'unsettled' THEN now() END, \
             p.seen, p.kept, p.set_aside, p.shown, $2, now() \
         FROM unnest($3::text[], $4::text[], $5::text[], $6::text[], $7::int[], $8::int[], $9::int[], $10::int[]) \
             AS p(prefix, state, code, detail, seen, kept, set_aside, shown) \
         ON CONFLICT (source_name, id_prefix) DO UPDATE SET \
             state = EXCLUDED.state, reason_code = EXCLUDED.reason_code, reason_detail = EXCLUDED.reason_detail, \
             unsettled_since = CASE WHEN EXCLUDED.state = 'unsettled' \
                 THEN COALESCE(ingestion_parts.unsettled_since, now()) END, \
             consecutive_unsettled_runs = CASE WHEN EXCLUDED.state = 'unsettled' \
                 THEN ingestion_parts.consecutive_unsettled_runs + 1 ELSE 0 END, \
             last_settled_at = COALESCE(EXCLUDED.last_settled_at, ingestion_parts.last_settled_at), \
             last_seen = EXCLUDED.last_seen, last_kept = EXCLUDED.last_kept, \
             last_set_aside = EXCLUDED.last_set_aside, shown = EXCLUDED.shown, \
             last_run_id = EXCLUDED.last_run_id, updated_at = now()",
    )
    .bind(source_name)
    .bind(run_id)
    .bind(&prefixes)
    .bind(&states)
    .bind(&codes)
    .bind(&details)
    .bind(&seen)
    .bind(&kept)
    .bind(&set_aside)
    .bind(&shown)
    .execute(pool)
    .await?;
    Ok(())
}

/// The fetch's parts as the run reports them: an empty prefix is a contract
/// violation (it would cover every programme of the source); an empty answer
/// for a part whose programmes are still shown counts as unsettled, since an
/// outage can answer the same way. How long that protects them is
/// [`LISTED_WITHIN_DAYS`].
async fn part_reports(
    pool: &PgPool,
    source_name: &str,
    parts: &[crate::sources::PartOutcome],
) -> Result<Vec<PartReport>> {
    use crate::sources::{PartScope, PartState, Unsettled};
    let mut reports = Vec::new();
    for part in parts {
        let PartScope::Prefix(prefix) = &part.scope else {
            continue;
        };
        anyhow::ensure!(
            !prefix.is_empty(),
            "{source_name}: a fetch part has an empty id prefix; it would cover every programme"
        );
        reports.push(PartReport {
            id_prefix: prefix.clone(),
            outcome: part.clone(),
            shown: 0,
        });
    }
    if reports.is_empty() {
        return Ok(reports);
    }
    let prefixes: Vec<String> = reports.iter().map(|r| r.id_prefix.clone()).collect();
    let shown: HashMap<String, i64> = sqlx::query_as::<_, (String, i64)>(
        "SELECT part.prefix, count(p.id) FROM unnest($2::text[]) AS part(prefix) \
         LEFT JOIN programs p ON p.source_type = $1 AND p.is_active \
             AND starts_with(p.source_id, part.prefix) \
         GROUP BY part.prefix",
    )
    .bind(source_name)
    .bind(&prefixes)
    .fetch_all(pool)
    .await?
    .into_iter()
    .collect();
    for report in &mut reports {
        report.shown = shown.get(&report.id_prefix).copied().unwrap_or(0);
        if report.outcome.state == PartState::Empty && report.shown > 0 {
            warn!(
                source = source_name,
                event = "ingestion_empty_part_held",
                id_prefix = %report.id_prefix,
                shown = report.shown,
                "the provider answered a part as empty while its programmes are shown; they are held"
            );
            report.outcome.state = PartState::Unsettled(Unsettled {
                code: ANSWERED_EMPTY,
                detail: "the provider answered this part as empty".into(),
            });
        }
    }
    Ok(reports)
}

/// `ingestion_listed_not_kept:<source>`: open while active programmes are
/// listed by the provider but not kept by the source, because its own
/// filter now leaves them out or their row was set aside as defective. From
/// the catalogue that looks like a product the provider removed, and it is
/// the one retirement cause that is ours, so it is named (up to 50 ids of
/// each kind) instead of only logged. Resolved by a run with none.
async fn report_listed_not_kept(
    pool: &PgPool,
    source_name: &str,
    run_id: Uuid,
    filtered_out: &[String],
    set_aside: &[String],
) {
    let fingerprint = format!("ingestion_listed_not_kept:{source_name}");
    let active = |ids: &[String]| {
        let ids = ids.to_vec();
        async move {
            if ids.is_empty() {
                return Vec::new();
            }
            sqlx::query_scalar::<_, String>(
                "SELECT source_id FROM programs \
                 WHERE source_type = $1 AND is_active AND source_id = ANY($2) \
                 ORDER BY source_id LIMIT 50",
            )
            .bind(source_name)
            .bind(&ids)
            .fetch_all(pool)
            .await
            .unwrap_or_default()
        }
    };
    let (filtered, set_aside) = (active(filtered_out).await, active(set_aside).await);
    for source_id in filtered.iter().chain(&set_aside) {
        warn!(
            source = source_name,
            event = "ingestion_was_kept_now_filtered",
            source_id = %source_id,
            "the provider still lists this programme but the source no longer keeps it"
        );
    }
    let outcome = if filtered.is_empty() && set_aside.is_empty() {
        operations::resolve_event(
            pool,
            &fingerprint,
            None,
            Some("every active programme the provider lists is kept"),
        )
        .await
        .map(|_| ())
    } else {
        let run_id_text = run_id.to_string();
        operations::record_event(
            pool,
            &EventCommand {
                fingerprint: &fingerprint,
                severity: Severity::Warning,
                component: "ingestion",
                event_type: "ingestion_listed_not_kept",
                subject_type: Some("ingestion_run"),
                subject_id: Some(&run_id_text),
                message: "Programmes the provider still lists are no longer kept by the source; they close as removed unless kept again",
                details: serde_json::json!({
                    "source": source_name,
                    "filtered_out": filtered,
                    "set_aside": set_aside,
                }),
            },
        )
        .await
        .map(|_| ())
    };
    if let Err(error) = outcome {
        warn!(source = source_name, error = %error, "failed to record or resolve listed-not-kept incident");
    }
}

/// Closes every programme of a list-driven source ([`LISTED_WITHIN_DAYS`])
/// the provider has not listed within its window: `source_snapshots.
/// fetched_at` is written each time a fetch carries the record. The close is
/// `source_removed`, which the next fetch that lists the programme undoes
/// (reconciliation restore and the upsert). Runs from the daily freshness
/// job, so it fires when ingestion itself fails.
pub async fn close_unlisted_programmes(pool: &PgPool) -> Result<serde_json::Value> {
    let mut closed = serde_json::Map::new();
    for (source_name, days) in LISTED_WITHIN_DAYS {
        let ids: Vec<String> = sqlx::query_scalar(
            "UPDATE programs p SET is_active = false, program_status = 'closed', retired_at = now(), \
                 retirement_reason = 'source_removed', updated_at = now() \
             FROM source_snapshots s \
             WHERE p.source_type = $1 AND p.is_active AND p.source_id IS NOT NULL \
               AND s.source_name = p.source_type AND s.source_id = p.source_id \
               AND s.fetched_at < now() - make_interval(days => $2) \
             RETURNING p.source_id",
        )
        .bind(source_name)
        .bind(days)
        .fetch_all(pool)
        .await?;
        if !ids.is_empty() {
            warn!(
                source = *source_name,
                event = "ingestion_unlisted_closed",
                closed = ids.len(),
                days,
                "programmes not listed by the provider within the window were closed"
            );
        }
        closed.insert((*source_name).to_string(), serde_json::json!(ids.len()));
    }
    Ok(serde_json::Value::Object(closed))
}

/// Core processing: fetch records, compare hashes, normalize, and upsert.
async fn process_source<S: DataSource>(
    pool: &PgPool,
    source: &S,
    run_id: Uuid,
    allow_empty: bool,
) -> Result<ProcessingStats> {
    let source_name = source.name();

    // 1. Fetch all raw records from the API
    let fetch = source.fetch_all().await?;
    // A provider that returns nothing at all (an expired key answered with
    // an empty list, a changed request contract, a first page that is empty
    // although the total is not) fails the run: it is retried and opens
    // `ingestion_failed:<source>` instead of passing as a success.
    anyhow::ensure!(
        fetch.upstream_seen > 0 || allow_empty,
        "{source_name}: the provider returned no records; set SOURCE_ALLOW_EMPTY_{} only if this source may be empty",
        source_name.to_ascii_uppercase()
    );
    let upstream_seen = fetch.upstream_seen;
    let parts = part_reports(pool, source_name, &fetch.parts).await?;
    report_listed_not_kept(
        pool,
        source_name,
        run_id,
        &fetch.filtered_out,
        &fetch.set_aside_ids,
    )
    .await;
    let fetch_complete = fetch.whole_settled();
    let records = fetch.records;
    let total = records.len();

    // Cross-source duplicate caches (`DedupCaches`), loaded once per run, not
    // once per record.
    let caches = load_dedup_caches(pool, source_name).await;

    // youth_center-only: refUrlAddr1/2 values reused across 2+ distinct
    // records within this batch are untrusted as deep links (see
    // `collect_shared_youth_center_ref_urls` doc comment). Computed once per
    // run, not once per record — it's a single pass over the batch already
    // held in memory.
    let shared_youth_center_ref_urls: HashSet<String> = if source_name == "youth_center" {
        collect_shared_youth_center_ref_urls(&records)
    } else {
        HashSet::new()
    };

    let (unique, duplicate_ids) = last_of_each_id(&records);
    if duplicate_ids > 0 {
        warn!(
            source = source_name,
            duplicate_ids, "records share a source id; the last of each is kept"
        );
    }

    let mut stats = ProcessingStats {
        total,
        duplicate_ids,
        upstream_seen,
        seen_source_ids: unique
            .iter()
            .map(|record| record.source_id.clone())
            .collect(),
        fetch_complete,
        parts,
        ..ProcessingStats::default()
    };

    for chunk in unique.chunks(4) {
        let results = futures_util::future::join_all(chunk.iter().map(|record| {
            process_record(
                pool,
                source_name,
                run_id,
                record,
                &caches,
                &shared_youth_center_ref_urls,
                RecordOrigin::FreshFetch,
            )
        }))
        .await;
        for (record, result) in chunk.iter().zip(results) {
            match result {
                Ok(ProcessOutcome::Changed) => stats.changed += 1,
                Ok(ProcessOutcome::Renormalized) => stats.renormalized += 1,
                Ok(ProcessOutcome::Unchanged) => {}
                Err(e) => {
                    let item_status = if is_rejection(&e) {
                        if quarantines_rejection(pool, source_name, &record.source_id).await {
                            stats.quarantined += 1;
                            "quarantined"
                        } else {
                            stats.rejected += 1;
                            "rejected"
                        }
                    } else {
                        stats.failed += 1;
                        "failed"
                    };
                    warn!(
                        source = source_name,
                        source_id = %record.source_id,
                        outcome = item_status,
                        error = %e,
                        "record processing did not complete, continuing"
                    );
                    let _ = sqlx::query(
                        "INSERT INTO ingestion_items \
                         (id, run_id, source_id, status, error_message, created_at) \
                     VALUES ($1, $2, $3, $4, $5, now()) \
                     ON CONFLICT (run_id, source_id) DO UPDATE \
                         SET status = EXCLUDED.status, error_message = EXCLUDED.error_message",
                    )
                    .bind(Uuid::new_v4())
                    .bind(run_id)
                    .bind(&record.source_id)
                    .bind(item_status)
                    .bind(persisted_error(&e))
                    .execute(pool)
                    .await;
                }
            }
        }
    }

    Ok(stats)
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct LifecycleSummary {
    /// Why no state changed, when the fetch did not qualify.
    pub(crate) skipped_reason: Option<&'static str>,
    pub(crate) marked_missing: u64,
    pub(crate) closed: u64,
    pub(crate) restored: u64,
}

/// `LifecycleSummary::skipped_reason` when the source itself reported that
/// the page loop delivered fewer records than its total.
const INCOMPLETE_FETCH_REASON: &str = "provider total mismatch";

/// A fetch may drive closures only when it is plausibly complete: the
/// provider's own total was reached (`fetch_complete`), at least one record,
/// a previous successful run to compare against, and no more than a 20% drop
/// from that run.
/// The last record of each source id, in fetch order, and how many earlier
/// ones were dropped. Keeping the last matches sequential upsert semantics;
/// bounded concurrency then never runs two writes for one upstream identity.
fn last_of_each_id(records: &[RawRecord]) -> (Vec<&RawRecord>, usize) {
    let mut seen = HashSet::new();
    let mut unique: Vec<&RawRecord> = records
        .iter()
        .rev()
        .filter(|record| seen.insert(&record.source_id))
        .collect();
    unique.reverse();
    let dropped = records.len() - unique.len();
    (unique, dropped)
}

#[cfg(test)]
mod duplicate_id_tests {
    use super::*;

    #[test]
    fn records_sharing_an_id_are_counted() {
        let record = |id: &str, end: &str| RawRecord {
            source_id: id.into(),
            payload: serde_json::json!({ "모집종료일": end }),
            content_hash: end.into(),
        };
        let records = vec![
            record("A재단_희망장학금", "2026-05-31"),
            record("B_x", "2026-01-01"),
            record("A재단_희망장학금", "2026-11-30"),
        ];
        let (unique, dropped) = last_of_each_id(&records);
        assert_eq!(dropped, 1);
        assert_eq!(unique.len(), 2);
        assert_eq!(unique[1].payload["모집종료일"], "2026-11-30");
    }
}

fn fetch_qualifies_for_reconciliation(
    fetch_complete: bool,
    fetched: usize,
    previous_total: Option<i32>,
) -> Result<(), &'static str> {
    if !fetch_complete {
        return Err(INCOMPLETE_FETCH_REASON);
    }
    if fetched == 0 {
        return Err("empty fetch");
    }
    let Some(previous) = previous_total else {
        return Err("no previous successful run");
    };
    if (fetched as f64) < (previous.max(0) as f64) * MIN_FETCH_RATIO_FOR_RECONCILIATION {
        return Err("fetched less than 80% of previous successful run");
    }
    Ok(())
}

/// Reconcile stored programs of one source against the identities present in
/// a complete fetch. Absent programs accumulate a miss counter and are closed
/// (never deleted) after `SOURCE_MISSING_RUNS_TO_CLOSE` consecutive runs;
/// programs closed that way are restored as soon as the source lists them
/// again, even with an unchanged payload. Everything happens in one
/// transaction so a failure leaves no half-applied counters.
async fn reconcile_source_lifecycle(
    pool: &PgPool,
    source_name: &str,
    run_id: Uuid,
    stats: &ProcessingStats,
) -> Result<LifecycleSummary> {
    let mut tx = pool.begin().await?;
    let summary = reconcile_source_lifecycle_tx(
        &mut tx,
        source_name,
        run_id,
        stats.fetch_complete,
        stats.total,
        &stats.seen_source_ids,
        &stats
            .parts
            .iter()
            .filter(|part| part.unsettled().is_some())
            .map(|part| part.id_prefix.clone())
            .collect::<Vec<_>>(),
    )
    .await?;
    tx.commit().await?;
    Ok(summary)
}

async fn reconcile_source_lifecycle_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    source_name: &str,
    run_id: Uuid,
    fetch_complete: bool,
    fetched: usize,
    seen_source_ids: &[String],
    unsettled_prefixes: &[String],
) -> Result<LifecycleSummary> {
    let previous_total: Option<i32> = sqlx::query_scalar(
        "SELECT total_fetched FROM ingestion_runs \
         WHERE source_name = $1 AND run_kind = 'ingestion' AND id <> $2 \
           AND status IN ('success', 'success_with_rejections') \
         ORDER BY started_at DESC LIMIT 1",
    )
    .bind(source_name)
    .bind(run_id)
    .fetch_optional(&mut **tx)
    .await?
    .flatten();
    // Listed again after an automatic source_removed closure (a missing run
    // count or the freshness window): reopen, from any fetch, complete or
    // not, unless its
    // own dates say it has ended, or the source itself declares the intake
    // closed (온통청년 마감 carries no dates). A source-declared status is
    // kept rather than replaced by the date-derived one. Audit each reopening.
    let restored = sqlx::query_scalar::<_, i64>(
        "WITH restored AS ( \
             UPDATE programs SET is_active = true, retired_at = NULL, retirement_reason = NULL, \
                 program_status = COALESCE(normalized_payload->>'source_status', \
                     CASE WHEN application_start_at > now() THEN 'upcoming' ELSE 'active' END), \
                 source_missing_runs = 0, source_missing_since = NULL, updated_at = now() \
             WHERE source_type = $1 AND NOT is_active AND retirement_reason = 'source_removed' \
               AND source_id IN (SELECT unnest($2::text[])) \
               AND normalized_payload->>'source_status' IS DISTINCT FROM 'closed' \
               AND (LEAST(benefit_valid_until, deadline_at, application_end_at) IS NULL \
                    OR LEAST(benefit_valid_until, deadline_at, application_end_at) > now()) \
             RETURNING id, source_id \
         ), audit AS ( \
             INSERT INTO ingestion_items (id, run_id, source_id, program_id, status, created_at) \
             SELECT gen_random_uuid(), $3, source_id, id, 'restored', now() FROM restored \
             ON CONFLICT (run_id, source_id) DO UPDATE \
                 SET status = 'restored', program_id = EXCLUDED.program_id \
             RETURNING 1 \
         ) SELECT count(*) FROM audit",
    )
    .bind(source_name)
    .bind(seen_source_ids)
    .bind(run_id)
    .fetch_one(&mut **tx)
    .await?;

    if let Err(reason) = fetch_qualifies_for_reconciliation(fetch_complete, fetched, previous_total)
    {
        return Ok(LifecycleSummary {
            skipped_reason: Some(reason),
            restored: restored.max(0) as u64,
            ..LifecycleSummary::default()
        });
    }

    // Seen again: forget any accumulated misses. Bookkeeping only, so the
    // public updated_at timestamp is left alone.
    sqlx::query(
        "UPDATE programs SET source_missing_runs = 0, source_missing_since = NULL \
         WHERE source_type = $1 AND source_missing_runs > 0 \
           AND source_id IN (SELECT unnest($2::text[]))",
    )
    .bind(source_name)
    .bind(seen_source_ids)
    .execute(&mut **tx)
    .await?;

    let marked_missing = sqlx::query(
        "UPDATE programs SET source_missing_runs = source_missing_runs + 1, \
             source_missing_since = COALESCE(source_missing_since, now()) \
         WHERE source_type = $1 AND is_active AND source_id IS NOT NULL \
           AND source_id NOT IN (SELECT unnest($2::text[])) \
           AND NOT EXISTS (SELECT 1 FROM unnest($3::text[]) AS part(prefix) \
                           WHERE starts_with(source_id, part.prefix))",
    )
    .bind(source_name)
    .bind(seen_source_ids)
    .bind(unsettled_prefixes)
    .execute(&mut **tx)
    .await?
    .rows_affected();

    let closed = sqlx::query_scalar::<_, i64>(
        "WITH closed AS ( \
             UPDATE programs SET is_active = false, program_status = 'closed', \
                 retired_at = now(), retirement_reason = 'source_removed', updated_at = now() \
             WHERE source_type = $1 AND is_active AND source_missing_runs >= $2 \
               AND NOT EXISTS (SELECT 1 FROM unnest($4::text[]) AS part(prefix) \
                               WHERE starts_with(source_id, part.prefix)) \
             RETURNING id, source_id \
         ), audit AS ( \
             INSERT INTO ingestion_items (id, run_id, source_id, program_id, status, created_at) \
             SELECT gen_random_uuid(), $3, source_id, id, 'closed_source_removed', now() FROM closed \
             ON CONFLICT (run_id, source_id) DO UPDATE \
                 SET status = 'closed_source_removed', program_id = EXCLUDED.program_id \
             RETURNING 1 \
         ) SELECT count(*) FROM audit",
    )
    .bind(source_name)
    .bind(SOURCE_MISSING_RUNS_TO_CLOSE)
    .bind(run_id)
    .bind(unsettled_prefixes)
    .fetch_one(&mut **tx)
    .await?;

    Ok(LifecycleSummary {
        skipped_reason: None,
        marked_missing,
        closed: closed.max(0) as u64,
        restored: restored.max(0) as u64,
    })
}

/// A run that never finished (worker restart, aborted job) must not stay
/// `running` forever. Daily jobs time out after 30 minutes, so anything older
/// than an hour is dead. Called at the start of every scheduler pass.
pub async fn fail_stale_ingestion_runs<'e, E: sqlx::PgExecutor<'e>>(executor: E) -> Result<u64> {
    Ok(sqlx::query(
        "UPDATE ingestion_runs \
         SET status = 'failed', finished_at = now(), \
             error_message = 'Run did not finish; marked stale after one hour' \
         WHERE finished_at IS NULL AND status = 'running' \
           AND started_at < now() - interval '1 hour'",
    )
    .execute(executor)
    .await?
    .rows_affected())
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use serde_json::json;

    async fn test_pool() -> PgPool {
        let url = std::env::var("TEST_DATABASE_URL")
            .unwrap_or_else(|_| "postgres://wello:wello@localhost:5432/wello".into());
        sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect(&url)
            .await
            .expect("isolated test database is required")
    }

    fn record(source_id: &str) -> RawRecord {
        let payload = json!({"title": format!("생명주기 {source_id}")});
        RawRecord {
            source_id: source_id.into(),
            content_hash: crate::sources::content_hash(&payload),
            payload,
        }
    }

    fn normalized(source_id: &str, end: Option<DateTime<Utc>>) -> NormalizedProgram {
        let mut program = normalize(
            "lifecycle_test",
            source_id,
            &record(source_id).payload,
            &HashSet::new(),
        );
        program.application_end_at = end;
        program
    }

    async fn insert_run(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        source: &str,
        status: &str,
        total: i32,
    ) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO ingestion_runs(id,source_name,status,started_at,finished_at,total_fetched,run_kind) \
             VALUES($1,$2,$3,now(),now(),$4,'ingestion')",
        )
        .bind(id)
        .bind(source)
        .bind(status)
        .bind(total)
        .execute(&mut **tx)
        .await
        .unwrap();
        id
    }

    /// (is_active, program_status, retirement_reason, retired_at set, source_missing_runs)
    async fn state(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        source: &str,
        source_id: &str,
    ) -> (bool, String, Option<String>, bool, i32) {
        sqlx::query_as(
            "SELECT is_active, program_status, retirement_reason, retired_at IS NOT NULL, source_missing_runs \
             FROM programs WHERE source_type=$1 AND source_id=$2",
        )
        .bind(source)
        .bind(source_id)
        .fetch_one(&mut **tx)
        .await
        .unwrap()
    }

    async fn audit_status(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        run: Uuid,
        source_id: &str,
    ) -> String {
        sqlx::query_scalar("SELECT status FROM ingestion_items WHERE run_id=$1 AND source_id=$2")
            .bind(run)
            .bind(source_id)
            .fetch_one(&mut **tx)
            .await
            .unwrap()
    }

    fn seen(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|id| id.to_string()).collect()
    }

    /// A failed reconciliation leaves removed programmes active, so it is
    /// an incident (not only a log line) until a reconciliation completes.
    #[tokio::test]
    async fn a_failed_lifecycle_reconciliation_opens_an_incident_until_one_completes() {
        let pool = test_pool().await;
        let source = format!("lifecycle_{}", Uuid::new_v4().simple());
        let fingerprint = format!("lifecycle_reconciliation_failed:{source}");
        let status = |pool: PgPool, fingerprint: String| async move {
            sqlx::query_as::<_, (String, String)>(
                "SELECT status, severity FROM operational_events WHERE fingerprint=$1",
            )
            .bind(fingerprint)
            .fetch_optional(&pool)
            .await
            .unwrap()
        };
        report_lifecycle(
            &pool,
            &source,
            Uuid::new_v4(),
            &Err(anyhow::anyhow!("deadlock detected")),
        )
        .await;
        assert_eq!(
            status(pool.clone(), fingerprint.clone()).await,
            Some(("open".into(), "error".into()))
        );
        report_lifecycle(
            &pool,
            &source,
            Uuid::new_v4(),
            &Ok(LifecycleSummary::default()),
        )
        .await;
        assert_eq!(
            status(pool.clone(), fingerprint.clone())
                .await
                .map(|s| s.0)
                .as_deref(),
            Some("resolved")
        );
        sqlx::query("DELETE FROM operational_events WHERE fingerprint=$1")
            .bind(&fingerprint)
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
    }

    #[test]
    fn fetch_guard_rejects_incomplete_empty_first_and_shrunken_fetches() {
        assert_eq!(
            fetch_qualifies_for_reconciliation(false, 100, Some(100)),
            Err("provider total mismatch")
        );
        assert_eq!(
            fetch_qualifies_for_reconciliation(true, 0, Some(100)),
            Err("empty fetch")
        );
        assert_eq!(
            fetch_qualifies_for_reconciliation(true, 100, None),
            Err("no previous successful run")
        );
        assert_eq!(
            fetch_qualifies_for_reconciliation(true, 79, Some(100)),
            Err("fetched less than 80% of previous successful run")
        );
        assert!(fetch_qualifies_for_reconciliation(true, 80, Some(100)).is_ok());
        assert!(fetch_qualifies_for_reconciliation(true, 5, Some(0)).is_ok());
    }

    #[test]
    fn a_run_without_records_is_empty_never_success() {
        let stats = |total, failed, rejected| ProcessingStats {
            total,
            failed,
            rejected,
            upstream_seen: 132,
            ..ProcessingStats::default()
        };
        assert_eq!(completed_run_status(&stats(0, 0, 0)).0, "empty");
        assert!(completed_run_status(&stats(0, 0, 0))
            .1
            .unwrap()
            .contains("returned 132 record(s)"));
        assert_eq!(completed_run_status(&stats(3, 0, 0)), ("success", None));
        assert_eq!(completed_run_status(&stats(3, 1, 0)).0, "failed");
        assert_eq!(
            completed_run_status(&stats(3, 0, 1)).0,
            "success_with_rejections"
        );
    }

    #[test]
    fn contract_violations_are_rejections_not_transient_failures() {
        let raw = json!({"plcyNm": "연령 역전", "sprtTrgtMinAge": "40", "sprtTrgtMaxAge": "20"});
        let mut program = normalize("youth_center", "x", &raw, &HashSet::new());
        let error = validate_normalized_program(&mut program)
            .map_err(|e| anyhow::Error::from(RecordRejection(e.to_string())))
            .unwrap_err();
        assert!(is_rejection(&error));
        assert!(error.to_string().contains("age range"));
        assert!(!is_rejection(&anyhow::anyhow!("connection reset by peer")));
    }

    /// A part of the catalogue the fetch could not settle (one finlife cell
    /// answered with the firewall's block page) keeps its programmes: they
    /// are neither counted missing nor closed, while an absent programme of
    /// a settled part is retired as before. Ids are the finlife ones.
    #[tokio::test]
    async fn an_unsettled_part_keeps_its_programmes_while_the_rest_reconciles() {
        let pool = test_pool().await;
        let mut tx = pool.begin().await.unwrap();
        let source = format!("lifecycle_{}", Uuid::new_v4().simple());
        let future = Some(Utc::now() + Duration::days(30));
        let blocked = "fss_030300_deposit_0010345_HK001";
        let gone = "fss_rent_loan_0010019_OLD";
        let kept = "fss_rent_loan_0010019_LN11010001072";
        // A bank deposit whose id the blocked prefix must not reach.
        let bank = "fss_deposit_0010001_WR0001B";
        insert_run(&mut tx, &source, "success", 4).await;
        for id in [blocked, gone, kept, bank] {
            upsert_program(
                &mut *tx,
                &source,
                &record(id),
                &normalized(id, future),
                "active",
                1,
                RecordOrigin::FreshFetch,
            )
            .await
            .unwrap();
        }
        let unsettled = ["fss_030300_deposit_".to_string()];
        for run_number in 1..=SOURCE_MISSING_RUNS_TO_CLOSE + 1 {
            let run = insert_run(&mut tx, &source, "success", 4).await;
            reconcile_source_lifecycle_tx(
                &mut tx,
                &source,
                run,
                true,
                4,
                &seen(&[kept, bank, "x1", "x2"]),
                &unsettled,
            )
            .await
            .unwrap();
            let (active, _, reason, retired, missing) = state(&mut tx, &source, blocked).await;
            assert!(active && reason.is_none() && !retired, "run {run_number}");
            assert_eq!(missing, 0, "run {run_number}");
        }
        let (active, _, reason, _, _) = state(&mut tx, &source, gone).await;
        assert!(!active);
        assert_eq!(reason.as_deref(), Some("source_removed"));
        assert!(state(&mut tx, &source, kept).await.0);
        assert!(state(&mut tx, &source, bank).await.0);
        tx.rollback().await.unwrap();
        pool.close().await;
    }

    #[test]
    fn a_run_names_the_parts_it_could_not_settle() {
        let stats = ProcessingStats {
            total: 3,
            upstream_seen: 90,
            parts: vec![PartReport {
                id_prefix: "fss_030300_deposit_".into(),
                outcome: crate::sources::PartOutcome {
                    scope: crate::sources::PartScope::Prefix("fss_030300_deposit_".into()),
                    state: crate::sources::PartState::Unsettled(crate::sources::Unsettled {
                        code: "non_json_body",
                        detail: "the body is not JSON (deposit × 저축은행)".into(),
                    }),
                    seen: 0,
                    kept: 0,
                    set_aside: 0,
                },
                shown: 0,
            }],
            ..ProcessingStats::default()
        };
        let (status, message) = completed_run_status(&stats);
        assert_eq!(status, "success");
        assert_eq!(
            message.as_deref(),
            Some("1 part(s) of the catalogue were not settled; their programmes were left as they were: fss_030300_deposit_* non_json_body: the body is not JSON (deposit × 저축은행)")
        );
    }

    #[tokio::test]
    async fn disappeared_program_closes_after_three_complete_runs_and_restores_on_return() {
        let pool = test_pool().await;
        let mut tx = pool.begin().await.unwrap();
        let source = format!("lifecycle_{}", Uuid::new_v4().simple());
        let future = Some(Utc::now() + Duration::days(30));
        let past = Some(Utc::now() - Duration::days(1));
        insert_run(&mut tx, &source, "success", 3).await;
        for id in ["a", "b", "c"] {
            upsert_program(
                &mut *tx,
                &source,
                &record(id),
                &normalized(id, future),
                "active",
                1,
                RecordOrigin::FreshFetch,
            )
            .await
            .unwrap();
        }
        // Operator and date-driven closures must survive every reconciliation.
        for (id, reason) in [("manual", "manual"), ("expired", "expired")] {
            upsert_program(
                &mut *tx,
                &source,
                &record(id),
                &normalized(id, past),
                "closed",
                1,
                RecordOrigin::FreshFetch,
            )
            .await
            .unwrap();
            sqlx::query("UPDATE programs SET is_active=false, program_status='closed', retirement_reason=$3, retired_at=now() WHERE source_type=$1 AND source_id=$2")
                .bind(&source).bind(id).bind(reason).execute(&mut *tx).await.unwrap();
        }

        // 2 of 3 is below the 80% completeness floor: nothing is counted.
        let run = insert_run(&mut tx, &source, "success", 2).await;
        let summary =
            reconcile_source_lifecycle_tx(&mut tx, &source, run, true, 2, &seen(&["a", "b"]), &[])
                .await
                .unwrap();
        assert_eq!(
            summary.skipped_reason,
            Some("fetched less than 80% of previous successful run")
        );
        assert_eq!(state(&mut tx, &source, "c").await.4, 0);

        // The source reported a total its pages never reached: the records
        // were upserted, but absences are not counted from such a fetch.
        let run = insert_run(&mut tx, &source, "success", 3).await;
        let summary = reconcile_source_lifecycle_tx(
            &mut tx,
            &source,
            run,
            false,
            3,
            &seen(&["a", "b", "d"]),
            &[],
        )
        .await
        .unwrap();
        assert_eq!(summary.skipped_reason, Some("provider total mismatch"));
        assert_eq!((summary.marked_missing, summary.closed), (0, 0));
        assert_eq!(state(&mut tx, &source, "c").await.4, 0);

        // Three complete runs without "c" (a new "d" keeps the total steady).
        for expected_missing in 1..=SOURCE_MISSING_RUNS_TO_CLOSE {
            let run = insert_run(&mut tx, &source, "success", 3).await;
            let summary = reconcile_source_lifecycle_tx(
                &mut tx,
                &source,
                run,
                true,
                3,
                &seen(&["a", "b", "d"]),
                &[],
            )
            .await
            .unwrap();
            assert_eq!(summary.skipped_reason, None);
            assert_eq!(summary.marked_missing, 1, "run {expected_missing}");
            let (active, status, reason, retired, missing) = state(&mut tx, &source, "c").await;
            assert_eq!(missing, expected_missing);
            if expected_missing < SOURCE_MISSING_RUNS_TO_CLOSE {
                assert_eq!(summary.closed, 0);
                assert!(active && status == "active" && reason.is_none() && !retired);
            } else {
                assert_eq!(summary.closed, 1);
                assert!(!active && retired);
                assert_eq!(status, "closed");
                assert_eq!(reason.as_deref(), Some("source_removed"));
                assert_eq!(
                    audit_status(&mut tx, run, "c").await,
                    "closed_source_removed"
                );
            }
            assert_eq!(state(&mut tx, &source, "a").await.4, 0);
        }

        // The source lists "c" again with an unchanged payload: reopen it, and
        // only it. Manual and expired closures are not the source's to undo.
        let run = insert_run(&mut tx, &source, "success", 5).await;
        let summary = reconcile_source_lifecycle_tx(
            &mut tx,
            &source,
            run,
            true,
            5,
            &seen(&["a", "b", "c", "manual", "expired"]),
            &[],
        )
        .await
        .unwrap();
        assert_eq!(
            (summary.restored, summary.closed, summary.marked_missing),
            (1, 0, 0)
        );
        let (active, status, reason, retired, missing) = state(&mut tx, &source, "c").await;
        assert!(active && !retired);
        assert_eq!((status.as_str(), reason, missing), ("active", None, 0));
        assert_eq!(audit_status(&mut tx, run, "c").await, "restored");
        for (id, reason) in [("manual", "manual"), ("expired", "expired")] {
            let held = state(&mut tx, &source, id).await;
            assert!(!held.0, "{id} must stay closed");
            assert_eq!(held.2.as_deref(), Some(reason));
        }

        // A source_removed closure whose own window has ended stays closed.
        sqlx::query("UPDATE programs SET is_active=false, program_status='closed', retirement_reason='source_removed', retired_at=now(), application_end_at=now()-interval '1 day' WHERE source_type=$1 AND source_id='b'")
            .bind(&source).execute(&mut *tx).await.unwrap();
        let run = insert_run(&mut tx, &source, "success", 5).await;
        let summary = reconcile_source_lifecycle_tx(
            &mut tx,
            &source,
            run,
            true,
            5,
            &seen(&["a", "b", "c", "manual", "expired"]),
            &[],
        )
        .await
        .unwrap();
        assert_eq!(summary.restored, 0);
        assert!(!state(&mut tx, &source, "b").await.0);
        tx.rollback().await.unwrap();
        pool.close().await;
    }

    #[tokio::test]
    async fn fresh_fetch_reopens_only_automatically_closed_programs() {
        let pool = test_pool().await;
        let mut tx = pool.begin().await.unwrap();
        let source = format!("lifecycle_{}", Uuid::new_v4().simple());
        let future = Some(Utc::now() + Duration::days(30));
        // (id, closure reason, new status, origin, expect active, expect reason)
        type RestoreCase<'a> = (
            &'a str,
            Option<&'a str>,
            &'a str,
            RecordOrigin,
            bool,
            Option<&'a str>,
        );
        let cases: [RestoreCase<'_>; 6] = [
            (
                "expired_reopened",
                Some("expired"),
                "active",
                RecordOrigin::FreshFetch,
                true,
                None,
            ),
            (
                "removed_reopened",
                Some("source_removed"),
                "upcoming",
                RecordOrigin::FreshFetch,
                true,
                None,
            ),
            (
                "expired_still_closed",
                Some("expired"),
                "closed",
                RecordOrigin::FreshFetch,
                false,
                Some("expired"),
            ),
            (
                "manual_hold",
                Some("manual"),
                "active",
                RecordOrigin::FreshFetch,
                false,
                Some("manual"),
            ),
            (
                "hidden_without_reason",
                None,
                "active",
                RecordOrigin::FreshFetch,
                false,
                None,
            ),
            (
                "replay_never_restores",
                Some("source_removed"),
                "active",
                RecordOrigin::StoredSnapshot,
                false,
                Some("source_removed"),
            ),
        ];
        for (id, reason, new_status, origin, expect_active, expect_reason) in cases {
            upsert_program(
                &mut *tx,
                &source,
                &record(id),
                &normalized(id, future),
                "active",
                1,
                RecordOrigin::FreshFetch,
            )
            .await
            .unwrap();
            sqlx::query("UPDATE programs SET is_active=false, program_status='closed', retirement_reason=$3, retired_at=CASE WHEN $3::text IS NULL THEN NULL ELSE now() END, source_missing_runs=2, source_missing_since=now() WHERE source_type=$1 AND source_id=$2")
                .bind(&source).bind(id).bind(reason).execute(&mut *tx).await.unwrap();
            upsert_program(
                &mut *tx,
                &source,
                &record(id),
                &normalized(id, future),
                new_status,
                2,
                origin,
            )
            .await
            .unwrap();
            let (active, status, got_reason, retired, missing) = state(&mut tx, &source, id).await;
            assert_eq!(active, expect_active, "{id}");
            assert_eq!(status, new_status, "{id}");
            assert_eq!(got_reason.as_deref(), expect_reason, "{id}");
            assert_eq!(retired, expect_reason.is_some(), "{id}");
            let fresh = origin == RecordOrigin::FreshFetch;
            assert_eq!(missing, if fresh { 0 } else { 2 }, "{id}");
        }
        tx.rollback().await.unwrap();
        pool.close().await;
    }

    /// A retained 온통청년 payload, trimmed to the fields the normalizer reads
    /// (shape copied from production snapshots; staff names removed).
    fn youth_payload(code: &str, aply_ymd: &str) -> serde_json::Value {
        json!({
            "plcyNo": "20231102005400200004", "plcyNm": "강원 고성군 청년 취업수당 지원",
            "plcySprtCn": "청년이 관내 업체 취업 시 매월 30만원씩 12개월간 지원(생애 1회 지원)",
            "sprvsnInstCdNm": "고성군 경제체육과", "lclsfNm": "일자리", "mclsfNm": "취업",
            "aplyPrdSeCd": code, "aplyYmd": aply_ymd, "bizPrdSeCd": "0056002",
            "bizPrdBgngYmd": "        ", "bizPrdEndYmd": "        ", "bizPrdEtcCn": "매년",
            "aplyUrlAddr": "", "refUrlAddr2": "",
            "refUrlAddr1": "https://gwgs.go.kr/prog/saeolGosi/GOSI/kor/sub04_030401/list.do",
            "sprtTrgtMinAge": "19", "sprtTrgtMaxAge": "39", "zipCd": "51820",
            "sprvsnInstPicNm": "-", "operInstPicNm": "-"
        })
    }

    async fn sync_youth(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        source: &str,
        source_id: &str,
        payload: serde_json::Value,
    ) {
        sync_as(tx, "youth_center", source, source_id, payload).await;
    }

    /// A fresh fetch of `payload` through the `normalizer` source's mapping,
    /// stored under the throwaway `source` type.
    async fn sync_as(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        normalizer: &str,
        source: &str,
        source_id: &str,
        payload: serde_json::Value,
    ) {
        let norm = normalize(normalizer, source_id, &payload, &HashSet::new());
        let record = RawRecord {
            source_id: source_id.into(),
            content_hash: crate::sources::content_hash(&payload),
            payload,
        };
        upsert_program(
            &mut **tx,
            source,
            &record,
            &norm,
            program_status_for(&norm),
            normalizer_version(normalizer),
            RecordOrigin::FreshFetch,
        )
        .await
        .unwrap();
    }

    async fn available(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        source: &str,
        source_id: &str,
    ) -> bool {
        sqlx::query_scalar(&format!(
            "SELECT {} FROM programs WHERE source_type=$1 AND source_id=$2",
            mazimi_core::currently_available_sql("", "now()")
        ))
        .bind(source)
        .bind(source_id)
        .fetch_one(&mut **tx)
        .await
        .unwrap()
    }

    /// 0057003 (마감) hides the intake and 0057002 (상시) shows it, on every
    /// sync and in both directions. Operator closures are never undone.
    #[tokio::test]
    async fn youth_center_intake_code_follows_the_source_and_respects_manual_holds() {
        let pool = test_pool().await;
        let mut tx = pool.begin().await.unwrap();
        let source = format!("lifecycle_{}", Uuid::new_v4().simple());
        let closed = || youth_payload("0057003", "");
        let open = || youth_payload("0057002", "");

        sync_youth(&mut tx, &source, "flip", closed()).await;
        let (active, status, reason, ..) = state(&mut tx, &source, "flip").await;
        assert_eq!((active, status.as_str(), reason), (true, "closed", None));
        assert!(!available(&mut tx, &source, "flip").await);
        sync_youth(&mut tx, &source, "flip", open()).await;
        assert_eq!(state(&mut tx, &source, "flip").await.1, "always_open");
        assert!(available(&mut tx, &source, "flip").await);
        sync_youth(&mut tx, &source, "flip", closed()).await;
        assert_eq!(state(&mut tx, &source, "flip").await.1, "closed");
        assert!(!available(&mut tx, &source, "flip").await);

        // 0057001 (특정기간): the period fields decide, as before.
        sync_youth(
            &mut tx,
            &source,
            "dated",
            youth_payload("0057001", "20260312 ~ 20260319"),
        )
        .await;
        assert_eq!(state(&mut tx, &source, "dated").await.1, "closed");

        // An operator closure stays closed even when the source says 상시.
        sync_youth(&mut tx, &source, "manual", closed()).await;
        sqlx::query("UPDATE programs SET is_active=false, retirement_reason='manual', retired_at=now() WHERE source_type=$1 AND source_id='manual'")
            .bind(&source).execute(&mut *tx).await.unwrap();
        sync_youth(&mut tx, &source, "manual", open()).await;
        let (active, _, reason, ..) = state(&mut tx, &source, "manual").await;
        assert!(!active);
        assert_eq!(reason.as_deref(), Some("manual"));
        assert!(!available(&mut tx, &source, "manual").await);

        // An automatic closure is the source's to undo, but 마감 keeps it shut.
        sync_youth(&mut tx, &source, "expired", open()).await;
        sqlx::query("UPDATE programs SET is_active=false, program_status='closed', retirement_reason='expired', retired_at=now() WHERE source_type=$1 AND source_id='expired'")
            .bind(&source).execute(&mut *tx).await.unwrap();
        sync_youth(&mut tx, &source, "expired", closed()).await;
        assert!(!state(&mut tx, &source, "expired").await.0);
        sync_youth(&mut tx, &source, "expired", open()).await;
        let (active, status, reason, ..) = state(&mut tx, &source, "expired").await;
        assert_eq!(
            (active, status.as_str(), reason),
            (true, "always_open", None)
        );

        // Relisted after a source_removed closure: 마감 stays closed (it has no
        // dates for the restore to check), 상시 comes back as always_open.
        for (id, payload) in [("removed_closed", closed()), ("removed_open", open())] {
            sync_youth(&mut tx, &source, id, payload).await;
            sqlx::query("UPDATE programs SET is_active=false, program_status='closed', retirement_reason='source_removed', retired_at=now() WHERE source_type=$1 AND source_id=$2")
                .bind(&source).bind(id).execute(&mut *tx).await.unwrap();
        }
        let all = [
            "flip",
            "dated",
            "manual",
            "expired",
            "removed_closed",
            "removed_open",
        ];
        insert_run(&mut tx, &source, "success", all.len() as i32).await;
        let run = insert_run(&mut tx, &source, "success", all.len() as i32).await;
        let summary =
            reconcile_source_lifecycle_tx(&mut tx, &source, run, true, all.len(), &seen(&all), &[])
                .await
                .unwrap();
        assert_eq!(summary.restored, 1);
        let (active, status, reason, ..) = state(&mut tx, &source, "removed_closed").await;
        assert_eq!(
            (active, status.as_str(), reason.as_deref()),
            (false, "closed", Some("source_removed"))
        );
        assert!(!available(&mut tx, &source, "removed_closed").await);
        let (active, status, reason, ..) = state(&mut tx, &source, "removed_open").await;
        assert_eq!(
            (active, status.as_str(), reason),
            (true, "always_open", None)
        );
        assert!(available(&mut tx, &source, "removed_open").await);
        tx.rollback().await.unwrap();
        pool.close().await;
    }

    /// An admin-approved AI window is what the row stores while the raw
    /// payload is unchanged, so the status follows it, not the parsed text.
    #[tokio::test]
    async fn approved_ai_window_decides_the_status_of_a_parsed_gov_window() {
        let pool = test_pool().await;
        let source_id = format!("ai-window-{}", Uuid::new_v4().simple());
        let payload = json!({"서비스ID": source_id, "서비스명": "청년 월세 지원", "신청기한": "2025.6.11. ~ 2025.6.24."});
        let record = RawRecord {
            source_id: source_id.clone(),
            content_hash: crate::sources::content_hash(&payload),
            payload: payload.clone(),
        };
        sqlx::query("INSERT INTO source_snapshots(id,source_name,source_id,content_hash,raw_payload,normalizer_version) VALUES($1,'gov_benefits',$2,$3,$4,0)")
            .bind(Uuid::new_v4()).bind(&source_id).bind(&record.content_hash).bind(&payload).execute(&pool).await.unwrap();
        let id: Uuid = sqlx::query_scalar("INSERT INTO programs(program_type,source_type,source_id,title,program_status,is_active,normalizer_version,raw_payload) VALUES('benefit','gov_benefits',$1,'청년 월세 지원','active',true,0,$2) RETURNING id")
            .bind(&source_id).bind(&payload).fetch_one(&pool).await.unwrap();
        let cache: Uuid = sqlx::query_scalar("INSERT INTO program_ai_extraction_cache(program_id,source_type,source_hash,provider,model,prompt_version,schema_version,request_hash,source_text,status,response_json)
            VALUES($1,'test',repeat('a',64),'mock','m','p',1,repeat('b',64),'x','succeeded','{}') RETURNING id")
            .bind(id).fetch_one(&pool).await.unwrap();
        let candidate: Uuid = sqlx::query_scalar("INSERT INTO program_ai_extraction_candidates(cache_id,program_id,source_hash,provider,model,prompt_version,schema_version,fact_key,fact_type,value,evidence,confidence,status,reviewed_at,review_note)
            VALUES($1,$2,repeat('a',64),'mock','m','p',1,'window','application_window','{}','{}','HIGH','approved',now(),'test approval note') RETURNING id")
            .bind(cache).bind(id).fetch_one(&pool).await.unwrap();
        sqlx::query("INSERT INTO program_ai_field_overrides(program_id,field,timestamp_value,raw_payload_md5,candidate_id)
            VALUES($1,'application_end_at',now()+interval '30 days',(SELECT md5(raw_payload::text) FROM programs WHERE id=$1),$2)")
            .bind(id).bind(candidate).execute(&pool).await.unwrap();
        let run = Uuid::new_v4();
        sqlx::query("INSERT INTO ingestion_runs(id,source_name,status,started_at,run_kind) VALUES($1,'gov_benefits','running',now(),'renormalization')")
            .bind(run).execute(&pool).await.unwrap();

        process_record(
            &pool,
            "gov_benefits",
            run,
            &record,
            &DedupCaches::default(),
            &HashSet::new(),
            RecordOrigin::StoredSnapshot,
        )
        .await
        .unwrap();
        let (status, end_in_future): (String, bool) = sqlx::query_as(
            "SELECT program_status, application_end_at > now() FROM programs WHERE id=$1",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!((status.as_str(), end_in_future), ("active", true));

        sqlx::query("DELETE FROM ingestion_items WHERE run_id=$1")
            .bind(run)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM ingestion_runs WHERE id=$1")
            .bind(run)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM programs WHERE id=$1")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "DELETE FROM source_snapshots WHERE source_name='gov_benefits' AND source_id=$1",
        )
        .bind(&source_id)
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;
    }

    /// A dated 신청기한 that has passed is closed by the daily expiry job,
    /// and the next round in the source reopens it. 상시 stays undated.
    #[tokio::test]
    async fn gov_benefits_stated_window_feeds_the_expiry_job() {
        let pool = test_pool().await;
        let mut tx = pool.begin().await.unwrap();
        let source = format!("lifecycle_{}", Uuid::new_v4().simple());
        let gov = |period: &str| {
            json!({
                "서비스ID": "fixture", "서비스명": "청년 월세 지원", "소관기관명": "서울특별시",
                "서비스분야": "주거·자립", "지원대상": "만 19~39세 무주택 청년",
                "신청기한": period, "상세조회URL": "https://www.gov.kr/portal/rcvfvrSvc/dtlEx/fixture"
            })
        };
        sync_as(
            &mut tx,
            "gov_benefits",
            &source,
            "past",
            gov("2025.6.11.(수) 10:00 ~ 6. 24.(화)18:00"),
        )
        .await;
        sync_as(&mut tx, "gov_benefits", &source, "always", gov("상시신청")).await;
        let (active, status, ..) = state(&mut tx, &source, "past").await;
        assert_eq!((active, status.as_str()), (true, "closed"));
        assert!(!available(&mut tx, &source, "past").await);
        assert!(available(&mut tx, &source, "always").await);

        let _: i64 = sqlx::query_scalar("SELECT expire_all_programs()")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        let (active, status, reason, retired, _) = state(&mut tx, &source, "past").await;
        assert_eq!(
            (active, status.as_str(), reason.as_deref(), retired),
            (false, "closed", Some("expired"), true)
        );
        let (active, status, reason, ..) = state(&mut tx, &source, "always").await;
        assert_eq!((active, status.as_str(), reason), (true, "active", None));

        // The source announces the next round: the expiry is undone.
        sync_as(
            &mut tx,
            "gov_benefits",
            &source,
            "past",
            gov("2099. 5. 6. ~ 2099. 5. 19."),
        )
        .await;
        let (active, status, reason, ..) = state(&mut tx, &source, "past").await;
        assert_eq!((active, status.as_str(), reason), (true, "upcoming", None));
        tx.rollback().await.unwrap();
        pool.close().await;
    }

    #[test]
    fn gov_benefits_windows_cover_whole_korean_days() {
        let raw = json!({"서비스명": "청년 지원", "신청기한": "2026. 3. 9.(월) 09:00 ~ 3. 23.(월) 18:00"});
        let norm = normalize("gov_benefits", "x", &raw, &HashSet::new());
        let day = |d| NaiveDate::from_ymd_opt(2026, 3, d).unwrap();
        assert_eq!(norm.application_start_at, Some(kst_start_of_day(day(9))));
        assert_eq!(norm.application_end_at, Some(kst_end_of_day(day(23))));
        let raw = json!({"서비스명": "청년 지원", "신청기한": "2026. 2. 6.까지"});
        let norm = normalize("gov_benefits", "x", &raw, &HashSet::new());
        assert_eq!(norm.application_start_at, None);
        assert_eq!(
            norm.application_end_at,
            Some(kst_end_of_day(NaiveDate::from_ymd_opt(2026, 2, 6).unwrap()))
        );
        for undated in [
            json!({"서비스명": "a", "신청기한": "상시신청"}),
            json!({"서비스명": "a"}),
            json!({"서비스명": "a", "신청기한": 3}),
        ] {
            let norm = normalize("gov_benefits", "x", &undated, &HashSet::new());
            assert_eq!(
                (norm.application_start_at, norm.application_end_at),
                (None, None)
            );
            assert_eq!(program_status_for(&norm), "active");
        }
    }

    #[test]
    fn youth_center_intake_codes_map_to_statuses() {
        let status = |code: &str, aply: &str| {
            let norm = normalize(
                "youth_center",
                "x",
                &youth_payload(code, aply),
                &HashSet::new(),
            );
            (norm.source_status, program_status_for(&norm))
        };
        assert_eq!(status("0057003", ""), (Some("closed"), "closed"));
        assert_eq!(status("0057002", ""), (Some("always_open"), "always_open"));
        // Dates decide for 특정기간, a blank code and an unknown code.
        assert_eq!(status("0057001", "20260312 ~ 20260319"), (None, "closed"));
        assert_eq!(status("", "20260312 ~ 20991231"), (None, "active"));
        assert_eq!(status("0057009", ""), (None, "active"));
        // A missing field behaves like a blank code.
        let mut raw = youth_payload("", "");
        raw.as_object_mut().unwrap().remove("aplyPrdSeCd");
        assert_eq!(
            normalize("youth_center", "x", &raw, &HashSet::new()).source_status,
            None
        );
    }

    #[tokio::test]
    async fn stale_running_runs_are_failed_after_one_hour() {
        let pool = test_pool().await;
        let mut tx = pool.begin().await.unwrap();
        let source = format!("lifecycle_{}", Uuid::new_v4().simple());
        let stale = Uuid::new_v4();
        let recent = Uuid::new_v4();
        for (id, age) in [(stale, "2 hours"), (recent, "10 minutes")] {
            sqlx::query(&format!(
                "INSERT INTO ingestion_runs(id,source_name,status,started_at,run_kind) \
                 VALUES($1,$2,'running',now()-interval '{age}','ingestion')"
            ))
            .bind(id)
            .bind(&source)
            .execute(&mut *tx)
            .await
            .unwrap();
        }
        assert!(fail_stale_ingestion_runs(&mut *tx).await.unwrap() >= 1);
        let rows: Vec<(Uuid, String, bool)> = sqlx::query_as(
            "SELECT id,status,finished_at IS NOT NULL FROM ingestion_runs WHERE source_name=$1",
        )
        .bind(&source)
        .fetch_all(&mut *tx)
        .await
        .unwrap();
        assert!(rows.contains(&(stale, "failed".into(), true)));
        assert!(rows.contains(&(recent, "running".into(), false)));
        tx.rollback().await.unwrap();
        pool.close().await;
    }

    /// Review M5: only a 보조금24 row users can see (the `current_programs`
    /// filters) may retire the 복지로 copy of the same service.
    #[tokio::test]
    async fn duplicate_cache_holds_only_visible_gov_rows() {
        let pool = test_pool().await;
        let marker = format!("중복검증{}", &Uuid::new_v4().simple().to_string()[..8]);
        let mut ids = Vec::new();
        for (tag, status, deadline_days) in [
            ("open", "active", 10),
            ("closed", "closed", 10),
            ("review", "review_required", 10),
            ("expired", "active", -1),
        ] {
            let id: Uuid = sqlx::query_scalar(
                "INSERT INTO programs(program_type,source_type,source_id,title,provider_name,\
                 program_status,regions,deadline_at,is_active) \
                 VALUES('benefit','gov_benefits',$1,$2,'보건복지부',$3,ARRAY['busan'],\
                 now() + make_interval(days => $4),true) RETURNING id",
            )
            .bind(format!("{marker}-{tag}"))
            .bind(format!("{marker} 지원"))
            .bind(status)
            .bind(deadline_days)
            .fetch_one(&pool)
            .await
            .unwrap();
            ids.push(id);
        }
        let cache = load_gov_benefits_cache(&pool).await.unwrap();
        let cached: Vec<Uuid> = cache
            .iter()
            .filter(|e| ids.contains(&e.id))
            .map(|e| e.id)
            .collect();
        sqlx::query("DELETE FROM programs WHERE id = ANY($1)")
            .bind(&ids)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(cached, vec![ids[0]], "only the open row");
        let regions = vec!["busan".to_string()];
        assert_eq!(
            find_gov_benefits_duplicate(&format!("{marker} 지원"), "보건복지부", &regions, &cache)
                .map(|e| e.id),
            Some(ids[0])
        );
        pool.close().await;
    }

    async fn rule_versions(
        pool: &PgPool,
        id: Uuid,
    ) -> Vec<(i32, Option<String>, serde_json::Value)> {
        sqlx::query_as(
            "SELECT version, created_by, rule_json FROM eligibility_rules \
             WHERE program_id=$1 ORDER BY version",
        )
        .bind(id)
        .fetch_all(pool)
        .await
        .unwrap()
    }

    /// Source-code conditions get their own rule version, are not rewritten
    /// when unchanged, sit beside another author's conditions, and withdraw
    /// cleanly when the source drops them.
    #[tokio::test]
    async fn source_code_rules_version_only_their_own_conditions() {
        let pool = test_pool().await;
        let insert_program = |tag: &'static str| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, Uuid>(
                    "INSERT INTO programs(program_type,source_type,source_id,title,program_status,is_active) \
                     VALUES('employment','youth_center',$1,'코드 규칙','active',true) RETURNING id",
                )
                .bind(format!("{tag}-{}", Uuid::new_v4().simple()))
                .fetch_one(&pool)
                .await
                .unwrap()
            }
        };
        let unemployed = crate::eligibility_codes::youth_rule_conditions(
            &crate::eligibility_codes::youth_conditions(&json!({"jobCd": "0013003"})),
        );
        assert_eq!(unemployed.len(), 1);

        // Alone: one version, idempotent, deleted on withdrawal.
        let alone = insert_program("source-rule-alone").await;
        sync_source_rule(&pool, alone, &unemployed).await.unwrap();
        sync_source_rule(&pool, alone, &unemployed).await.unwrap();
        let versions = rule_versions(&pool, alone).await;
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].1.as_deref(), Some(SOURCE_RULE_AUTHOR));
        assert_eq!(versions[0].2, json!({"all": unemployed}));
        sync_source_rule(&pool, alone, &[]).await.unwrap();
        assert!(rule_versions(&pool, alone).await.is_empty());

        // Beside a reviewed condition: appended, then withdrawn, never lost.
        let reviewed = json!({"field": "age", "op": "gte", "value": 19});
        let shared = insert_program("source-rule-shared").await;
        sqlx::query(
            "INSERT INTO eligibility_rules(program_id,rule_json,hard_filter_json,version,created_by) \
             VALUES($1,$2,$2,1,'admin')",
        )
        .bind(shared)
        .bind(json!({"all": [reviewed.clone()]}))
        .execute(&pool)
        .await
        .unwrap();
        sync_source_rule(&pool, shared, &unemployed).await.unwrap();
        let versions = rule_versions(&pool, shared).await;
        assert_eq!(versions.len(), 2);
        assert_eq!(
            versions[1].2,
            json!({"all": [reviewed.clone(), unemployed[0].clone()]})
        );
        sync_source_rule(&pool, shared, &[]).await.unwrap();
        let versions = rule_versions(&pool, shared).await;
        assert_eq!(versions.len(), 3);
        assert_eq!(versions[2].2, json!({"all": [reviewed]}));

        sqlx::query("DELETE FROM programs WHERE id = ANY($1)")
            .bind(vec![alone, shared])
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
    }

    /// A stored-snapshot replay of a v4 local_welfare row writes its districts.
    #[tokio::test]
    async fn stored_local_welfare_replay_writes_its_districts() {
        let pool = test_pool().await;
        let tag = Uuid::new_v4().simple().to_string();
        let run = Uuid::new_v4();
        sqlx::query("INSERT INTO ingestion_runs(id,source_name,status,started_at,run_kind) VALUES($1,'local_welfare','running',now(),'renormalization')")
            .bind(run).execute(&pool).await.unwrap();
        let record = |id: &str, sgg: &str| {
            let payload = json!({
                "servId": id, "servNm": format!("밀양 청년 {tag} 지원"),
                "ctpvNm": "경상남도", "sggNm": sgg, "lifeNmArray": "청년",
            });
            RawRecord {
                source_id: format!("local_welfare_{id}"),
                content_hash: crate::sources::content_hash(&payload),
                payload,
            }
        };
        let district = record(&format!("lw-d-{tag}"), "밀양시");
        let sido = record(&format!("lw-s-{tag}"), "");
        let ids = vec![district.source_id.clone(), sido.source_id.clone()];
        for r in [&district, &sido] {
            process_record(
                &pool,
                "local_welfare",
                run,
                r,
                &DedupCaches::default(),
                &HashSet::new(),
                RecordOrigin::FreshFetch,
            )
            .await
            .unwrap();
        }
        // The rows as v4 stored them: no districts.
        sqlx::query("UPDATE programs SET region_districts=NULL, normalizer_version=4 WHERE source_type='local_welfare' AND source_id=ANY($1)")
            .bind(&ids).execute(&pool).await.unwrap();
        sqlx::query("UPDATE source_snapshots SET normalizer_version=4 WHERE source_name='local_welfare' AND source_id=ANY($1)")
            .bind(&ids).execute(&pool).await.unwrap();

        for r in [&district, &sido] {
            let outcome = process_record(
                &pool,
                "local_welfare",
                run,
                r,
                &DedupCaches::default(),
                &HashSet::new(),
                RecordOrigin::StoredSnapshot,
            )
            .await
            .unwrap();
            assert_eq!(outcome, ProcessOutcome::Renormalized);
        }
        let row = |id: String| {
            let pool = pool.clone();
            async move {
                sqlx::query_as::<_, (Vec<String>, Option<Vec<String>>, i32, i32)>(
                    "SELECT p.regions, p.region_districts, p.normalizer_version, s.normalizer_version \
                     FROM programs p JOIN source_snapshots s \
                       ON s.source_name=p.source_type AND s.source_id=p.source_id \
                     WHERE p.source_type='local_welfare' AND p.source_id=$1",
                )
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap()
            }
        };
        assert_eq!(
            row(district.source_id.clone()).await,
            (vec!["gyeongnam".into()], Some(vec!["48270".into()]), 5, 5)
        );
        assert_eq!(
            row(sido.source_id.clone()).await,
            (vec!["gyeongnam".into()], None, 5, 5)
        );

        sqlx::query("DELETE FROM ingestion_items WHERE run_id=$1")
            .bind(run)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM ingestion_runs WHERE id=$1")
            .bind(run)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM programs WHERE source_type='local_welfare' AND source_id=ANY($1)")
            .bind(&ids)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "DELETE FROM source_snapshots WHERE source_name='local_welfare' AND source_id=ANY($1)",
        )
        .bind(&ids)
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;
    }

    /// A 복지로 copy of an active 보조금24 service under the same ministry is
    /// retired as a duplicate, and returns once the 보조금24 row is gone.
    #[tokio::test]
    async fn welfare_copy_of_a_gov_benefits_service_is_retired_and_restored() {
        let pool = test_pool().await;
        let tag = Uuid::new_v4().simple().to_string();
        let gov_id: Uuid = sqlx::query_scalar(
            "INSERT INTO programs(program_type,source_type,source_id,title,provider_name,regions,program_status,is_active) \
             VALUES('housing','gov_benefits',$1,$2,'국토교통부','{}','active',true) RETURNING id",
        )
        .bind(format!("dup-gov-{tag}"))
        .bind(format!("청년월세 {tag} 지원"))
        .fetch_one(&pool)
        .await
        .unwrap();
        let run = Uuid::new_v4();
        sqlx::query("INSERT INTO ingestion_runs(id,source_name,status,started_at,run_kind) VALUES($1,'national_welfare','running',now(),'ingestion')")
            .bind(run).execute(&pool).await.unwrap();
        let source_id = format!("dup-nw-{tag}");
        let record = |summary: &str| {
            let payload = json!({
                "servId": source_id, "servNm": format!("청년월세 {tag} 지원사업"),
                "jurMnofNm": "국토교통부", "servDgst": summary,
                "servDtlLink": "https://www.bokjiro.go.kr/ssis-tbu/twataa/wlfareInfo/moveTWAT52011M.do?wlfareInfoId=WLF00000001",
            });
            RawRecord {
                source_id: source_id.clone(),
                content_hash: crate::sources::content_hash(&payload),
                payload,
            }
        };
        let state = || async {
            sqlx::query_as::<_, (bool, Option<String>, Option<String>)>(
                "SELECT is_active, retirement_reason, normalized_payload->>'duplicate_of' \
                 FROM programs WHERE source_type='national_welfare' AND source_id=$1",
            )
            .bind(&source_id)
            .fetch_one(&pool)
            .await
            .unwrap()
        };

        process_record(
            &pool,
            "national_welfare",
            run,
            &record("v1"),
            &DedupCaches::default(),
            &HashSet::new(),
            RecordOrigin::FreshFetch,
        )
        .await
        .unwrap();
        assert_eq!(state().await, (true, None, None));

        let caches = load_dedup_caches(&pool, "national_welfare").await;
        process_record(
            &pool,
            "national_welfare",
            run,
            &record("v2"),
            &caches,
            &HashSet::new(),
            RecordOrigin::FreshFetch,
        )
        .await
        .unwrap();
        assert_eq!(
            state().await,
            (false, Some("duplicate".into()), Some(gov_id.to_string()))
        );

        sqlx::query("UPDATE programs SET is_active=false WHERE id=$1")
            .bind(gov_id)
            .execute(&pool)
            .await
            .unwrap();
        let caches = load_dedup_caches(&pool, "national_welfare").await;
        process_record(
            &pool,
            "national_welfare",
            run,
            &record("v3"),
            &caches,
            &HashSet::new(),
            RecordOrigin::FreshFetch,
        )
        .await
        .unwrap();
        assert_eq!(state().await, (true, None, None));

        sqlx::query("DELETE FROM ingestion_items WHERE run_id=$1")
            .bind(run)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM ingestion_runs WHERE id=$1")
            .bind(run)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM programs WHERE source_id = ANY($1)")
            .bind(vec![format!("dup-gov-{tag}"), source_id.clone()])
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "DELETE FROM source_snapshots WHERE source_name='national_welfare' AND source_id=$1",
        )
        .bind(&source_id)
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;
    }

    // ── Duplicate lifecycle: the duplicate state is re-derived on every run ──

    /// One 복지로 record plus the 온통청년/보조금24 rows it may duplicate,
    /// tagged so other tests' rows never match.
    struct DupFixture {
        pool: PgPool,
        tag: String,
        run: Uuid,
        source: &'static str,
        source_id: String,
        targets: Vec<Uuid>,
    }

    type DupState = Option<(bool, Option<String>, Option<String>)>;

    const LIVE: DupState = Some((true, None, None));

    fn retired_as(target: Uuid) -> DupState {
        Some((false, Some("duplicate".into()), Some(target.to_string())))
    }

    impl DupFixture {
        async fn new(source: &'static str) -> Self {
            let pool = test_pool().await;
            // Letters only: a title key drops 4-digit years.
            let tag: String = Uuid::new_v4()
                .simple()
                .to_string()
                .chars()
                .filter(|c| c.is_ascii_alphabetic())
                .take(8)
                .collect();
            let run = Uuid::new_v4();
            sqlx::query("INSERT INTO ingestion_runs(id,source_name,status,started_at,run_kind) VALUES($1,$2,'running',now(),'renormalization')")
                .bind(run).bind(source).execute(&pool).await.unwrap();
            Self {
                pool,
                tag,
                run,
                source,
                source_id: format!("{source}_dup_{}", Uuid::new_v4().simple()),
                targets: Vec::new(),
            }
        }

        fn title(&self) -> String {
            format!("청년{}자립지원", self.tag)
        }

        /// The same national programme on 온통청년: same title, the
        /// record's ministry as 주관기관, nationwide.
        async fn youth_target(&mut self) -> Uuid {
            let id: Uuid = sqlx::query_scalar(
                "INSERT INTO programs(program_type,source_type,source_id,title,provider_name,official_url,regions,program_status,is_active) \
                 VALUES('welfare','youth_center',$1,$2,'보건복지부','https://www.youthcenter.go.kr/','{}','always_open',true) RETURNING id",
            )
            .bind(format!("dup-yc-{}", Uuid::new_v4().simple()))
            .bind(self.title())
            .fetch_one(&self.pool)
            .await
            .unwrap();
            self.targets.push(id);
            id
        }

        /// The same service under the same jurisdiction: the ministry for
        /// national_welfare, 시도 for a 시도-wide local_welfare record.
        async fn gov_target(&mut self) -> Uuid {
            let (provider, regions): (&str, &[&str]) = match self.source {
                "local_welfare" => ("서울특별시", &["seoul"]),
                _ => ("보건복지부", &[]),
            };
            let id: Uuid = sqlx::query_scalar(
                "INSERT INTO programs(program_type,source_type,source_id,title,provider_name,regions,program_status,is_active) \
                 VALUES('welfare','gov_benefits',$1,$2,$3,$4,'active',true) RETURNING id",
            )
            .bind(format!("dup-gov-{}", Uuid::new_v4().simple()))
            .bind(format!("{} 사업", self.title()))
            .bind(provider)
            .bind(regions)
            .fetch_one(&self.pool)
            .await
            .unwrap();
            self.targets.push(id);
            id
        }

        fn record(&self, summary: &str) -> RawRecord {
            let payload = match self.source {
                "local_welfare" => json!({
                    "servId": self.source_id, "servNm": self.title(), "ctpvNm": "서울특별시",
                    "sggNm": "", "servDgst": summary, "lifeNmArray": "청년",
                }),
                _ => json!({
                    "servId": self.source_id, "servNm": self.title(),
                    "jurMnofNm": "보건복지부", "servDgst": summary,
                    "servDtlLink": "https://www.bokjiro.go.kr/ssis-tbu/twataa/wlfareInfo/moveTWAT52011M.do?wlfareInfoId=WLF00000002",
                }),
            };
            RawRecord {
                source_id: self.source_id.clone(),
                content_hash: crate::sources::content_hash(&payload),
                payload,
            }
        }

        /// One record of a scheduled run, with that run's caches.
        async fn sync(&self, summary: &str) {
            let caches = load_dedup_caches(&self.pool, self.source).await;
            self.sync_with(summary, &caches).await;
        }

        async fn sync_with(&self, summary: &str, caches: &DedupCaches) {
            process_record(
                &self.pool,
                self.source,
                self.run,
                &self.record(summary),
                caches,
                &HashSet::new(),
                RecordOrigin::FreshFetch,
            )
            .await
            .unwrap();
        }

        /// (is_active, retirement_reason, duplicate_of); None: no row.
        async fn state(&self) -> DupState {
            sqlx::query_as(
                "SELECT is_active, retirement_reason, normalized_payload->>'duplicate_of' \
                 FROM programs WHERE source_type=$1 AND source_id=$2",
            )
            .bind(self.source)
            .bind(&self.source_id)
            .fetch_optional(&self.pool)
            .await
            .unwrap()
        }

        /// Runs `sql` against this record's row ($1 source, $2 source_id).
        async fn update_row(&self, sql: &str) {
            sqlx::query(sql)
                .bind(self.source)
                .bind(&self.source_id)
                .execute(&self.pool)
                .await
                .unwrap();
        }

        async fn set_target_status(&self, target: Uuid, status: &str) {
            sqlx::query("UPDATE programs SET program_status=$2 WHERE id=$1")
                .bind(target)
                .bind(status)
                .execute(&self.pool)
                .await
                .unwrap();
        }

        async fn row_text(&self, id: Uuid, column: &str) -> Option<String> {
            sqlx::query_scalar(&format!("SELECT {column}::text FROM programs WHERE id=$1"))
                .bind(id)
                .fetch_one(&self.pool)
                .await
                .unwrap()
        }

        async fn own_id(&self) -> Uuid {
            sqlx::query_scalar("SELECT id FROM programs WHERE source_type=$1 AND source_id=$2")
                .bind(self.source)
                .bind(&self.source_id)
                .fetch_one(&self.pool)
                .await
                .unwrap()
        }

        async fn cleanup(self) {
            let pool = &self.pool;
            sqlx::query("DELETE FROM ingestion_items WHERE run_id=$1")
                .bind(self.run)
                .execute(pool)
                .await
                .unwrap();
            sqlx::query("DELETE FROM ingestion_runs WHERE id=$1")
                .bind(self.run)
                .execute(pool)
                .await
                .unwrap();
            sqlx::query(
                "DELETE FROM programs WHERE (source_type=$1 AND source_id=$2) OR id=ANY($3)",
            )
            .bind(self.source)
            .bind(&self.source_id)
            .bind(&self.targets)
            .execute(pool)
            .await
            .unwrap();
            sqlx::query("DELETE FROM source_snapshots WHERE source_name=$1 AND source_id=$2")
                .bind(self.source)
                .bind(&self.source_id)
                .execute(pool)
                .await
                .unwrap();
            pool.close().await;
        }
    }

    /// HIGH 3/4: a 복지로 row that already exists when its 온통청년 copy
    /// appears is retired, not left live beside it, even with an unchanged
    /// payload; it returns when the 온통청년 intake closes (0057003 keeps
    /// `is_active` but leaves `current_programs`).
    #[tokio::test]
    async fn welfare_row_duplicating_youth_center_is_retired_and_restored_every_run() {
        let mut fx = DupFixture::new("national_welfare").await;
        fx.sync_with("v1", &DedupCaches::default()).await;
        assert_eq!(fx.state().await, LIVE);

        let youth = fx.youth_target().await;
        fx.sync("v1").await;
        assert_eq!(fx.state().await, retired_as(youth), "unchanged payload");
        let url = fx.row_text(youth, "official_url").await.unwrap();
        assert!(url.contains("bokjiro.go.kr"), "{url}");

        // Nothing changed: nothing is written.
        let own = fx.own_id().await;
        let before = fx.row_text(own, "updated_at").await;
        fx.sync("v1").await;
        assert_eq!(fx.state().await, retired_as(youth));
        assert_eq!(fx.row_text(own, "updated_at").await, before, "idempotent");

        // A content change refreshes the hidden row and keeps it retired.
        fx.sync("v2").await;
        assert_eq!(fx.state().await, retired_as(youth));
        assert_eq!(fx.row_text(own, "summary").await.as_deref(), Some("v2"));

        fx.set_target_status(youth, "closed").await;
        fx.sync("v2").await;
        assert_eq!(fx.state().await, LIVE, "target closed: the copy returns");
        fx.cleanup().await;
    }

    /// A run whose caches failed (`recheck` false) still upserts a changed
    /// record; the upsert must keep `duplicate_of`, or the hidden row can
    /// never be restored once its target closes.
    #[tokio::test]
    async fn content_change_during_a_cache_failure_keeps_the_duplicate_tag() {
        let mut fx = DupFixture::new("national_welfare").await;
        let youth = fx.youth_target().await;
        fx.sync("v1").await;
        assert_eq!(fx.state().await, retired_as(youth));

        fx.sync_with("v2", &DedupCaches::default()).await;
        assert_eq!(fx.state().await, retired_as(youth), "cache failure");

        fx.set_target_status(youth, "closed").await;
        fx.sync("v3").await;
        assert_eq!(fx.state().await, LIVE, "target closed: the copy returns");
        fx.cleanup().await;
    }

    /// Production state: the youth branch never created the 복지로 row and
    /// marked its snapshot current, so later runs returned before the check.
    #[tokio::test]
    async fn never_created_youth_duplicate_appears_once_the_target_closes() {
        let mut fx = DupFixture::new("national_welfare").await;
        let youth = fx.youth_target().await;
        let record = fx.record("v1");
        sqlx::query("INSERT INTO source_snapshots(id,source_name,source_id,content_hash,raw_payload,normalizer_version,normalized_at) VALUES($1,$2,$3,$4,$5,$6,now())")
            .bind(Uuid::new_v4()).bind(fx.source).bind(&fx.source_id).bind(&record.content_hash)
            .bind(&record.payload).bind(normalizer_version(fx.source)).execute(&fx.pool).await.unwrap();

        fx.sync("v1").await;
        assert_eq!(fx.state().await, retired_as(youth), "stored, hidden");
        fx.set_target_status(youth, "closed").await;
        fx.sync("v1").await;
        assert_eq!(fx.state().await, LIVE);
        fx.cleanup().await;
    }

    /// HIGH 5: the 보조금24 row closing brings the unchanged 복지로 copy back;
    /// a copy whose own window ended while hidden comes back as expired.
    #[tokio::test]
    async fn welfare_copy_returns_when_the_gov_benefits_row_closes() {
        for source in ["national_welfare", "local_welfare"] {
            let mut fx = DupFixture::new(source).await;
            fx.sync_with("v1", &DedupCaches::default()).await;
            assert_eq!(fx.state().await, LIVE, "{source}");
            let gov = fx.gov_target().await;
            fx.sync("v1").await;
            assert_eq!(fx.state().await, retired_as(gov), "{source}");
            fx.set_target_status(gov, "closed").await;
            fx.sync("v1").await;
            assert_eq!(fx.state().await, LIVE, "{source}: target closed");

            fx.set_target_status(gov, "active").await;
            fx.sync("v1").await;
            assert_eq!(fx.state().await, retired_as(gov), "{source}");
            fx.update_row("UPDATE programs SET application_end_at=now()-interval '1 day' WHERE source_type=$1 AND source_id=$2").await;
            fx.set_target_status(gov, "review_required").await;
            fx.sync("v1").await;
            assert_eq!(
                fx.state().await,
                Some((false, Some("expired".into()), None)),
                "{source}: ended while hidden"
            );
            fx.cleanup().await;
        }
    }

    /// MEDIUM: an automatically closed copy is tagged too, so the
    /// source_removed restore cannot reopen it beside the 보조금24 row.
    /// Operator holds are never touched.
    #[tokio::test]
    async fn closed_welfare_copy_is_tagged_and_not_reopened_beside_its_target() {
        let mut fx = DupFixture::new("national_welfare").await;
        fx.sync_with("v1", &DedupCaches::default()).await;
        fx.update_row("UPDATE programs SET is_active=false, program_status='closed', retirement_reason='source_removed', retired_at=now() WHERE source_type=$1 AND source_id=$2").await;
        let gov = fx.gov_target().await;
        fx.sync("v1").await;
        assert_eq!(fx.state().await, retired_as(gov));

        // The source keeps listing it: the source_removed restore skips it.
        let mut tx = fx.pool.begin().await.unwrap();
        insert_run(&mut tx, fx.source, "success", 1).await;
        let run = insert_run(&mut tx, fx.source, "success", 1).await;
        let summary = reconcile_source_lifecycle_tx(
            &mut tx,
            fx.source,
            run,
            true,
            1,
            std::slice::from_ref(&fx.source_id),
            &[],
        )
        .await
        .unwrap();
        assert_eq!(summary.restored, 0);
        tx.rollback().await.unwrap();

        fx.update_row("UPDATE programs SET is_active=false, retirement_reason='manual', normalized_payload=normalized_payload-'duplicate_of' WHERE source_type=$1 AND source_id=$2").await;
        fx.sync("v1").await;
        assert_eq!(fx.state().await, Some((false, Some("manual".into()), None)));
        fx.set_target_status(gov, "closed").await;
        fx.sync("v1").await;
        assert_eq!(fx.state().await, Some((false, Some("manual".into()), None)));
        fx.cleanup().await;
    }

    /// MEDIUM: the 복지로 deep link written into the 온통청년 row is written
    /// again by the next 복지로 run after an 온통청년 upsert replaced it.
    #[tokio::test]
    async fn bokjiro_link_is_reapplied_after_a_youth_center_upsert() {
        let mut fx = DupFixture::new("national_welfare").await;
        let youth = fx.youth_target().await;
        fx.sync("v1").await;
        sqlx::query(
            "UPDATE programs SET official_url='https://www.youthcenter.go.kr/' WHERE id=$1",
        )
        .bind(youth)
        .execute(&fx.pool)
        .await
        .unwrap();
        fx.sync("v1").await;
        let url = fx.row_text(youth, "official_url").await.unwrap();
        assert!(url.contains("bokjiro.go.kr"), "{url}");
        fx.cleanup().await;
    }
}

/// Process a single record and distinguish upstream content changes from a
/// deterministic replay caused only by a normalizer-version bump.
async fn process_record(
    pool: &PgPool,
    source_name: &str,
    run_id: Uuid,
    record: &RawRecord,
    caches: &DedupCaches,
    shared_youth_center_ref_urls: &HashSet<String>,
    origin: RecordOrigin,
) -> Result<ProcessOutcome> {
    // 2. Compare content hash against last snapshot
    let existing_snapshot: Option<(String, i32)> = sqlx::query_as(
        "SELECT content_hash, normalizer_version FROM source_snapshots \
         WHERE source_name = $1 AND source_id = $2",
    )
    .bind(source_name)
    .bind(&record.source_id)
    .fetch_optional(pool)
    .await?;

    let current_normalizer_version = normalizer_version(source_name);
    // The change hash leaves out the source's view counters. It is derived
    // here, not trusted from the record, so every caller hashes alike.
    let content_hash = crate::sources::record_hash(source_name, &record.payload);
    // A stored hash of an earlier scheme (the whole payload, counters
    // included) differs although nothing but a counter changed: the stored
    // payload, hashed the current way, still matches. Such a snapshot is
    // rehashed in place and keeps its applied normalizer version.
    let equivalent_stored_hash = match &existing_snapshot {
        Some((hash, _)) if hash != &content_hash => {
            let stored: Option<serde_json::Value> = sqlx::query_scalar(
                "SELECT raw_payload FROM source_snapshots \
                 WHERE source_name = $1 AND source_id = $2",
            )
            .bind(source_name)
            .bind(&record.source_id)
            .fetch_optional(pool)
            .await?;
            stored
                .filter(|payload| crate::sources::record_hash(source_name, payload) == content_hash)
                .map(|_| hash.clone())
        }
        _ => None,
    };
    let content_changed = existing_snapshot
        .as_ref()
        .map(|(hash, _)| hash != &content_hash && equivalent_stored_hash.is_none())
        .unwrap_or(true);
    let version_stale = existing_snapshot
        .as_ref()
        .map(|(_, version)| *version < current_normalizer_version)
        .unwrap_or(true);

    // A replay must never overwrite a newer concurrently fetched snapshot.
    // Record it as superseded and leave the newer payload for the next pass.
    if origin == RecordOrigin::StoredSnapshot && content_changed {
        sqlx::query(
            "INSERT INTO ingestion_items \
                 (id, run_id, source_id, status, created_at) \
             VALUES ($1, $2, $3, 'superseded_snapshot', now()) \
             ON CONFLICT (run_id, source_id) DO UPDATE \
                 SET status = 'superseded_snapshot', program_id = NULL",
        )
        .bind(Uuid::new_v4())
        .bind(run_id)
        .bind(&record.source_id)
        .execute(pool)
        .await?;
        return Ok(ProcessOutcome::Unchanged);
    }

    // Fresh upstream data replaces the retained payload. A changed hash resets
    // its applied version to zero until the program upsert succeeds; a
    // rehash of the same content ($6, the stored hash judged equivalent
    // above) keeps it.
    if origin == RecordOrigin::FreshFetch {
        sqlx::query(
            "INSERT INTO source_snapshots ( \
             id, source_name, source_id, content_hash, raw_payload, fetched_at, \
             normalizer_version, normalized_at \
         ) VALUES ($1, $2, $3, $4, $5, now(), 0, NULL) \
         ON CONFLICT (source_name, source_id) DO UPDATE \
             SET content_hash = EXCLUDED.content_hash, \
                 raw_payload   = CASE WHEN EXCLUDED.source_name IN ('gov_benefits', 'national_welfare') \
                      AND jsonb_typeof(EXCLUDED.raw_payload) = 'object' AND NOT (EXCLUDED.raw_payload ? 'detail') \
                      AND jsonb_typeof(source_snapshots.raw_payload) = 'object' AND source_snapshots.raw_payload ? 'detail' \
                 THEN EXCLUDED.raw_payload || jsonb_build_object('detail', source_snapshots.raw_payload->'detail') \
                 ELSE EXCLUDED.raw_payload END, \
                 fetched_at    = EXCLUDED.fetched_at, \
                 normalizer_version = CASE \
                     WHEN source_snapshots.content_hash = EXCLUDED.content_hash \
                       OR source_snapshots.content_hash = $6 \
                     THEN source_snapshots.normalizer_version \
                     ELSE 0 \
                 END, \
                 normalized_at = CASE \
                     WHEN source_snapshots.content_hash = EXCLUDED.content_hash \
                       OR source_snapshots.content_hash = $6 \
                     THEN source_snapshots.normalized_at \
                     ELSE NULL \
                 END",
        )
        .bind(Uuid::new_v4())
        .bind(source_name)
        .bind(&record.source_id)
        .bind(&content_hash)
        .bind(&record.payload)
        .bind(equivalent_stored_hash.as_deref())
        .execute(pool)
        .await?;
    }

    // A welfare record's duplicate state depends on 온통청년/보조금24 rows that
    // change without this record changing, so with complete caches it is
    // re-derived on every run instead of returning early as unchanged.
    let processed = content_changed || version_stale;
    if !processed {
        sync_side_documents(pool, source_name, &record.payload, &record.source_id).await?;
    }
    if !processed && !caches.recheck {
        sqlx::query(
            "INSERT INTO ingestion_items \
                 (id, run_id, source_id, status, created_at) \
             VALUES ($1, $2, $3, 'unchanged', now()) \
             ON CONFLICT (run_id, source_id) DO NOTHING",
        )
        .bind(Uuid::new_v4())
        .bind(run_id)
        .bind(&record.source_id)
        .execute(pool)
        .await?;
        return Ok(ProcessOutcome::Unchanged);
    }

    // 3. Normalize the raw payload
    let mut norm = normalize(
        source_name,
        &record.source_id,
        &record.payload,
        shared_youth_center_ref_urls,
    );
    validate_normalized_program(&mut norm).map_err(|e| RecordRejection(e.to_string()))?;
    if processed && norm.regions.iter().any(|r| r == crate::regions::UNKNOWN) {
        warn!(source = source_name, source_id = %record.source_id,
            "residence scope unresolved; excluded from local filters and recommendations");
    }

    // An unchanged snapshot normally has its row; one the old duplicate
    // branch skipped does not, and is created now.
    let row_missing = !processed && {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM programs WHERE source_type = $1 AND source_id = $2)",
        )
        .bind(source_name)
        .bind(&record.source_id)
        .fetch_one(pool)
        .await?;
        !exists
    };

    // 3b. Cross-source duplicates (see `DedupCaches::find_duplicate`). The
    // welfare row is kept current but retired with `duplicate_of`, so it can
    // return as soon as the row it duplicates leaves `current_programs`.
    if let Some(dup) = caches.find_duplicate(source_name, &record.payload, &norm) {
        if let DuplicateOf::YouthCenter(entry) = dup {
            prefer_bokjiro_link(pool, &norm, entry).await?;
        }
        // Read before the transaction: it must not wait on a second
        // connection while holding one.
        let program_status = if processed || row_missing {
            Some(effective_program_status(pool, source_name, record, &norm).await?)
        } else {
            None
        };
        let mut tx = pool.begin().await?;
        if let Some(program_status) = program_status {
            upsert_program(
                &mut *tx,
                source_name,
                record,
                &norm,
                program_status,
                current_normalizer_version,
                origin,
            )
            .await?;
        }
        retire_as_duplicate(&mut tx, source_name, &record.source_id, dup.id()).await?;
        tx.commit().await?;
        sqlx::query(
            "INSERT INTO ingestion_items \
                 (id, run_id, source_id, program_id, status, created_at) \
             VALUES ($1, $2, $3, $4, $5, now()) \
             ON CONFLICT (run_id, source_id) DO UPDATE \
                 SET status = EXCLUDED.status, program_id = EXCLUDED.program_id",
        )
        .bind(Uuid::new_v4())
        .bind(run_id)
        .bind(&record.source_id)
        .bind(dup.id())
        .bind(dup.item_status())
        .execute(pool)
        .await?;
        mark_snapshot_normalized(
            pool,
            source_name,
            &record.source_id,
            current_normalizer_version,
        )
        .await?;
        return Ok(if content_changed {
            ProcessOutcome::Changed
        } else if processed || row_missing {
            ProcessOutcome::Renormalized
        } else {
            ProcessOutcome::Unchanged
        });
    }

    // No longer a duplicate: undo only this check's own retirement.
    if caches.recheck {
        let restored = restore_from_duplicate(pool, source_name, &record.source_id).await?;
        if !processed && !row_missing {
            sqlx::query(
                "INSERT INTO ingestion_items \
                     (id, run_id, source_id, status, created_at) \
                 VALUES ($1, $2, $3, $4, now()) \
                 ON CONFLICT (run_id, source_id) DO UPDATE SET status = EXCLUDED.status",
            )
            .bind(Uuid::new_v4())
            .bind(run_id)
            .bind(&record.source_id)
            .bind(if restored { "restored" } else { "unchanged" })
            .execute(pool)
            .await?;
            return Ok(ProcessOutcome::Unchanged);
        }
    }

    // 4. Compute program_status, then 5. upsert into programs table.
    let program_status = effective_program_status(pool, source_name, record, &norm).await?;
    let program_id = upsert_program(
        pool,
        source_name,
        record,
        &norm,
        program_status,
        current_normalizer_version,
        origin,
    )
    .await?;

    // 5b. Conditions the source publishes as codes become the program's own
    // rule conditions, replacing only what an earlier sync compiled.
    if matches!(source_name, "youth_center" | "gov_benefits" | "scholarship") {
        sync_source_rule(pool, program_id, &norm.source_rule_conditions).await?;
    }

    // 6. Record success in ingestion_items
    let item_status = if content_changed {
        "upserted"
    } else {
        "renormalized"
    };
    sqlx::query(
        "INSERT INTO ingestion_items \
             (id, run_id, source_id, program_id, status, created_at) \
         VALUES ($1, $2, $3, $4, $5, now()) \
         ON CONFLICT (run_id, source_id) DO UPDATE \
             SET status = EXCLUDED.status, program_id = EXCLUDED.program_id",
    )
    .bind(Uuid::new_v4())
    .bind(run_id)
    .bind(&record.source_id)
    .bind(program_id)
    .bind(item_status)
    .execute(pool)
    .await?;

    mark_snapshot_normalized(
        pool,
        source_name,
        &record.source_id,
        current_normalizer_version,
    )
    .await?;

    Ok(if content_changed {
        ProcessOutcome::Changed
    } else {
        ProcessOutcome::Renormalized
    })
}

/// 보조금24's 구비서류 is side data (the detail row, outside the change hash),
/// so a record whose hash is unchanged can still carry a new or first
/// documents text. It is written to the row's `submission_documents` alone:
/// no renormalization, no `updated_at`, no ingestion change. This is also
/// how rows stored before the detail mapping are filled in. A record
/// without the text keeps the stored one; an identical text writes nothing.
/// `normalized_payload.submission_documents` is not refreshed here; nothing
/// reads it.
async fn sync_side_documents(
    pool: &PgPool,
    source_name: &str,
    payload: &serde_json::Value,
    source_id: &str,
) -> Result<()> {
    if !matches!(source_name, "gov_benefits" | "national_welfare") {
        return Ok(());
    }
    sync_side_detail(pool, source_name, payload, source_id).await?;
    if source_name != "gov_benefits" {
        return Ok(());
    }
    let Some(documents) = crate::sources::gov_benefits::detail_documents(payload) else {
        return Ok(());
    };
    sqlx::query(
        "UPDATE programs SET submission_documents = $3 \
         WHERE source_type = $1 AND source_id = $2 \
           AND submission_documents IS DISTINCT FROM $3",
    )
    .bind(source_name)
    .bind(source_id)
    .bind(documents)
    .execute(pool)
    .await?;
    Ok(())
}

/// A detail row (보조금24 serviceDetail fields, 복지로 중앙 `applmetList`) is
/// side data: an unchanged record still carries a new or first detail,
/// written into the row's `raw_payload.detail` alone
/// (no renormalization; `ingestion_stable_payload` leaves `detail` out, so
/// AI pins stay valid). This is how stored rows are backfilled on the next
/// sync. A record without a detail (the read stopped) keeps the stored one.
async fn sync_side_detail(
    pool: &PgPool,
    source_name: &str,
    payload: &serde_json::Value,
    source_id: &str,
) -> Result<()> {
    // Both sources keep it under the same key ("detail").
    let Some(detail) = payload.get(crate::sources::national_welfare::DETAIL_KEY) else {
        return Ok(());
    };
    sqlx::query(
        "UPDATE programs SET raw_payload = jsonb_set(raw_payload, '{detail}', $3) \
         WHERE source_type = $1 AND source_id = $2 AND jsonb_typeof(raw_payload) = 'object' \
           AND raw_payload->'detail' IS DISTINCT FROM $3",
    )
    .bind(source_name)
    .bind(source_id)
    .bind(detail)
    .execute(pool)
    .await?;
    Ok(())
}

/// The status the row will store: the source's own intake status, else the
/// window the row will actually store. An admin-approved AI date wins over
/// the parsed one while the raw payload is unchanged (see upsert_program),
/// so the status must be derived from that same effective window.
async fn effective_program_status(
    pool: &PgPool,
    source_name: &str,
    record: &RawRecord,
    norm: &NormalizedProgram,
) -> Result<&'static str> {
    if norm.source_status.is_some() {
        return Ok(program_status_for(norm));
    }
    let overrides: Vec<(String, Option<DateTime<Utc>>)> = sqlx::query_as(
        "SELECT o.field, o.timestamp_value FROM program_ai_field_overrides o \
         JOIN programs p ON p.id = o.program_id \
         WHERE p.source_type = $1 AND p.source_id = $2 \
           AND o.field IN ('application_start_at', 'application_end_at') \
           AND o.raw_payload_md5 IN (md5(ingestion_stable_payload($1, $3::jsonb)::text), md5($3::jsonb::text))",
    )
    .bind(source_name)
    .bind(&record.source_id)
    .bind(&record.payload)
    .fetch_all(pool)
    .await?;
    let effective = |field: &str, parsed: Option<DateTime<Utc>>| {
        overrides
            .iter()
            .find(|(f, value)| f == field && value.is_some())
            .and_then(|(_, value)| *value)
            .or(parsed)
    };
    Ok(program_status_with(
        norm,
        effective("application_start_at", norm.application_start_at),
        effective("application_end_at", norm.application_end_at),
    ))
}

/// 복지로's own deep link beats 온통청년's often bare URL. The 온통청년 upsert
/// writes its own URL back; this runs on every 복지로 run, so the link
/// returns on the next one.
async fn prefer_bokjiro_link(
    pool: &PgPool,
    norm: &NormalizedProgram,
    target: &YouthCenterCacheEntry,
) -> Result<()> {
    let target_has_bokjiro = target
        .official_url
        .as_deref()
        .is_some_and(|u| u.contains("bokjiro.go.kr"));
    if norm.official_url.is_none() || target_has_bokjiro {
        return Ok(());
    }
    sqlx::query("UPDATE programs SET official_url = $1, updated_at = now() WHERE id = $2")
        .bind(&norm.official_url)
        .bind(target.id)
        .execute(pool)
        .await?;
    info!(program_id = %target.id, "duplicate of youth_center program — preferred bokjiro deep link");
    Ok(())
}

/// Retire a welfare row as a duplicate of `target`. Live rows and automatic
/// closures (`expired`, `source_removed`, which ingestion may reopen) are
/// tagged; operator holds are never touched. A row already retired for the
/// same target is not rewritten.
async fn retire_as_duplicate(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    source_name: &str,
    source_id: &str,
    target: Uuid,
) -> Result<()> {
    sqlx::query(
        "UPDATE programs SET is_active = false, \
             retired_at = CASE WHEN is_active OR retired_at IS NULL THEN now() ELSE retired_at END, \
             retirement_reason = 'duplicate', \
             normalized_payload = COALESCE(normalized_payload, '{}'::jsonb) \
                 || jsonb_build_object('duplicate_of', $3::text), \
             updated_at = now() \
         WHERE source_type = $1 AND source_id = $2 \
           AND (is_active OR retirement_reason IN ('expired', 'source_removed', 'duplicate')) \
           AND (is_active OR retirement_reason <> 'duplicate' \
                OR normalized_payload->>'duplicate_of' IS DISTINCT FROM $3::text)",
    )
    .bind(source_name)
    .bind(source_id)
    .bind(target.to_string())
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Undo [`retire_as_duplicate`]. A row whose own window ended (or whose
/// status is closed) while it was hidden comes back as `expired`, which a
/// later fresh fetch with a new window reopens like any other expiry.
/// Returns whether the row became active.
async fn restore_from_duplicate(pool: &PgPool, source_name: &str, source_id: &str) -> Result<bool> {
    let restored: Option<bool> = sqlx::query_scalar(
        "WITH r AS ( \
             SELECT id, program_status <> 'closed' \
                 AND (LEAST(benefit_valid_until, deadline_at, application_end_at) IS NULL \
                      OR LEAST(benefit_valid_until, deadline_at, application_end_at) > now()) AS open \
             FROM programs \
             WHERE source_type = $1 AND source_id = $2 AND NOT is_active \
               AND retirement_reason = 'duplicate' AND normalized_payload ? 'duplicate_of' \
         ) \
         UPDATE programs p SET is_active = r.open, \
             program_status = CASE WHEN r.open THEN p.program_status ELSE 'closed' END, \
             retirement_reason = CASE WHEN r.open THEN NULL ELSE 'expired' END, \
             retired_at = CASE WHEN r.open THEN NULL ELSE p.retired_at END, \
             normalized_payload = p.normalized_payload - 'duplicate_of', \
             updated_at = now() \
         FROM r WHERE p.id = r.id \
         RETURNING p.is_active",
    )
    .bind(source_name)
    .bind(source_id)
    .fetch_optional(pool)
    .await?;
    Ok(restored == Some(true))
}

/// Insert or update the catalogue row for one upstream record.
///
/// `search_tsv` is built from title + summary + provider_name with the
/// 'simple' text-search config (no Korean dictionary required); `regions` is
/// cast to TEXT[] explicitly so SQLx knows the target type.
///
/// `is_active` is restored only when a fresh fetch lists a program that was
/// closed automatically (`retirement_reason` expired or source_removed) and
/// the new window is not already closed. Operator holds (manual, or a NULL
/// reason with review_required) are never reopened by ingestion, and a
/// network-free snapshot replay never changes visibility at all.
///
/// `min_age`, `max_age` and the application window keep an admin-approved
/// AI value (`program_ai_field_overrides`) while the incoming raw payload
/// equals the one it was approved against, view counters aside
/// (`ingestion_stable_payload`; a pin written before that key keeps
/// matching the full payload's md5); any other upstream change falls back to
/// the normalizer until a new review.
async fn upsert_program<'e, E: sqlx::PgExecutor<'e>>(
    executor: E,
    source_name: &str,
    record: &RawRecord,
    norm: &NormalizedProgram,
    program_status: &str,
    normalizer_version: i32,
    origin: RecordOrigin,
) -> Result<Uuid> {
    let fresh_fetch = origin == RecordOrigin::FreshFetch;
    let program_id: Uuid = sqlx::query_scalar(
        "INSERT INTO programs ( \
             id, program_type, source_type, source_id, \
             title, summary, provider_name, official_url, \
             program_status, application_start_at, application_end_at, \
             min_age, max_age, regions, \
             company_name, benefit_category, provider_kind, \
             raw_payload, normalized_payload, \
             search_tsv, \
             application_method, submission_documents, screening_method, \
             normalizer_version, normalized_at, \
             region_districts, applicant_types, target_groups, \
             target_household_types, target_gender, income_median_bands, \
             age_basis, \
             is_active, last_synced_at, created_at, updated_at \
         ) VALUES ( \
             $1,  $2,  $3,  $4, \
             $5,  $6,  $7,  $8, \
             $9,  $10, $11, \
             $12, $13, $14::text[], \
             $15, $16, $17, \
             $18, $19, \
             to_tsvector('simple', \
                 coalesce($5, '') || ' ' || \
                 coalesce($6, '') || ' ' || \
                 coalesce($7, '') \
             ), \
             $20, $21, $22, \
             $23, now(), \
             $25::text[], $26::text[], $27::text[], \
             $28::text[], $29, $30::text[], \
             $32, \
             true, now(), now(), now() \
         ) \
         ON CONFLICT (source_type, source_id) DO UPDATE SET \
             program_type          = EXCLUDED.program_type, \
             title                 = EXCLUDED.title, \
             summary               = EXCLUDED.summary, \
             provider_name         = EXCLUDED.provider_name, \
             official_url          = EXCLUDED.official_url, \
             program_status        = EXCLUDED.program_status, \
             application_start_at  = COALESCE((SELECT o.timestamp_value FROM program_ai_field_overrides o WHERE o.program_id = programs.id AND o.field = 'application_start_at' AND o.raw_payload_md5 IN (md5(ingestion_stable_payload(EXCLUDED.source_type, EXCLUDED.raw_payload)::text), md5(EXCLUDED.raw_payload::text))), EXCLUDED.application_start_at), \
             application_end_at    = COALESCE((SELECT o.timestamp_value FROM program_ai_field_overrides o WHERE o.program_id = programs.id AND o.field = 'application_end_at' AND o.raw_payload_md5 IN (md5(ingestion_stable_payload(EXCLUDED.source_type, EXCLUDED.raw_payload)::text), md5(EXCLUDED.raw_payload::text))), EXCLUDED.application_end_at), \
             min_age               = COALESCE((SELECT o.int_value FROM program_ai_field_overrides o WHERE o.program_id = programs.id AND o.field = 'min_age' AND o.raw_payload_md5 IN (md5(ingestion_stable_payload(EXCLUDED.source_type, EXCLUDED.raw_payload)::text), md5(EXCLUDED.raw_payload::text))), EXCLUDED.min_age), \
             max_age               = COALESCE((SELECT o.int_value FROM program_ai_field_overrides o WHERE o.program_id = programs.id AND o.field = 'max_age' AND o.raw_payload_md5 IN (md5(ingestion_stable_payload(EXCLUDED.source_type, EXCLUDED.raw_payload)::text), md5(EXCLUDED.raw_payload::text))), EXCLUDED.max_age), \
             regions               = EXCLUDED.regions, \
             company_name          = EXCLUDED.company_name, \
             benefit_category      = EXCLUDED.benefit_category, \
             provider_kind         = EXCLUDED.provider_kind, \
             raw_payload           = CASE WHEN EXCLUDED.source_type IN ('gov_benefits', 'national_welfare') \
                      AND jsonb_typeof(EXCLUDED.raw_payload) = 'object' AND NOT (EXCLUDED.raw_payload ? 'detail') \
                      AND jsonb_typeof(programs.raw_payload) = 'object' AND programs.raw_payload ? 'detail' \
                 THEN EXCLUDED.raw_payload || jsonb_build_object('detail', programs.raw_payload->'detail') \
                 ELSE EXCLUDED.raw_payload END, \
             normalized_payload    = EXCLUDED.normalized_payload || CASE \
                 WHEN programs.retirement_reason = 'duplicate' AND programs.normalized_payload ? 'duplicate_of' \
                 THEN jsonb_build_object('duplicate_of', programs.normalized_payload->'duplicate_of') \
                 ELSE '{}'::jsonb END, \
             search_tsv            = EXCLUDED.search_tsv, \
             application_method    = EXCLUDED.application_method, \
             submission_documents  = CASE WHEN $31 THEN programs.submission_documents \
                                          ELSE EXCLUDED.submission_documents END, \
             screening_method      = EXCLUDED.screening_method, \
             normalizer_version     = EXCLUDED.normalizer_version, \
             normalized_at          = EXCLUDED.normalized_at, \
             region_districts       = EXCLUDED.region_districts, \
             applicant_types        = EXCLUDED.applicant_types, \
             target_groups          = EXCLUDED.target_groups, \
             target_household_types = EXCLUDED.target_household_types, \
             target_gender          = EXCLUDED.target_gender, \
             income_median_bands    = EXCLUDED.income_median_bands, \
             age_basis              = EXCLUDED.age_basis, \
             is_active = CASE \
                 WHEN $24 AND NOT programs.is_active \
                      AND programs.retirement_reason IN ('expired', 'source_removed') \
                 THEN EXCLUDED.program_status <> 'closed' \
                 ELSE programs.is_active END, \
             retired_at = CASE \
                 WHEN $24 AND NOT programs.is_active \
                      AND programs.retirement_reason IN ('expired', 'source_removed') \
                      AND EXCLUDED.program_status <> 'closed' \
                 THEN NULL ELSE programs.retired_at END, \
             retirement_reason = CASE \
                 WHEN $24 AND NOT programs.is_active \
                      AND programs.retirement_reason IN ('expired', 'source_removed') \
                      AND EXCLUDED.program_status <> 'closed' \
                 THEN NULL ELSE programs.retirement_reason END, \
             source_missing_runs   = CASE WHEN $24 THEN 0 ELSE programs.source_missing_runs END, \
             source_missing_since  = CASE WHEN $24 THEN NULL ELSE programs.source_missing_since END, \
             last_synced_at        = now(), \
             updated_at            = now() \
         RETURNING id",
    )
    .bind(Uuid::new_v4()) // $1  id
    .bind(&norm.program_type) // $2  program_type
    .bind(source_name) // $3  source_type
    .bind(&record.source_id) // $4  source_id
    .bind(&norm.title) // $5  title
    .bind(&norm.summary) // $6  summary
    .bind(&norm.provider_name) // $7  provider_name
    .bind(&norm.official_url) // $8  official_url
    .bind(program_status) // $9  program_status
    .bind(norm.application_start_at) // $10 application_start_at
    .bind(norm.application_end_at) // $11 application_end_at
    .bind(norm.min_age) // $12 min_age
    .bind(norm.max_age) // $13 max_age
    .bind(&norm.regions) // $14 regions  (Vec<String> → text[])
    .bind(&norm.company_name) // $15 company_name
    .bind(&norm.benefit_category) // $16 benefit_category
    .bind(&norm.provider_kind) // $17 provider_kind
    .bind(&record.payload) // $18 raw_payload
    .bind(serde_json::to_value(norm).ok()) // $19 normalized_payload
    .bind(&norm.application_method) // $20 application_method
    .bind(&norm.submission_documents) // $21 submission_documents
    .bind(&norm.screening_method) // $22 screening_method
    .bind(normalizer_version) // $23 normalizer_version
    .bind(fresh_fetch) // $24 restore automatic closures only on a live fetch
    .bind(&norm.region_districts) // $25
    .bind(&norm.applicant_types) // $26
    .bind(&norm.target_groups) // $27
    .bind(&norm.target_household_types) // $28
    .bind(&norm.target_gender) // $29
    .bind(&norm.income_median_bands) // $30
    .bind(norm.keep_stored_submission_documents) // $31
    .bind(&norm.age_basis) // $32
    .fetch_one(executor)
    .await?;
    Ok(program_id)
}

/// A deterministic rejection (contract violation in the upstream payload) of
/// a record that has no programme row can neither hide nor corrupt anything a
/// user sees: there is nothing to update. Such records (for example
/// youth_center `20260422005400212868`, whose application window is
/// inverted) are quarantined: counted in the run, never alerted on, so a real
/// rejection of a published programme or a transient failure stays visible.
/// A lookup error keeps the conservative, visible outcome.
async fn quarantines_rejection(pool: &PgPool, source_name: &str, source_id: &str) -> bool {
    match sqlx::query_scalar::<_, bool>(
        "SELECT NOT EXISTS (SELECT 1 FROM programs WHERE source_type = $1 AND source_id = $2)",
    )
    .bind(source_name)
    .bind(source_id)
    .fetch_one(pool)
    .await
    {
        Ok(quarantine) => quarantine,
        Err(error) => {
            warn!(source = source_name, error = %error, "quarantine lookup failed; counting as rejected");
            false
        }
    }
}

async fn mark_snapshot_normalized(
    pool: &PgPool,
    source_name: &str,
    source_id: &str,
    version: i32,
) -> Result<()> {
    sqlx::query(
        "UPDATE source_snapshots \
         SET normalizer_version = $3, normalized_at = now() \
         WHERE source_name = $1 AND source_id = $2",
    )
    .bind(source_name)
    .bind(source_id)
    .bind(version)
    .execute(pool)
    .await?;
    Ok(())
}

#[derive(Debug, serde::Serialize)]
pub struct SourceRenormalizationSummary {
    pub source_name: String,
    pub normalizer_version: i32,
    pub selected: usize,
    pub renormalized: usize,
    pub failed: usize,
    /// Deterministic rejections of snapshots that never had a programme row.
    pub quarantined: usize,
    pub skipped: usize,
}

#[derive(Debug, Default, serde::Serialize)]
pub struct RenormalizationSummary {
    pub selected: usize,
    pub renormalized: usize,
    pub failed: usize,
    pub quarantined: usize,
    pub skipped: usize,
    pub sources: Vec<SourceRenormalizationSummary>,
}

/// Replay retained raw payloads whose applied normalizer version is stale.
/// No upstream HTTP request is made. A global limit keeps the command bounded;
/// running it again safely continues from the remaining snapshots.
pub async fn renormalize_stored_snapshots(
    pool: &PgPool,
    source_filter: Option<&str>,
    limit: usize,
) -> Result<RenormalizationSummary> {
    anyhow::ensure!(
        (1..=100_000).contains(&limit),
        "renormalization limit must be between 1 and 100000"
    );

    let source_filter = source_filter
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let source_names: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT source_name \
         FROM source_snapshots \
         WHERE ($1::text IS NULL OR source_name = $1) \
         ORDER BY source_name",
    )
    .bind(source_filter)
    .fetch_all(pool)
    .await?;

    let mut summary = RenormalizationSummary::default();
    let mut remaining = limit;

    for source_name in source_names {
        if remaining == 0 {
            break;
        }

        let version = normalizer_version(&source_name);
        let stored: Vec<(String, String, serde_json::Value)> = sqlx::query_as(
            "SELECT source_id, content_hash, raw_payload \
             FROM source_snapshots \
             WHERE source_name = $1 AND normalizer_version < $2 \
             ORDER BY fetched_at, source_id \
             LIMIT $3",
        )
        .bind(&source_name)
        .bind(version)
        .bind(remaining as i64)
        .fetch_all(pool)
        .await?;
        if stored.is_empty() {
            continue;
        }

        let records: Vec<RawRecord> = stored
            .into_iter()
            .map(|(source_id, content_hash, payload)| RawRecord {
                source_id,
                content_hash,
                payload,
            })
            .collect();
        remaining -= records.len();

        let caches = load_dedup_caches(pool, &source_name).await;

        // URL-sharing detection must consider all retained youth records, not
        // only the limited stale page, or a replay could reintroduce a known
        // copy-pasted deep link.
        let shared_youth_center_ref_urls = if source_name == "youth_center" {
            let all_payloads: Vec<(String, String, serde_json::Value)> = sqlx::query_as(
                "SELECT source_id, content_hash, raw_payload \
                 FROM source_snapshots WHERE source_name = 'youth_center'",
            )
            .fetch_all(pool)
            .await?;
            let all_records: Vec<RawRecord> = all_payloads
                .into_iter()
                .map(|(source_id, content_hash, payload)| RawRecord {
                    source_id,
                    content_hash,
                    payload,
                })
                .collect();
            collect_shared_youth_center_ref_urls(&all_records)
        } else {
            HashSet::new()
        };

        let run_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO ingestion_runs ( \
                 id, source_name, status, started_at, normalizer_version, run_kind \
             ) VALUES ($1, $2, 'running', now(), $3, 'renormalization')",
        )
        .bind(run_id)
        .bind(&source_name)
        .bind(version)
        .execute(pool)
        .await?;

        let mut stats = ProcessingStats {
            total: records.len(),
            ..ProcessingStats::default()
        };
        for chunk in records.chunks(4) {
            let results = futures_util::future::join_all(chunk.iter().map(|record| {
                process_record(
                    pool,
                    &source_name,
                    run_id,
                    record,
                    &caches,
                    &shared_youth_center_ref_urls,
                    RecordOrigin::StoredSnapshot,
                )
            }))
            .await;
            for (record, result) in chunk.iter().zip(results) {
                match result {
                    Ok(ProcessOutcome::Renormalized) => stats.renormalized += 1,
                    Ok(ProcessOutcome::Changed) => stats.changed += 1,
                    Ok(ProcessOutcome::Unchanged) => {}
                    Err(error) => {
                        let item_status = if is_rejection(&error)
                            && quarantines_rejection(pool, &source_name, &record.source_id).await
                        {
                            stats.quarantined += 1;
                            "quarantined"
                        } else {
                            stats.failed += 1;
                            "failed"
                        };
                        warn!(
                            source = source_name,
                            source_id = %record.source_id,
                            outcome = item_status,
                            error = %error,
                            "stored snapshot renormalization did not complete"
                        );
                        // The replay incident settles from these rows
                        // (`settle_renormalization_partial`); a lost row is
                        // logged, and the settle check then falls back to
                        // every stale snapshot of the source.
                        if let Err(audit_error) = sqlx::query(
                            "INSERT INTO ingestion_items ( \
                             id, run_id, source_id, status, error_message, created_at \
                         ) VALUES ($1, $2, $3, $4, $5, now()) \
                         ON CONFLICT (run_id, source_id) DO UPDATE \
                             SET status = EXCLUDED.status, error_message = EXCLUDED.error_message",
                        )
                        .bind(Uuid::new_v4())
                        .bind(run_id)
                        .bind(&record.source_id)
                        .bind(item_status)
                        .bind(persisted_error(&error))
                        .execute(pool)
                        .await
                        {
                            warn!(source = source_name, source_id = %record.source_id,
                                error = %audit_error, "failed to record the replay item");
                        }
                    }
                }
            }
        }

        let skipped = stats
            .total
            .saturating_sub(stats.renormalized + stats.changed + stats.failed + stats.quarantined);
        sqlx::query(
            "UPDATE ingestion_runs \
             SET status = CASE WHEN $5 > 0 THEN 'failed' ELSE 'success' END, \
                 error_message = CASE WHEN $5 > 0 THEN 'One or more stored records were rejected' ELSE NULL END, \
                 finished_at = now(), total_fetched = $2, \
                 total_changed = $3, total_renormalized = $4, total_failed = $5, \
                 total_quarantined = $6 \
             WHERE id = $1",
        )
        .bind(run_id)
        .bind(stats.total as i32)
        .bind(stats.changed as i32)
        .bind(stats.renormalized as i32)
        .bind(stats.failed as i32)
        .bind(stats.quarantined as i32)
        .execute(pool)
        .await?;

        let fingerprint = format!("renormalization_partial:{source_name}");
        if stats.failed > 0 {
            let run_id_text = run_id.to_string();
            if let Err(event_error) = operations::record_event(
                pool,
                &EventCommand {
                    fingerprint: &fingerprint,
                    severity: Severity::Warning,
                    component: "normalization",
                    event_type: "stored_snapshot_renormalization_partial",
                    subject_type: Some("ingestion_run"),
                    subject_id: Some(&run_id_text),
                    message: "Stored snapshot re-normalization completed with rejected records",
                    details: serde_json::json!({
                        "source": source_name,
                        "selected": stats.total,
                        "failed": stats.failed
                    }),
                },
            )
            .await
            {
                warn!(source = source_name, error = %event_error, "failed to record renormalization incident");
            }
        } else if let Err(event_error) = operations::resolve_event(
            pool,
            &fingerprint,
            None,
            Some(&format!(
                "stored snapshot re-normalization recovered; {} deterministic upstream \
                 defect(s) without a programme row quarantined",
                stats.quarantined
            )),
        )
        .await
        {
            warn!(source = source_name, error = %event_error, "failed to resolve renormalization incident");
        }

        summary.selected += stats.total;
        summary.renormalized += stats.renormalized;
        summary.failed += stats.failed;
        summary.quarantined += stats.quarantined;
        summary.skipped += skipped;
        summary.sources.push(SourceRenormalizationSummary {
            source_name,
            normalizer_version: version,
            selected: stats.total,
            renormalized: stats.renormalized,
            failed: stats.failed,
            quarantined: stats.quarantined,
            skipped,
        });
    }

    Ok(summary)
}

// ── Normalization ─────────────────────────────────────────────────────────────

#[derive(Debug, Default, serde::Serialize)]
pub struct NormalizedProgram {
    pub program_type: String,
    pub title: String,
    pub summary: Option<String>,
    pub provider_name: Option<String>,
    /// Brand/company shown as the benefit provider when explicitly known.
    pub company_name: Option<String>,
    /// Domain category of the benefit, independent from provider origin.
    pub benefit_category: Option<String>,
    /// Origin classification: corporate, government, municipality, etc.
    pub provider_kind: Option<String>,
    pub official_url: Option<String>,
    pub application_start_at: Option<chrono::DateTime<Utc>>,
    pub application_end_at: Option<chrono::DateTime<Utc>>,
    pub min_age: Option<i32>,
    pub max_age: Option<i32>,
    pub regions: Vec<String>,
    /// Every dated round the source states, when more than one
    /// (`normalized_payload.application_rounds`); the window columns hold
    /// the current one, and the daily step moves them to the next round
    /// when one ends ([`crate::data_maintenance`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub application_rounds: Option<Vec<ApplicationRound>>,
    /// The day the notice counts age on, when it states one
    /// (`programs.age_basis`, [`mazimi_core::models::NoticeAgeBasis`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub age_basis: Option<String>,
    /// 신청 방법 안내 텍스트 (신청 가이드 카드용)
    pub application_method: Option<String>,
    /// 제출 서류 안내 텍스트 (신청 가이드 카드용)
    pub submission_documents: Option<String>,
    /// The record came without its documents field: 보조금24 keeps 구비서류
    /// in the side-data detail row, which a failed or partial serviceDetail
    /// read leaves out. The stored `submission_documents` is kept instead of
    /// being blanked (`raw_payload` is still replaced, so the documents
    /// citation, which needs the text in the stored payload, is not shown).
    #[serde(skip)]
    pub keep_stored_submission_documents: bool,
    /// 심사 방법 안내 텍스트 (신청 가이드 카드용)
    pub screening_method: Option<String>,
    /// Intake status the source itself declares (온통청년 `aplyPrdSeCd`).
    /// When set it replaces the date-derived status on every sync, so it
    /// follows the source in both directions. `None`: the dates decide.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_status: Option<&'static str>,
    /// Current 법정동 시군구 codes the residence scope is limited to
    /// (`programs.region_districts`); see [`crate::regions::Scope`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region_districts: Option<Vec<String>>,
    /// Why the residence scope was resolved as it was, when not from one
    /// plain field ([`crate::regions::Scope::note`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope_note: Option<&'static str>,
    /// Who may apply: individual, household, small_business, organization
    /// (`programs.applicant_types`). `None`: the source does not say.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub applicant_types: Option<Vec<String>>,
    /// The personal characteristics a programme is limited to, any of which
    /// qualifies (`programs.target_groups`). `None`: no restriction stated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_groups: Option<Vec<String>>,
    /// The household types a programme is limited to, any of which qualifies.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_household_types: Option<Vec<String>>,
    /// "male" or "female" when only one is eligible.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_gender: Option<String>,
    /// Accepted 기준 중위소득 bands, when not all of them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub income_median_bands: Option<Vec<String>>,
    /// Profile `enrollment_status` values the programme requires; read by the
    /// structural check from `normalized_payload.enrollment_statuses`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enrollment_statuses: Option<Vec<String>>,
    /// The youth source's coded conditions, for display and audit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub youth_conditions: Option<crate::eligibility_codes::YouthConditions>,
    /// Hard-filter conditions compiled from source codes; written to
    /// `eligibility_rules` by [`sync_source_rule`], not stored here.
    #[serde(skip)]
    pub source_rule_conditions: Vec<serde_json::Value>,
}

/// The `program_status` a normalized record is stored with, when it stores
/// its own parsed window.
fn program_status_for(norm: &NormalizedProgram) -> &'static str {
    program_status_with(norm, norm.application_start_at, norm.application_end_at)
}

/// The source's own intake status, else the status of the window the row
/// will actually store (which an approved AI override may replace).
fn program_status_with(
    norm: &NormalizedProgram,
    start: Option<DateTime<Utc>>,
    end: Option<DateTime<Utc>>,
) -> &'static str {
    norm.source_status
        .unwrap_or_else(|| compute_program_status(start, end))
}

#[cfg(test)]
mod normalization_golden_tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Deserialize)]
    struct GoldenCase {
        name: String,
        source_name: String,
        source_id: String,
        normalizer_version: i32,
        raw: serde_json::Value,
        expected: serde_json::Value,
        /// The compiled source-code rule conditions, which `expected` does
        /// not carry (`source_rule_conditions` is not serialised): pinned
        /// where a case exercises them, so a compiler change is reviewed.
        #[serde(default)]
        expected_source_rules: Option<serde_json::Value>,
    }

    #[test]
    fn source_normalizers_match_reviewed_golden_outputs() {
        let cases: Vec<GoldenCase> = serde_json::from_str(include_str!(
            "../tests/fixtures/normalization_golden_cases.json"
        ))
        .expect("golden fixture must be valid JSON");

        assert!(!cases.is_empty(), "at least one golden case is required");
        for case in cases {
            assert_eq!(
                normalizer_version(&case.source_name),
                case.normalizer_version,
                "{} has an unreviewed normalizer version",
                case.name
            );
            let mut normalized = normalize(
                &case.source_name,
                &case.source_id,
                &case.raw,
                &HashSet::new(),
            );
            validate_normalized_program(&mut normalized)
                .expect("golden output must satisfy the storage contract");
            let rules = serde_json::Value::from(normalized.source_rule_conditions.clone());
            // GOLDEN_PRINT_RULES=1 prints each case's outputs, to review a
            // version bump before writing them into the fixture.
            if std::env::var_os("GOLDEN_PRINT_RULES").is_some() {
                eprintln!("RULES {} {}", case.name, rules);
            }
            if let Some(expected) = &case.expected_source_rules {
                assert_eq!(&rules, expected, "golden rule mismatch: {}", case.name);
            }
            let actual =
                serde_json::to_value(normalized).expect("normalized output must serialize");
            if std::env::var_os("GOLDEN_PRINT_RULES").is_some() {
                eprintln!("ACTUAL {} {}", case.name, actual);
            }
            assert_eq!(actual, case.expected, "golden mismatch: {}", case.name);
        }
    }

    #[test]
    fn guide_texts_are_capped_for_every_source() {
        let long = "가".repeat(MAX_GUIDE_TEXT_CHARS + 10);
        let mut program = normalize(
            "gov_benefits",
            "x",
            &serde_json::json!({"서비스명": "x", "서비스목적요약": long, "신청방법": long}),
            &HashSet::new(),
        );
        validate_normalized_program(&mut program).unwrap();
        assert_eq!(
            program.summary.unwrap().chars().count(),
            MAX_GUIDE_TEXT_CHARS
        );
        assert_eq!(
            program.application_method.unwrap().chars().count(),
            MAX_GUIDE_TEXT_CHARS
        );
    }

    #[test]
    fn youth_age_sentinels_become_unknown() {
        assert_eq!(parse_youth_age(Some("0")), None);
        assert_eq!(parse_youth_age(Some("999")), None);
        assert_eq!(parse_youth_age(Some("19")), Some(19));
    }

    #[test]
    fn financial_support_is_not_confused_with_education_or_item_lending() {
        for title in [
            "청년내일저축계좌",
            "대학생 학자금 대출이자 지원",
            "청년 전세보증금 반환보증 보증료 지원",
            "기쁨두배통장",
        ] {
            assert!(is_financial_support_title(title), "{title}");
        }
        for title in [
            "청년 금융교육",
            "저축과 대출 특강",
            "채무조정 상담",
            "도서대출 서비스",
            "장난감 대출",
            "취업 장려금",
            "미래인재 장학금",
        ] {
            assert!(!is_financial_support_title(title), "{title}");
        }
    }

    /// Review B1: the app's "대학 (전문대 포함)" answer is stored as
    /// `university` and cannot tell 4년제 from 전문대. A 전문대-only row is
    /// never hidden from it and a 4년제-only row never passes it; only the
    /// 대학원 side is decided.
    #[test]
    fn a_university_answer_cannot_tell_4year_from_junior_college() {
        use mazimi_core::rule_engine::{evaluate, parse_rule_json, AgeBasis, RuleVerdict};
        let verdict = |school: &str, profile: serde_json::Value| {
            let raw = serde_json::json!({"상품명": "X", "운영기관명": "A재단", "상품구분": "장학금",
                "대학구분": school, "학년구분": "대학1학기,대학2학기", "모집종료일": "2026-12-31"});
            let norm = normalize("scholarship", "A재단_X", &raw, &HashSet::new());
            let tree = parse_rule_json(&serde_json::json!({ "all": norm.source_rule_conditions }))
                .unwrap();
            let mut base = serde_json::json!({"birth_year": 2006, "region_code": "seoul"});
            base.as_object_mut()
                .unwrap()
                .extend(profile.as_object().unwrap().clone());
            let profile: mazimi_core::models::ProfileInput = serde_json::from_value(base).unwrap();
            evaluate(&profile, &tree, &AgeBasis::today()).verdict
        };
        let app_student = serde_json::json!({"school_type": "university",
            "enrollment_status": "enrolled", "school_year": 1});
        assert_eq!(
            verdict("전문대(2~3년제)", app_student.clone()),
            RuleVerdict::Unknown
        );
        assert_eq!(
            verdict("4년제(5~6년제포함)", app_student.clone()),
            RuleVerdict::Unknown
        );
        assert_eq!(
            verdict("4년제(5~6년제포함),전문대(2~3년제)", app_student),
            RuleVerdict::Pass
        );
        let graduate_student =
            serde_json::json!({"school_type": "graduate", "enrollment_status": "enrolled"});
        assert_eq!(
            verdict("전문대(2~3년제)", graduate_student),
            RuleVerdict::Fail
        );
    }

    /// Audit H2: a 학자금지원정보 row keeps its school and year conditions,
    /// and a 학자금대출 is not a scholarship.
    #[test]
    fn scholarship_rows_keep_their_school_conditions() {
        use mazimi_core::rule_engine::{evaluate, parse_rule_json, AgeBasis, RuleVerdict};
        let raw = serde_json::json!({"상품명": "X", "운영기관명": "A재단", "상품구분": "장학금",
            "대학구분": "전문대", "학년구분": "대학신입생", "모집종료일": "2026-12-31",
            "지역거주여부 상세내용": "해당없음"});
        let norm = normalize("scholarship", "A재단_X", &raw, &HashSet::new());
        assert_eq!(norm.program_type, "scholarship");
        let rules = &norm.source_rule_conditions;
        assert!(
            rules.iter().any(|r| r["field"] == "education_stage"),
            "{rules:?}"
        );
        let tree = parse_rule_json(&serde_json::json!({ "all": rules })).unwrap();
        let profile = |extra: serde_json::Value| {
            let mut base = serde_json::json!({"birth_year": 1997, "region_code": "seoul"});
            base.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            serde_json::from_value::<mazimi_core::models::ProfileInput>(base).unwrap()
        };
        let graduate = profile(serde_json::json!({"school_type": "university",
            "enrollment_status": "graduated", "employment_status": "employed"}));
        assert_ne!(
            evaluate(&graduate, &tree, &AgeBasis::today()).verdict,
            RuleVerdict::Pass
        );
        let fresher = profile(serde_json::json!({"school_type": "college",
            "enrollment_status": "enrolled", "school_year": 1}));
        assert_eq!(
            evaluate(&fresher, &tree, &AgeBasis::today()).verdict,
            RuleVerdict::Pass
        );

        // Enrolled 4년제 students only: another school type is outside it.
        let raw = serde_json::json!({"상품명": "Y", "운영기관명": "B재단", "상품구분": "장학금",
            "대학구분": "4년제(5~6년제포함)", "학년구분": "대학3학기,대학4학기", "모집종료일": "2026-12-31"});
        let norm = normalize("scholarship", "B재단_Y", &raw, &HashSet::new());
        let tree =
            parse_rule_json(&serde_json::json!({ "all": norm.source_rule_conditions })).unwrap();
        let college =
            profile(serde_json::json!({"school_type": "college", "enrollment_status": "enrolled"}));
        assert_eq!(
            evaluate(&college, &tree, &AgeBasis::today()).verdict,
            RuleVerdict::Fail
        );
        // The app's `university` answer includes 전문대 (review B1): a check.
        let student = profile(serde_json::json!({"school_type": "university",
            "enrollment_status": "enrolled", "school_year": 2}));
        assert_eq!(
            evaluate(&student, &tree, &AgeBasis::today()).verdict,
            RuleVerdict::Unknown
        );

        // 제한없음 or an unknown token says nothing.
        for (school, year) in [("제한없음", "제한없음"), ("해외대학", "대학원신입생")]
        {
            let raw = serde_json::json!({"상품명": "Z", "운영기관명": "C", "대학구분": school,
                "학년구분": year, "모집종료일": "2026-12-31"});
            let norm = normalize("scholarship", "C_Z", &raw, &HashSet::new());
            assert!(norm.source_rule_conditions.is_empty(), "{school} {year}");
        }

        let loan = serde_json::json!({"상품명": "학자금 대출", "운영기관명": "D", "상품구분": "학자금대출",
            "모집종료일": "2026-12-31"});
        assert_eq!(
            normalize("scholarship", "D_L", &loan, &HashSet::new()).program_type,
            "financial_product"
        );
    }

    /// Audit M6: a record that extends its upper age for military service
    /// (real payload 20261008005400113930) never hides a 36-year-old: the
    /// served band is widened by the statutory extension and the stated
    /// upper age is a named check.
    #[test]
    fn a_stated_military_age_extension_never_hides_on_the_stated_upper_age() {
        use chrono::Datelike;
        use mazimi_core::rule_engine::{evaluate, parse_rule_json, AgeBasis, RuleVerdict};
        let page: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/youth_center_page_inverted_window.json"
        ))
        .unwrap();
        let raw = page["result"]["youthPolicyList"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["plcyNo"] == "20261008005400113930")
            .unwrap()
            .clone();
        let norm = normalize(
            "youth_center",
            "20261008005400113930",
            &raw,
            &HashSet::new(),
        );
        let today = mazimi_core::seoul_date(Utc::now());
        let born = |age: i32| {
            serde_json::from_value::<mazimi_core::models::ProfileInput>(serde_json::json!({
                "birth_year": today.year() - age - 1, "region_code": "seoul",
                "birth_date": format!("{}-01-01", today.year() - age - 1)}))
            .unwrap()
        };
        let gate = |age: i32| {
            mazimi_core::rule_engine::age_gate(today.year() - age - 1, norm.min_age, norm.max_age)
        };
        assert_ne!(gate(36), RuleVerdict::Fail, "max_age {:?}", norm.max_age);
        let age: Vec<&serde_json::Value> = norm
            .source_rule_conditions
            .iter()
            .filter(|r| r["field"] == "age")
            .collect();
        assert_eq!(age.len(), 1, "{:?}", norm.source_rule_conditions);
        let tree = parse_rule_json(&serde_json::json!({"all": age})).unwrap();
        assert_eq!(
            evaluate(&born(36), &tree, &AgeBasis::today()).verdict,
            RuleVerdict::Unknown
        );
        assert_eq!(
            evaluate(&born(30), &tree, &AgeBasis::today()).verdict,
            RuleVerdict::Pass
        );
        // Beyond any extension the band still hides.
        assert_eq!(gate(45), RuleVerdict::Fail);
        // A record without the wording keeps its stated band.
        let plain =
            serde_json::json!({"plcyNm": "x", "sprtTrgtMinAge": "19", "sprtTrgtMaxAge": "34"});
        assert_eq!(
            normalize("youth_center", "p", &plain, &HashSet::new()).max_age,
            Some(34)
        );
    }

    /// Audit M5: every round of 온통청년 `aplyYmd` is read; the first line's
    /// end is not the deadline once that round is over.
    #[test]
    fn youth_application_rounds_are_all_read() {
        let raw = serde_json::json!({"plcyNm": "청년 2회 모집",
            "aplyYmd": "20260301 ~ 20260331\\N20260901 ~ 20260930"});
        let norm = normalize("youth_center", "R1", &raw, &HashSet::new());
        let march = kst_end_of_day(NaiveDate::from_ymd_opt(2026, 3, 31).unwrap());
        assert_ne!(norm.application_end_at, Some(march));
        assert_eq!(norm.application_rounds.as_ref().map(Vec::len), Some(2));
        let single = serde_json::json!({"plcyNm": "청년", "aplyYmd": "20260101 ~ 20261231"});
        let norm = normalize("youth_center", "R2", &single, &HashSet::new());
        assert!(norm.application_rounds.is_none());
        assert_eq!(
            norm.application_end_at,
            Some(kst_end_of_day(
                NaiveDate::from_ymd_opt(2026, 12, 31).unwrap()
            ))
        );
    }

    #[test]
    fn official_sources_reach_the_correct_browsing_tabs() {
        for source in [
            "youth_center",
            "gov_benefits",
            "national_welfare",
            "local_welfare",
        ] {
            let raw = serde_json::json!({
                "plcyNm": "청년내일저축계좌", "서비스명": "청년내일저축계좌",
                "servNm": "청년내일저축계좌", "lclsfNm": "복지문화", "서비스분야": "생활안정"
            });
            assert_eq!(
                normalize(source, "category-test", &raw, &HashSet::new()).program_type,
                "financial_product",
                "{source}"
            );
        }
        for source in ["national_welfare", "local_welfare"] {
            let raw = serde_json::json!({"servNm": "긴급 생계 지원", "srvPvsnNm": "현금"});
            assert_eq!(
                normalize(source, "category-test", &raw, &HashSet::new()).program_type,
                "welfare",
                "{source}"
            );
        }
        let raw = serde_json::json!({"plcyNm": "청년 참여위원회", "lclsfNm": "참여권리"});
        assert_eq!(
            normalize("youth_center", "category-test", &raw, &HashSet::new()).program_type,
            "policy"
        );
        let raw = serde_json::json!({"plcyNm": "청년 금융교육", "lclsfNm": "교육"});
        assert_eq!(
            normalize("youth_center", "category-test", &raw, &HashSet::new()).program_type,
            "policy"
        );
    }
}

/// Longest guide text (summary, 신청방법, 제출서류, 심사방법) a programme stores,
/// for every source (review2-fss L9): a provider field of megabytes is cut,
/// not served. The raw payload keeps the whole text.
const MAX_GUIDE_TEXT_CHARS: usize = 4000;

fn validate_normalized_program(program: &mut NormalizedProgram) -> Result<()> {
    program.program_type = program.program_type.trim().to_string();
    program.title = program.title.trim().to_string();
    for text in [
        &mut program.summary,
        &mut program.application_method,
        &mut program.submission_documents,
        &mut program.screening_method,
    ]
    .into_iter()
    .flatten()
    {
        if let Some((cut, _)) = text.char_indices().nth(MAX_GUIDE_TEXT_CHARS) {
            text.truncate(cut);
        }
    }
    anyhow::ensure!(
        mazimi_core::is_program_type(&program.program_type),
        "normalized program_type {:?} is not a canonical program type",
        program.program_type
    );
    anyhow::ensure!(
        !program.title.is_empty()
            && program.title != "Unknown"
            && program.title.chars().count() <= 300,
        "normalized title must contain 1 to 300 non-placeholder characters"
    );
    anyhow::ensure!(
        program.min_age.is_none_or(|age| (0..=100).contains(&age))
            && program.max_age.is_none_or(|age| (0..=100).contains(&age))
            && program
                .min_age
                .zip(program.max_age)
                .is_none_or(|(minimum, maximum)| minimum <= maximum),
        "normalized age range is invalid"
    );
    anyhow::ensure!(
        !matches!(
            (program.application_start_at, program.application_end_at),
            (Some(start), Some(end)) if end < start
        ),
        "normalized application date range is invalid"
    );

    let mut regions = HashSet::new();
    for region in &program.regions {
        // An explicit unknown scope must never become the empty (=nationwide)
        // array. It is source data, not a selectable user profile region.
        if region == crate::regions::UNKNOWN {
            regions.clear();
            regions.insert(crate::regions::UNKNOWN.to_string());
            break;
        }
        let canonical = mazimi_core::canonical_region_code(region)
            .ok_or_else(|| anyhow::anyhow!("normalized region is not canonicalizable"))?;
        regions.insert(canonical.to_string());
    }
    program.regions = regions.into_iter().collect();
    program.regions.sort_unstable();
    Ok(())
}

fn normalize(
    source_name: &str,
    _source_id: &str,
    payload: &serde_json::Value,
    shared_youth_center_ref_urls: &HashSet<String>,
) -> NormalizedProgram {
    match source_name {
        "youth_center" => normalize_youth_center(payload, shared_youth_center_ref_urls),
        "gov_benefits" => normalize_gov_benefits(payload),
        "scholarship" => normalize_scholarship(payload),
        // local_scraper payloads come from HTML-scraped portals.
        // source_name is always "local_scraper"; the per-portal name lives
        // inside payload["source_portal"]. We dispatch on that inner field.
        "local_scraper" => normalize_local_scraper(payload),
        "fss_financial" => crate::sources::financial::normalize_financial(payload),
        "national_welfare" => crate::sources::national_welfare::normalize_national_welfare(payload),
        "local_welfare" => crate::sources::local_welfare::normalize_local_welfare(payload),
        _ => NormalizedProgram {
            program_type: "benefit".into(),
            title: payload["title"].as_str().unwrap_or("Unknown").to_string(),
            summary: None,
            provider_name: None,
            company_name: None,
            benefit_category: None,
            provider_kind: None,
            official_url: None,
            application_start_at: None,
            application_end_at: None,
            min_age: None,
            max_age: None,
            regions: vec![],
            application_method: None,
            submission_documents: None,
            screening_method: None,
            source_status: None,
            ..Default::default()
        },
    }
}

fn normalize_youth_center(
    p: &serde_json::Value,
    shared_ref_urls: &HashSet<String>,
) -> NormalizedProgram {
    let title = p["plcyNm"].as_str().unwrap_or("Unknown").to_string();

    // plcySprtCn (지원 내용) → plcyExplnCn (정책 설명) 순 fallback
    let summary = p["plcySprtCn"]
        .as_str()
        .or_else(|| p["plcyExplnCn"].as_str())
        .map(|s| s.to_string());

    let provider_name = p["sprvsnInstCdNm"].as_str().map(|s| s.to_string());

    // URL selection: prefer the most "specific" (non-homepage) candidate.
    // aplyUrlAddr is populated on only ~19% of records and is often just the
    // provider's bare homepage; refUrlAddr1/2 frequently point to the actual
    // detail/application page instead. When none of those are specific, fall
    // back to 온통청년's own detail page (built from plcyNo), which renders a
    // real detail/apply page for ~100% of records. See select_official_url
    // for the full priority rules.
    let official_url = select_official_url(
        p["aplyUrlAddr"].as_str(),
        p["refUrlAddr1"].as_str(),
        p["refUrlAddr2"].as_str(),
        p["plcyNo"].as_str(),
        shared_ref_urls,
    );

    // 신청 가이드 텍스트 필드 (모바일 가이드 카드용 원문 캡처)
    let application_method = p["plcyAplyMthdCn"].as_str().map(|s| s.to_string());
    let submission_documents = p["sbmsnDcmntCn"].as_str().map(|s| s.to_string());
    let screening_method = p["srngMthdCn"].as_str().map(|s| s.to_string());

    let min_age = parse_youth_age(p["sprtTrgtMinAge"].as_str());
    let stated_max_age = parse_youth_age(p["sprtTrgtMaxAge"].as_str());
    // A record that extends its upper age for military service: the served
    // band is widened by the largest extension and the stated upper age
    // becomes a named check, so a veteran above it is never hidden (audit M6).
    let extended = stated_max_age.filter(|_| military_age_extension(p));
    let max_age = match extended {
        Some(max) => Some((max + MILITARY_AGE_EXTENSION_YEARS).min(100)),
        None => stated_max_age,
    };

    // aplyYmd: "YYYYMMDD ~ YYYYMMDD", one line per round. The one window
    // parser of 보조금24 reads it (audit M5); free text ("예산소진시까지",
    // "상시") carries no window.
    let rounds = p["aplyYmd"]
        .as_str()
        .and_then(crate::sources::gov_benefits::parse_stated_rounds);
    let window = current_round_on(rounds.as_deref());
    // A range written with both years but inverted is a provider defect the
    // record is rejected for (validate_normalized_program), never "no window".
    let inverted = p["aplyYmd"]
        .as_str()
        .filter(|_| window.is_none())
        .and_then(crate::sources::gov_benefits::stated_inverted_range);
    let (start, end) = match inverted {
        Some((start, end)) => (Some(kst_start_of_day(start)), Some(kst_end_of_day(end))),
        None => (
            window.and_then(|w| w.start).map(kst_start_of_day),
            window.map(|w| kst_end_of_day(w.end)),
        ),
    };

    // lclsfNm (대분류) 기반 타입 매핑
    let lclsf = p["lclsfNm"].as_str().unwrap_or("");
    let mclsf = p["mclsfNm"].as_str().unwrap_or("");
    let keyword = p["plcyKywdNm"].as_str().unwrap_or("");
    let combined = format!("{} {} {} {}", title, lclsf, mclsf, keyword);

    let scope = crate::regions::youth_scope(p);
    let conditions = crate::eligibility_codes::youth_conditions(p);

    let program_type = if is_financial_support_title(&title) {
        "financial_product"
    } else if combined.contains("장학") || combined.contains("학자금") {
        "scholarship"
    } else if combined.contains("주거") || combined.contains("임대") || combined.contains("전세")
    {
        "housing"
    } else if combined.contains("일자리")
        || combined.contains("취업")
        || combined.contains("창업")
        || combined.contains("고용")
    {
        "employment"
    } else if combined.contains("복지") {
        "welfare"
    } else {
        "policy"
    }
    .to_string();

    let mut norm = NormalizedProgram {
        program_type: program_type.clone(),
        title,
        summary,
        provider_name,
        company_name: None,
        benefit_category: Some(program_type.clone()),
        provider_kind: None,
        official_url,
        application_start_at: start,
        application_end_at: end,
        application_rounds: stored_rounds(rounds.as_deref()),
        min_age,
        max_age,
        regions: scope.regions,
        application_method,
        submission_documents,
        screening_method,
        source_status: youth_center_intake_status(p["aplyPrdSeCd"].as_str()),
        region_districts: scope.districts,
        scope_note: scope.note,
        target_groups: conditions.target_groups.clone(),
        target_gender: conditions.target_gender.clone(),
        enrollment_statuses: conditions.enrollment_statuses.clone(),
        source_rule_conditions: crate::eligibility_codes::youth_rule_conditions(&conditions)
            .into_iter()
            .chain(extended.map(crate::eligibility_codes::stated_upper_age_label))
            .collect(),
        youth_conditions: (conditions != Default::default()).then_some(conditions),
        ..Default::default()
    };
    apply_age_statements(
        &texts_of(p, &["addAplyQlfcCndCn", "ptcpPrpTrgtCn", "plcyExplnCn"]),
        &mut norm,
    );
    norm
}

/// Years the upper age of a youth programme may be raised for military
/// service. 제대군인지원법 raises 응시연령 by up to 3 years by length of
/// service; youth employment rules count service of up to 6 years. The
/// larger bound only widens the band that is a named check, never a pass.
const MILITARY_AGE_EXTENSION_YEARS: i32 = 6;

/// Whether an 온통청년 record's additional conditions extend the age limit
/// for military service ("제대군인 지원법에 따라 군 복무자는 신청가능 연령
/// 연장", "병역 이행 기간만큼 연령 상한 연장"): one statutory concept, read
/// from the two condition fields.
fn military_age_extension(p: &serde_json::Value) -> bool {
    static PATTERN: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let pattern = PATTERN.get_or_init(|| {
        regex::Regex::new(r"(?:군\s*복무|병역|제대\s*군인|군필)[^.\n]{0,30}(?:연장|가산|상향)")
            .expect("static pattern compiles")
    });
    ["addAplyQlfcCndCn", "ptcpPrpTrgtCn"]
        .iter()
        .filter_map(|field| p[*field].as_str())
        .any(|text| pattern.is_match(text))
}

/// 온통청년 `aplyPrdSeCd` (신청기간 구분). The meanings come from the
/// official detail page's own script (`ythPlcyDetail`): `0057001 // 특정기간`,
/// `0057002 // 상시`, `0057003 // 마감`. In the retained payloads every
/// 0057002 and 0057003 record has an empty `aplyYmd`, and every 0057001
/// record has one, so the code is the only intake signal for the first two.
fn youth_center_intake_status(code: Option<&str>) -> Option<&'static str> {
    match code.map(str::trim).unwrap_or("") {
        "0057003" => Some("closed"),
        "0057002" => Some("always_open"),
        // 특정기간, or no code at all: the period fields decide.
        "0057001" | "" => None,
        unknown => {
            warn!(
                code = unknown,
                "unknown youth_center aplyPrdSeCd; application dates decide"
            );
            None
        }
    }
}

fn parse_youth_age(value: Option<&str>) -> Option<i32> {
    value
        .map(str::trim)
        .and_then(|value| value.parse::<i32>().ok())
        .filter(|age| (1..=100).contains(age))
}

/// Pick the most "specific" URL from the 온통청년 candidate fields
/// (aplyUrlAddr, refUrlAddr1, refUrlAddr2), falling back to the platform's
/// own detail page (keyed by plcyNo) before giving up entirely.
///
/// "특정" (specific) means the URL is http/https AND has a path beyond the
/// bare domain (e.g. `/notice/view.do`) or a query string (e.g. `?id=42`).
/// A bare homepage like `https://example.go.kr` or `https://example.go.kr/`
/// is NOT specific.
///
/// `shared_ref_urls` is the batch-wide set of refUrlAddr1/2 values that were
/// observed on 2+ distinct records (see
/// `collect_shared_youth_center_ref_urls`). 온통청년 occasionally copy-pastes
/// the same refUrl into unrelated policy records — e.g. two different
/// plcyNo both pointing at the same 경남 detail page (policy_no=1816). A
/// refUrl in this set is untrusted as a deep link and is skipped as if it
/// weren't specific, even though its shape passes `is_specific_url`.
/// aplyUrlAddr is never subject to this guard — it's the provider's own
/// authoritative apply link, not a copy-pasted reference. Pass an empty set
/// to disable the guard entirely (existing callers/behavior unaffected).
///
/// Priority order:
///   1. specific aplyUrlAddr
///   2. specific refUrlAddr1, unless it's in `shared_ref_urls`
///   3. specific refUrlAddr2, unless it's in `shared_ref_urls`
///   4. (no specific/trusted candidate) 온통청년 detail page built from
///      plcyNo —
///      `https://www.youthcenter.go.kr/youthPolicy/ythPlcyTotalSearch/ythPlcyDetail/{plcyNo}`.
///      Verified via curl to return 200 for records with a plcyNo, and always
///      renders a real detail/apply page (never a bare homepage).
///   5. (no plcyNo either) non-empty aplyUrlAddr
///   6. (no plcyNo, no aplyUrlAddr) refUrlAddr1
///
/// Any URL candidate that isn't http/https (e.g. plain text like "전화문의")
/// is discarded entirely.
fn select_official_url(
    aply_url: Option<&str>,
    ref_url1: Option<&str>,
    ref_url2: Option<&str>,
    plcy_no: Option<&str>,
    shared_ref_urls: &HashSet<String>,
) -> Option<String> {
    fn clean(s: Option<&str>) -> Option<&str> {
        s.map(|s| s.trim())
            .filter(|s| !s.is_empty() && is_http_url(s))
    }

    let aply = clean(aply_url);
    let ref1 = clean(ref_url1);
    let ref2 = clean(ref_url2);

    if let Some(u) = aply.filter(|u| is_specific_url(u)) {
        return Some(u.to_string());
    }
    if let Some(u) = ref1.filter(|u| is_specific_url(u) && !is_shared_ref_url(u, shared_ref_urls)) {
        return Some(u.to_string());
    }
    if let Some(u) = ref2.filter(|u| is_specific_url(u) && !is_shared_ref_url(u, shared_ref_urls)) {
        return Some(u.to_string());
    }

    // No specific candidate among aply/ref1/ref2 — fall back to 온통청년's
    // own detail page, which is guaranteed to be a real detail/apply page
    // (not a bare homepage) for any valid plcyNo.
    if let Some(no) = plcy_no.map(|s| s.trim()).filter(|s| !s.is_empty()) {
        return Some(youth_center_detail_url(no));
    }

    // No plcyNo either — fall back to whatever non-empty http(s) URL exists,
    // preferring aplyUrlAddr over refUrlAddr1.
    // (refUrlAddr2 is intentionally excluded from this bare fallback.)
    aply.or(ref1).map(|u| u.to_string())
}

/// Build the 온통청년 platform's own detail page URL for a given plcyNo.
/// This always renders a real detail page with an apply button, unlike the
/// provider-supplied aplyUrlAddr/refUrlAddr fields which are frequently a
/// bare homepage or empty.
fn youth_center_detail_url(plcy_no: &str) -> String {
    format!("https://www.youthcenter.go.kr/youthPolicy/ythPlcyTotalSearch/ythPlcyDetail/{plcy_no}")
}

/// Aggregate refUrlAddr1/2 values that appear as a *specific* URL (see
/// `is_specific_url`) on 2+ distinct records (identified by `source_id`,
/// i.e. plcyNo — see `youth_center::fetch_all`) within a single youth_center
/// ingestion batch.
///
/// This exists because the upstream API occasionally copy-pastes the same
/// refUrl into unrelated policy records — observed in production: two
/// different plcyNo values both carrying the same 경남 detail page as their
/// refUrl (policy_no=1816). Trusting that refUrl as a deep link sends the
/// user to the wrong policy's detail page. `select_official_url` consults
/// the returned set (via `is_shared_ref_url`) and refuses to use a refUrl
/// found here as a deep link, falling back to 온통청년's own plcyNo detail
/// page instead.
///
/// Only meaningful for the youth_center source — see the call site in
/// `process_source`, which only computes a non-empty set when
/// `source_name == "youth_center"`. No effect on national_welfare,
/// local_welfare, gov_benefits, etc.
///
/// Normalization for the dedup comparison is deliberately minimal (trim +
/// strip trailing slash, via `normalize_ref_url_for_dedup`) so we don't mask
/// genuinely different URLs as duplicates. Only "specific" candidates are
/// tracked — a bare homepage shared across many unrelated policies is
/// common and legitimate (it was never going to win the "specific" priority
/// tier anyway), so including it here would just be noise.
fn collect_shared_youth_center_ref_urls(records: &[RawRecord]) -> HashSet<String> {
    let mut owners: HashMap<String, HashSet<&str>> = HashMap::new();

    for record in records {
        for field in ["refUrlAddr1", "refUrlAddr2"] {
            let Some(raw) = record.payload[field].as_str() else {
                continue;
            };
            let trimmed = raw.trim();
            if trimmed.is_empty() || !is_http_url(trimmed) || !is_specific_url(trimmed) {
                continue;
            }
            owners
                .entry(normalize_ref_url_for_dedup(trimmed))
                .or_default()
                .insert(record.source_id.as_str());
        }
    }

    owners
        .into_iter()
        .filter(|(_, source_ids)| source_ids.len() >= 2)
        .map(|(url, _)| url)
        .collect()
}

/// Returns true if `url` (after minimal normalization) is in the batch-wide
/// shared/untrusted refUrl set produced by
/// `collect_shared_youth_center_ref_urls`.
fn is_shared_ref_url(url: &str, shared_ref_urls: &HashSet<String>) -> bool {
    shared_ref_urls.contains(&normalize_ref_url_for_dedup(url))
}

/// Minimal normalization for refUrl dedup comparison: trim whitespace and
/// strip trailing slash(es). Deliberately does NOT lowercase, strip query
/// strings, or otherwise touch the URL — over-normalizing risks treating
/// genuinely different pages as the same shared/untrusted URL.
fn normalize_ref_url_for_dedup(url: &str) -> String {
    url.trim().trim_end_matches('/').to_string()
}

/// Returns true if `s` starts with an http/https scheme.
fn is_http_url(s: &str) -> bool {
    s.starts_with("http://") || s.starts_with("https://")
}

/// Returns true if the URL has a path beyond the bare domain or a query
/// string. Caller must already ensure `url` is http/https.
fn is_specific_url(url: &str) -> bool {
    if mazimi_core::application_resources::is_generic_portal(url) {
        return false;
    }
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    match rest.find('/') {
        // No slash after the domain at all — only specific if there's a
        // query string glued directly onto the domain (rare, but handle it).
        None => rest.contains('?'),
        Some(idx) => {
            let after_domain = &rest[idx..]; // starts with '/'
            !after_domain.trim_start_matches('/').is_empty()
        }
    }
}

fn normalize_gov_benefits(p: &serde_json::Value) -> NormalizedProgram {
    let title = p["서비스명"].as_str().unwrap_or("Unknown").to_string();
    let summary = p["서비스목적요약"].as_str().map(|s| s.to_string());
    let provider_name = p["소관기관명"].as_str().map(|s| s.to_string());
    let official_url = p["상세조회URL"].as_str().map(|s| s.to_string());

    // 지원대상 and 서비스분야 used to infer program_type
    let program_type = if is_financial_support_title(&title) {
        "financial_product".into()
    } else {
        infer_gov_program_type(p["지원대상"].as_str(), p["서비스분야"].as_str())
    };

    // 신청기한 is free text. Only explicit dated rounds become dates; "상시"
    // and anything ambiguous stay undated. The current round is stored; all
    // rounds go to `application_rounds` for the daily step that opens the
    // next one (audit M4).
    let rounds = p["신청기한"]
        .as_str()
        .and_then(crate::sources::gov_benefits::parse_stated_rounds);
    let window = current_round_on(rounds.as_deref());
    // "신규신청불가" / "신청불필요": nobody can apply, whatever the dates say.
    let source_status = p["신청기한"]
        .as_str()
        .is_some_and(crate::sources::gov_benefits::declares_not_open)
        .then_some("closed");
    let scope = crate::regions::government_scope(p);
    let conditions = crate::eligibility_codes::gov_conditions(p);
    let source_rule_conditions = crate::eligibility_codes::gov_rule_conditions(&conditions);
    let documents = crate::sources::gov_benefits::detail_documents(p);

    let mut norm = NormalizedProgram {
        program_type: program_type.clone(),
        title,
        summary,
        provider_name,
        company_name: None,
        benefit_category: Some(program_type.clone()),
        provider_kind: Some("government".into()),
        official_url,
        application_start_at: window.and_then(|w| w.start).map(kst_start_of_day),
        application_end_at: window.map(|w| kst_end_of_day(w.end)),
        application_rounds: stored_rounds(rounds.as_deref()),
        min_age: conditions.min_age,
        max_age: conditions.max_age,
        regions: scope.regions,
        application_method: source_text(p, "신청방법"),
        // 구비서류 is a serviceDetail field, stored under the detail key, not
        // a serviceList field.
        keep_stored_submission_documents: documents.is_none(),
        submission_documents: documents,
        screening_method: None,
        source_status,
        region_districts: scope.districts,
        scope_note: scope.note,
        applicant_types: conditions.applicant_types,
        target_groups: conditions.target_groups,
        source_rule_conditions,
        target_household_types: conditions.target_household_types,
        target_gender: conditions.target_gender,
        income_median_bands: conditions.income_median_bands,
        ..Default::default()
    };
    apply_age_statements(&texts_of(p, &["지원대상", "선정기준"]), &mut norm);
    norm
}

/// Preserve source guidance without inventing a submission channel or consent.
pub(crate) fn source_text(payload: &serde_json::Value, field: &str) -> Option<String> {
    payload[field]
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

fn normalize_scholarship(p: &serde_json::Value) -> NormalizedProgram {
    let title = p["상품명"].as_str().unwrap_or("Unknown").to_string();

    // 지원내역 상세내용 is the most useful summary; fall back to 소득기준 상세내용
    let summary = p["지원내역 상세내용"]
        .as_str()
        .or_else(|| p["소득기준 상세내용"].as_str())
        .map(|s| s.to_string());

    let provider_name = p["운영기관명"].as_str().map(|s| s.to_string());
    let official_url = p["홈페이지 주소"].as_str().map(|s| s.to_string());
    let provider_kind = classify_scholarship_provider(p["운영기관구분"].as_str());

    // 모집시작일 / 모집종료일 format is "2024-09-23" (YYYY-MM-DD); "상시모집"
    // and similar free text yield None.
    let start = parse_yyyy_mm_dd(p["모집시작일"].as_str()).map(kst_start_of_day);
    let end = parse_yyyy_mm_dd(p["모집종료일"].as_str()).map(kst_end_of_day);

    let scope = crate::regions::scholarship_scope(p);
    let program_type = if crate::eligibility_codes::scholarship_is_loan(p) {
        "financial_product"
    } else {
        "scholarship"
    };

    let mut norm = NormalizedProgram {
        program_type: program_type.into(),
        title,
        summary,
        provider_name,
        company_name: None,
        benefit_category: Some(program_type.into()),
        provider_kind,
        official_url,
        application_start_at: start,
        application_end_at: end,
        min_age: None,
        max_age: None,
        regions: scope.regions,
        region_districts: scope.districts,
        scope_note: scope.note,
        application_method: None,
        // The source's own documents field, verbatim (the API splits it
        // into a checklist for display).
        submission_documents: source_text(p, "제출서류 상세내용"),
        screening_method: None,
        source_status: None,
        source_rule_conditions: crate::eligibility_codes::scholarship_conditions(p),
        ..Default::default()
    };
    apply_age_statements(
        &texts_of(p, &["특정자격 상세내용", "자격제한 상세내용"]),
        &mut norm,
    );
    norm
}

fn classify_scholarship_provider(kind: Option<&str>) -> Option<String> {
    let kind = kind?.trim();
    if kind.contains("민간") {
        Some("private_foundation".into())
    } else if kind.contains("대학") {
        Some("university".into())
    } else if kind.contains("지자체") || kind.contains("지방자치") {
        Some("municipality".into())
    } else if kind.contains("정부") || kind.contains("중앙부처") {
        Some("government".into())
    } else if kind.contains("공공") {
        Some("public_agency".into())
    } else {
        None
    }
}

#[cfg(test)]
mod age_statement_tests {
    use super::*;

    /// A row with a 19–34 band whose application runs in 2026.
    fn youth_row() -> NormalizedProgram {
        NormalizedProgram {
            min_age: Some(19),
            max_age: Some(34),
            application_start_at: Some(
                chrono::DateTime::parse_from_rfc3339("2026-03-01T00:00:00+09:00")
                    .unwrap()
                    .with_timezone(&Utc),
            ),
            ..Default::default()
        }
    }

    fn birth_rules(norm: &NormalizedProgram) -> usize {
        norm.source_rule_conditions
            .iter()
            .filter(|r| r["field"] == "birth_year")
            .count()
    }

    /// Review B3 (a): a child's birth range is not the applicant's; a range
    /// counts only when it agrees with the row's own age band.
    #[test]
    fn a_childs_birth_range_is_not_an_applicant_rule() {
        for text in [
            "○ 2025. 1. 1. ~ 2025. 12. 31. 출생한 자녀의 부 또는 모",
            "○ 2024~2026년 출생아 부모",
            "○ 2019년생 ~ 2022년생 자녀를 양육하는 가구",
            "2026.1.1.~2026.12.31. 출생아 1인당 200만원 지원",
        ] {
            for mut norm in [youth_row(), NormalizedProgram::default()] {
                let band = (norm.min_age, norm.max_age);
                apply_age_statements(&[text], &mut norm);
                assert_eq!(birth_rules(&norm), 0, "{text}");
                assert_eq!((norm.min_age, norm.max_age), band, "{text}");
            }
        }
        // The applicant's own range, agreeing with 만 19–34 in 2026.
        let mut norm = youth_row();
        apply_age_statements(&["만 19세~34세(1991.1.1.~2007.12.31. 출생자)"], &mut norm);
        assert_eq!(birth_rules(&norm), 1);
    }

    /// Review B3 (b, c): a negated 연 나이 states nothing, and a date counts
    /// only inside the applicant's age expression.
    #[test]
    fn a_reference_day_is_read_only_inside_the_age_expression() {
        for text in [
            "※ 연 나이가 아닌 만 나이 기준입니다",
            "만 19세 ~ 34세 (연 나이 아님)",
            "부산시 거주 만 19~34세 미취업 청년 (2026.1.1. 기준 주민등록)",
            "소득기준: 2025. 12. 31. 기준 건강보험료, 연령: 만 19~39세",
            "1월 1일 기준 주민등록상 부산 거주, 만 19~34세",
        ] {
            let mut norm = youth_row();
            apply_age_statements(&[text], &mut norm);
            assert_eq!(norm.age_basis, None, "{text}");
        }
    }

    /// Verify V-M2: a 연 나이 mention its sentence negates states nothing,
    /// and a date in a parenthetical that names another fact (거주,
    /// 주민등록 …) is no age reference day.
    #[test]
    fn a_negated_or_residence_reference_is_no_age_basis() {
        let read: Vec<(&str, Option<String>)> = [
            "연 나이 기준 아님",
            "연 나이로 계산하지 않음, 만 19~34세",
            "만 나이 기준 (연 나이 적용 안 함)",
            "만 19세~34세 (2026. 1. 1. 기준 부산 거주자)",
            "만 19~34세 (주민등록 2026.1.1. 기준)",
        ]
        .into_iter()
        .map(|text| {
            let mut norm = youth_row();
            apply_age_statements(&[text], &mut norm);
            (text, norm.age_basis)
        })
        .filter(|(_, basis)| basis.is_some())
        .collect();
        assert!(read.is_empty(), "read: {read:?}");
        // The applicant's own reference day still reads.
        let mut norm = youth_row();
        apply_age_statements(&["만 19세~34세 (2026. 1. 1. 기준)"], &mut norm);
        assert!(norm.age_basis.is_some());
    }
}

#[cfg(test)]
mod provider_kind_tests {
    use super::classify_scholarship_provider;

    #[test]
    fn classifies_only_explicit_scholarship_provider_types() {
        assert_eq!(
            classify_scholarship_provider(Some("민간 장학재단")).as_deref(),
            Some("private_foundation")
        );
        assert_eq!(
            classify_scholarship_provider(Some("지방자치단체")).as_deref(),
            Some("municipality")
        );
        assert_eq!(classify_scholarship_provider(Some("기타")), None);
        assert_eq!(classify_scholarship_provider(None), None);
    }
}

/// Normalise a payload produced by `LocalScraperSource`.
///
/// The payload shape is:
/// ```json
/// {
///   "source_portal": "부산청년센터",
///   "region":        "busan",
///   "title":         "...",
///   "description":   "...",
///   "deadline_text": "...",
///   "link":          "https://...",
/// }
/// ```
///
/// Deadline text is free-form (e.g. "~2026.04.30", "2026년 4월 30일까지").
/// We do a best-effort parse; unrecognised strings become `None`.
fn normalize_local_scraper(p: &serde_json::Value) -> NormalizedProgram {
    let title = p["title"].as_str().unwrap_or("Unknown").to_string();
    let summary = p["description"].as_str().map(|s| s.to_string());
    let official_url = p["link"].as_str().map(|s| s.to_string());

    // Provider name is the portal name stored in source_portal.
    let provider_name = p["source_portal"].as_str().map(|s| s.to_string());

    // Region comes from the static ScraperSource definition.
    let region = p["region"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let regions = region.map(|r| vec![r]).unwrap_or_default();

    // Best-effort deadline parsing from free-form Korean/mixed text.
    let end = p["deadline_text"].as_str().and_then(parse_scraper_deadline);

    // Heuristic program_type from title keywords.
    let program_type = infer_scraper_program_type(title.as_str());

    NormalizedProgram {
        program_type: program_type.clone(),
        title,
        summary,
        provider_name,
        company_name: None,
        benefit_category: Some(program_type.clone()),
        provider_kind: None,
        official_url,
        application_start_at: None,
        application_end_at: end,
        min_age: None,
        max_age: None,
        regions,
        application_method: None,
        submission_documents: None,
        screening_method: None,
        source_status: None,
        ..Default::default()
    }
}

/// Parse a free-form deadline string into the end of that Korean calendar day.
///
/// Handles common patterns found on Korean government portals:
///   "~2026.04.30"  "2026년 4월 30일" "2026-04-30" "2026/04/30"
/// Returns None for anything that doesn't match.
fn parse_scraper_deadline(s: &str) -> Option<DateTime<Utc>> {
    // Strip leading "~" or "까지" and whitespace/punctuation.
    let cleaned = s
        .trim_start_matches('~')
        .trim()
        .trim_end_matches("까지")
        .trim();

    // Try YYYYMMDD (no separator).
    if cleaned.len() == 8 && cleaned.chars().all(|c| c.is_ascii_digit()) {
        return parse_yyyymmdd(Some(cleaned)).map(kst_end_of_day);
    }

    // Extract up to 3 digit runs of length 1-4; interpret as year, month, day.
    let parts: Vec<u32> = cleaned
        .split(|c: char| !c.is_ascii_digit())
        .filter(|t| !t.is_empty() && t.len() <= 4)
        .filter_map(|t| t.parse().ok())
        .collect();

    match parts.as_slice() {
        [y, m, d] if *y >= 2000 && *y <= 2100 => {
            NaiveDate::from_ymd_opt(*y as i32, *m, *d).map(kst_end_of_day)
        }
        _ => None,
    }
}

/// Infer program_type from scraped title keywords.
fn infer_scraper_program_type(title: &str) -> String {
    let t = title.to_lowercase();
    if t.contains("장학") || t.contains("학자금") || t.contains("scholarship") {
        "scholarship".into()
    } else if t.contains("주거")
        || t.contains("임대")
        || t.contains("전세")
        || t.contains("청년주택")
    {
        "housing".into()
    } else if t.contains("취업") || t.contains("창업") || t.contains("인턴") || t.contains("고용")
    {
        "employment".into()
    } else {
        "benefit".into()
    }
}

/// Only explicit product/support names establish a finance category. A
/// financial education or counselling event must not look like a loan offer.
/// This is a browsing category, never an eligibility or application guarantee.
pub(crate) fn is_financial_support_title(title: &str) -> bool {
    let title: String = title.chars().filter(|c| !c.is_whitespace()).collect();
    if [
        "교육",
        "상담",
        "강좌",
        "특강",
        "컨설팅",
        "도서대출",
        "장난감대출",
    ]
    .iter()
    .any(|word| title.contains(word))
    {
        return false;
    }
    [
        "대출",
        "융자",
        "적금",
        "저축",
        "자산형성",
        "이자지원",
        "이차보전",
        "보증료",
        "채무조정",
        "신용회복",
        "도약계좌",
        "내일채움공제",
        "기쁨두배통장",
        "희망두배청년통장",
        "청년통장",
    ]
    .iter()
    .any(|word| title.contains(word))
}

/// Crude heuristic: inspect target/theme text to assign program_type.
fn infer_gov_program_type(target: Option<&str>, theme: Option<&str>) -> String {
    let combined = format!("{} {}", target.unwrap_or(""), theme.unwrap_or("")).to_lowercase();

    if combined.contains("장학") || combined.contains("학자금") {
        "scholarship".into()
    } else if combined.contains("주거") || combined.contains("임대") || combined.contains("전세")
    {
        "housing".into()
    } else if combined.contains("취업") || combined.contains("창업") || combined.contains("고용")
    {
        "employment".into()
    } else {
        "benefit".into()
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// What the notice texts state about age beyond the coded band (review
/// R-M1): the reference day, stored as `age_basis`, and a birth-date range,
/// which becomes a birth-year source condition. A range of whole calendar
/// years replaces the age band, which cannot express it on any one day.
fn apply_age_statements(texts: &[&str], norm: &mut NormalizedProgram) {
    use chrono::Datelike;
    norm.age_basis = age_basis::stated_age_basis(texts).map(|b| b.as_stored());
    // The notice's own years; today's when it states none.
    let mut years: Vec<i32> = [norm.application_start_at, norm.application_end_at]
        .into_iter()
        .flatten()
        .map(|at| mazimi_core::seoul_date(at).year())
        .collect();
    if years.is_empty() {
        years.push(mazimi_core::seoul_date(Utc::now()).year());
    }
    if let Some(range) = age_basis::birth_range(texts, (norm.min_age, norm.max_age), &years) {
        let whole = range.whole_years();
        norm.source_rule_conditions
            .push(crate::eligibility_codes::birth_year_condition(
                range.first.year(),
                range.last.year(),
                whole,
            ));
        if whole {
            norm.min_age = None;
            norm.max_age = None;
        }
    }
}

/// The non-empty text fields of a payload.
fn texts_of<'a>(p: &'a serde_json::Value, fields: &[&str]) -> Vec<&'a str> {
    fields
        .iter()
        .filter_map(|f| p[*f].as_str())
        .filter(|t| !t.trim().is_empty())
        .collect()
}

/// One stated application round in stored form: whole Korean days.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct ApplicationRound {
    pub start: Option<DateTime<Utc>>,
    pub end: DateTime<Utc>,
}

/// The round a programme stands in today (Korean date).
fn current_round_on(
    rounds: Option<&[crate::sources::gov_benefits::StatedWindow]>,
) -> Option<crate::sources::gov_benefits::StatedWindow> {
    let today = mazimi_core::seoul_date(Utc::now());
    rounds
        .filter(|r| !r.is_empty())
        .map(|r| crate::sources::gov_benefits::current_round(r, today))
}

/// Rounds worth storing: only several.
fn stored_rounds(
    rounds: Option<&[crate::sources::gov_benefits::StatedWindow]>,
) -> Option<Vec<ApplicationRound>> {
    rounds.filter(|r| r.len() > 1).map(|r| {
        r.iter()
            .map(|w| ApplicationRound {
                start: w.start.map(kst_start_of_day),
                end: kst_end_of_day(w.end),
            })
            .collect()
    })
}

/// Determine program_status from application window relative to now (UTC).
///
/// - No dates at all  → "active" (assume always-open)
/// - Before start     → "upcoming"
/// - Within window    → "active"
/// - After end        → "closed"
fn compute_program_status(
    start: Option<chrono::DateTime<Utc>>,
    end: Option<chrono::DateTime<Utc>>,
) -> &'static str {
    let now = Utc::now();
    match (start, end) {
        (None, None) => "active",
        (Some(s), None) => {
            if now < s {
                "upcoming"
            } else {
                "active"
            }
        }
        (None, Some(e)) => {
            if now > e {
                "closed"
            } else {
                "active"
            }
        }
        (Some(s), Some(e)) => {
            if now < s {
                "upcoming"
            } else if now > e {
                "closed"
            } else {
                "active"
            }
        }
    }
}

/// Korea Standard Time has no daylight saving, so a fixed +09:00 is exact.
const KST_OFFSET: Duration = Duration::hours(9);

/// The first instant of a Korean calendar day: 00:00 KST = 15:00Z the day before.
pub fn kst_start_of_day(date: NaiveDate) -> DateTime<Utc> {
    date.and_time(NaiveTime::MIN).and_utc() - KST_OFFSET
}

/// The last whole second of a Korean calendar day: 23:59:59 KST = 14:59:59Z.
/// An upstream "~ 20261231" deadline means the applicant has all of Dec 31 in
/// Korea, so the program must stay open until then.
pub fn kst_end_of_day(date: NaiveDate) -> DateTime<Utc> {
    kst_start_of_day(date) + Duration::seconds(24 * 60 * 60 - 1)
}

/// Parse a calendar date from the first `len` bytes of `s` when they are all
/// ASCII digits (and the separator positions in `sep_at` are non-digits).
/// Upstream fields carry free text such as "예산소진시까지" or "상시모집";
/// multi-byte input must never be byte-sliced, so this returns None instead.
fn parse_fixed_width_date(s: &str, len: usize, sep_at: &[usize]) -> Option<NaiveDate> {
    let s = s.trim();
    let head = s.get(..len)?;
    let digit_positions_ok = head
        .bytes()
        .enumerate()
        .all(|(index, byte)| sep_at.contains(&index) || byte.is_ascii_digit());
    if !digit_positions_ok || !head.is_ascii() {
        return None;
    }
    let year: i32 = head[0..4].parse().ok()?;
    let (month_at, day_at) = if sep_at.is_empty() { (4, 6) } else { (5, 8) };
    let month: u32 = head[month_at..month_at + 2].parse().ok()?;
    let day: u32 = head[day_at..day_at + 2].parse().ok()?;
    NaiveDate::from_ymd_opt(year, month, day)
}

/// Parse "YYYYMMDD" → calendar date. Callers pick the KST boundary.
fn parse_yyyymmdd(s: Option<&str>) -> Option<NaiveDate> {
    parse_fixed_width_date(s?, 8, &[])
}

/// Parse "YYYY-MM-DD" (any single-byte separator) → calendar date.
fn parse_yyyy_mm_dd(s: Option<&str>) -> Option<NaiveDate> {
    parse_fixed_width_date(s?, 10, &[4, 7])
}

/// Read-only dry run of the intake-signal mapping (온통청년 aplyPrdSeCd,
/// gov_benefits 신청기한) over an export of production snapshots: one JSON
/// object per line with the fields the normalizer reads and the program's
/// current state. The export query is in the handoff report
/// `stale-intake-signals.md`. Nothing is written anywhere.
///
/// `INTAKE_DRY_RUN=/path/intake.jsonl cargo test -p mazimi-worker --lib
/// intake_dry_run -- --ignored --nocapture`; `INTAKE_DRY_RUN_OUT` optionally
/// receives the ids of programs that would stop being available.
#[cfg(test)]
mod intake_dry_run {
    use super::*;
    use std::collections::BTreeMap;

    #[derive(serde::Deserialize)]
    struct Row {
        source: String,
        source_id: String,
        payload: serde_json::Value,
        program: Option<Current>,
    }

    #[derive(serde::Deserialize)]
    struct Current {
        id: Uuid,
        is_active: bool,
        status: String,
        reason: Option<String>,
        start: Option<DateTime<Utc>>,
        end: Option<DateTime<Utc>>,
        deadline: Option<DateTime<Utc>>,
        valid_until: Option<DateTime<Utc>>,
        available: bool,
        date_override: bool,
    }

    /// The row after one fresh fetch (`upsert_program`) and one expiry run
    /// (`expire_all_programs`), mirrored from their SQL.
    fn after_sync(cur: &Current, norm: &NormalizedProgram, now: DateTime<Utc>) -> (bool, bool) {
        let status = program_status_for(norm);
        // An admin-approved AI window wins while the raw payload is unchanged.
        let (start, end) = if cur.date_override {
            (cur.start, cur.end)
        } else {
            (norm.application_start_at, norm.application_end_at)
        };
        let reopened = !cur.is_active
            && matches!(cur.reason.as_deref(), Some("expired" | "source_removed"))
            && status != "closed";
        let mut active = cur.is_active || reopened;
        let last = [cur.valid_until, cur.deadline, end]
            .into_iter()
            .flatten()
            .min();
        if active && last.is_some_and(|last| last <= now) {
            active = false;
        }
        let available = active
            && mazimi_core::models::OPEN_PROGRAM_STATUSES.contains(&status)
            && start.is_none_or(|s| s <= now)
            && last.is_none_or(|l| l > now);
        (available, reopened)
    }

    #[test]
    #[ignore = "needs INTAKE_DRY_RUN export"]
    fn intake_dry_run() {
        let Ok(path) = std::env::var("INTAKE_DRY_RUN") else {
            return;
        };
        let now = Utc::now();
        let mut counts: BTreeMap<String, i64> = BTreeMap::new();
        let mut bump = |key: String| *counts.entry(key).or_default() += 1;
        let mut closed_ids = Vec::new();
        for line in std::fs::read_to_string(path).unwrap().lines() {
            let row: Row = serde_json::from_str(line).expect("export line is JSON");
            let src = row.source.as_str();
            let Some(cur) = row.program else {
                bump(format!("{src}: snapshot without program row"));
                continue;
            };
            let norm = normalize(src, &row.source_id, &row.payload, &HashSet::new());
            let (available, reopened) = after_sync(&cur, &norm, now);
            bump(format!("{src}: programs"));
            if cur.available {
                bump(format!("{src}: available before"));
            }
            if available {
                bump(format!("{src}: available after"));
            }
            if cur.available && !available {
                bump(format!("{src}: becomes unavailable"));
                closed_ids.push(cur.id.to_string());
            }
            if !cur.available && available {
                bump(format!("{src}: becomes available"));
            }
            if reopened {
                bump(format!("{src}: automatic closure reopened"));
            }
            if src == "youth_center" {
                let code = row.payload["aplyPrdSeCd"]
                    .as_str()
                    .unwrap_or("")
                    .trim()
                    .to_string();
                let code = if code.is_empty() {
                    "blank".into()
                } else {
                    code
                };
                bump(format!("youth_center {code}: programs"));
                if cur.available {
                    bump(format!("youth_center {code}: available before"));
                }
                if cur.available && !available {
                    bump(format!("youth_center {code}: becomes unavailable"));
                }
                if cur.reason.as_deref() == Some("manual") {
                    bump(format!("youth_center {code}: manual hold (left closed)"));
                }
                let new_status = program_status_for(&norm);
                if new_status != cur.status {
                    bump(format!(
                        "youth_center {code}: status {} -> {new_status}",
                        cur.status
                    ));
                }
            }
            if src == "gov_benefits"
                && (norm.application_start_at.is_some() || norm.application_end_at.is_some())
            {
                bump("gov_benefits dated: programs".into());
                if cur.start.is_none() && cur.end.is_none() {
                    bump("gov_benefits dated: dates newly filled".into());
                }
                if cur.date_override {
                    bump("gov_benefits dated: AI override kept".into());
                }
                let end = norm.application_end_at;
                let start = norm.application_start_at;
                let phase = if end.is_some_and(|e| e <= now) {
                    "window passed (closed, then expired)"
                } else if start.is_some_and(|s| s > now) {
                    "window not open yet (upcoming)"
                } else {
                    "window open now"
                };
                bump(format!("gov_benefits dated: {phase}"));
                if cur.available {
                    bump(format!("gov_benefits dated, available before: {phase}"));
                }
            }
        }
        for (key, value) in &counts {
            println!("{value:>7}  {key}");
        }
        if let Ok(out) = std::env::var("INTAKE_DRY_RUN_OUT") {
            std::fs::write(out, closed_ids.join("\n")).unwrap();
        }
    }
}

/// Read-only dry run of the eligibility/scope mapping (gov_benefits v6,
/// youth_center v7, local_welfare v5, welfare ↔ 보조금24 dedup) over an
/// export of production snapshots: one JSON object per line,
/// `{"source", "source_id", "payload", "program": {"id", "regions", "min_age",
/// "max_age", "is_active", ...} | null}`, e.g. exported read-only with
/// `psql -At -c "SELECT json_build_object('source', s.source_name,
/// 'source_id', s.source_id, 'payload', s.raw_payload, 'program', (SELECT
/// json_build_object('id', p.id, 'title', p.title, 'regions', p.regions,
/// 'is_active', p.is_active, 'served', true, 'min_age', p.min_age, 'max_age',
/// p.max_age, 'region_districts', p.region_districts, 'source_rule', (SELECT
/// r.hard_filter_json FROM
/// eligibility_rules r WHERE r.program_id = p.id ORDER BY r.version DESC
/// LIMIT 1)) FROM programs p WHERE p.source_type = s.source_name AND
/// p.source_id = s.source_id)) FROM source_snapshots s WHERE s.source_name IN
/// ('gov_benefits', 'youth_center', 'national_welfare', 'local_welfare')"`.
/// `source_rule` (the latest hard filter) is optional; with it the run
/// reports which programmes gain, change or lose source-code conditions.
/// Nothing is written anywhere.
///
/// `RECO_DRY_RUN=/path/snapshots.jsonl cargo test -p mazimi-worker --lib
/// reco_ingestion_dry_run -- --ignored --nocapture`; `RECO_DRY_RUN_OUT`
/// optionally receives per-change program ids (TSV) for spot checks.
#[cfg(test)]
mod reco_ingestion_dry_run {
    use super::*;
    use std::collections::BTreeMap;

    #[derive(serde::Deserialize)]
    struct Row {
        source: String,
        source_id: String,
        payload: serde_json::Value,
        program: Option<Current>,
    }

    #[derive(serde::Deserialize)]
    struct Current {
        id: Uuid,
        title: Option<String>,
        regions: Option<Vec<String>>,
        is_active: bool,
        #[serde(default)]
        source_rule: Option<serde_json::Value>,
        /// Served columns, compared when the export carries them
        /// (`served: true`).
        #[serde(default)]
        served: bool,
        #[serde(default)]
        min_age: Option<i32>,
        #[serde(default)]
        max_age: Option<i32>,
        #[serde(default)]
        region_districts: Option<Vec<String>>,
    }

    /// `field` of a leaf, `any(a|b)` of a group: what a condition checks.
    fn rule_shape(rule: &serde_json::Value) -> String {
        match rule["any"].as_array() {
            Some(children) => format!(
                "any({})",
                children
                    .iter()
                    .map(rule_shape)
                    .collect::<Vec<_>>()
                    .join("|")
            ),
            None => format!(
                "{}{}",
                rule["field"].as_str().unwrap_or("?"),
                if rule["severity"] == "label" {
                    "~label"
                } else {
                    ""
                }
            ),
        }
    }

    fn scope_kind(regions: &[String]) -> &'static str {
        if regions.is_empty() {
            "nationwide"
        } else if regions.iter().any(|r| r == crate::regions::UNKNOWN) {
            "unknown"
        } else {
            "regional"
        }
    }

    #[test]
    #[ignore = "needs RECO_DRY_RUN export"]
    fn reco_ingestion_dry_run() {
        let Ok(path) = std::env::var("RECO_DRY_RUN") else {
            return;
        };
        let mut counts: BTreeMap<String, i64> = BTreeMap::new();
        let mut bump = |key: String| *counts.entry(key).or_default() += 1;
        let mut changes: Vec<String> = Vec::new();
        let mut gov_cache = Vec::new();
        let mut welfare = Vec::new();
        for line in std::fs::read_to_string(path).unwrap().lines() {
            let row: Row = serde_json::from_str(line).expect("export line is JSON");
            let src = row.source.as_str();
            let Some(cur) = row.program else {
                bump(format!("{src}: snapshot without program row"));
                continue;
            };
            let mut norm = normalize(src, &row.source_id, &row.payload, &HashSet::new());
            if validate_normalized_program(&mut norm).is_err() {
                bump(format!("{src}: rejected by validation"));
                continue;
            }
            bump(format!("{src}: programs"));
            let id = cur.id.to_string();
            let note = |key: String, changes: &mut Vec<String>| {
                changes.push(format!(
                    "{key}\t{id}\t{}",
                    cur.title.as_deref().unwrap_or("")
                ));
            };
            let before = cur.regions.clone().unwrap_or_default();
            let (was, now) = (scope_kind(&before), scope_kind(&norm.regions));
            let mut sorted_before = before.clone();
            sorted_before.sort();
            if was != now {
                let key = format!("{src}: scope {was} -> {now}");
                bump(key.clone());
                note(key, &mut changes);
            } else if sorted_before != norm.regions {
                bump(format!("{src}: scope regions changed ({was})"));
            }
            if let Some(n) = norm.scope_note {
                let key = format!("{src}: scope note {n}");
                bump(key.clone());
                note(key, &mut changes);
            }
            match norm.region_districts.as_deref() {
                Some([]) => bump(format!("{src}: districts sido-wide (empty list)")),
                Some(_) => bump(format!("{src}: districts listed")),
                None if src == "local_welfare"
                    && row.payload["sggNm"]
                        .as_str()
                        .is_some_and(|s| !s.trim().is_empty()) =>
                {
                    let key = format!("{src}: sggNm not resolved to a district");
                    bump(key.clone());
                    note(key, &mut changes);
                }
                None => {}
            }
            if cur.served {
                let active = if cur.is_active {
                    " (active)"
                } else {
                    " (inactive)"
                };
                if (cur.min_age, cur.max_age) != (norm.min_age, norm.max_age) {
                    let key = format!(
                        "{src}: age band {:?}-{:?} -> {:?}-{:?}{active}",
                        cur.min_age, cur.max_age, norm.min_age, norm.max_age
                    );
                    bump(format!("{src}: age band changes{active}"));
                    note(key, &mut changes);
                }
                let sorted = |d: &Option<Vec<String>>| {
                    d.clone().map(|mut d| {
                        d.sort();
                        d
                    })
                };
                if sorted(&cur.region_districts) != sorted(&norm.region_districts) {
                    let key = format!(
                        "{src}: districts {:?} -> {:?}{active}",
                        cur.region_districts, norm.region_districts
                    );
                    bump(format!("{src}: districts change{active}"));
                    note(key, &mut changes);
                }
                if sorted_before != norm.regions {
                    bump(format!("{src}: regions change{active}"));
                }
            }
            if norm.min_age.is_some() || norm.max_age.is_some() {
                bump(format!("{src}: has an age band"));
            }
            if norm.source_status == Some("closed") && src == "gov_benefits" {
                let key = "gov_benefits: closed by 신청기한 (신규신청불가/신청불필요)".to_string();
                bump(key.clone());
                if cur.is_active {
                    note(key, &mut changes);
                }
            }
            if let Some(types) = &norm.applicant_types {
                bump(format!("{src}: applicant_types {}", types.join("+")));
                if !types.iter().any(|t| t == "individual" || t == "household") {
                    let key = format!("{src}: no individual or household applicant");
                    bump(key.clone());
                    if cur.is_active {
                        bump(format!(
                            "{src}: no individual or household applicant (active)"
                        ));
                    }
                }
            }
            if norm.target_groups.is_some() {
                bump(format!("{src}: target_groups set"));
            }
            if norm.target_household_types.is_some() {
                bump(format!("{src}: target_household_types set"));
            }
            if norm.target_gender.is_some() {
                bump(format!("{src}: target_gender set"));
            }
            if norm.income_median_bands.is_some() {
                bump(format!("{src}: income_median_bands set"));
            }
            if src == "gov_benefits" {
                if row.payload[crate::sources::gov_benefits::SUPPORT_CONDITIONS_KEY].is_object() {
                    bump("gov_benefits: supportConditions present".into());
                }
                let personal = norm
                    .applicant_types
                    .as_ref()
                    .is_none_or(|a| a.iter().any(|t| t == "individual" || t == "household"));
                if cur.is_active && personal && now != "unknown" {
                    gov_cache.push(GovCacheEntry {
                        id: cur.id,
                        title_key: cross_source_title_key(&norm.title),
                        provider: compact(norm.provider_name.as_deref().unwrap_or("")),
                        regions: norm.regions.clone(),
                    });
                }
            }
            if src == "youth_center" {
                if let Some(c) = &norm.youth_conditions {
                    for (flag, label) in [
                        (c.employment_statuses.is_some(), "employment rule"),
                        (c.employment_requirements.is_some(), "jobCd restricted"),
                        (c.enrollment_statuses.is_some(), "enrollment_statuses gate"),
                        (c.education_requirements.is_some(), "schoolCd restricted"),
                        (c.marital_status.is_some(), "marital status"),
                        (c.major_requirements.is_some(), "major restricted"),
                        (c.education_stages.is_some(), "education_stage rule"),
                        (c.major_categories.is_some(), "major_category rule"),
                        (
                            c.income_annual_max_manwon.is_some()
                                || c.income_annual_min_manwon.is_some(),
                            "annual income bounds",
                        ),
                        (
                            c.income_condition_text.is_some(),
                            "income free text (AI queue)",
                        ),
                    ] {
                        if flag {
                            bump(format!("youth_center: {label}"));
                        }
                    }
                }
            }
            if matches!(src, "youth_center" | "gov_benefits") {
                let desired = &norm.source_rule_conditions;
                let current: Vec<serde_json::Value> =
                    mazimi_core::rule_engine::top_level_conditions(cur.source_rule.as_ref())
                        .into_iter()
                        .filter(|c| mazimi_core::rule_engine::is_source_condition(c))
                        .cloned()
                        .collect();
                let active = if cur.is_active {
                    " (active)"
                } else {
                    " (inactive)"
                };
                if !desired.is_empty() {
                    bump(format!("{src}: writes a source-code rule{active}"));
                }
                for rule in desired {
                    bump(format!("{src}: rule {}{active}", rule_shape(rule)));
                }
                let change = match (current.is_empty(), desired.is_empty()) {
                    (true, false) => Some("gains a source-code rule"),
                    (false, true) => Some("loses its source-code rule"),
                    (false, false) if current != *desired => Some("source-code rule changes"),
                    _ => None,
                };
                if let Some(change) = change {
                    let key = format!("{src}: {change}{active}");
                    bump(key.clone());
                    note(key, &mut changes);
                    let had: Vec<String> = current.iter().map(rule_shape).collect();
                    for shape in desired.iter().map(rule_shape) {
                        if !had.contains(&shape) {
                            bump(format!("{src}: adds {shape}{active}"));
                        }
                    }
                }
            }
            if matches!(src, "national_welfare" | "local_welfare") && cur.is_active {
                let jurisdiction = welfare_jurisdiction(src, &row.payload, &norm);
                welfare.push((
                    src.to_string(),
                    id.clone(),
                    norm.title.clone(),
                    norm.regions.clone(),
                    jurisdiction,
                ));
            }
        }
        for (src, id, title, regions, jurisdiction) in welfare {
            let dup = jurisdiction
                .as_deref()
                .and_then(|j| find_gov_benefits_duplicate(&title, j, &regions, &gov_cache));
            if let Some(dup) = dup {
                let key = format!("{src}: retired as duplicate of gov_benefits");
                bump(key.clone());
                changes.push(format!("{key}\t{id}\t{title}\t{}", dup.id));
            }
        }
        for (key, value) in &counts {
            println!("{value:>7}  {key}");
        }
        if let Ok(out) = std::env::var("RECO_DRY_RUN_OUT") {
            std::fs::write(out, changes.join("\n")).unwrap();
        }
    }
}

#[cfg(test)]
mod date_parsing_tests {
    use super::*;

    #[test]
    fn korean_free_text_dates_are_none_not_panics() {
        // Byte 4 of "예산소진시까지" is inside a multi-byte character; the old
        // slice-based parser panicked here and took down the whole daily run.
        assert_eq!(parse_yyyymmdd(Some("예산소진시까지")), None);
        assert_eq!(parse_yyyy_mm_dd(Some("상시모집")), None);
        assert_eq!(parse_yyyy_mm_dd(Some("상시모집 2026년")), None);
        assert_eq!(parse_yyyymmdd(Some("2026년12월31일")), None);
        assert_eq!(parse_yyyymmdd(Some("")), None);
        assert_eq!(parse_yyyymmdd(Some("2026123")), None);
        assert_eq!(parse_yyyymmdd(Some("20261332")), None);
        assert_eq!(parse_yyyy_mm_dd(Some("2026-1-01")), None);
    }

    #[test]
    fn digit_dates_parse_with_trailing_text_ignored() {
        let date = NaiveDate::from_ymd_opt(2026, 12, 31).unwrap();
        assert_eq!(parse_yyyymmdd(Some("20261231")), Some(date));
        assert_eq!(parse_yyyymmdd(Some(" 20261231 ")), Some(date));
        assert_eq!(parse_yyyy_mm_dd(Some("2026-12-31")), Some(date));
        assert_eq!(parse_yyyy_mm_dd(Some("2026.12.31")), Some(date));
        assert_eq!(parse_yyyy_mm_dd(Some("2026-12-31T00:00:00")), Some(date));
    }

    #[test]
    fn application_windows_cover_the_whole_korean_day() {
        let date = NaiveDate::from_ymd_opt(2026, 12, 31).unwrap();
        assert_eq!(
            kst_start_of_day(date).to_rfc3339(),
            "2026-12-30T15:00:00+00:00"
        );
        assert_eq!(
            kst_end_of_day(date).to_rfc3339(),
            "2026-12-31T14:59:59+00:00"
        );
        // 10:00 KST on the deadline day is still inside the window.
        let morning_kst: DateTime<Utc> = "2026-12-31T01:00:00Z".parse().unwrap();
        assert!(morning_kst < kst_end_of_day(date));
        assert_eq!(
            compute_program_status(None, Some(kst_end_of_day(date))),
            "active"
        );
    }

    #[test]
    fn youth_center_free_text_window_normalizes_without_dates() {
        let raw = serde_json::json!({
            "plcyNo": "R1", "plcyNm": "예산 소진형 지원", "aplyYmd": "예산소진시까지"
        });
        let normalized = normalize("youth_center", "R1", &raw, &HashSet::new());
        assert_eq!(normalized.application_start_at, None);
        assert_eq!(normalized.application_end_at, None);
        let raw = serde_json::json!({"상품명": "상시 장학", "모집시작일": "상시모집", "모집종료일": "상시모집"});
        let normalized = normalize("scholarship", "K1", &raw, &HashSet::new());
        assert_eq!(normalized.application_start_at, None);
        assert_eq!(normalized.application_end_at, None);
    }

    #[test]
    fn scholarship_keeps_its_documents_field() {
        let raw = serde_json::json!({"상품명": "상시 장학"});
        assert_eq!(normalize_scholarship(&raw).submission_documents, None);
        let with_documents =
            serde_json::json!({"상품명": "장학", "제출서류 상세내용": " ○ 신청서○ 재학증명서 "});
        assert_eq!(
            normalize_scholarship(&with_documents)
                .submission_documents
                .as_deref(),
            Some("○ 신청서○ 재학증명서")
        );
    }

    #[test]
    fn scraper_deadline_uses_korean_end_of_day() {
        let expected = kst_end_of_day(NaiveDate::from_ymd_opt(2026, 4, 30).unwrap());
        for text in [
            "~2026.04.30",
            "2026년 4월 30일까지",
            "20260430",
            "2026-04-30",
        ] {
            assert_eq!(parse_scraper_deadline(text), Some(expected), "{text}");
        }
        assert_eq!(parse_scraper_deadline("상시"), None);
    }
}

// ── Cross-source duplicate detection (national_welfare ↔ youth_center) ──────
//
// 온통청년 (youth_center) and 복지로's 중앙부처 서비스 (national_welfare) list
// some national youth programmes twice (청년내일저축계좌, 국민취업지원제도).
// The 온통청년 row stays and takes 복지로's deep link; the 복지로 row is retired
// as a duplicate. A pair is one programme only on structured facts both
// sources carry, the same rule as the 보조금24 check below:
// - the same title key ([`cross_source_title_key`]: years, brackets and
//   spacing removed, round numbers kept, so "1차" and "2차" stay apart);
// - the same ministry: 복지로's 소관부처 (`jurMnofNm`) opens the 온통청년 row's
//   주관기관 (`sprvsnInstCdNm`) or 등록 상위기관 (`rgtrHghrkInstCdNm`);
// - the same residence scope: both nationwide ([`nationwide`]: 온통청년
//   lists every sido for a national programme) or the same sidos. A
//   시·도 programme that shares a ministry programme's name ("청년월세
//   지원", 경남) is never the national one.
// Title similarity only breaks a tie between rows that already match on all
// three; a tie it cannot break matches nothing.
//
// local_welfare is EXCLUDED: its 1,202 청년-tagged rows (2026-07) are mostly
// generically named 시·군·구 programmes (공공근로사업, 결혼축하금 지원사업)
// whose 온통청년 copies cannot be told apart by a ministry; they are checked
// against 보조금24 only, by jurisdiction.

struct YouthCenterCacheEntry {
    id: Uuid,
    title: String,
    official_url: Option<String>,
    /// [`compact`] 주관기관 (`sprvsnInstCdNm`, the stored `provider_name`)
    /// and 등록 상위기관 (`rgtrHghrkInstCdNm`), either of which names the
    /// ministry of a national programme.
    jurisdictions: Vec<String>,
    /// Resolved residence regions, sorted; see [`nationwide`].
    regions: Vec<String>,
}

/// Load the youth_center programs users can see (`current_programs`: an
/// 온통청년 마감 keeps `is_active` but must not hide the 복지로 copy) for
/// in-memory duplicate matching. Cheap at current scale (~2k rows); run once
/// per national_welfare ingestion run, not once per record.
async fn load_youth_center_cache(pool: &PgPool) -> Result<Vec<YouthCenterCacheEntry>> {
    #[allow(clippy::type_complexity)]
    let rows: Vec<(
        Uuid,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<Vec<String>>,
    )> = sqlx::query_as(
        "SELECT id, title, official_url, provider_name, \
                raw_payload->>'rgtrHghrkInstCdNm', regions \
         FROM current_programs WHERE source_type = 'youth_center'",
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|(id, title, official_url, provider, registrar, regions)| {
            youth_cache_entry(id, title, official_url, provider, registrar, regions)
        })
        .collect())
}

fn youth_cache_entry(
    id: Uuid,
    title: String,
    official_url: Option<String>,
    provider: Option<String>,
    registrar: Option<String>,
    regions: Option<Vec<String>>,
) -> YouthCenterCacheEntry {
    let jurisdictions = [provider, registrar]
        .into_iter()
        .flatten()
        .map(|name| compact(&name))
        .filter(|name| !name.is_empty())
        .collect();
    let mut regions = regions.unwrap_or_default();
    regions.sort();
    YouthCenterCacheEntry {
        id,
        title,
        official_url,
        jurisdictions,
        regions,
    }
}

/// A residence scope that covers the whole country: no region (nationwide)
/// or every sido.
fn nationwide(regions: &[String]) -> bool {
    regions.is_empty()
        || mazimi_core::agent::REGION_CODES
            .iter()
            .all(|code| regions.iter().any(|r| r == code))
}

/// Whether two resolved scopes are the same: both nationwide, or the same
/// sidos. An unknown scope is never the same as anything.
fn same_scope(a: &[String], b: &[String]) -> bool {
    let unknown = |r: &[String]| r.iter().any(|x| x == crate::regions::UNKNOWN);
    if unknown(a) || unknown(b) {
        return false;
    }
    if nationwide(a) || nationwide(b) {
        return nationwide(a) && nationwide(b);
    }
    let (mut a, mut b) = (a.to_vec(), b.to_vec());
    a.sort();
    b.sort();
    a == b
}

/// The 온통청년 row a national_welfare record is a copy of (see the section
/// comment): same title key, same ministry, same residence scope; title
/// similarity breaks a tie, and an unbroken tie matches nothing.
fn find_youth_center_duplicate<'a>(
    title: &str,
    jurisdiction: &str,
    regions: &[String],
    cache: &'a [YouthCenterCacheEntry],
) -> Option<&'a YouthCenterCacheEntry> {
    let key = cross_source_title_key(title);
    let jurisdiction = compact(jurisdiction);
    if key.chars().count() < 3 || jurisdiction.is_empty() {
        return None;
    }
    let mut candidates: Vec<(f64, &YouthCenterCacheEntry)> = cache
        .iter()
        .filter(|entry| cross_source_title_key(&entry.title) == key)
        .filter(|entry| {
            entry
                .jurisdictions
                .iter()
                .any(|j| j.starts_with(&jurisdiction))
        })
        .filter(|entry| same_scope(regions, &entry.regions))
        .map(|entry| {
            (
                dice_bigram_similarity(&compact(title), &compact(&entry.title)),
                entry,
            )
        })
        .collect();
    candidates.sort_by(|a, b| b.0.total_cmp(&a.0));
    match candidates.as_slice() {
        [] => None,
        [(_, only)] => Some(only),
        [(best, entry), (next, _), ..] => (best > next).then_some(*entry),
    }
}

/// Normalize a program title for duplicate detection: strip parenthetical
/// notes, 4-digit years (optionally followed by "년"), punctuation and all
/// whitespace. Other digits stay: "1차" and "2차", "1인 가구" name what the
/// programme is.
fn normalize_title_for_dedup(title: &str) -> String {
    let no_parens = strip_parenthetical(title);
    let chars: Vec<char> = no_parens.chars().collect();
    let mut out = String::with_capacity(chars.len());
    let mut i = 0;
    while i < chars.len() {
        // Skip a run of exactly 4 digits (a year mention), optionally
        // followed by '년'.
        let run = chars[i..].iter().take_while(|c| c.is_ascii_digit()).count();
        if run == 4 && (i == 0 || !chars[i - 1].is_ascii_digit()) {
            i += 4;
            if i < chars.len() && chars[i] == '년' {
                i += 1;
            }
            continue;
        }
        if run > 0 {
            out.extend(&chars[i..i + run]);
            i += run;
            continue;
        }
        let c = chars[i];
        if c.is_whitespace() || matches!(c, '.' | '-' | '/' | ':' | '~' | ',') {
            i += 1;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Strip `(...)` parenthetical notes (non-nested-aware but tolerant of stray
/// unmatched parens, since these are free-text government titles).
fn strip_parenthetical(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut depth = 0i32;
    for c in s.chars() {
        match c {
            '(' => depth += 1,
            ')' => {
                if depth > 0 {
                    depth -= 1;
                }
            }
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

/// Sørensen-Dice coefficient over character bigrams (multiset intersection).
/// Unicode-safe: operates on `char`, not bytes, so multi-byte Korean
/// characters are never split mid-codepoint.
fn dice_bigram_similarity(a: &str, b: &str) -> f64 {
    fn bigrams(s: &str) -> Vec<(char, char)> {
        let chars: Vec<char> = s.chars().collect();
        if chars.len() < 2 {
            return Vec::new();
        }
        chars.windows(2).map(|w| (w[0], w[1])).collect()
    }

    let ba = bigrams(a);
    let bb = bigrams(b);
    if ba.is_empty() || bb.is_empty() {
        return 0.0;
    }

    let mut counts: HashMap<(char, char), i32> = HashMap::new();
    for bg in &ba {
        *counts.entry(*bg).or_insert(0) += 1;
    }
    let mut intersection = 0i32;
    for bg in &bb {
        if let Some(c) = counts.get_mut(bg) {
            if *c > 0 {
                *c -= 1;
                intersection += 1;
            }
        }
    }

    2.0 * intersection as f64 / (ba.len() + bb.len()) as f64
}

/// Per-run caches for cross-source duplicate checks, loaded once per run.
/// A cache that failed to load is `None`: it retires nothing.
#[derive(Default)]
pub(crate) struct DedupCaches {
    /// national_welfare ↔ youth_center (title only; see above).
    youth: Option<Vec<YouthCenterCacheEntry>>,
    /// national/local welfare ↔ 보조금24 (title and jurisdiction).
    gov: Option<Vec<GovCacheEntry>>,
    /// Every cache this source uses loaded, so "no match" is authoritative:
    /// each record is re-checked on every run, unchanged or not, and an
    /// earlier duplicate retirement that no longer matches is undone.
    recheck: bool,
}

/// Which visible row a welfare record duplicates.
#[derive(Clone, Copy)]
enum DuplicateOf<'a> {
    YouthCenter(&'a YouthCenterCacheEntry),
    GovBenefits(&'a GovCacheEntry),
}

impl DuplicateOf<'_> {
    fn id(self) -> Uuid {
        match self {
            Self::YouthCenter(e) => e.id,
            Self::GovBenefits(e) => e.id,
        }
    }

    fn item_status(self) -> &'static str {
        match self {
            Self::YouthCenter(_) => "duplicate_of_youth_center",
            Self::GovBenefits(_) => "duplicate_of_gov_benefits",
        }
    }
}

impl DedupCaches {
    /// 온통청년 first (national_welfare only), then 보조금24.
    fn find_duplicate(
        &self,
        source_name: &str,
        payload: &serde_json::Value,
        norm: &NormalizedProgram,
    ) -> Option<DuplicateOf<'_>> {
        if let Some(entry) = self.youth.as_deref().and_then(|cache| {
            let jurisdiction = welfare_jurisdiction(source_name, payload, norm)?;
            find_youth_center_duplicate(&norm.title, &jurisdiction, &norm.regions, cache)
        }) {
            return Some(DuplicateOf::YouthCenter(entry));
        }
        let gov = self.gov.as_deref()?;
        let jurisdiction = welfare_jurisdiction(source_name, payload, norm)?;
        find_gov_benefits_duplicate(&norm.title, &jurisdiction, &norm.regions, gov)
            .map(DuplicateOf::GovBenefits)
    }
}

async fn load_dedup_caches(pool: &PgPool, source_name: &str) -> DedupCaches {
    let mut caches = DedupCaches::default();
    if source_name == "national_welfare" {
        caches.youth = load_youth_center_cache(pool)
            .await
            .inspect_err(|e| {
                warn!(source = source_name, error = %e, "failed to load youth_center dedup cache, continuing without dedup");
            })
            .ok();
    }
    if matches!(source_name, "national_welfare" | "local_welfare") {
        caches.gov = load_gov_benefits_cache(pool)
            .await
            .inspect_err(|e| {
                warn!(source = source_name, error = %e, "failed to load gov_benefits dedup cache, continuing without dedup");
            })
            .ok();
    }
    caches.recheck = match source_name {
        "national_welfare" => caches.youth.is_some() && caches.gov.is_some(),
        "local_welfare" => caches.gov.is_some(),
        _ => false,
    };
    caches
}

// ── Cross-source duplicate detection (welfare ↔ gov_benefits) ───────────────
//
// 복지로 (national_welfare, local_welfare) and 보조금24 (gov_benefits) list
// many of the same services. Fuzzy title matching is not safe here: generic
// names ("청년 이사비 지원사업") are registered by many 시·군·구. A pair is a
// duplicate only when (a) the titles are identical after removing years,
// brackets, spacing and a trailing 사업/안내/서비스/제도, (b) the jurisdiction is
// the same — the ministry name for national_welfare, "시도 시군구" for
// local_welfare, against 보조금24's 소관기관명 — and (c) the 보조금24 row is
// visible to users (`current_programs`: active, not closed or under review,
// not expired) with the same resolved residence regions. The 보조금24 copy wins
// because it carries supportConditions.

struct GovCacheEntry {
    id: Uuid,
    title_key: String,
    provider: String,
    regions: Vec<String>,
}

async fn load_gov_benefits_cache(pool: &PgPool) -> Result<Vec<GovCacheEntry>> {
    #[allow(clippy::type_complexity)]
    let rows: Vec<(
        Uuid,
        String,
        Option<String>,
        Option<Vec<String>>,
        Option<Vec<String>>,
    )> = sqlx::query_as(
        "SELECT id, title, provider_name, regions, applicant_types FROM programs \
         WHERE id IN (SELECT id FROM current_programs WHERE source_type = 'gov_benefits')",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|(id, title, provider, regions, applicants)| {
            gov_cache_entry(id, &title, provider, regions, applicants)
        })
        .collect())
}

/// A 보조금24 row that is hidden (scope unknown) or not for individuals must
/// not take the welfare copy down with it.
fn gov_cache_entry(
    id: Uuid,
    title: &str,
    provider: Option<String>,
    regions: Option<Vec<String>>,
    applicants: Option<Vec<String>>,
) -> Option<GovCacheEntry> {
    let mut regions = regions.unwrap_or_default();
    let personal =
        applicants.is_none_or(|a| a.iter().any(|t| t == "individual" || t == "household"));
    if !personal || regions.iter().any(|r| r == crate::regions::UNKNOWN) {
        return None;
    }
    regions.sort();
    Some(GovCacheEntry {
        id,
        title_key: cross_source_title_key(title),
        provider: compact(provider.as_deref().unwrap_or("")),
        regions,
    })
}

fn compact(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Title identity across sources: [`normalize_title_for_dedup`] plus one
/// trailing generic word ("청년월세 지원사업" = "청년월세 지원").
fn cross_source_title_key(title: &str) -> String {
    let mut key = normalize_title_for_dedup(title);
    for suffix in ["사업", "안내", "서비스", "제도"] {
        if let Some(stripped) = key.strip_suffix(suffix) {
            if stripped.chars().count() >= 3 {
                key = stripped.to_string();
                break;
            }
        }
    }
    key
}

/// The jurisdiction a welfare record names, in 보조금24 소관기관명 form.
fn welfare_jurisdiction(
    source_name: &str,
    payload: &serde_json::Value,
    norm: &NormalizedProgram,
) -> Option<String> {
    match source_name {
        "national_welfare" => norm.provider_name.as_deref().map(compact),
        "local_welfare" => {
            let sido = payload["ctpvNm"].as_str()?.trim();
            let district = payload["sggNm"].as_str().unwrap_or("").trim();
            Some(compact(&format!("{sido}{district}")))
        }
        _ => None,
    }
    .filter(|j| !j.is_empty())
}

fn find_gov_benefits_duplicate<'a>(
    title: &str,
    jurisdiction: &str,
    regions: &[String],
    cache: &'a [GovCacheEntry],
) -> Option<&'a GovCacheEntry> {
    let key = cross_source_title_key(title);
    if key.chars().count() < 3 || regions.iter().any(|r| r == crate::regions::UNKNOWN) {
        return None;
    }
    let mut regions = regions.to_vec();
    regions.sort();
    let mut matches = cache
        .iter()
        .filter(|e| e.title_key == key && e.provider == jurisdiction && e.regions == regions);
    let first = matches.next()?;
    // Two 보조금24 rows for one welfare record: ambiguous, keep everything.
    matches.next().is_none().then_some(first)
}

// ── Source-code rules ──────────────────────────────────────────────────────

/// `eligibility_rules.created_by` of a version written by [`sync_source_rule`].
pub(crate) const SOURCE_RULE_AUTHOR: &str = mazimi_core::rule_engine::SOURCE_RULE_AUTHOR;

/// Owned [`mazimi_core::rule_engine::top_level_conditions`].
fn rule_children(tree: Option<&serde_json::Value>) -> Vec<serde_json::Value> {
    mazimi_core::rule_engine::top_level_conditions(tree)
        .into_iter()
        .cloned()
        .collect()
}

/// Make the program's latest rule carry exactly `desired` as its source-code
/// conditions, in both `hard_filter_json` and `rule_json`, keeping every other
/// condition (seeds, reviewed or AI-applied) as it is. Writes a new version
/// only when something changes; when nothing but this function's own
/// conditions would remain, its own versions are removed instead, restoring
/// the version below them.
async fn sync_source_rule(
    pool: &PgPool,
    program_id: Uuid,
    desired: &[serde_json::Value],
) -> Result<()> {
    let mut tx = pool.begin().await?;
    // Serializes with an AI or admin apply, which locks the program row.
    sqlx::query("SELECT 1 FROM programs WHERE id = $1 FOR UPDATE")
        .bind(program_id)
        .execute(&mut *tx)
        .await?;
    #[allow(clippy::type_complexity)]
    let latest: Option<(
        serde_json::Value,
        Option<serde_json::Value>,
        Option<serde_json::Value>,
        Option<String>,
    )> = sqlx::query_as(
        "SELECT rule_json, hard_filter_json, explain_json, created_by FROM eligibility_rules \
         WHERE program_id = $1 ORDER BY version DESC LIMIT 1",
    )
    .bind(program_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((rule_json, hard_json, explain_json, created_by)) = latest else {
        if !desired.is_empty() {
            insert_source_rule(
                &mut tx,
                program_id,
                desired.to_vec(),
                desired.to_vec(),
                None,
            )
            .await?;
        }
        tx.commit().await?;
        return Ok(());
    };
    let soft = rule_children(Some(&rule_json));
    let hard = rule_children(hard_json.as_ref());
    let ours = |children: &[serde_json::Value]| -> Vec<serde_json::Value> {
        children
            .iter()
            .filter(|c| mazimi_core::rule_engine::is_source_condition(c))
            .cloned()
            .collect()
    };
    if ours(&soft) == desired && ours(&hard) == desired {
        tx.commit().await?;
        return Ok(());
    }
    let keep = |children: Vec<serde_json::Value>| -> Vec<serde_json::Value> {
        children
            .into_iter()
            .filter(|c| !mazimi_core::rule_engine::is_source_condition(c))
            .chain(desired.iter().cloned())
            .collect()
    };
    let (soft, hard) = (keep(soft), keep(hard));
    if soft.is_empty() && hard.is_empty() {
        if created_by.as_deref() == Some(SOURCE_RULE_AUTHOR) {
            sqlx::query(
                "DELETE FROM eligibility_rules WHERE program_id = $1 AND created_by = $2 \
                   AND version > COALESCE((SELECT max(version) FROM eligibility_rules \
                       WHERE program_id = $1 AND created_by IS DISTINCT FROM $2), 0)",
            )
            .bind(program_id)
            .bind(SOURCE_RULE_AUTHOR)
            .execute(&mut *tx)
            .await?;
        } else {
            warn!(%program_id, "source-code conditions withdrawn but another author's rule holds only them; left for review");
        }
        tx.commit().await?;
        return Ok(());
    }
    insert_source_rule(&mut tx, program_id, soft, hard, explain_json).await?;
    tx.commit().await?;
    Ok(())
}

async fn insert_source_rule(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    program_id: Uuid,
    soft: Vec<serde_json::Value>,
    hard: Vec<serde_json::Value>,
    explain_json: Option<serde_json::Value>,
) -> Result<()> {
    let hard_tree = (!hard.is_empty()).then(|| serde_json::json!({ "all": hard }));
    let soft_tree = if soft.is_empty() {
        hard_tree
            .clone()
            .unwrap_or_else(|| serde_json::json!({ "all": [] }))
    } else {
        serde_json::json!({ "all": soft })
    };
    for tree in [Some(&soft_tree), hard_tree.as_ref()].into_iter().flatten() {
        mazimi_core::rule_engine::parse_rule_json(tree)
            .map_err(|e| anyhow::anyhow!("compiled source rule is invalid: {e}"))?;
    }
    sqlx::query(
        "INSERT INTO eligibility_rules ( \
             program_id, rule_json, hard_filter_json, explain_json, version, \
             created_by, compiled_at, created_at, updated_at \
         ) VALUES ($1, $2, $3, $4, \
             (SELECT COALESCE(max(version), 0) + 1 FROM eligibility_rules WHERE program_id = $1), \
             $5, now(), now(), now())",
    )
    .bind(program_id)
    .bind(&soft_tree)
    .bind(&hard_tree)
    .bind(&explain_json)
    .bind(SOURCE_RULE_AUTHOR)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

#[cfg(test)]
mod cross_source_tests {
    use super::*;

    fn gov(title: &str, provider: &str, regions: &[&str]) -> GovCacheEntry {
        GovCacheEntry {
            id: Uuid::new_v4(),
            title_key: cross_source_title_key(title),
            provider: compact(provider),
            regions: regions.iter().map(|r| r.to_string()).collect(),
        }
    }

    #[test]
    fn same_service_under_the_same_jurisdiction_is_a_duplicate() {
        // 청년월세: national_welfare 0c6767f2 and gov_benefits c7323b31.
        let cache = [gov("청년월세 지원", "국토교통부", &[])];
        assert!(
            find_gov_benefits_duplicate("청년월세 지원사업", "국토교통부", &[], &cache).is_some()
        );
        assert!(
            find_gov_benefits_duplicate("2026년 청년월세 지원사업", "국토교통부", &[], &cache)
                .is_some()
        );
    }

    #[test]
    fn a_different_jurisdiction_region_or_title_is_not() {
        let cache = [gov("청년 이사비 지원", "경기도 용인시", &["gyeonggi"])];
        assert!(find_gov_benefits_duplicate(
            "청년 이사비 지원사업",
            "경기도평택시",
            &["gyeonggi".into()],
            &cache
        )
        .is_none());
        assert!(find_gov_benefits_duplicate(
            "청년 이사비 지원사업",
            "경기도용인시",
            &["seoul".into()],
            &cache
        )
        .is_none());
        assert!(find_gov_benefits_duplicate(
            "청년 이사비 지원사업",
            "경기도용인시",
            &["gyeonggi".into()],
            &cache
        )
        .is_some());
        let cache = [gov("청년월세 지원", "국토교통부", &[])];
        assert!(
            find_gov_benefits_duplicate("청년월세 특별지원", "국토교통부", &[], &cache).is_none()
        );
        // Two candidate rows: ambiguous.
        let cache = [
            gov("청년월세 지원", "국토교통부", &[]),
            gov("청년월세 지원사업", "국토교통부", &[]),
        ];
        assert!(find_gov_benefits_duplicate("청년월세 지원", "국토교통부", &[], &cache).is_none());
    }

    #[test]
    fn rule_children_keep_non_all_trees_whole() {
        let any = serde_json::json!({"any": [{"field": "age", "op": "gte", "value": 19}]});
        assert_eq!(rule_children(Some(&any)), vec![any.clone()]);
        let all = serde_json::json!({"all": [any.clone()]});
        assert_eq!(rule_children(Some(&all)), vec![any]);
        assert!(rule_children(None).is_empty());
    }
}

#[cfg(test)]
mod dedup_tests {
    use super::*;

    fn entry(id: Uuid, title: &str, official_url: Option<&str>) -> YouthCenterCacheEntry {
        youth(id, title, official_url, "보건복지부", &[])
    }

    fn youth(
        id: Uuid,
        title: &str,
        official_url: Option<&str>,
        provider: &str,
        regions: &[&str],
    ) -> YouthCenterCacheEntry {
        YouthCenterCacheEntry {
            id,
            title: title.to_string(),
            official_url: official_url.map(String::from),
            jurisdictions: vec![compact(provider)],
            regions: regions.iter().map(|r| r.to_string()).collect(),
        }
    }

    const ALL_SIDOS: [&str; 17] = [
        "busan",
        "chungbuk",
        "chungnam",
        "daegu",
        "daejeon",
        "gangwon",
        "gwangju",
        "gyeongbuk",
        "gyeonggi",
        "gyeongnam",
        "incheon",
        "jeju",
        "jeonbuk",
        "jeonnam",
        "sejong",
        "seoul",
        "ulsan",
    ];

    /// Audit H3: the national_welfare ↔ youth_center check needs the same
    /// ministry and the same residence scope, not only a similar title.
    #[test]
    fn a_regional_or_other_ministry_youth_row_is_not_the_national_programme() {
        let id = Uuid::new_v4();
        // 경남-only 청년월세 vs 국토교통부's nationwide 청년 월세 지원.
        let cache = vec![youth(
            id,
            "청년월세지원(2026년)",
            None,
            "경상남도",
            &["gyeongnam"],
        )];
        assert!(find_youth_center_duplicate("청년 월세 지원", "국토교통부", &[], &cache).is_none());
        let cache = vec![youth(
            id,
            "청년월세지원(2026년)",
            None,
            "국토교통부",
            &["gyeongnam"],
        )];
        assert!(find_youth_center_duplicate("청년 월세 지원", "국토교통부", &[], &cache).is_none());
        let cache = vec![youth(id, "청년 월세 지원", None, "고용노동부", &[])];
        assert!(find_youth_center_duplicate("청년 월세 지원", "국토교통부", &[], &cache).is_none());
        // Rounds are different programmes.
        let cache = vec![youth(id, "청년 도약 지원 1차", None, "국토교통부", &[])];
        assert!(
            find_youth_center_duplicate("청년 도약 지원 2차", "국토교통부", &[], &cache).is_none()
        );
        // Same ministry, nationwide on both sides (온통청년 lists every sido).
        let cache = vec![
            youth(
                Uuid::new_v4(),
                "청년월세지원(2026년)",
                None,
                "경상남도",
                &["gyeongnam"],
            ),
            youth(
                id,
                "2026년 청년월세 지원사업",
                None,
                "국토교통부",
                &ALL_SIDOS,
            ),
        ];
        assert_eq!(
            find_youth_center_duplicate("청년월세 지원", "국토교통부", &[], &cache).map(|e| e.id),
            Some(id)
        );
    }

    // ── normalize_title_for_dedup ────────────────────────────────────────

    #[test]
    fn strips_year_prefix_and_parens_and_whitespace() {
        assert_eq!(
            normalize_title_for_dedup("2026년 청년월세 지원사업"),
            normalize_title_for_dedup("청년월세 지원사업")
        );
        assert_eq!(
            normalize_title_for_dedup("(동구) 2026년 동구 청년 컬처페이 지원사업 (접수마감)"),
            "동구청년컬처페이지원사업"
        );
    }

    // ── dice_bigram_similarity ────────────────────────────────────────────

    #[test]
    fn identical_strings_score_one() {
        assert_eq!(
            dice_bigram_similarity("청년내일저축계좌", "청년내일저축계좌"),
            1.0
        );
    }

    #[test]
    fn completely_different_strings_score_zero() {
        assert_eq!(dice_bigram_similarity("가나다라", "마바사아"), 0.0);
    }

    /// The labelled pairs of `tests/fixtures/dedupe_20261111`: no false
    /// merge and no miss (the e31d9d4f title-only rule: 4 false merges, 0
    /// misses of 8).
    #[test]
    fn labelled_welfare_youth_pairs_merge_only_the_same_programme() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/dedupe_20261111/welfare_youth_pairs.json"
        ))
        .unwrap();
        let strings = |v: &serde_json::Value| -> Vec<String> {
            match v.as_str() {
                Some("ALL") => mazimi_core::agent::REGION_CODES
                    .iter()
                    .map(|r| r.to_string())
                    .collect(),
                _ => serde_json::from_value(v.clone()).unwrap(),
            }
        };
        let mut wrong = Vec::new();
        for pair in fixture["pairs"].as_array().unwrap() {
            let (w, y) = (&pair["welfare"], &pair["youth"]);
            let cache = vec![youth_cache_entry(
                Uuid::new_v4(),
                y["title"].as_str().unwrap().into(),
                None,
                y["provider"].as_str().map(String::from),
                y["registrar"].as_str().map(String::from),
                Some(strings(&y["regions"])),
            )];
            let merged = find_youth_center_duplicate(
                w["title"].as_str().unwrap(),
                w["ministry"].as_str().unwrap(),
                &strings(&w["regions"]),
                &cache,
            )
            .is_some();
            if merged != pair["same"].as_bool().unwrap() {
                wrong.push(pair["name"].as_str().unwrap().to_string());
            }
        }
        assert!(wrong.is_empty(), "{wrong:?}");
    }

    #[test]
    fn round_numbers_stay_in_the_title_key_and_years_do_not() {
        assert_ne!(
            normalize_title_for_dedup("청년 도약 지원 1차"),
            normalize_title_for_dedup("청년 도약 지원 2차")
        );
        assert_eq!(
            normalize_title_for_dedup("1인가구 2026년 지원"),
            "1인가구지원"
        );
        assert_eq!(normalize_title_for_dedup("20261231 공고"), "20261231공고");
    }

    #[test]
    fn similarity_only_breaks_a_tie() {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        // Both keys equal "청년내일저축계좌"; the closer full title wins.
        let cache = vec![
            youth(
                a,
                "청년내일저축계좌(2026년 하반기)",
                None,
                "보건복지부",
                &[],
            ),
            youth(b, "청년내일저축계좌", None, "보건복지부", &[]),
        ];
        assert_eq!(
            find_youth_center_duplicate("청년내일저축계좌", "보건복지부", &[], &cache)
                .map(|e| e.id),
            Some(b)
        );
        let cache = vec![
            youth(a, "청년내일저축계좌", None, "보건복지부", &[]),
            youth(b, "청년내일저축계좌", None, "보건복지부", &[]),
        ];
        assert!(
            find_youth_center_duplicate("청년내일저축계좌", "보건복지부", &[], &cache).is_none()
        );
    }

    // ── find_youth_center_duplicate ──────────────────────────────────────

    #[test]
    fn finds_high_confidence_duplicate() {
        let id = Uuid::new_v4();
        let cache = vec![
            entry(id, "청년내일저축계좌", Some("https://example.go.kr")),
            entry(
                Uuid::new_v4(),
                "청년 부동산 중개보수 및 이사비 지원사업",
                None,
            ),
        ];
        let dup = find_youth_center_duplicate("청년내일저축계좌", "보건복지부", &[], &cache);
        assert!(dup.is_some());
        assert_eq!(dup.unwrap().id, id);
    }

    #[test]
    fn does_not_match_below_threshold() {
        let cache = vec![entry(
            Uuid::new_v4(),
            "인천형 대중교통비 환급 지원(인천 i-패스)",
            None,
        )];
        let dup = find_youth_center_duplicate(
            "대중교통비 환급 지원(모두의카드)",
            "보건복지부",
            &[],
            &cache,
        );
        assert!(dup.is_none());
    }

    #[test]
    fn empty_cache_never_matches() {
        assert!(find_youth_center_duplicate("청년내일저축계좌", "보건복지부", &[], &[]).is_none());
    }
}

#[cfg(test)]
mod official_url_tests {
    use super::*;

    /// Shorthand for "guard disabled" — matches pre-guard call sites/behavior.
    fn no_shared() -> HashSet<String> {
        HashSet::new()
    }

    // ── is_specific_url ──────────────────────────────────────────────────

    #[test]
    fn bare_homepage_is_not_specific() {
        assert!(!is_specific_url("https://example.go.kr"));
        assert!(!is_specific_url("https://example.go.kr/"));
    }

    #[test]
    fn path_makes_it_specific() {
        assert!(is_specific_url("https://example.go.kr/notice/view.do"));
        assert!(is_specific_url("http://example.go.kr/apply/123"));
    }

    #[test]
    fn query_string_makes_it_specific() {
        assert!(is_specific_url("https://example.go.kr/board/detail?no=7"));
        // Query glued directly onto the bare domain (no slash at all).
        assert!(is_specific_url("https://example.go.kr?no=7"));
    }

    // ── select_official_url ──────────────────────────────────────────────

    #[test]
    fn prefers_specific_aply_url_over_everything() {
        let result = select_official_url(
            Some("https://apply.example.go.kr/apply/123"),
            Some("https://example.go.kr/notice/view.do?id=42"),
            Some("https://example.go.kr/board/detail?no=7"),
            Some("12345"),
            &no_shared(),
        );
        assert_eq!(
            result,
            Some("https://apply.example.go.kr/apply/123".to_string())
        );
    }

    #[test]
    fn falls_back_to_specific_ref_url1_when_aply_is_bare_homepage() {
        let result = select_official_url(
            Some("https://example.go.kr"),
            Some("https://example.go.kr/notice/view.do?id=42"),
            None,
            Some("12345"),
            &no_shared(),
        );
        assert_eq!(
            result,
            Some("https://example.go.kr/notice/view.do?id=42".to_string())
        );
    }

    #[test]
    fn falls_back_to_specific_ref_url2_when_aply_and_ref1_are_bare() {
        let result = select_official_url(
            Some("https://example.go.kr"),
            Some("https://example.go.kr/"),
            Some("https://example.go.kr/board/detail?no=7"),
            Some("12345"),
            &no_shared(),
        );
        assert_eq!(
            result,
            Some("https://example.go.kr/board/detail?no=7".to_string())
        );
    }

    #[test]
    fn falls_back_to_youth_center_detail_page_when_nothing_is_specific() {
        // aply/ref1 are bare homepages and ref2 is absent, but plcyNo is
        // present — the 온통청년 detail page template must win over the bare
        // homepage fallback (this is the ~37% case that used to dead-end on
        // a bare homepage before the plcyNo fallback was added).
        let result = select_official_url(
            Some("https://example.go.kr"),
            Some("https://example.go.kr/"),
            None,
            Some("12345"),
            &no_shared(),
        );
        assert_eq!(
            result,
            Some(
                "https://www.youthcenter.go.kr/youthPolicy/ythPlcyTotalSearch/ythPlcyDetail/12345"
                    .to_string()
            )
        );
    }

    #[test]
    fn youth_center_detail_page_wins_over_bare_fallback_even_with_no_ref_urls() {
        // No ref URLs at all, only a bare aply homepage + plcyNo. The detail
        // page template still takes priority over falling back to the bare
        // aply URL.
        let result = select_official_url(
            Some("https://example.go.kr/"),
            None,
            None,
            Some("999"),
            &no_shared(),
        );
        assert_eq!(
            result,
            Some(
                "https://www.youthcenter.go.kr/youthPolicy/ythPlcyTotalSearch/ythPlcyDetail/999"
                    .to_string()
            )
        );
    }

    #[test]
    fn falls_back_to_bare_aply_url_when_nothing_specific_and_no_plcy_no() {
        let result = select_official_url(
            Some("https://example.go.kr"),
            Some("https://example.go.kr/"),
            None,
            None,
            &no_shared(),
        );
        assert_eq!(result, Some("https://example.go.kr".to_string()));
    }

    #[test]
    fn falls_back_to_ref_url1_when_aply_is_empty_and_no_plcy_no() {
        let result = select_official_url(
            Some(""),
            Some("https://example.go.kr"),
            None,
            None,
            &no_shared(),
        );
        assert_eq!(result, Some("https://example.go.kr".to_string()));
    }

    #[test]
    fn blank_plcy_no_is_treated_as_absent() {
        // Whitespace-only plcyNo must not produce a bogus detail URL; it
        // should fall through to the bare-URL fallback tier.
        let result = select_official_url(
            Some("https://example.go.kr"),
            None,
            None,
            Some("   "),
            &no_shared(),
        );
        assert_eq!(result, Some("https://example.go.kr".to_string()));
    }

    #[test]
    fn returns_none_when_everything_is_empty_or_missing() {
        assert_eq!(
            select_official_url(None, None, None, None, &no_shared()),
            None
        );
        assert_eq!(
            select_official_url(Some(""), Some(""), Some(""), Some(""), &no_shared()),
            None
        );
    }

    #[test]
    fn rejects_non_http_schemes_and_plain_text() {
        // Non-http scheme and plain Korean text (e.g. "전화문의") must be
        // discarded entirely, even though they're non-empty. With no plcyNo
        // either, the result is None.
        let result = select_official_url(
            Some("ftp://example.com/file"),
            Some("전화문의"),
            None,
            None,
            &no_shared(),
        );
        assert_eq!(result, None);
    }

    #[test]
    fn bare_fallback_ignores_ref_url2() {
        // When only ref_url2 is a non-empty candidate (bare or otherwise),
        // aply/ref1 are absent, and there's no plcyNo, the spec says the
        // bare fallback should NOT reach into ref_url2 — but a *specific*
        // ref_url2 still wins earlier in the priority chain. This test
        // covers the case where ref_url2 is itself just a bare homepage: no
        // candidate should be returned.
        let result = select_official_url(
            None,
            None,
            Some("https://example.go.kr"),
            None,
            &no_shared(),
        );
        assert_eq!(result, None);
    }

    // ── shared/untrusted refUrl guard ─────────────────────────────────────

    #[test]
    fn shared_specific_ref_url1_is_skipped_in_favor_of_plcy_no_fallback() {
        // The real-world bug this guards against: two different plcyNo
        // records both carry the same (specific) refUrlAddr1, e.g. a
        // copy-pasted 경남 detail page. That refUrl must not be trusted as
        // this record's deep link — the plcyNo detail-page fallback wins
        // instead, even though ref1 passes is_specific_url.
        let shared: HashSet<String> = ["https://example.go.kr/policy?no=1816".to_string()].into();
        let result = select_official_url(
            None,
            Some("https://example.go.kr/policy?no=1816"),
            None,
            Some("12345"),
            &shared,
        );
        assert_eq!(
            result,
            Some(
                "https://www.youthcenter.go.kr/youthPolicy/ythPlcyTotalSearch/ythPlcyDetail/12345"
                    .to_string()
            )
        );
    }

    #[test]
    fn shared_specific_ref_url1_falls_through_to_specific_ref_url2_when_ref2_is_untainted() {
        // ref1 is shared/untrusted but ref2 is a distinct, non-shared,
        // specific URL — ref2 should still win over jumping straight to the
        // plcyNo fallback.
        let shared: HashSet<String> = ["https://example.go.kr/policy?no=1816".to_string()].into();
        let result = select_official_url(
            None,
            Some("https://example.go.kr/policy?no=1816"),
            Some("https://example.go.kr/board/detail?no=7"),
            Some("12345"),
            &shared,
        );
        assert_eq!(
            result,
            Some("https://example.go.kr/board/detail?no=7".to_string())
        );
    }

    #[test]
    fn shared_ref_url_guard_ignores_unrelated_urls() {
        // Regression safety: a shared set containing some other URL must not
        // affect selection of an unrelated, non-shared ref1.
        let shared: HashSet<String> = ["https://other.go.kr/detail?no=1".to_string()].into();
        let result = select_official_url(
            None,
            Some("https://example.go.kr/notice/view.do?id=42"),
            None,
            Some("12345"),
            &shared,
        );
        assert_eq!(
            result,
            Some("https://example.go.kr/notice/view.do?id=42".to_string())
        );
    }

    #[test]
    fn shared_ref_url_guard_normalizes_trailing_slash_before_matching() {
        // collect_shared_youth_center_ref_urls normalizes with a trailing
        // slash stripped; select_official_url must compare using the same
        // normalization so a trailing-slash mismatch doesn't let a shared
        // URL slip through.
        let shared: HashSet<String> = ["https://example.go.kr/policy/1816".to_string()].into();
        let result = select_official_url(
            None,
            Some("https://example.go.kr/policy/1816/"),
            None,
            Some("12345"),
            &shared,
        );
        assert_eq!(
            result,
            Some(
                "https://www.youthcenter.go.kr/youthPolicy/ythPlcyTotalSearch/ythPlcyDetail/12345"
                    .to_string()
            )
        );
    }

    #[test]
    fn aply_url_is_never_guarded_even_if_present_in_shared_set() {
        // aplyUrlAddr is the provider's own authoritative apply link, never a
        // copy-pasted reference — the shared-refUrl guard must not apply to
        // it even if (hypothetically) the exact same string shows up in the
        // shared set.
        let shared: HashSet<String> = ["https://apply.example.go.kr/apply/123".to_string()].into();
        let result = select_official_url(
            Some("https://apply.example.go.kr/apply/123"),
            None,
            None,
            Some("12345"),
            &shared,
        );
        assert_eq!(
            result,
            Some("https://apply.example.go.kr/apply/123".to_string())
        );
    }

    // ── collect_shared_youth_center_ref_urls ──────────────────────────────

    fn raw_record(source_id: &str, ref1: Option<&str>, ref2: Option<&str>) -> RawRecord {
        RawRecord {
            source_id: source_id.to_string(),
            payload: serde_json::json!({
                "refUrlAddr1": ref1,
                "refUrlAddr2": ref2,
            }),
            content_hash: "irrelevant".to_string(),
        }
    }

    #[test]
    fn flags_ref_url_shared_across_two_distinct_plcy_no() {
        let records = vec![
            raw_record("111", Some("https://example.go.kr/policy?no=1816"), None),
            raw_record("222", Some("https://example.go.kr/policy?no=1816"), None),
        ];
        let shared = collect_shared_youth_center_ref_urls(&records);
        assert!(shared.contains("https://example.go.kr/policy?no=1816"));
    }

    #[test]
    fn flags_ref_url_shared_across_ref1_and_ref2_slots() {
        // The bug isn't tied to a specific field — the same URL value can
        // land in refUrlAddr1 on one record and refUrlAddr2 on another.
        let records = vec![
            raw_record("111", Some("https://example.go.kr/policy?no=1816"), None),
            raw_record("222", None, Some("https://example.go.kr/policy?no=1816")),
        ];
        let shared = collect_shared_youth_center_ref_urls(&records);
        assert!(shared.contains("https://example.go.kr/policy?no=1816"));
    }

    #[test]
    fn does_not_flag_ref_url_appearing_once_per_record_even_in_both_slots() {
        // Same plcyNo (single distinct owner) using the identical URL in
        // both ref1 and ref2 is not the bug pattern — only 2+ *distinct*
        // records sharing a URL should be flagged.
        let records = vec![raw_record(
            "111",
            Some("https://example.go.kr/policy?no=1816"),
            Some("https://example.go.kr/policy?no=1816"),
        )];
        let shared = collect_shared_youth_center_ref_urls(&records);
        assert!(shared.is_empty());
    }

    #[test]
    fn does_not_flag_distinct_ref_urls() {
        let records = vec![
            raw_record("111", Some("https://example.go.kr/policy?no=1816"), None),
            raw_record("222", Some("https://example.go.kr/policy?no=2000"), None),
        ];
        let shared = collect_shared_youth_center_ref_urls(&records);
        assert!(shared.is_empty());
    }

    #[test]
    fn does_not_flag_shared_bare_homepage() {
        // Many unrelated policies legitimately share the same provider
        // homepage as refUrlAddr1 — that's not the copy-paste bug and must
        // not be flagged (it wouldn't win the "specific" priority tier
        // anyway, but keeping it out of the set keeps the guard's intent
        // clear).
        let records = vec![
            raw_record("111", Some("https://example.go.kr"), None),
            raw_record("222", Some("https://example.go.kr"), None),
        ];
        let shared = collect_shared_youth_center_ref_urls(&records);
        assert!(shared.is_empty());
    }

    #[test]
    fn matches_shared_url_regardless_of_trailing_slash_variance() {
        let records = vec![
            raw_record("111", Some("https://example.go.kr/policy/1816"), None),
            raw_record("222", Some("https://example.go.kr/policy/1816/"), None),
        ];
        let shared = collect_shared_youth_center_ref_urls(&records);
        assert!(shared.contains("https://example.go.kr/policy/1816"));
    }

    #[test]
    fn empty_batch_yields_empty_set() {
        assert!(collect_shared_youth_center_ref_urls(&[]).is_empty());
    }
}

/// What the first welfare runs after the duplicate-lifecycle fix will do,
/// computed from a production export with the pipeline's own matchers.
/// Nothing is written anywhere.
///
/// `DUP_DRY_RUN=/path/export.jsonl cargo test -p mazimi-worker --lib
/// duplicate_lifecycle_dry_run -- --ignored --nocapture`; the export lines
/// are produced by the SQL in the duplicate-lifecycle handoff.
#[cfg(test)]
mod duplicate_dry_run {
    use super::*;
    use std::collections::BTreeMap;

    #[derive(serde::Deserialize)]
    #[serde(tag = "kind", rename_all = "snake_case")]
    enum Line {
        Welfare {
            source: String,
            source_id: String,
            payload: serde_json::Value,
            program: Option<Current>,
        },
        Youth {
            id: Uuid,
            title: String,
            official_url: Option<String>,
            #[serde(default)]
            provider: Option<String>,
            /// `raw_payload->>'rgtrHghrkInstCdNm'`.
            #[serde(default)]
            registrar: Option<String>,
            #[serde(default)]
            regions: Option<Vec<String>>,
        },
        Gov {
            id: Uuid,
            title: String,
            provider: Option<String>,
            regions: Option<Vec<String>>,
            applicants: Option<Vec<String>>,
        },
    }

    #[derive(serde::Deserialize)]
    struct Current {
        id: Uuid,
        is_active: bool,
        status: String,
        reason: Option<String>,
        duplicate_of: Option<String>,
        /// LEAST(benefit_valid_until, deadline_at, application_end_at)
        ends: Option<DateTime<Utc>>,
        visible: bool,
    }

    #[test]
    #[ignore = "needs DUP_DRY_RUN export"]
    fn duplicate_lifecycle_dry_run() {
        let Ok(path) = std::env::var("DUP_DRY_RUN") else {
            return;
        };
        let mut youth = Vec::new();
        let mut gov = Vec::new();
        let mut welfare = Vec::new();
        for line in std::fs::read_to_string(path).unwrap().lines() {
            match serde_json::from_str::<Line>(line).expect("export line is JSON") {
                Line::Youth {
                    id,
                    title,
                    official_url,
                    provider,
                    registrar,
                    regions,
                } => youth.push(youth_cache_entry(
                    id,
                    title,
                    official_url,
                    provider,
                    registrar,
                    regions,
                )),
                Line::Gov {
                    id,
                    title,
                    provider,
                    regions,
                    applicants,
                } => gov.extend(gov_cache_entry(id, &title, provider, regions, applicants)),
                Line::Welfare {
                    source,
                    source_id,
                    payload,
                    program,
                } => welfare.push((source, source_id, payload, program)),
            }
        }
        let national = DedupCaches {
            youth: Some(youth),
            gov: Some(gov),
            recheck: true,
        };
        let local = DedupCaches {
            youth: None,
            gov: national.gov.as_ref().map(|g| {
                g.iter()
                    .map(|e| GovCacheEntry {
                        id: e.id,
                        title_key: e.title_key.clone(),
                        provider: e.provider.clone(),
                        regions: e.regions.clone(),
                    })
                    .collect()
            }),
            recheck: true,
        };
        let now = Utc::now();
        let mut counts: BTreeMap<String, i64> = BTreeMap::new();
        for (src, source_id, payload, cur) in &welfare {
            let caches = if src == "national_welfare" {
                &national
            } else {
                &local
            };
            let mut norm = normalize(src, source_id, payload, &HashSet::new());
            if validate_normalized_program(&mut norm).is_err() {
                *counts.entry(format!("{src}: rejected")).or_default() += 1;
                continue;
            }
            let dup = caches.find_duplicate(src, payload, &norm);
            let change = match (cur, dup) {
                (None, Some(d)) => format!("create retired ({})", d.item_status()),
                (None, None) => "create live".into(),
                (Some(c), Some(d)) => {
                    let target = d.id().to_string();
                    let automatic =
                        matches!(c.reason.as_deref(), Some("expired" | "source_removed"));
                    if c.is_active {
                        format!(
                            "retire live row ({}){}",
                            d.item_status(),
                            if c.visible { ", was visible" } else { "" }
                        )
                    } else if automatic {
                        format!("tag closed copy ({})", d.item_status())
                    } else if c.reason.as_deref() == Some("duplicate")
                        && c.duplicate_of.as_deref() != Some(target.as_str())
                    {
                        format!("retarget ({})", d.item_status())
                    } else {
                        "no change".into()
                    }
                }
                (Some(c), None) => {
                    if !c.is_active
                        && c.reason.as_deref() == Some("duplicate")
                        && c.duplicate_of.is_some()
                    {
                        let open = c.status != "closed" && c.ends.is_none_or(|e| e > now);
                        if open {
                            "restore live".into()
                        } else {
                            "restore as expired".into()
                        }
                    } else {
                        "no change".into()
                    }
                }
            };
            let id = cur.as_ref().map(|c| c.id.to_string()).unwrap_or_default();
            if change != "no change" {
                println!("{src}\t{change}\t{source_id}\t{id}\t{}", norm.title);
            }
            *counts.entry(format!("{src}: {change}")).or_default() += 1;
        }
        for (key, value) in &counts {
            println!("{value:>7}  {key}");
        }
    }
}
