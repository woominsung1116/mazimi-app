//! Source freshness and material-change workflow.
//!
//! Fetching and publication are intentionally separate concerns. This module
//! records hash-only checks, updates many-to-many source monitors, and places
//! reversible publication holds on every affected program. It never decides
//! that changed source prose is safe to republish; that remains an admin task.

use anyhow::{Context, Result};
use chrono::Duration;
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::operations::{self, EventCommand, Severity};

const MAX_ERROR_CHARS: usize = 2_000;
const FAILURE_HOLD_THRESHOLD: i32 = 3;
/// A monitor without a verified hash may adopt its first automated fetch as
/// the baseline only while the human verification is this recent. Older
/// facts could already be stale, so the page goes to human review instead.
const BASELINE_TRUST_WINDOW_SECONDS: i64 = 7 * 24 * 3_600;
/// Operators are warned this long before a manual re-verification deadline
/// on a domain the worker is not allowed to fetch.
const UPCOMING_REVIEW_WINDOW_SECONDS: i64 = 7 * 24 * 3_600;
const UPCOMING_REVIEW_SWEEP_LIMIT: i64 = 1_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentChange {
    BaselineEstablished,
    Unchanged,
    Changed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnavailableKind {
    Blocked,
    Redirected,
    NotFound,
}

impl UnavailableKind {
    fn check_kind(self) -> &'static str {
        match self {
            Self::Blocked => "blocked",
            Self::Redirected => "redirected",
            Self::NotFound => "not_found",
        }
    }

    fn hold_reason(self) -> &'static str {
        match self {
            Self::Blocked => "access_revoked",
            Self::Redirected => "redirected",
            Self::NotFound => "not_found",
        }
    }
}

#[derive(Debug)]
pub struct PageCheck<'a> {
    pub candidate_id: Uuid,
    pub previous_content_hash: Option<&'a str>,
    pub content_hash: &'a str,
    pub page_title: Option<&'a str>,
    pub review_text: Option<&'a str>,
    pub etag: Option<&'a str>,
    pub last_modified: Option<&'a str>,
    pub http_status: i32,
    pub fetched_bytes: i32,
    pub recheck_interval_seconds: i32,
    /// The extracted text is too short to stand in for the offer (script
    /// shell, error page). It may match a verified hash, but it can never
    /// become a baseline by itself.
    pub thin_content: bool,
}

#[derive(Debug, Default)]
pub struct FreshnessSummary {
    pub candidates_held: usize,
    pub programs_held: u64,
}

#[derive(Debug, Default)]
pub struct MaintenanceSummary {
    pub schedules_reconciled: u64,
    pub unmonitored_programs_held: usize,
    pub overdue_candidates_held: usize,
    pub overdue_programs_held: u64,
    pub upcoming_reviews_flagged: usize,
    /// Open source events resolved because every monitor of their source
    /// candidate is retired.
    pub retired_source_events_resolved: u64,
}

pub fn freshness_policy_from_env() -> Result<(Duration, i64)> {
    let grace_hours = parse_bounded_env("SOURCE_REVIEW_GRACE_HOURS", 72, 1, 720)?;
    let sweep_limit = parse_bounded_env("SOURCE_REVIEW_SWEEP_LIMIT", 100, 1, 1_000)?;
    Ok((Duration::hours(grace_hours), sweep_limit))
}

pub fn classify_content_change(previous: Option<&str>, current: &str) -> ContentChange {
    match previous {
        None => ContentChange::BaselineEstablished,
        Some(previous) if previous == current => ContentChange::Unchanged,
        Some(_) => ContentChange::Changed,
    }
}

/// Runs the bounded, non-destructive parts of source operations in one place.
/// It may fail closed (hide an official program whose monitoring relation is
/// missing), but it never republishes, merges, or deletes a program.
pub async fn run_source_maintenance(
    pool: &PgPool,
    grace: Duration,
    overdue_limit: i64,
) -> Result<MaintenanceSummary> {
    let schedules_reconciled = reconcile_monitor_schedules(pool).await?;
    let unmonitored_programs_held = hold_unmonitored_official_programs(pool).await?;
    let overdue = hold_overdue_sources(pool, grace, overdue_limit).await?;
    let relabelled = relabel_unreadable_manual_holds(pool).await?;
    if relabelled > 0 {
        tracing::info!(
            event = "source_monitoring.manual_holds_relabelled",
            relabelled,
            "overdue holds of unreadable manual corporate sources carry their reason"
        );
    }
    let upcoming_reviews_flagged = warn_upcoming_manual_reviews(pool).await?;
    let retired_source_events_resolved = resolve_retired_source_events(pool).await?;

    Ok(MaintenanceSummary {
        schedules_reconciled,
        unmonitored_programs_held,
        overdue_candidates_held: overdue.candidates_held,
        overdue_programs_held: overdue.programs_held,
        upcoming_reviews_flagged,
        retired_source_events_resolved,
    })
}

/// A source whose monitors are all retired (page gone, provider withdrew,
/// retired by an operator or a data fix) has nothing left to review: no
/// programme of it can be re-verified or republished through it. Its open
/// `source_review:` / `source_review_upcoming:` events are resolved here, so
/// a retirement made outside the review API (a migration, for example) does
/// not leave them open forever. A candidate without any monitor is left
/// alone: that is a missing relation, not a retirement.
pub async fn resolve_retired_source_events(pool: &PgPool) -> Result<u64> {
    Ok(sqlx::query(
        "UPDATE operational_events event
         SET status = 'resolved', resolved_at = now(),
             resolution_note = 'Automatically resolved: every source monitor of this candidate is retired',
             delivery_claim_token = NULL, delivery_claimed_until = NULL,
             updated_at = now()
         FROM (
             SELECT monitor.candidate_id
             FROM program_source_monitors monitor
             GROUP BY monitor.candidate_id
             HAVING bool_and(monitor.verification_state = 'retired')
         ) retired
         WHERE event.status <> 'resolved'
           AND event.fingerprint IN ('source_review:' || retired.candidate_id::text,
                                     'source_review_upcoming:' || retired.candidate_id::text)",
    )
    .execute(pool)
    .await
    .context("failed to resolve events of retired sources")?
    .rows_affected())
}

