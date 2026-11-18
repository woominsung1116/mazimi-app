//! Lifecycle of the source-monitoring operator events once evidence
//! verification owns a source.
//!
//! The hash path opens one event per source candidate
//! (`source_review:{candidate}` for overdue, changed, unreachable and
//! unavailable sources, `source_review_upcoming:{candidate}` for the 7-day
//! warning). Under live evidence verification nobody needs to act on them:
//! - confirmed → resolved, the offer is current again;
//! - hidden by evidence (quote missing, value changed, robots disallow,
//!   bot wall, gone) → resolved, the offer is hidden and comes back on its
//!   own when evidence re-verifies;
//! - still blocked while every programme on the source is already hidden →
//!   resolved with the blocking reason. The reason stays in
//!   `corporate_evidence_targets.last_outcome` and is listed in the daily
//!   digest instead of re-alerting;
//! - transient (one or two fetch failures, 429) → left open.

use anyhow::Result;
use serde_json::{json, Value};
use sqlx::{PgPool, Postgres, Transaction};
use std::collections::BTreeMap;
use uuid::Uuid;

use crate::operations;

/// Event types the hash path raises per source candidate.
pub const SOURCE_EVENT_TYPES: &[&str] = &[
    "source_verification_overdue",
    "source_review_upcoming",
    "source_content_changed",
    "source_unavailable",
    "source_unreachable",
];

const HIDING_OUTCOMES: &[&str] = &[
    "evidence_failed",
    "quote_missing",
    "window_changed",
    "field_mismatch",
    "no_evidence",
    "robots_disallowed",
    "blocked",
    "opted_out",
    "policy_blocked",
    "domain_withdrawn",
    "gone",
    "retired_gone",
    "redirected",
    "unsupported",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Projection {
    Confirmed,
    Hidden,
    StillBlocked,
    Open,
}

impl Projection {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Confirmed => "resolve_confirmed",
            Self::Hidden => "resolve_hidden",
            Self::StillBlocked => "resolve_still_blocked",
            Self::Open => "stay_open",
        }
    }
}

/// What resolving does for an evidence outcome. `all_hidden` = every
/// programme on the source is already inactive.
pub fn project(outcome: &str, all_hidden: bool) -> Projection {
    if outcome == "confirmed" {
        Projection::Confirmed
    } else if HIDING_OUTCOMES.contains(&outcome) {
        Projection::Hidden
    } else if all_hidden {
        Projection::StillBlocked
    } else {
        Projection::Open
    }
}

fn note(projection: Projection, outcome: &str) -> String {
    match projection {
        Projection::Confirmed => "Automatically resolved: verbatim evidence re-verified on the live official page; the offer is current".into(),
        Projection::Hidden => format!(
            "Automatically resolved: evidence auto-verification hid the offer ({outcome}); it returns on its own when the evidence re-verifies"
        ),
        Projection::StillBlocked => format!(
            "Automatically resolved: the offer stays hidden, still blocked ({outcome}); tracked in the daily corporate digest"
        ),
        Projection::Open => String::new(),
    }
}

async fn all_hidden(tx: &mut Transaction<'_, Postgres>, candidate_id: Uuid) -> Result<bool> {
    Ok(sqlx::query_scalar(
        "SELECT NOT EXISTS (
             SELECT 1 FROM program_source_monitors m JOIN programs p ON p.id = m.program_id
             WHERE m.candidate_id = $1 AND p.is_active
         )",
    )
    .bind(candidate_id)
    .fetch_one(&mut **tx)
    .await?)
}

/// Live only: resolves the candidate's source events after a decision.
pub async fn resolve_for_candidate(
    tx: &mut Transaction<'_, Postgres>,
    candidate_id: Uuid,
    outcome: &str,
) -> Result<u64> {
    let hidden = all_hidden(tx, candidate_id).await?;
    let projection = project(outcome, hidden);
    if projection == Projection::Open {
        return Ok(0);
    }
    let text = note(projection, outcome);
    let mut resolved = 0;
    for fingerprint in [
        format!("source_review:{candidate_id}"),
        format!("source_review_upcoming:{candidate_id}"),
    ] {
        resolved +=
            u64::from(operations::resolve_event_tx(tx, &fingerprint, None, Some(&text)).await?);
    }
    Ok(resolved)
}

