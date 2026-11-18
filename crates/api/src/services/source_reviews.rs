//! Transactional resolution of source-change and freshness holds.
//!
//! Administrators may confirm that existing normalized facts are still
//! accurate, confirm that they updated the program first, or retire a source.
//! No resolution can republish a program while another linked source remains
//! unhealthy.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{FromRow, PgPool};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    ConfirmedUnchanged,
    ProgramUpdated,
    Retired,
}

impl Resolution {
    fn as_str(self) -> &'static str {
        match self {
            Self::ConfirmedUnchanged => "confirmed_unchanged",
            Self::ProgramUpdated => "program_updated",
            Self::Retired => "retired",
        }
    }
}

#[derive(Debug)]
pub struct ReviewCommand {
    pub program_id: Uuid,
    pub candidate_id: Uuid,
    pub reviewer_id: Uuid,
    pub resolution: Resolution,
    pub note: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ReviewOutcome {
    pub program_id: Uuid,
    pub candidate_id: Uuid,
    pub resolution: &'static str,
    pub source_health: String,
    pub is_active: bool,
    pub program_status: String,
    pub candidate_status: String,
    pub next_review_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Error)]
pub enum ServiceError {
    #[error("program source monitor not found")]
    NotFound,
    #[error("source monitor is not in a reviewable state")]
    InvalidState,
    #[error("a retirement decision requires a review note")]
    RetirementNoteRequired,
    #[error("the offer is held by a publisher opt-out")]
    PublisherOptOut,
    #[error("source review storage operation failed")]
    Storage(#[from] sqlx::Error),
    #[error("source review operational-event update failed")]
    OperationalEvent(#[source] anyhow::Error),
}

#[derive(Debug, FromRow)]
struct LockedMonitor {
    verification_state: String,
    recheck_interval_seconds: i32,
    content_hash: Option<String>,
    candidate_status: String,
    program_is_active: bool,
    program_status: String,
}

#[derive(Debug, FromRow)]
struct OpenHold {
    id: Uuid,
    hold_reason: String,
    source_check_id: Option<Uuid>,
    previous_is_active: bool,
    previous_program_status: String,
}

#[derive(Debug, FromRow)]
struct AggregateHealth {
    source_health: String,
    verified_at: Option<DateTime<Utc>>,
    review_due_at: Option<DateTime<Utc>>,
}

pub async fn review_program_source(
    pool: &PgPool,
    command: ReviewCommand,
) -> Result<ReviewOutcome, ServiceError> {
    if command.resolution == Resolution::Retired && command.note.is_none() {
        return Err(ServiceError::RetirementNoteRequired);
    }

    let mut tx = pool.begin().await?;
    let monitor = sqlx::query_as::<_, LockedMonitor>(
        "SELECT monitor.verification_state, monitor.recheck_interval_seconds,
                candidate.content_hash, candidate.status AS candidate_status,
                program.is_active AS program_is_active,
                program.program_status
         FROM program_source_monitors monitor
         JOIN benefit_source_candidates candidate ON candidate.id = monitor.candidate_id
         JOIN programs program ON program.id = monitor.program_id
         WHERE monitor.program_id = $1 AND monitor.candidate_id = $2
         FOR UPDATE OF monitor, candidate, program",
    )
    .bind(command.program_id)
    .bind(command.candidate_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(ServiceError::NotFound)?;

    if monitor.verification_state == "retired" {
        return Err(ServiceError::InvalidState);
    }

    let hold = sqlx::query_as::<_, OpenHold>(
        "SELECT id, hold_reason, source_check_id, previous_is_active, previous_program_status
         FROM program_source_holds
         WHERE program_id = $1 AND candidate_id = $2 AND resolved_at IS NULL
         FOR UPDATE",
    )
    .bind(command.program_id)
    .bind(command.candidate_id)
    .fetch_optional(&mut *tx)
    .await?;

    // A publisher opt-out is released only by lifting the opt-out itself
    // (publisher proof or the operator lift), never by a source review,
    // which would leave the opt-out row active.
    if hold
        .as_ref()
        .is_some_and(|h| h.hold_reason == "publisher_optout")
    {
        return Err(ServiceError::PublisherOptOut);
    }
    if monitor.verification_state != "current" && hold.is_none() {
        return Err(ServiceError::InvalidState);
    }

    let resolution = command.resolution.as_str();
    if command.resolution == Resolution::Retired {
        sqlx::query(
            "UPDATE program_source_monitors
             SET verification_state = 'retired', change_detected_at = NULL,
                 last_error = NULL, updated_at = now()
             WHERE program_id = $1 AND candidate_id = $2",
        )
        .bind(command.program_id)
        .bind(command.candidate_id)
        .execute(&mut *tx)
        .await?;
    } else {
        sqlx::query(
            "UPDATE program_source_monitors
             SET verification_state = 'current',
                 verified_content_hash = COALESCE($3, verified_content_hash),
                 verified_at = now(),
                 review_due_at = now() + make_interval(secs => $4),
                 change_detected_at = NULL, last_error = NULL, updated_at = now()
             WHERE program_id = $1 AND candidate_id = $2",
        )
        .bind(command.program_id)
        .bind(command.candidate_id)
        .bind(&monitor.content_hash)
        .bind(monitor.recheck_interval_seconds as f64)
        .execute(&mut *tx)
        .await?;
    }

    if let Some(hold) = &hold {
        sqlx::query(
            "UPDATE program_source_holds
             SET resolved_at = now(), resolved_by = $2, resolution = $3,
                 resolution_note = $4
             WHERE id = $1",
        )
        .bind(hold.id)
        .bind(command.reviewer_id)
        .bind(resolution)
        .bind(&command.note)
        .execute(&mut *tx)
        .await?;
    }

    let aggregate = sqlx::query_as::<_, AggregateHealth>(
        "SELECT
             CASE
                 WHEN bool_or(verification_state = 'change_detected') THEN 'change_detected'
                 WHEN bool_or(verification_state = 'unreachable') THEN 'unreachable'
                 WHEN bool_or(verification_state = 'review_due') THEN 'review_due'
                 WHEN bool_and(verification_state = 'retired') THEN 'retired'
                 ELSE 'current'
             END AS source_health,
             max(verified_at) AS verified_at,
             min(review_due_at) FILTER (WHERE verification_state <> 'retired') AS review_due_at
         FROM program_source_monitors
         WHERE program_id = $1",
    )
    .bind(command.program_id)
    .fetch_one(&mut *tx)
    .await?;

    let (desired_active, desired_status) = match (command.resolution, hold.as_ref()) {
        (Resolution::Retired, _) if aggregate.source_health == "retired" => {
            (false, "closed".to_string())
        }
        (Resolution::ConfirmedUnchanged, Some(hold)) => (
            hold.previous_is_active,
            hold.previous_program_status.clone(),
        ),
        (Resolution::ProgramUpdated, Some(hold)) => {
            let status = if monitor.program_status == "review_required" {
                hold.previous_program_status.clone()
            } else {
                monitor.program_status.clone()
            };
            (hold.previous_is_active, status)
        }
        (_, _) => (monitor.program_is_active, monitor.program_status.clone()),
    };
    let can_publish = aggregate.source_health == "current";
    let final_active = desired_active && can_publish;
    let final_status = if can_publish {
        desired_status
    } else if desired_active {
        "review_required".to_string()
    } else {
        monitor.program_status.clone()
    };

    // Restoring a held program also clears its retirement record so later
    // automation does not read it as still expired.
    sqlx::query(
        "UPDATE programs
         SET source_health = $2, source_verified_at = $3,
             source_review_due_at = $4,
             source_change_detected_at = CASE WHEN $2 = 'current' THEN NULL ELSE source_change_detected_at END,
             is_active = $5, program_status = $6,
             retired_at = CASE WHEN $5 THEN NULL ELSE retired_at END,
             retirement_reason = CASE WHEN $5 THEN NULL ELSE retirement_reason END,
             updated_at = now()
         WHERE id = $1",
    )
    .bind(command.program_id)
    .bind(&aggregate.source_health)
    .bind(aggregate.verified_at)
    .bind(aggregate.review_due_at)
    .bind(final_active)
    .bind(&final_status)
    .execute(&mut *tx)
    .await?;

    // The source-level candidate becomes current only after every linked
    // program has resolved its review. This is what makes shared CJ ONE or
    // HappyPoint pages safe to review one benefit at a time.
    let unresolved_count = sqlx::query_scalar::<_, i64>(
        "SELECT count(*)
         FROM program_source_monitors
         WHERE candidate_id = $1
           AND verification_state IN ('change_detected', 'unreachable', 'review_due')",
    )
    .bind(command.candidate_id)
    .fetch_one(&mut *tx)
    .await?;
    let active_monitor_count = sqlx::query_scalar::<_, i64>(
        "SELECT count(*)
         FROM program_source_monitors
         WHERE candidate_id = $1 AND verification_state <> 'retired'",
    )
    .bind(command.candidate_id)
    .fetch_one(&mut *tx)
    .await?;

    if unresolved_count == 0 {
        // Both the hold incident and the pre-deadline warning for this source
        // are answered by this review; the maintenance sweep re-raises the
        // warning if the new deadline is still inside its window.
        for fingerprint in [
            format!("source_review:{}", command.candidate_id),
            format!("source_review_upcoming:{}", command.candidate_id),
        ] {
            mazimi_worker::operations::resolve_event_tx(
                &mut tx,
                &fingerprint,
                Some(command.reviewer_id),
                command.note.as_deref(),
            )
            .await
            .map_err(ServiceError::OperationalEvent)?;
        }
    }

    let candidate_status = if unresolved_count > 0 {
        monitor.candidate_status
    } else if active_monitor_count == 0 {
        "rejected".to_string()
    } else {
        let next_check_at = sqlx::query_scalar::<_, Option<DateTime<Utc>>>(
            "SELECT min(review_due_at)
             FROM program_source_monitors
             WHERE candidate_id = $1 AND verification_state = 'current'",
        )
        .bind(command.candidate_id)
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE benefit_source_candidates
             SET status = 'approved', previous_content_hash = NULL,
                 raw_text = NULL, extracted_payload = NULL,
                 consecutive_failures = 0, last_error = NULL,
                 last_successful_check_at = now(), next_check_at = $2,
                 updated_at = now()
             WHERE id = $1",
        )
        .bind(command.candidate_id)
        .bind(next_check_at)
        .execute(&mut *tx)
        .await?;
        "approved".to_string()
    };

    if active_monitor_count == 0 {
        sqlx::query(
            "UPDATE benefit_source_candidates
             SET status = 'rejected', raw_text = NULL, extracted_payload = NULL,
                 next_check_at = NULL, updated_at = now()
             WHERE id = $1",
        )
        .bind(command.candidate_id)
        .execute(&mut *tx)
        .await?;
    }

    if let Some(check_id) = hold.as_ref().and_then(|hold| hold.source_check_id) {
        let open_holds = sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM program_source_holds
             WHERE source_check_id = $1 AND resolved_at IS NULL",
        )
        .bind(check_id)
        .fetch_one(&mut *tx)
        .await?;
        if open_holds == 0 {
            sqlx::query(
                "UPDATE benefit_source_checks
                 SET review_status = $2, reviewed_at = now(), reviewed_by = $3,
                     review_note = $4
                 WHERE id = $1",
            )
            .bind(check_id)
            .bind(if command.resolution == Resolution::Retired {
                "retired"
            } else {
                "confirmed"
            })
            .bind(command.reviewer_id)
            .bind(&command.note)
            .execute(&mut *tx)
            .await?;
        }
    } else {
        sqlx::query(
            "INSERT INTO benefit_source_checks (
                 candidate_id, check_kind, previous_content_hash, content_hash,
                 review_status, reviewed_at, reviewed_by, review_note
             ) VALUES ($1, $2, $3, $3, $4, now(), $5, $6)",
        )
        .bind(command.candidate_id)
        .bind(if command.resolution == Resolution::Retired {
            "retired"
        } else {
            "manual_confirmation"
        })
        .bind(&monitor.content_hash)
        .bind(if command.resolution == Resolution::Retired {
            "retired"
        } else {
            "confirmed"
        })
        .bind(command.reviewer_id)
        .bind(&command.note)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    Ok(ReviewOutcome {
        program_id: command.program_id,
        candidate_id: command.candidate_id,
        resolution,
        source_health: aggregate.source_health,
        is_active: final_active,
        program_status: final_status,
        candidate_status,
        next_review_at: aggregate.review_due_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolution_names_match_database_contract() {
        assert_eq!(
            Resolution::ConfirmedUnchanged.as_str(),
            "confirmed_unchanged"
        );
        assert_eq!(Resolution::ProgramUpdated.as_str(), "program_updated");
        assert_eq!(Resolution::Retired.as_str(), "retired");
    }
}