/// Seeded and manually curated benefits live on domains the worker may not
/// fetch, so their only re-verification path is a human. Without this warning
/// the first signal an operator receives is the hold that already hid the
/// benefit. One open warning per source; it closes itself once the issuing
/// condition below no longer holds: no published program on that source is
/// due inside the window, or the domain became crawlable and the worker
/// re-verifies it on its own.
pub async fn warn_upcoming_manual_reviews(pool: &PgPool) -> Result<usize> {
    // Every source the overdue sweep can hold is warned about first,
    // evidence_auto ones included: evidence moves the deadline only on a
    // confirmation, so a warning means it has not confirmed for a while.
    let mut tx = pool.begin().await?;
    let window_seconds = UPCOMING_REVIEW_WINDOW_SECONDS as f64;
    // Once the source is overdue (`source_review:<candidate>` open as
    // `source_verification_overdue`), that incident carries the action; the
    // warning is resolved as superseded and not raised again while it is
    // open.
    sqlx::query(
        "UPDATE operational_events event
         SET status = 'resolved', resolved_at = now(),
             resolution_note = 'Automatically resolved: superseded by the open source_verification_overdue incident of this source',
             delivery_claim_token = NULL, delivery_claimed_until = NULL,
             updated_at = now()
         WHERE event.event_type = 'source_review_upcoming'
           AND event.status <> 'resolved'
           AND EXISTS (
               SELECT 1 FROM operational_events overdue
               WHERE overdue.fingerprint = 'source_review:' || substr(event.fingerprint, length('source_review_upcoming:') + 1)
                 AND overdue.event_type = 'source_verification_overdue'
                 AND overdue.status <> 'resolved'
           )",
    )
    .execute(&mut *tx)
    .await
    .context("failed to resolve superseded upcoming-review warnings")?;
    sqlx::query(
        "UPDATE operational_events event
         SET status = 'resolved', resolved_at = now(),
             resolution_note = 'Automatically resolved: no published program on this non-crawlable source is due within the warning window',
             delivery_claim_token = NULL, delivery_claimed_until = NULL,
             updated_at = now()
         WHERE event.event_type = 'source_review_upcoming'
           AND event.status <> 'resolved'
           AND NOT EXISTS (
               SELECT 1
               FROM program_source_monitors monitor
               JOIN benefit_source_candidates candidate ON candidate.id = monitor.candidate_id
               JOIN benefit_source_domains domain ON domain.id = candidate.domain_id
               JOIN programs program ON program.id = monitor.program_id
               WHERE 'source_review_upcoming:' || monitor.candidate_id::text = event.fingerprint
                 AND monitor.verification_state = 'current'
                 AND program.is_active = true
                 AND monitor.review_due_at <= now() + make_interval(secs => $1)
                 AND NOT COALESCE(
                     domain.crawl_allowed
                     AND domain.compliance_status = 'approved'
                     AND domain.robots_status = 'allowed'
                     AND domain.approval_expires_at > now(),
                     false
                 )
           )",
    )
    .bind(window_seconds)
    .execute(&mut *tx)
    .await
    .context("failed to close stale upcoming-review warnings")?;

    let upcoming = sqlx::query_as::<_, (Uuid, i64, chrono::DateTime<chrono::Utc>)>(
        "SELECT monitor.candidate_id,
                count(DISTINCT monitor.program_id)::bigint AS programs,
                min(monitor.review_due_at) AS review_due_at
         FROM program_source_monitors monitor
         JOIN benefit_source_candidates candidate ON candidate.id = monitor.candidate_id
         JOIN benefit_source_domains domain ON domain.id = candidate.domain_id
         JOIN programs program ON program.id = monitor.program_id
         WHERE monitor.verification_state = 'current'
           AND monitor.verification_mode <> 'door'
           AND program.is_active = true
           AND monitor.review_due_at <= now() + make_interval(secs => $1)
           AND NOT COALESCE(
               domain.crawl_allowed
               AND domain.compliance_status = 'approved'
               AND domain.robots_status = 'allowed'
               AND domain.approval_expires_at > now(),
               false
           )
           AND NOT EXISTS (
               SELECT 1 FROM operational_events event
               WHERE event.status <> 'resolved'
                 AND (event.fingerprint = 'source_review_upcoming:' || monitor.candidate_id::text
                      OR (event.fingerprint = 'source_review:' || monitor.candidate_id::text
                          AND event.event_type = 'source_verification_overdue'))
           )
         GROUP BY monitor.candidate_id
         ORDER BY min(monitor.review_due_at), monitor.candidate_id
         LIMIT $2",
    )
    .bind(window_seconds)
    .bind(UPCOMING_REVIEW_SWEEP_LIMIT)
    .fetch_all(&mut *tx)
    .await
    .context("failed to select upcoming manual source reviews")?;

    for (candidate_id, programs, review_due_at) in &upcoming {
        let fingerprint = format!("source_review_upcoming:{candidate_id}");
        let subject_id = candidate_id.to_string();
        let hours_remaining = (*review_due_at - chrono::Utc::now()).num_hours();
        operations::record_event_tx(
            &mut tx,
            &EventCommand {
                fingerprint: &fingerprint,
                severity: Severity::Warning,
                component: "source_monitoring",
                event_type: "source_review_upcoming",
                subject_type: Some("benefit_source_candidate"),
                subject_id: Some(&subject_id),
                message: "An official benefit source on a non-crawlable domain needs manual re-verification within 7 days or its programs will be hidden",
                details: serde_json::json!({
                    "programs": programs,
                    "review_due_at": review_due_at,
                    "hours_remaining": hours_remaining,
                    "action": "PUT /api/v1/admin/programs/{program_id}/source-verification or /api/v1/admin/programs/source-verification/batch"
                }),
            },
        )
        .await?;
    }
    tx.commit().await?;
    Ok(upcoming.len())
}

/// Keeps candidate-level scheduling aligned with the strictest active monitor.
/// The expression only moves a check earlier, so a bookkeeping repair can
/// never silently extend a program's verified lifetime.
async fn reconcile_monitor_schedules(pool: &PgPool) -> Result<u64> {
    Ok(sqlx::query(
        "WITH desired AS (
             SELECT candidate_id,
                    min(recheck_interval_seconds)::int AS interval_seconds,
                    min(review_due_at) AS review_due_at
             FROM program_source_monitors
             WHERE verification_state = 'current'
             GROUP BY candidate_id
         )
         UPDATE benefit_source_candidates candidate
         SET recheck_interval_seconds = desired.interval_seconds,
             next_check_at = LEAST(
                 COALESCE(candidate.next_check_at, desired.review_due_at),
                 desired.review_due_at
             ),
             updated_at = now()
         FROM desired
         WHERE candidate.id = desired.candidate_id
           AND candidate.status = 'approved'
           AND (
               candidate.recheck_interval_seconds IS DISTINCT FROM desired.interval_seconds
               OR candidate.next_check_at IS NULL
               OR candidate.next_check_at > desired.review_due_at
           )",
    )
    .execute(pool)
    .await
    .context("failed to reconcile source-monitor schedules")?
    .rows_affected())
}

/// A raw SQL/admin write can otherwise mark an official program current
/// without creating its monitor. Fail closed and create a durable incident;
/// recovery remains a human decision because the worker does not know whether
/// the original publication state was valid.
async fn hold_unmonitored_official_programs(pool: &PgPool) -> Result<usize> {
    let mut tx = pool.begin().await?;
    let program_ids = sqlx::query_scalar::<_, Uuid>(
        "UPDATE programs program
         SET is_active = false,
             program_status = 'review_required',
             source_health = 'review_due',
             source_change_detected_at = COALESCE(source_change_detected_at, now()),
             updated_at = now()
         WHERE program.source_type = 'corporate_official'
           AND program.is_active = true
           AND NOT EXISTS (
               SELECT 1
               FROM program_source_monitors monitor
               WHERE monitor.program_id = program.id
                 AND monitor.verification_state = 'current'
           )
         RETURNING program.id",
    )
    .fetch_all(&mut *tx)
    .await
    .context("failed to hold unmonitored official programs")?;

    for program_id in &program_ids {
        let fingerprint = format!("source_integrity:unmonitored:{program_id}");
        let subject_id = program_id.to_string();
        operations::record_event_tx(
            &mut tx,
            &EventCommand {
                fingerprint: &fingerprint,
                severity: Severity::Error,
                component: "source_monitoring",
                event_type: "official_program_unmonitored",
                subject_type: Some("program"),
                subject_id: Some(&subject_id),
                message: "An active official benefit had no current source monitor and was held",
                details: serde_json::json!({}),
            },
        )
        .await?;
    }

    tx.commit().await?;
    Ok(program_ids.len())
}