/// Live only: sources that evidence verification can never take over
/// because the latest census rates them other than direct (robots
/// disallow, bot wall, render-only …) and whose programmes are all hidden
/// already. Their events resolve once with the census reason, and the daily
/// digest lists them; nothing re-raises them while they stay hidden.
pub async fn settle_census_blocked(pool: &PgPool) -> Result<u64> {
    let rows = sqlx::query_as::<_, (String, String)>(
        "WITH latest AS (
             SELECT id FROM corporate_census_runs
             WHERE status = 'completed' AND finished_at > now() - make_interval(days => $2)
             ORDER BY finished_at DESC LIMIT 1
         ), lanes AS (
             SELECT DISTINCT ON (entry.candidate_id) entry.candidate_id, entry.lane || ':' || entry.reason AS lane
             FROM corporate_census_entries entry JOIN latest ON latest.id = entry.run_id
             WHERE entry.subject_kind = 'monitor' AND entry.lane <> 'direct'
             ORDER BY entry.candidate_id, entry.lane
         )
         SELECT event.fingerprint, lanes.lane
         FROM operational_events event
         JOIN lanes ON event.fingerprint IN ('source_review:' || lanes.candidate_id::text,
                                             'source_review_upcoming:' || lanes.candidate_id::text)
         WHERE event.status <> 'resolved' AND event.component = 'source_monitoring'
           AND event.event_type = ANY($1)
           AND NOT EXISTS (SELECT 1 FROM program_source_monitors m
                           WHERE m.candidate_id = lanes.candidate_id AND m.verification_mode = 'evidence_auto')
           AND NOT EXISTS (SELECT 1 FROM program_source_monitors m JOIN programs p ON p.id = m.program_id
                           WHERE m.candidate_id = lanes.candidate_id AND p.is_active)",
    )
    .bind(SOURCE_EVENT_TYPES)
    .bind(super::evidence::CENSUS_MAX_AGE_DAYS as i32)
    .fetch_all(pool)
    .await?;
    let mut resolved = 0;
    for (fingerprint, lane) in rows {
        let text = note(Projection::StillBlocked, &format!("census {lane}"));
        resolved +=
            u64::from(operations::resolve_event(pool, &fingerprint, None, Some(&text)).await?);
    }
    Ok(resolved)
}

/// Dry run over every open source-monitoring event: which would resolve
/// and how, from the latest evidence decision (shadow or live) for its
/// source, or why the source is not under evidence verification yet.
pub async fn dry_run_report(pool: &PgPool) -> Result<Value> {
    let rows = sqlx::query_as::<_, (String, String, String, Option<String>, Option<String>, Option<bool>, Option<String>, Option<String>, Option<String>)>(
        "WITH open_events AS (
             SELECT fingerprint, event_type, severity,
                    NULLIF(substring(fingerprint FROM '([0-9a-f-]{36})$'), '')::uuid AS candidate_id
             FROM operational_events
             WHERE status <> 'resolved' AND component = 'source_monitoring'
               AND event_type = ANY($1)
         ), latest_check AS (
             SELECT DISTINCT ON (candidate_id) candidate_id, outcome, mode
             FROM corporate_evidence_checks ORDER BY candidate_id, checked_at DESC
         ), latest_census AS (
             SELECT DISTINCT ON (entry.candidate_id) entry.candidate_id, entry.lane || ':' || entry.reason AS lane
             FROM corporate_census_entries entry
             JOIN corporate_census_runs run ON run.id = entry.run_id AND run.status = 'completed'
             WHERE entry.subject_kind = 'monitor'
             ORDER BY entry.candidate_id, run.finished_at DESC
         )
         SELECT e.fingerprint, e.event_type, e.severity, e.candidate_id::text,
                c.outcome,
                NOT EXISTS (SELECT 1 FROM program_source_monitors m JOIN programs p ON p.id = m.program_id
                            WHERE m.candidate_id = e.candidate_id AND p.is_active) AS all_hidden,
                (SELECT string_agg(DISTINCT m.verification_mode, ',') FROM program_source_monitors m
                 WHERE m.candidate_id = e.candidate_id AND m.verification_state <> 'retired') AS modes,
                census.lane,
                c.mode
         FROM open_events e
         LEFT JOIN latest_check c ON c.candidate_id = e.candidate_id
         LEFT JOIN latest_census census ON census.candidate_id = e.candidate_id
         ORDER BY e.event_type, e.fingerprint",
    )
    .bind(SOURCE_EVENT_TYPES)
    .fetch_all(pool)
    .await?;

    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut items = Vec::new();
    for (fingerprint, event_type, severity, candidate, outcome, hidden, modes, census, mode) in rows
    {
        let modes = modes.unwrap_or_default();
        let (projection, reason) = match outcome.as_deref() {
            Some(outcome) => (
                project(outcome, hidden.unwrap_or(false)).as_str(),
                outcome.to_string(),
            ),
            None if !modes.contains("evidence_auto") => match census.as_deref() {
                Some(lane) if !lane.starts_with("direct:") && hidden.unwrap_or(false) => {
                    ("resolve_still_blocked", format!("census {lane}"))
                }
                Some(lane) if lane.starts_with("direct:") => (
                    "stay_open",
                    format!("census {lane}; evidence_auto once every programme on it has evidence"),
                ),
                Some(lane) => ("stay_open", format!("census {lane}; offer still visible")),
                None => ("stay_open", "no census entry".into()),
            },
            None => ("stay_open", "evidence_auto, not checked yet".into()),
        };
        *counts.entry(projection.to_string()).or_default() += 1;
        *counts.entry(format!("{projection}:{reason}")).or_default() += 1;
        items.push(json!({
            "fingerprint": fingerprint, "event_type": event_type, "severity": severity,
            "candidate_id": candidate, "projection": projection, "reason": reason,
            "decision_mode": mode, "census": census, "monitor_modes": modes,
        }));
    }
    Ok(json!({"open_events": items.len(), "counts": counts, "items": items}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projections_cover_every_outcome_class() {
        assert_eq!(project("confirmed", false), Projection::Confirmed);
        assert_eq!(project("quote_missing", false), Projection::Hidden);
        assert_eq!(project("robots_disallowed", true), Projection::Hidden);
        assert_eq!(project("fetch_failed", false), Projection::Open);
        assert_eq!(project("rate_limited", true), Projection::StillBlocked);
        assert_eq!(project("adoption_expired", true), Projection::StillBlocked);
        assert_eq!(project("adoption_expired", false), Projection::Open);
    }
}