pub async fn record_page_check(pool: &PgPool, check: PageCheck<'_>) -> Result<ContentChange> {
    let mut tx = pool.begin().await?;
    // Compare against the last VERIFIED facts, never yesterday's unverified
    // fetch. Otherwise a changed page seen twice would silently approve itself.
    let incompatible = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (SELECT 1 FROM program_source_monitors WHERE candidate_id=$1
         AND verification_state <> 'retired' AND
         (verified_content_hash IS DISTINCT FROM $2 AND
          (verified_content_hash IS NOT NULL OR verification_state <> 'current')))",
    )
    .bind(check.candidate_id)
    .bind(check.content_hash)
    .fetch_one(&mut *tx)
    .await?;
    let verified_match = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (SELECT 1 FROM program_source_monitors WHERE candidate_id=$1
         AND verification_state <> 'retired' AND verified_content_hash=$2)",
    )
    .bind(check.candidate_id)
    .bind(check.content_hash)
    .fetch_one(&mut *tx)
    .await?;
    // A hash-less monitor (seeded or manually curated) is allowed to adopt the
    // first automated fetch only while its human verification is recent.
    let stale_baseline = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (SELECT 1 FROM program_source_monitors WHERE candidate_id=$1
         AND verification_state <> 'retired' AND verified_content_hash IS NULL
         AND verified_at < now() - make_interval(secs => $2))",
    )
    .bind(check.candidate_id)
    .bind(BASELINE_TRUST_WINDOW_SECONDS as f64)
    .fetch_one(&mut *tx)
    .await?;
    let mut baseline_block_reason: Option<&str> = None;
    let classification = if incompatible {
        ContentChange::Changed
    } else if verified_match {
        ContentChange::Unchanged
    } else {
        match classify_content_change(check.previous_content_hash, check.content_hash) {
            ContentChange::BaselineEstablished if check.thin_content => {
                baseline_block_reason =
                    Some("extracted text is too short to serve as a verified baseline");
                ContentChange::Changed
            }
            ContentChange::BaselineEstablished if stale_baseline => {
                baseline_block_reason = Some(
                    "human verification predates the first automated fetch; confirm the baseline",
                );
                ContentChange::Changed
            }
            other => other,
        }
    };
    let check_id = insert_check(
        &mut tx,
        CheckRecord {
            candidate_id: check.candidate_id,
            check_kind: match classification {
                ContentChange::BaselineEstablished => "initial",
                ContentChange::Unchanged => "unchanged",
                ContentChange::Changed => "changed",
            },
            previous_content_hash: check.previous_content_hash,
            content_hash: Some(check.content_hash),
            http_status: Some(check.http_status),
            fetched_bytes: Some(check.fetched_bytes),
            error_summary: baseline_block_reason,
            requires_review: classification == ContentChange::Changed,
        },
    )
    .await?;

    if classification == ContentChange::Changed {
        sqlx::query(
            "UPDATE benefit_source_candidates
             SET status = 'change_review', previous_content_hash = $2,
                 content_hash = $3, page_title = $4, raw_text = $5,
                 etag = $6, last_modified = $7, http_status = $8,
                 fetched_bytes = $9, next_check_at = now() + interval '1 day',
                 consecutive_failures = 0, last_error = NULL,
                 last_successful_check_at = now(), last_checked_at = now(),
                 updated_at = now()
             WHERE id = $1",
        )
        .bind(check.candidate_id)
        .bind(check.previous_content_hash)
        .bind(check.content_hash)
        .bind(check.page_title)
        .bind(check.review_text)
        .bind(check.etag)
        .bind(check.last_modified)
        .bind(check.http_status)
        .bind(check.fetched_bytes)
        .execute(&mut *tx)
        .await?;
        let programs_held = place_holds(
            &mut tx,
            check.candidate_id,
            check_id,
            "content_changed",
            Some(check.content_hash),
            "change_detected",
            None,
        )
        .await?;
        record_source_review_event(
            &mut tx,
            check.candidate_id,
            "source_content_changed",
            if baseline_block_reason.is_some() {
                "An official benefit source needs a human-confirmed baseline before automated monitoring can trust it"
            } else {
                "An official benefit source changed and requires review"
            },
            programs_held,
        )
        .await?;
    } else {
        sqlx::query(
            "UPDATE benefit_source_candidates
             SET status = 'approved', previous_content_hash = NULL,
                 content_hash = $2, page_title = $3, raw_text = NULL,
                 extracted_payload = NULL, etag = $4, last_modified = $5,
                 http_status = $6, fetched_bytes = $7,
                 next_check_at = now() + make_interval(secs => $8),
                 consecutive_failures = 0, last_error = NULL,
                 last_successful_check_at = now(), last_checked_at = now(),
                 updated_at = now()
             WHERE id = $1",
        )
        .bind(check.candidate_id)
        .bind(check.content_hash)
        .bind(check.page_title)
        .bind(check.etag)
        .bind(check.last_modified)
        .bind(check.http_status)
        .bind(check.fetched_bytes)
        .bind(check.recheck_interval_seconds as f64)
        .execute(&mut *tx)
        .await?;
        mark_monitors_current(
            &mut tx,
            check.candidate_id,
            Some(check.content_hash),
            check.recheck_interval_seconds,
        )
        .await?;
        refresh_program_health(&mut tx, check.candidate_id).await?;
        restore_verified_programs(&mut tx, check.candidate_id).await?;
    }

    tx.commit().await?;
    Ok(classification)
}

pub async fn record_not_modified(
    pool: &PgPool,
    candidate_id: Uuid,
    content_hash: Option<&str>,
    recheck_interval_seconds: i32,
) -> Result<()> {
    let mut tx = pool.begin().await?;
    insert_check(
        &mut tx,
        CheckRecord {
            candidate_id,
            check_kind: "not_modified",
            previous_content_hash: content_hash,
            content_hash,
            http_status: Some(304),
            fetched_bytes: None,
            error_summary: None,
            requires_review: false,
        },
    )
    .await?;
    sqlx::query(
        "UPDATE benefit_source_candidates
         SET status = 'approved', raw_text = NULL, extracted_payload = NULL,
             http_status = 304,
             next_check_at = now() + make_interval(secs => $2),
             consecutive_failures = 0, last_error = NULL,
             last_successful_check_at = now(), last_checked_at = now(),
             updated_at = now()
         WHERE id = $1",
    )
    .bind(candidate_id)
    .bind(recheck_interval_seconds as f64)
    .execute(&mut *tx)
    .await?;
    mark_monitors_current(
        &mut tx,
        candidate_id,
        content_hash,
        recheck_interval_seconds,
    )
    .await?;
    refresh_program_health(&mut tx, candidate_id).await?;
    restore_verified_programs(&mut tx, candidate_id).await?;
    tx.commit().await?;
    Ok(())
}

/// Records a transient request failure. Two retries keep the published facts
/// available; the third consecutive failure becomes a reversible source hold.
pub async fn record_fetch_failure(pool: &PgPool, candidate_id: Uuid, reason: &str) -> Result<bool> {
    let reason = truncate(reason);
    let mut tx = pool.begin().await?;
    let failures = sqlx::query_scalar::<_, i32>(
        "UPDATE benefit_source_candidates
         SET consecutive_failures = LEAST(consecutive_failures + 1, 100),
             last_error = $2, last_checked_at = now(), updated_at = now()
         WHERE id = $1
         RETURNING consecutive_failures",
    )
    .bind(candidate_id)
    .bind(&reason)
    .fetch_one(&mut *tx)
    .await?;
    let held = failures >= FAILURE_HOLD_THRESHOLD;
    let check_id = insert_check(
        &mut tx,
        CheckRecord {
            candidate_id,
            check_kind: "fetch_error",
            previous_content_hash: None,
            content_hash: None,
            http_status: None,
            fetched_bytes: None,
            error_summary: Some(&reason),
            requires_review: held,
        },
    )
    .await?;

    if held {
        sqlx::query(
            "UPDATE benefit_source_candidates
             SET status = 'unreachable_review', next_check_at = now() + interval '1 day',
                 updated_at = now()
             WHERE id = $1",
        )
        .bind(candidate_id)
        .execute(&mut *tx)
        .await?;
        let programs_held = place_holds(
            &mut tx,
            candidate_id,
            check_id,
            "source_unreachable",
            None,
            "unreachable",
            Some(&reason),
        )
        .await?;
        record_source_review_event(
            &mut tx,
            candidate_id,
            "source_unreachable",
            "An official benefit source failed three consecutive checks",
            programs_held,
        )
        .await?;
    } else {
        // 6 hours after the first failure, 12 after the second.
        let retry_seconds = 21_600_i32.saturating_mul(failures);
        sqlx::query(
            "UPDATE benefit_source_candidates
             SET status = 'approved',
                 next_check_at = now() + make_interval(secs => $2),
                 updated_at = now()
             WHERE id = $1",
        )
        .bind(candidate_id)
        .bind(retry_seconds as f64)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(held)
}

pub async fn hold_source_unavailable(
    pool: &PgPool,
    candidate_id: Uuid,
    kind: UnavailableKind,
    http_status: Option<i32>,
    reason: &str,
) -> Result<u64> {
    let reason = truncate(reason);
    let mut tx = pool.begin().await?;
    let check_id = insert_check(
        &mut tx,
        CheckRecord {
            candidate_id,
            check_kind: kind.check_kind(),
            previous_content_hash: None,
            content_hash: None,
            http_status,
            fetched_bytes: None,
            error_summary: Some(&reason),
            requires_review: true,
        },
    )
    .await?;
    sqlx::query(
        "UPDATE benefit_source_candidates
         SET status = 'unreachable_review', http_status = $2,
             consecutive_failures = LEAST(consecutive_failures + 1, 100),
             last_error = $3, next_check_at = now() + interval '1 day',
             last_checked_at = now(), updated_at = now()
         WHERE id = $1",
    )
    .bind(candidate_id)
    .bind(http_status)
    .bind(&reason)
    .execute(&mut *tx)
    .await?;
    let held = place_holds(
        &mut tx,
        candidate_id,
        check_id,
        kind.hold_reason(),
        None,
        "unreachable",
        Some(&reason),
    )
    .await?;
    record_source_review_event(
        &mut tx,
        candidate_id,
        "source_unavailable",
        "An official benefit source became unavailable and requires review",
        held,
    )
    .await?;
    tx.commit().await?;
    Ok(held)
}

/// Why a source the census rated other than direct is closed to our
/// reader, for the hold that hides it: the one classification of census
/// reasons ([`crate::corporate_auto::policy::LaneReason`]). `None` for a
/// page our reader may read as direct HTML.
pub fn unreadable_reason(
    reason: crate::corporate_auto::policy::LaneReason,
) -> Option<&'static str> {
    use crate::corporate_auto::policy::LaneReason as R;
    match reason {
        R::DirectHtml | R::RobotsAllowedUnprobed => None,
        R::RobotsDisallow => Some("robots.txt disallows this page for our agent"),
        R::RobotsBotWall => Some("the site answers our agent's robots.txt request with a wall"),
        R::PageBotWall => Some("the page answers our agent with a wall"),
        R::RobotsMissingGrayOff => Some("the site has no robots.txt and reading it is not enabled"),
        R::RobotsUnavailable | R::DomainPaused => {
            Some("the site's robots.txt cannot be read by our agent")
        }
        R::RateLimited => Some("the site limits our agent's requests"),
        R::JsShell => Some("the page shows its text only in a browser"),
        R::TosSpecificCollection | R::TosGenericGrayOff => {
            Some("the site's terms restrict automated collection")
        }
        R::OptOut => Some("the publisher opted out"),
        R::PartnerFeed => Some("the brand publishes this offer through a partner feed"),
        R::PageGone => Some("the page is gone"),
        R::RedirectOffHost | R::RedirectLoop => {
            Some("the page redirects where our agent does not follow")
        }
        R::PageUnreachable | R::UnsupportedContent => Some("the page cannot be read as HTML"),
    }
}

/// The hold for a corporate offer that only a person's review kept shown
/// (`human` monitors only) once its timer ran out: unreadable to our reader
/// per its census verdict (`access_revoked`, the real reason, no incident:
/// nothing for anyone to act on), or readable and not yet derived by intake
/// (`verification_overdue`, an incident). `None` for every other source.
async fn manual_corporate_hold(
    tx: &mut Transaction<'_, Postgres>,
    candidate_id: Uuid,
) -> Result<Option<(&'static str, &'static str)>> {
    let only_manual_corporate: bool = sqlx::query_scalar::<_, Option<bool>>(
        "SELECT bool_and(monitor.verification_mode = 'human' AND program.program_type = 'corporate_benefit')
         FROM program_source_monitors monitor JOIN programs program ON program.id = monitor.program_id
         WHERE monitor.candidate_id = $1 AND monitor.verification_state <> 'retired'",
    )
    .bind(candidate_id)
    .fetch_one(&mut **tx)
    .await?
    .unwrap_or(false);
    if !only_manual_corporate {
        return Ok(None);
    }
    let verdict: Option<String> = sqlx::query_scalar(&format!(
        "SELECT entry.reason FROM ({verdicts}) entry WHERE entry.candidate_id = $2",
        verdicts = crate::corporate_auto::policy::census_verdicts_sql("$1"),
    ))
    .bind(crate::corporate_auto::evidence::CENSUS_MAX_AGE_DAYS as i32)
    .bind(candidate_id)
    .fetch_optional(&mut **tx)
    .await?;
    let reason = verdict.as_deref().and_then(|text| {
        crate::corporate_auto::policy::LaneReason::ALL
            .iter()
            .find(|reason| reason.as_str() == text)
            .copied()
    });
    Ok(Some(match reason.and_then(unreadable_reason) {
        Some(why) => ("access_revoked", why),
        None => (
            "verification_overdue",
            "no automatic reading of this page has derived the offer yet",
        ),
    }))
}

/// Holds the overdue sweep placed on manual corporate sources before it
/// stated the reason (RC5): each open `verification_overdue` hold whose
/// source the census rates unreadable to our reader becomes
/// `access_revoked` with that reason, and its overdue incident is closed,
/// since nobody can act on it. The offer stays hidden: no hold is resolved
/// and nothing is shown. Idempotent; at most `RELABEL_LIMIT` sources a run.
pub async fn relabel_unreadable_manual_holds(pool: &PgPool) -> Result<u64> {
    const RELABEL_LIMIT: i64 = 200;
    let candidates: Vec<Uuid> = sqlx::query_scalar(
        // Only sources every live monitor of which is a manual corporate
        // one (`manual_corporate_hold`'s own condition), so overdue holds of
        // other kinds cannot fill the limit (review A-L2).
        "SELECT DISTINCT hold.candidate_id FROM program_source_holds hold
         WHERE hold.resolved_at IS NULL AND hold.hold_reason = 'verification_overdue'
           AND NOT EXISTS (
               SELECT 1 FROM program_source_monitors monitor
               JOIN programs program ON program.id = monitor.program_id
               WHERE monitor.candidate_id = hold.candidate_id
                 AND monitor.verification_state <> 'retired'
                 AND NOT (monitor.verification_mode = 'human'
                          AND program.program_type = 'corporate_benefit'))
         ORDER BY hold.candidate_id LIMIT $1",
    )
    .bind(RELABEL_LIMIT)
    .fetch_all(pool)
    .await
    .context("failed to select overdue holds")?;
    let mut relabelled = 0;
    for candidate_id in candidates {
        let mut tx = pool.begin().await?;
        let Some(("access_revoked", why)) = manual_corporate_hold(&mut tx, candidate_id).await?
        else {
            tx.rollback().await?;
            continue;
        };
        sqlx::query(
            "UPDATE program_source_holds SET hold_reason = 'access_revoked'
             WHERE candidate_id = $1 AND resolved_at IS NULL AND hold_reason = 'verification_overdue'",
        )
        .bind(candidate_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE benefit_source_candidates SET last_error = $2, updated_at = now() WHERE id = $1",
        )
        .bind(candidate_id)
        .bind(why)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE program_source_monitors SET last_error = $2, updated_at = now()
             WHERE candidate_id = $1 AND verification_state <> 'retired'",
        )
        .bind(candidate_id)
        .bind(why)
        .execute(&mut *tx)
        .await?;
        crate::operations::resolve_event_tx(
            &mut tx,
            &format!("source_review:{candidate_id}"),
            None,
            Some("Not readable by the automatic reader; held with that reason"),
        )
        .await?;
        tx.commit().await?;
        relabelled += 1;
    }
    Ok(relabelled)
}

pub async fn hold_overdue_sources(
    pool: &PgPool,
    grace: Duration,
    limit: i64,
) -> Result<FreshnessSummary> {
    let grace_seconds = grace.num_seconds().max(0);
    // Every mode, evidence_auto included: evidence moves a deadline only on
    // a confirmation (live, or shadow/hide_only with the page read and
    // every fact confirmed), so a source that stopped confirming is held
    // here, fail closed.
    let candidate_ids = sqlx::query_scalar::<_, Uuid>(
        "SELECT DISTINCT monitor.candidate_id
         FROM program_source_monitors monitor
         JOIN benefit_source_candidates candidate ON candidate.id = monitor.candidate_id
         WHERE monitor.verification_state = 'current'
           AND monitor.review_due_at + make_interval(secs => $1) <= now()
           AND candidate.status = 'approved'
         ORDER BY monitor.candidate_id
         LIMIT $2",
    )
    .bind(grace_seconds as f64)
    .bind(limit.clamp(1, 1_000))
    .fetch_all(pool)
    .await
    .context("failed to select overdue source monitors")?;

    let mut summary = FreshnessSummary::default();
    for candidate_id in candidate_ids {
        let mut tx = pool.begin().await?;
        let locked = sqlx::query_scalar::<_, Uuid>(
            "SELECT id
             FROM benefit_source_candidates
             WHERE id = $1 AND status = 'approved'
             FOR UPDATE SKIP LOCKED",
        )
        .bind(candidate_id)
        .fetch_optional(&mut *tx)
        .await?;
        if locked.is_none() {
            tx.rollback().await?;
            continue;
        }
        // A corporate offer only a person's timer kept shown is held with
        // the reason the system has (RC5); every other source as before.
        let (hold_reason, why) = manual_corporate_hold(&mut tx, candidate_id)
            .await?
            .unwrap_or((
                "verification_overdue",
                "source verification deadline exceeded",
            ));
        let check_id = insert_check(
            &mut tx,
            CheckRecord {
                candidate_id,
                check_kind: "review_overdue",
                previous_content_hash: None,
                content_hash: None,
                http_status: None,
                fetched_bytes: None,
                error_summary: Some(why),
                requires_review: hold_reason == "verification_overdue",
            },
        )
        .await?;
        sqlx::query(
            "UPDATE benefit_source_candidates
             SET status = 'verification_due', next_check_at = now() + interval '1 hour',
                 last_error = $2,
                 updated_at = now()
             WHERE id = $1",
        )
        .bind(candidate_id)
        .bind(why)
        .execute(&mut *tx)
        .await?;
        let held = place_holds(
            &mut tx,
            candidate_id,
            check_id,
            hold_reason,
            None,
            "review_due",
            Some(why),
        )
        .await?;
        // An unreadable source asks nothing of anyone: no incident.
        if hold_reason == "verification_overdue" {
            record_source_review_event(
                &mut tx,
                candidate_id,
                "source_verification_overdue",
                "An official benefit source exceeded its verification deadline",
                held,
            )
            .await?;
        }
        tx.commit().await?;
        summary.candidates_held += 1;
        summary.programs_held += held;
    }
    Ok(summary)
}

async fn record_source_review_event(
    tx: &mut Transaction<'_, Postgres>,
    candidate_id: Uuid,
    event_type: &str,
    message: &str,
    programs_held: u64,
) -> Result<()> {
    let fingerprint = format!("source_review:{candidate_id}");
    let subject_id = candidate_id.to_string();
    operations::record_event_tx(
        tx,
        &EventCommand {
            fingerprint: &fingerprint,
            severity: if programs_held > 0 {
                Severity::Error
            } else {
                Severity::Warning
            },
            component: "source_monitoring",
            event_type,
            subject_type: Some("benefit_source_candidate"),
            subject_id: Some(&subject_id),
            message,
            details: serde_json::json!({ "programs_held": programs_held }),
        },
    )
    .await?;
    Ok(())
}

struct CheckRecord<'a> {
    candidate_id: Uuid,
    check_kind: &'a str,
    previous_content_hash: Option<&'a str>,
    content_hash: Option<&'a str>,
    http_status: Option<i32>,
    fetched_bytes: Option<i32>,
    error_summary: Option<&'a str>,
    requires_review: bool,
}

async fn insert_check(tx: &mut Transaction<'_, Postgres>, check: CheckRecord<'_>) -> Result<Uuid> {
    Ok(sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO benefit_source_checks (
             candidate_id, check_kind, previous_content_hash, content_hash,
             http_status, fetched_bytes, error_summary, review_status
         ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
         RETURNING id",
    )
    .bind(check.candidate_id)
    .bind(check.check_kind)
    .bind(check.previous_content_hash)
    .bind(check.content_hash)
    .bind(check.http_status)
    .bind(check.fetched_bytes)
    .bind(check.error_summary)
    .bind(if check.requires_review {
        "pending"
    } else {
        "not_required"
    })
    .fetch_one(&mut **tx)
    .await?)
}

async fn mark_monitors_current(
    tx: &mut Transaction<'_, Postgres>,
    candidate_id: Uuid,
    content_hash: Option<&str>,
    interval_seconds: i32,
) -> Result<()> {
    sqlx::query(
        "UPDATE program_source_monitors
         SET verification_state = 'current',
             verified_content_hash = COALESCE($2, verified_content_hash),
             verified_at = now(),
             review_due_at = now() + make_interval(secs => $3),
             recheck_interval_seconds = $3,
             change_detected_at = NULL, last_error = NULL, updated_at = now()
         WHERE candidate_id = $1
           AND verification_state <> 'retired'
           AND (verified_content_hash = $2 OR (verified_content_hash IS NULL AND verification_state = 'current'))",
    )
    .bind(candidate_id)
    .bind(content_hash)
    .bind(interval_seconds as f64)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Recover transient outages or reverted changes without an operator. Only
/// exact verified content may reach this point; another unhealthy source,
/// provider expiry, access revocation or a manual closed state prevents release.
/// A hold placed because evidence failed or the page was gone
/// (`evidence_failed`, `not_found`) is never released here, in any mode: an
/// unchanged hash says nothing about an end date that passed.
async fn restore_verified_programs(
    tx: &mut Transaction<'_, Postgres>,
    candidate_id: Uuid,
) -> Result<()> {
    sqlx::query(
        "WITH recoverable AS (
          SELECT p.id, bool_or(h.previous_is_active) AS active,
                 min(h.previous_program_status) FILTER (WHERE h.previous_is_active) AS status
          FROM programs p JOIN program_source_holds h ON h.program_id=p.id AND h.resolved_at IS NULL
          WHERE p.source_health='current' AND p.program_status='review_required'
            AND (LEAST(p.benefit_valid_until,p.application_end_at,p.deadline_at) IS NULL
                 OR LEAST(p.benefit_valid_until,p.application_end_at,p.deadline_at)>now())
            AND EXISTS(SELECT 1 FROM program_source_monitors m WHERE m.program_id=p.id AND m.candidate_id=$1)
          GROUP BY p.id HAVING bool_and(h.hold_reason NOT IN ('access_revoked', 'publisher_optout', 'evidence_failed', 'not_found'))
        ) UPDATE programs p SET is_active=r.active, program_status=COALESCE(r.status,'review_required'), updated_at=now()
          FROM recoverable r WHERE p.id=r.id AND r.active")
        .bind(candidate_id).execute(&mut **tx).await?;
    sqlx::query(
        "UPDATE programs p SET official_url=c.url, updated_at=now()
         FROM program_source_monitors m JOIN benefit_source_candidates c ON c.id=m.candidate_id
         WHERE m.program_id=p.id AND c.id=$1 AND p.source_health='current'
           AND m.verified_content_hash=c.content_hash AND p.official_url=c.canonical_url AND c.url<>c.canonical_url")
        .bind(candidate_id).execute(&mut **tx).await?;
    sqlx::query(
        "UPDATE program_source_holds h SET resolved_at=now(), resolution='confirmed_unchanged',
            resolution_note='Automatically restored: all source content matches verified hashes'
         FROM programs p WHERE p.id=h.program_id AND h.resolved_at IS NULL AND p.is_active AND p.source_health='current'
           AND h.hold_reason NOT IN ('publisher_optout', 'evidence_failed', 'not_found')
           AND EXISTS(SELECT 1 FROM program_source_monitors m WHERE m.program_id=p.id AND m.candidate_id=$1)")
        .bind(candidate_id).execute(&mut **tx).await?;
    Ok(())
}

async fn place_holds(
    tx: &mut Transaction<'_, Postgres>,
    candidate_id: Uuid,
    check_id: Uuid,
    hold_reason: &str,
    detected_content_hash: Option<&str>,
    monitor_state: &str,
    last_error: Option<&str>,
) -> Result<u64> {
    let inserted = sqlx::query(
        "INSERT INTO program_source_holds (
             program_id, candidate_id, source_check_id, hold_reason,
             previous_is_active, previous_program_status, detected_content_hash
         )
         SELECT monitor.program_id, monitor.candidate_id, $2, $3,
                program.is_active, program.program_status, $4
         FROM program_source_monitors monitor
         JOIN programs program ON program.id = monitor.program_id
         WHERE monitor.candidate_id = $1
           AND monitor.verification_state <> 'retired'
         ON CONFLICT (program_id, candidate_id) WHERE resolved_at IS NULL
         DO NOTHING",
    )
    .bind(candidate_id)
    .bind(check_id)
    .bind(hold_reason)
    .bind(detected_content_hash)
    .execute(&mut **tx)
    .await?
    .rows_affected();

    sqlx::query(
        "UPDATE program_source_monitors
         SET verification_state = $2, change_detected_at = now(),
             last_error = $3, updated_at = now()
         WHERE candidate_id = $1
           AND verification_state <> 'retired'",
    )
    .bind(candidate_id)
    .bind(monitor_state)
    .bind(last_error)
    .execute(&mut **tx)
    .await?;
    refresh_program_health(tx, candidate_id).await?;
    Ok(inserted)
}

pub(crate) async fn refresh_program_health(
    tx: &mut Transaction<'_, Postgres>,
    candidate_id: Uuid,
) -> Result<()> {
    sqlx::query(
        "WITH affected AS (
             SELECT DISTINCT program_id
             FROM program_source_monitors
             WHERE candidate_id = $1
         ), aggregate_state AS (
             SELECT
                 monitor.program_id,
                 CASE
                     WHEN bool_or(monitor.verification_state = 'change_detected')
                         THEN 'change_detected'
                     WHEN bool_or(monitor.verification_state = 'unreachable')
                         THEN 'unreachable'
                     WHEN bool_or(monitor.verification_state = 'review_due')
                         THEN 'review_due'
                     WHEN bool_and(monitor.verification_state = 'retired')
                         THEN 'retired'
                     ELSE 'current'
                 END AS source_health,
                 max(monitor.verified_at) AS verified_at,
                 min(monitor.review_due_at) FILTER (
                     WHERE monitor.verification_state <> 'retired'
                 ) AS review_due_at
             FROM program_source_monitors monitor
             JOIN affected ON affected.program_id = monitor.program_id
             GROUP BY monitor.program_id
         )
         UPDATE programs program
         SET source_health = state.source_health,
             source_verified_at = state.verified_at,
             source_review_due_at = state.review_due_at,
             source_change_detected_at = CASE
                 WHEN state.source_health = 'current' THEN NULL
                 ELSE COALESCE(program.source_change_detected_at, now())
             END,
             program_status = CASE
                 WHEN state.source_health <> 'current' AND program.is_active
                 THEN 'review_required'
                 ELSE program.program_status
             END,
             is_active = CASE
                 WHEN state.source_health = 'current' THEN program.is_active
                 ELSE false
             END,
             updated_at = now()
         FROM aggregate_state state
         WHERE program.id = state.program_id",
    )
    .bind(candidate_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

fn truncate(value: &str) -> String {
    value.chars().take(MAX_ERROR_CHARS).collect()
}

fn parse_bounded_env(name: &str, default: i64, min: i64, max: i64) -> Result<i64> {
    let value = match std::env::var(name) {
        Ok(value) if !value.trim().is_empty() => value
            .parse::<i64>()
            .with_context(|| format!("{name} must be an integer"))?,
        _ => default,
    };
    if !(min..=max).contains(&value) {
        anyhow::bail!("{name} must be between {min} and {max}");
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_hash_establishes_a_baseline_without_a_change_hold() {
        assert_eq!(
            classify_content_change(None, "0123456789abcdef0123456789abcdef"),
            ContentChange::BaselineEstablished
        );
    }

    #[test]
    fn identical_hash_is_unchanged() {
        let hash = "0123456789abcdef0123456789abcdef";
        assert_eq!(
            classify_content_change(Some(hash), hash),
            ContentChange::Unchanged
        );
    }

    #[test]
    fn different_hash_requires_review() {
        assert_eq!(
            classify_content_change(
                Some("0123456789abcdef0123456789abcdef"),
                "fedcba9876543210fedcba9876543210"
            ),
            ContentChange::Changed
        );
    }

    #[test]
    fn freshness_policy_rejects_unsafe_bounds() {
        // The parser itself is deterministic; environment mutation is avoided
        // because Rust tests run concurrently in one process.
        assert_eq!(
            parse_bounded_env("MAZIMI_TEST_UNSET_POLICY", 72, 1, 720).unwrap(),
            72
        );
    }

    async fn test_pool() -> PgPool {
        let url = std::env::var("TEST_DATABASE_URL")
            .unwrap_or_else(|_| "postgres://wello:wello@localhost:5432/wello".into());
        sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect(&url)
            .await
            .expect("isolated test database is required")
    }

    /// (domain, program, candidate): a published benefit on a manual-review
    /// domain whose current monitor is due in `due_in_days`.
    async fn manual_review_fixture(pool: &PgPool, due_in_days: i32) -> (Uuid, Uuid, Uuid) {
        let unique = Uuid::new_v4().simple().to_string();
        let domain = format!("superseded-{unique}.example.com");
        let url = format!("https://{domain}/benefit");
        let domain_id: Uuid = sqlx::query_scalar(
            "INSERT INTO benefit_source_domains (
                 organization_name, domain, source_role, crawl_allowed,
                 verification_status, verification_reference, verified_at,
                 compliance_status, collection_mode, is_active
             ) VALUES ('Monitor Fixture', $1, 'brand_official', false,
                 'verified', $2, now(), 'pending', 'manual_review', true) RETURNING id",
        )
        .bind(&domain)
        .bind(&url)
        .fetch_one(pool)
        .await
        .unwrap();
        let program_id: Uuid = sqlx::query_scalar(
            "INSERT INTO programs (
                 program_type, source_type, source_id, title, summary,
                 provider_name, official_url, program_status, is_active,
                 provider_kind, company_name, benefit_category,
                 source_health, source_verified_at, source_review_due_at
             ) VALUES ('corporate_benefit', 'corporate_official', $1,
                 '모니터 픽스처 혜택', '출처 모니터링 테스트입니다.',
                 'Monitor Fixture', $1, 'active', true,
                 'corporate', 'Monitor Fixture', 'discount',
                 'current', now() - interval '27 days', now() + make_interval(days => $2))
             RETURNING id",
        )
        .bind(&url)
        .bind(due_in_days)
        .fetch_one(pool)
        .await
        .unwrap();
        let candidate_id: Uuid = sqlx::query_scalar(
            "INSERT INTO benefit_source_candidates (
                 domain_id, url, canonical_url, discovered_via, status,
                 candidate_kind, next_check_at, recheck_interval_seconds
             ) VALUES ($1, $2, $2, 'manual', 'approved', 'detail', now(), 2592000) RETURNING id",
        )
        .bind(domain_id)
        .bind(&url)
        .fetch_one(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO program_source_monitors (
                 program_id, candidate_id, verification_state, verified_at,
                 review_due_at, recheck_interval_seconds
             ) VALUES ($1, $2, 'current', now() - interval '27 days',
                 now() + make_interval(days => $3), 2592000)",
        )
        .bind(program_id)
        .bind(candidate_id)
        .bind(due_in_days)
        .execute(pool)
        .await
        .unwrap();
        (domain_id, program_id, candidate_id)
    }

    async fn cleanup_fixture(
        pool: &PgPool,
        (domain_id, program_id, candidate_id): (Uuid, Uuid, Uuid),
    ) {
        for prefix in ["source_review", "source_review_upcoming"] {
            sqlx::query("DELETE FROM operational_events WHERE fingerprint = $1")
                .bind(format!("{prefix}:{candidate_id}"))
                .execute(pool)
                .await
                .unwrap();
        }
        for sql in [
            "DELETE FROM programs WHERE id = $1",
            "DELETE FROM benefit_source_candidates WHERE id = $1",
            "DELETE FROM benefit_source_domains WHERE id = $1",
        ] {
            let id = match sql {
                s if s.contains("programs") => program_id,
                s if s.contains("candidates") => candidate_id,
                _ => domain_id,
            };
            sqlx::query(sql).bind(id).execute(pool).await.unwrap();
        }
    }

    async fn event(pool: &PgPool, fingerprint: &str) -> (String, Option<String>) {
        sqlx::query_as(
            "SELECT status, resolution_note FROM operational_events WHERE fingerprint = $1",
        )
        .bind(fingerprint)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    /// Production on 2026-10-21: `source_review_upcoming:<candidate>` stayed
    /// open next to the overdue incident of the same source. Once the
    /// overdue event is open, the warning is superseded and says so.
    #[tokio::test]
    async fn an_overdue_source_supersedes_its_upcoming_review_warning() {
        let pool = test_pool().await;
        let fixture = manual_review_fixture(&pool, 3).await;
        let candidate_id = fixture.2;
        let upcoming = format!("source_review_upcoming:{candidate_id}");
        warn_upcoming_manual_reviews(&pool).await.unwrap();
        assert_eq!(event(&pool, &upcoming).await.0, "open");

        // Past due and past the grace: the overdue sweep holds the source.
        sqlx::query("UPDATE program_source_monitors SET review_due_at = now() - interval '4 days' WHERE candidate_id = $1")
            .bind(candidate_id)
            .execute(&pool)
            .await
            .unwrap();
        let held = hold_overdue_sources(&pool, Duration::hours(72), 1_000)
            .await
            .unwrap();
        assert!(held.candidates_held >= 1);
        warn_upcoming_manual_reviews(&pool).await.unwrap();
        let (status, note) = event(&pool, &upcoming).await;
        assert_eq!(status, "resolved");
        assert!(
            note.unwrap_or_default().contains("superseded"),
            "the note names the overdue incident"
        );
        // The next sweep does not raise the warning again.
        warn_upcoming_manual_reviews(&pool).await.unwrap();
        assert_eq!(event(&pool, &upcoming).await.0, "resolved");
        cleanup_fixture(&pool, fixture).await;
        pool.close().await;
    }

    /// An overdue incident opened while the monitor is still `current` (a
    /// hold that did not complete, or one recorded by another path) also
    /// supersedes the warning, and the warning is not raised again.
    #[tokio::test]
    async fn an_open_overdue_event_supersedes_the_warning_while_the_monitor_is_current() {
        let pool = test_pool().await;
        let fixture = manual_review_fixture(&pool, -1).await;
        let candidate_id = fixture.2;
        let upcoming = format!("source_review_upcoming:{candidate_id}");
        warn_upcoming_manual_reviews(&pool).await.unwrap();
        assert_eq!(event(&pool, &upcoming).await.0, "open");
        let mut tx = pool.begin().await.unwrap();
        record_source_review_event(
            &mut tx,
            candidate_id,
            "source_verification_overdue",
            "overdue",
            0,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        warn_upcoming_manual_reviews(&pool).await.unwrap();
        let (status, note) = event(&pool, &upcoming).await;
        assert_eq!(status, "resolved");
        assert!(note.unwrap_or_default().contains("superseded"));
        warn_upcoming_manual_reviews(&pool).await.unwrap();
        assert_eq!(event(&pool, &upcoming).await.0, "resolved");
        cleanup_fixture(&pool, fixture).await;
        pool.close().await;
    }

    /// RC5 (production 2026-11-11: Framer, Master.dev, Obsidian on a manual
    /// deadline nobody renews): a corporate offer on a person's timer is
    /// hidden when the timer runs out, with the reason the system has. A
    /// page the census rated unreadable to us is held as such
    /// (`access_revoked`, the real reason, no overdue incident: nothing for
    /// anyone to act on); one it has not rated stays an overdue source.
    #[tokio::test]
    async fn f5_a_manual_timer_never_outlives_its_deadline_and_says_why() {
        let pool = test_pool().await;
        let blocked = manual_review_fixture(&pool, -4).await;
        let unrated = manual_review_fixture(&pool, -4).await;
        let run: Uuid = sqlx::query_scalar(
            "INSERT INTO corporate_census_runs(status, started_at, finished_at) VALUES('completed', now(), now()) RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO corporate_census_entries(run_id,subject_kind,domain_id,candidate_id,program_id,url,lane,reason,probed,robots_state,tos_signal)
             VALUES($1,'monitor',$2,$3,$4,'https://x.example/','snippet','robots_disallow',false,'ok','none')",
        )
        .bind(run)
        .bind(blocked.0)
        .bind(blocked.2)
        .bind(blocked.1)
        .execute(&pool)
        .await
        .unwrap();
        hold_overdue_sources(&pool, Duration::hours(72), 1_000)
            .await
            .unwrap();
        for (fixture, hold, error, incident) in [
            (blocked, "access_revoked", "robots.txt disallows", false),
            (
                unrated,
                "verification_overdue",
                "no automatic reading",
                true,
            ),
        ] {
            let shown: bool = sqlx::query_scalar("SELECT is_active FROM programs WHERE id = $1")
                .bind(fixture.1)
                .fetch_one(&pool)
                .await
                .unwrap();
            assert!(!shown, "{hold}: hidden once the manual timer ran out");
            let reason: String = sqlx::query_scalar(
                "SELECT hold_reason FROM program_source_holds WHERE program_id = $1 AND resolved_at IS NULL",
            )
            .bind(fixture.1)
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(reason, hold);
            let last_error: String = sqlx::query_scalar(
                "SELECT last_error FROM benefit_source_candidates WHERE id = $1",
            )
            .bind(fixture.2)
            .fetch_one(&pool)
            .await
            .unwrap();
            assert!(last_error.contains(error), "{last_error}");
            assert!(!last_error.contains("deadline exceeded"), "{last_error}");
            let events: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM operational_events WHERE fingerprint = $1 AND status <> 'resolved'",
            )
            .bind(format!("source_review:{}", fixture.2))
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(events > 0, incident, "{hold}: overdue incident");
        }
        sqlx::query("DELETE FROM corporate_census_runs WHERE id = $1")
            .bind(run)
            .execute(&pool)
            .await
            .unwrap();
        cleanup_fixture(&pool, blocked).await;
        cleanup_fixture(&pool, unrated).await;
        pool.close().await;
    }

    /// F5: no migration from this change on creates a person-owned monitor
    /// for a corporate benefit; a programme is evidence-read (`evidence_auto`,
    /// `door`) or hidden from its first day.
    #[test]
    fn no_new_migration_creates_a_human_corporate_monitor() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../infra/migrations");
        let mut offending = Vec::new();
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            if name.as_str() < "20261112000000" || !name.ends_with(".sql") {
                continue;
            }
            let sql = std::fs::read_to_string(&path).unwrap().to_lowercase();
            for insert in sql.split("insert into program_source_monitors").skip(1) {
                let statement = insert.split(';').next().unwrap_or("");
                if !statement.contains("verification_mode") || statement.contains("'human'") {
                    offending.push(name.clone());
                }
            }
        }
        assert!(
            offending.is_empty(),
            "human or default-mode monitors: {offending:?}"
        );
    }

    /// F5 for the rows held before the reason was stated: an open overdue
    /// hold on a manual corporate source the census rates unreadable is
    /// relabelled with the real reason and its incident closed. It stays
    /// hidden; nothing is shown or resolved. Idempotent.
    #[tokio::test]
    async fn f5_holds_placed_before_the_real_reason_are_relabelled() {
        let pool = test_pool().await;
        let fixture = manual_review_fixture(&pool, -4).await;
        // Held by the sweep as it was before this change.
        hold_overdue_sources(&pool, Duration::hours(72), 1_000)
            .await
            .unwrap();
        let reason = |pool: PgPool| async move {
            sqlx::query_scalar::<_, String>(
                "SELECT hold_reason FROM program_source_holds WHERE program_id = $1 AND resolved_at IS NULL",
            )
            .bind(fixture.1)
            .fetch_one(&pool)
            .await
            .unwrap()
        };
        assert_eq!(reason(pool.clone()).await, "verification_overdue");
        let run: Uuid = sqlx::query_scalar(
            "INSERT INTO corporate_census_runs(status, started_at, finished_at) VALUES('completed', now(), now()) RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO corporate_census_entries(run_id,subject_kind,domain_id,candidate_id,program_id,url,lane,reason,probed,robots_state,tos_signal)
             VALUES($1,'monitor',$2,$3,$4,'https://x.example/','snippet','robots_disallow',false,'ok','none')",
        )
        .bind(run)
        .bind(fixture.0)
        .bind(fixture.2)
        .bind(fixture.1)
        .execute(&pool)
        .await
        .unwrap();
        assert!(relabel_unreadable_manual_holds(&pool).await.unwrap() >= 1);
        assert_eq!(
            relabel_unreadable_manual_holds(&pool).await.unwrap(),
            0,
            "idempotent"
        );
        assert_eq!(reason(pool.clone()).await, "access_revoked");
        let (shown, last_error): (bool, String) = sqlx::query_as(
            "SELECT p.is_active, c.last_error FROM programs p, benefit_source_candidates c WHERE p.id = $1 AND c.id = $2",
        )
        .bind(fixture.1)
        .bind(fixture.2)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(!shown, "still hidden");
        assert!(last_error.contains("robots.txt disallows"), "{last_error}");
        let (status, _) = event(&pool, &format!("source_review:{}", fixture.2)).await;
        assert_eq!(status, "resolved", "nothing for anyone to act on");
        sqlx::query("DELETE FROM corporate_census_runs WHERE id = $1")
            .bind(run)
            .execute(&pool)
            .await
            .unwrap();
        cleanup_fixture(&pool, fixture).await;
        pool.close().await;
    }
}
