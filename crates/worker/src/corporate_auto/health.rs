//! Self-reported health: one `corporate_pipeline_daily` row per KST day,
//! read by the admin operations API, plus threshold alerts. Every alert is a
//! WARN log with a stable `event` name and an operational event that
//! resolves itself once the condition clears.
//!
//! The row also carries the daily digest of sources that stay blocked
//! (robots disallow, bot wall, expired adoption …): their source events
//! were resolved with the reason, and they are reported here instead of
//! re-alerting.

use anyhow::Result;
use chrono::{FixedOffset, NaiveDate, Utc};
use serde_json::{json, Value};
use sqlx::PgPool;
use tracing::warn;

use super::reverify::CONFIRMING;
use crate::operations::{self, EventCommand, Severity};

/// Visible offers falling more than this share day over day.
pub const VISIBLE_DROP_THRESHOLD: f64 = 0.10;
/// Share of evidence fetches failing (with at least `MIN_FETCHES`).
pub const FETCH_FAILURE_THRESHOLD: f64 = 0.30;
const MIN_FETCHES: i64 = 5;
pub const CENSUS_STALE_DAYS: f64 = 8.0;
pub const OPTOUT_SPIKE: i64 = 5;
/// A domain with at least this many redesign-path decisions in 24 h, making
/// up at least half of its decisions, has probably been redesigned.
pub const REDESIGN_MIN_DECISIONS: i64 = 3;
/// Outcomes that mean the exact anchor no longer held.
const REDESIGN_OUTCOMES: &[&str] = &[
    "confirmed_fuzzy",
    "relocated",
    "url_move_proposed",
    "ambiguous",
];

/// The current moment in KST.
pub fn kst_now() -> chrono::NaiveDateTime {
    Utc::now()
        .with_timezone(&FixedOffset::east_opt(9 * 3_600).expect("KST offset"))
        .naive_local()
}

pub fn kst_today() -> NaiveDate {
    Utc::now()
        .with_timezone(&FixedOffset::east_opt(9 * 3_600).expect("KST offset"))
        .date_naive()
}

/// The census's lanes by subject, its domain rows labelled. A domain row
/// summarises the domain's pages and has no lane of its own: the census
/// records it as `impossible` (`domain_summary`), which is no coverage
/// figure, so it is reported as `domain_summary_rows` instead. A domain
/// served by a partner feed keeps its lane.
fn census_lanes(summary: &Value) -> Value {
    let mut lanes = summary.get("lanes").cloned().unwrap_or(Value::Null);
    if let Some(domain) = lanes.get_mut("domain").and_then(Value::as_object_mut) {
        let rows = domain.remove("impossible").unwrap_or(json!(0));
        domain.insert("domain_summary_rows".into(), rows);
    }
    lanes
}

pub async fn collect(pool: &PgPool) -> Result<Value> {
    // Door health and must-have coverage (`doors::metrics::health_block`).
    let doors = super::doors::metrics::health_block(pool).await?;
    // Hub offers a door published come and go with the hub's monthly
    // promotions (review H8): they are counted by the doors block, whose
    // drop alert discounts offers that ended by date, never here.
    let (total, visible): (i64, i64) = sqlx::query_as(
        "SELECT count(*), count(*) FILTER (WHERE is_active)
         FROM programs WHERE program_type IN ('corporate_benefit', 'campus_benefit')
           AND COALESCE(source_id, '') NOT LIKE 'door:%'",
    )
    .fetch_one(pool)
    .await?;
    // `overdue_evidence_auto` is what the evidence job should have checked
    // by its own selection and did not: no target, its schedule missed by
    // two hours, or a deadline due ([`evidence::DEADLINE_DUE_SQL`]) and not
    // checked for three (alarm `evidence_overdue`). A deadline passed on a
    // source evidence reads but cannot confirm is `past_deadline_evidence_auto`,
    // a digest count: the overdue sweep holds those offers. Only the ones
    // whose programme is still shown after the sweep's own grace
    // (`past_deadline_visible_evidence_auto`: state `review_due`, or
    // `current` past deadline + SOURCE_REVIEW_GRACE_HOURS) mean the sweep
    // failed to hide an unconfirmed offer, and alarm
    // (`evidence_past_deadline`). Inside the grace an offer is shown by
    // design.
    let (grace, _) = crate::source_monitoring::freshness_policy_from_env()?;
    let (
        monitors,
        evidence_auto,
        overdue,
        past_deadline,
        past_deadline_visible,
        overdue_evidence_auto,
    ): (i64, i64, i64, i64, i64, i64) = sqlx::query_as(&format!(
        "SELECT count(*) FILTER (WHERE monitor.verification_state <> 'retired'),
                count(*) FILTER (WHERE monitor.verification_mode = 'evidence_auto' AND monitor.verification_state <> 'retired'),
                count(*) FILTER (WHERE monitor.verification_state = 'current' AND monitor.review_due_at < now()
                                 OR monitor.verification_state = 'review_due'),
                count(*) FILTER (WHERE monitor.verification_mode = 'evidence_auto'
                                 AND (monitor.verification_state = 'current' AND monitor.review_due_at < now()
                                      OR monitor.verification_state = 'review_due')),
                count(*) FILTER (WHERE monitor.verification_mode = 'evidence_auto'
                                 AND (monitor.verification_state = 'current'
                                      AND monitor.review_due_at + make_interval(secs => $1) <= now()
                                      OR monitor.verification_state = 'review_due')
                                 AND program.is_active),
                count(*) FILTER (WHERE monitor.verification_mode = 'evidence_auto'
                                 AND monitor.verification_state <> 'retired'
                                 AND (target.candidate_id IS NULL
                                      OR target.next_check_at <= now() - interval '2 hours'
                                      OR ({deadline_due}
                                          AND (target.last_checked_at IS NULL
                                               OR target.last_checked_at <= now() - interval '3 hours'))))
         FROM program_source_monitors monitor
         LEFT JOIN programs program ON program.id = monitor.program_id
         LEFT JOIN corporate_evidence_targets target ON target.candidate_id = monitor.candidate_id",
        deadline_due = super::evidence::DEADLINE_DUE_SQL
    ))
    .bind(grace.num_seconds().max(0) as f64)
    .fetch_one(pool)
    .await?;
    // RC5: corporate offers on a person's timer. Target: none shown past
    // its deadline (`human_corporate_overdue_shown`, alarm), and fewer of
    // them every day as intake moves the readable ones to evidence.
    let (human_corporate, human_corporate_shown, human_corporate_overdue_shown): (i64, i64, i64) =
        sqlx::query_as(
            "SELECT count(*), count(*) FILTER (WHERE program.is_active),
                    count(*) FILTER (WHERE program.is_active AND monitor.review_due_at < now())
             FROM program_source_monitors monitor JOIN programs program ON program.id = monitor.program_id
             WHERE monitor.verification_mode = 'human' AND monitor.verification_state <> 'retired'
               AND program.program_type = 'corporate_benefit'",
        )
        .fetch_one(pool)
        .await?;
    // RC4: a section-changed hold on a page whose visible text reads as at
    // the offer's last confirmation (same visible-text hash): the section
    // digest moved with nothing a reader sees. Target 0 (alarm).
    let section_changed_same_text: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM corporate_evidence_checks c
         WHERE c.checked_at > now() - interval '24 hours' AND c.outcome = 'ambiguous'
           AND EXISTS (SELECT 1 FROM jsonb_array_elements(COALESCE(c.details->'facts', '[]'::jsonb)) e
                       WHERE COALESCE(e->'detail'->>'reason', e->>'detail') = $1)
           AND c.details->>'page_hash' = (
               SELECT prev.details->>'page_hash' FROM corporate_evidence_checks prev
               WHERE prev.program_id = c.program_id AND prev.checked_at < c.checked_at
                 AND prev.outcome IN ('confirmed', 'confirmed_fuzzy', 'relocated')
               ORDER BY prev.checked_at DESC LIMIT 1)",
    )
    .bind(super::resilience::SECTION_CHANGED)
    .fetch_one(pool)
    .await?;
    // RC6 / F7: sources with a programme (held or human) whose latest
    // intake decision is a reader or accuracy refusal, by outcome: the page
    // shapes the reader does not handle yet. Measurement only.
    let reader_backlog: Vec<(String, i64)> = sqlx::query_as(
        "SELECT latest.gate || ':' || latest.outcome, count(*)
         FROM (SELECT DISTINCT ON (candidate_id) candidate_id, gate, outcome
               FROM corporate_intake_decisions ORDER BY candidate_id, decided_at DESC, id DESC) latest
         WHERE latest.gate IN ('reader_a', 'reader_b', 'scope', 'ending_scan', 'display')
           AND EXISTS (SELECT 1 FROM program_source_monitors monitor JOIN programs program ON program.id = monitor.program_id
                       WHERE monitor.candidate_id = latest.candidate_id AND monitor.verification_state <> 'retired'
                         AND program.program_type = 'corporate_benefit' AND NOT program.is_active)
         GROUP BY 1 ORDER BY 1",
    )
    .fetch_all(pool)
    .await?;
    let outcomes: Vec<(String, String, i64, i64)> = sqlx::query_as(
        "SELECT mode, outcome, count(*), count(*) FILTER (WHERE applied)
         FROM corporate_evidence_checks WHERE checked_at > now() - interval '24 hours'
         GROUP BY mode, outcome ORDER BY mode, outcome",
    )
    .fetch_all(pool)
    .await?;
    let checked: i64 = sqlx::query_scalar(
        "SELECT count(DISTINCT candidate_id) FROM corporate_evidence_checks WHERE checked_at > now() - interval '24 hours'",
    )
    .fetch_one(pool)
    .await?;
    let blocked: Vec<(String, i64)> = sqlx::query_as(
        "SELECT last_outcome, count(*) FROM corporate_evidence_targets
         WHERE last_outcome IS NOT NULL AND last_outcome NOT IN ('confirmed')
         GROUP BY last_outcome ORDER BY last_outcome",
    )
    .fetch_all(pool)
    .await?;
    let digest: Vec<(String, String, Option<String>)> = sqlx::query_as(
        "SELECT t.candidate_id::text, t.last_outcome, c.url
         FROM corporate_evidence_targets t JOIN benefit_source_candidates c ON c.id = t.candidate_id
         WHERE t.last_outcome IS NOT NULL AND t.last_outcome <> 'confirmed'
         ORDER BY t.last_outcome, c.url LIMIT 200",
    )
    .fetch_all(pool)
    .await?;
    // Hidden sources the census rates other than direct: their events were
    // resolved once; the digest carries them from here on.
    let census_blocked: Vec<(String, i64)> = sqlx::query_as(
        "WITH latest AS (
             SELECT id FROM corporate_census_runs WHERE status = 'completed'
             ORDER BY finished_at DESC LIMIT 1
         ), lanes AS (
             SELECT DISTINCT ON (entry.candidate_id) entry.candidate_id, entry.lane || ':' || entry.reason AS lane
             FROM corporate_census_entries entry JOIN latest ON latest.id = entry.run_id
             WHERE entry.subject_kind = 'monitor' AND entry.lane <> 'direct'
             ORDER BY entry.candidate_id, entry.lane
         )
         SELECT lane, count(*) FROM lanes
         WHERE NOT EXISTS (SELECT 1 FROM program_source_monitors m JOIN programs p ON p.id = m.program_id
                           WHERE m.candidate_id = lanes.candidate_id AND p.is_active)
         GROUP BY lane ORDER BY lane",
    )
    .fetch_all(pool)
    .await?;
    let census: Option<(chrono::DateTime<Utc>, Value)> = sqlx::query_as(
        "SELECT finished_at, summary FROM corporate_census_runs WHERE status = 'completed'
         ORDER BY finished_at DESC LIMIT 1",
    )
    .fetch_optional(pool)
    .await?;
    let optouts: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM benefit_source_optouts WHERE received_at > now() - interval '24 hours'",
    )
    .fetch_one(pool)
    .await?;
    let optout_max_seconds: Option<f64> = sqlx::query_scalar(
        "SELECT max(extract(epoch FROM effective_at - received_at))::float8 FROM benefit_source_optouts
         WHERE received_at > now() - interval '24 hours'",
    )
    .fetch_one(pool)
    .await?;
    let per_domain: Vec<(String, i64, i64)> = sqlx::query_as(
        "SELECT d.domain,
                count(*) FILTER (WHERE c.outcome = ANY($1)),
                count(*)
         FROM corporate_evidence_checks c
         JOIN benefit_source_candidates s ON s.id = c.candidate_id
         JOIN benefit_source_domains d ON d.id = s.domain_id
         WHERE c.checked_at > now() - interval '24 hours'
         GROUP BY d.domain ORDER BY d.domain",
    )
    .bind(REDESIGN_OUTCOMES)
    .fetch_all(pool)
    .await?;
    // Fact decisions by hide reason, and the value changes the shadow
    // matcher read on changed pages (never applied): the false-hide share
    // before live is measured from these.
    use super::resilience::{reason_key, HIDE_REASONS};
    let reason_counts: Vec<(String, i64)> = sqlx::query_as(
        "SELECT COALESCE(f->'detail'->>'reason', f->>'detail'), count(*)
         FROM corporate_evidence_checks c,
              jsonb_array_elements(CASE WHEN jsonb_typeof(c.details->'facts') = 'array'
                                        THEN c.details->'facts' ELSE '[]'::jsonb END) f
         WHERE c.checked_at > now() - interval '24 hours'
           AND COALESCE(f->'detail'->>'reason', f->>'detail') = ANY($1)
         GROUP BY 1",
    )
    .bind(
        HIDE_REASONS
            .iter()
            .map(|r| r.to_string())
            .collect::<Vec<_>>(),
    )
    .fetch_all(pool)
    .await?;
    let shadow_updates: i64 = sqlx::query_scalar(
        "SELECT count(*)
         FROM corporate_evidence_checks c,
              jsonb_array_elements(CASE WHEN jsonb_typeof(c.details->'facts') = 'array'
                                        THEN c.details->'facts' ELSE '[]'::jsonb END) f
         WHERE c.checked_at > now() - interval '24 hours'
           AND jsonb_typeof(f->'detail') = 'object' AND f->'detail' ? 'shadow_update'",
    )
    .fetch_one(pool)
    .await?;
    let mut hidden_by_reason = serde_json::Map::new();
    for reason in HIDE_REASONS {
        let count = reason_counts
            .iter()
            .find(|(r, _)| r == reason)
            .map_or(0, |(_, n)| *n);
        if let Some(key) = reason_key(reason) {
            hidden_by_reason.insert(format!("hidden_{key}"), json!(count));
        }
    }
    let reextract_pending: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM corporate_evidence_targets WHERE reextract_requested_at IS NOT NULL",
    )
    .fetch_one(pool)
    .await?;
    // Measurement only: agree rate, disagreements by field and the list.
    // A failed audit query degrades this block only, never the row.
    let weekly_audit = match super::weekly_audit::health_block(pool).await {
        Ok(block) => block,
        Err(error) => {
            warn!(event = "corporate_health.weekly_audit_unavailable",
                  error = %crate::error_safety::persisted_error(&error),
                  "weekly audit aggregate unavailable; health row written without it");
            json!({"unavailable": true})
        }
    };
    // Telemetry of the current-fact rule: offers confirmed on a page that
    // reads otherwise than at their last confirmation (never a hide reason).
    let confirmed_on_changed_page: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM corporate_evidence_checks
         WHERE checked_at > now() - interval '24 hours'
           AND details->>'page_changed' = 'true' AND outcome = ANY($1)",
    )
    .bind(CONFIRMING)
    .fetch_one(pool)
    .await?;
    let open_source_events: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM operational_events WHERE status <> 'resolved' AND component = 'source_monitoring'",
    )
    .fetch_one(pool)
    .await?;

    let live: i64 = outcomes.iter().filter(|o| o.0 == "live").map(|o| o.2).sum();
    let mut by_outcome = serde_json::Map::new();
    let (mut fetches, mut failures, mut confirmed, mut hidden) = (0i64, 0i64, 0i64, 0i64);
    for (mode, outcome, count, applied) in &outcomes {
        by_outcome.insert(
            format!("{mode}:{outcome}"),
            json!({"count": count, "applied": applied}),
        );
        fetches += count;
        if matches!(outcome.as_str(), "fetch_failed" | "rate_limited") {
            failures += count;
        }
        if CONFIRMING.contains(&outcome.as_str()) {
            confirmed += count;
        } else if *applied > 0 {
            hidden += applied;
        }
    }
    let count_of = |name: &str| -> i64 {
        outcomes
            .iter()
            .filter(|(_, outcome, _, _)| outcome == name)
            .map(|(_, _, count, _)| count)
            .sum()
    };
    let redesign_suspected: Vec<Value> = per_domain
        .iter()
        .filter(|(_, redesign, total)| *redesign >= REDESIGN_MIN_DECISIONS && redesign * 2 >= *total)
        .map(|(domain, redesign, total)| json!({"domain": domain, "redesign_decisions": redesign, "decisions": total}))
        .collect();
    Ok(json!({
        "day": kst_today(),
        "programs": {"total": total, "visible": visible,
                     "visible_share": if total > 0 { visible as f64 / total as f64 } else { 0.0 }},
        "monitors": {"active": monitors, "evidence_auto": evidence_auto,
                     "overdue": overdue, "overdue_evidence_auto": overdue_evidence_auto,
                     "past_deadline_evidence_auto": past_deadline,
                     "past_deadline_visible_evidence_auto": past_deadline_visible,
                     "human_corporate": human_corporate,
                     "human_corporate_shown": human_corporate_shown,
                     "human_corporate_overdue_shown": human_corporate_overdue_shown},
        "evidence_24h": {"sources_checked": checked, "decisions": fetches, "confirmed": confirmed,
                          "hidden": hidden, "live_decisions": live, "by_outcome": by_outcome,
                          "fetch_failure_rate": if fetches > 0 { failures as f64 / fetches as f64 } else { 0.0 }},
        "blocked_by_reason": blocked.into_iter().collect::<std::collections::BTreeMap<_, _>>(),
        "digest_census_blocked_hidden": census_blocked.into_iter().collect::<std::collections::BTreeMap<_, _>>(),
        "digest_still_blocked": digest.into_iter().map(|(id, outcome, url)| json!({"candidate_id": id, "outcome": outcome, "url": url})).collect::<Vec<_>>(),
        "census": census.as_ref().map(|(at, summary)| json!({
            "finished_at": at,
            "age_days": (Utc::now() - *at).num_minutes() as f64 / 1_440.0,
            "lanes": census_lanes(summary),
        })),
        "optouts_24h": {"received": optouts, "max_effective_seconds": optout_max_seconds},
        "resilience_24h": {
            "confirmed_by_anchor": count_of("confirmed"),
            "confirmed_by_normalized_anchor": count_of("confirmed_fuzzy"),
            "relocated": count_of("relocated"),
            "hidden_by_reason": hidden_by_reason,
            "shadow_updates_read": shadow_updates,
            "url_moves_proposed": count_of("url_move_proposed"),
            "url_moves_adopted": count_of("url_adopted"),
            "ambiguous": count_of("ambiguous"),
            "hidden": hidden,
            "retired": count_of("retired_gone"),
            "redesign_suspected": redesign_suspected,
            "fresh_extraction_requested": reextract_pending,
            "confirmed_on_changed_page": confirmed_on_changed_page,
            "section_changed_same_text": section_changed_same_text,
        },
        "reader_backlog": reader_backlog.into_iter().collect::<std::collections::BTreeMap<_, _>>(),
        "open_source_events": open_source_events,
        "weekly_audit": weekly_audit,
        "doors": doors,
    }))
}

#[derive(Debug, Default, PartialEq)]
pub struct Alerts {
    pub visible_drop: Option<f64>,
    pub fetch_failure_rate: Option<f64>,
    pub census_stale_days: Option<f64>,
    pub optout_spike: Option<i64>,
    pub overdue_evidence_auto: Option<i64>,
    /// Evidence_auto monitors past their review deadline (or in state
    /// `review_due`) whose programme is still shown: the overdue sweep did
    /// not hide an unconfirmed offer. Held ones are only digest counts
    /// (`monitors.past_deadline_evidence_auto`).
    pub past_deadline_evidence_auto: Option<i64>,
    /// Domains whose evidence mostly needed the redesign paths today.
    pub redesign_suspected: Option<Vec<String>>,
    /// Offers whose weekly-audit read now disagrees while their previous
    /// read agreed (in the last 24 h); titles. Only once the audit is armed.
    pub audit_new_disagreements: Option<Vec<String>>,
    /// Shown offers whose current weekly-audit verdict disagrees: a standing
    /// warning until none does. Only once the audit is armed.
    pub audit_visible_disagreements: Option<i64>,
    /// Once armed, the share of established programmes with a verdict in
    /// the cycle fell below `weekly_audit::COVERAGE_FLOOR` while the audit
    /// is on.
    pub audit_coverage_short: Option<f64>,
    /// Must-have brands visible fell more than
    /// `doors::metrics::VISIBLE_BRANDS_DROP` day over day after discounting
    /// those whose offers ended by date (never the monthly expiry cliff).
    pub must_have_visible_drop: Option<f64>,
    /// Doors lost for more than 7 days.
    pub doors_lost_over_7d: Option<i64>,
    /// Corporate offers shown on a person's timer past its deadline (RC5).
    pub human_corporate_overdue_shown: Option<i64>,
    /// Section-changed holds on an unchanged visible text today (RC4).
    pub section_changed_same_text: Option<i64>,
}

/// Threshold evaluation. Pure; `previous` is yesterday's metrics.
pub fn evaluate(today: &Value, previous: Option<&Value>, census_enabled: bool) -> Alerts {
    let mut alerts = Alerts {
        must_have_visible_drop: super::doors::metrics::visible_brands_drop(today, previous),
        doors_lost_over_7d: today["doors"]["lost_over_7d"].as_i64().filter(|n| *n > 0),
        ..Alerts::default()
    };
    let visible = today["programs"]["visible"].as_f64().unwrap_or(0.0);
    if let Some(before) = previous.and_then(|p| p["programs"]["visible"].as_f64()) {
        if before > 0.0 {
            let drop = (before - visible) / before;
            if drop > VISIBLE_DROP_THRESHOLD {
                alerts.visible_drop = Some(drop);
            }
        }
    }
    let decisions = today["evidence_24h"]["decisions"].as_i64().unwrap_or(0);
    let rate = today["evidence_24h"]["fetch_failure_rate"]
        .as_f64()
        .unwrap_or(0.0);
    if decisions >= MIN_FETCHES && rate > FETCH_FAILURE_THRESHOLD {
        alerts.fetch_failure_rate = Some(rate);
    }
    if census_enabled {
        let age = today["census"]["age_days"]
            .as_f64()
            .unwrap_or(f64::INFINITY);
        if age > CENSUS_STALE_DAYS {
            alerts.census_stale_days = Some(if age.is_finite() { age } else { -1.0 });
        }
    }
    let optouts = today["optouts_24h"]["received"].as_i64().unwrap_or(0);
    if optouts > OPTOUT_SPIKE {
        alerts.optout_spike = Some(optouts);
    }
    let overdue = today["monitors"]["overdue_evidence_auto"]
        .as_i64()
        .unwrap_or(0);
    if overdue > 0 {
        alerts.overdue_evidence_auto = Some(overdue);
    }
    let past_deadline = today["monitors"]["past_deadline_visible_evidence_auto"]
        .as_i64()
        .unwrap_or(0);
    if past_deadline > 0 {
        alerts.past_deadline_evidence_auto = Some(past_deadline);
    }
    let positive = |value: &Value| value.as_i64().filter(|n| *n > 0);
    alerts.human_corporate_overdue_shown =
        positive(&today["monitors"]["human_corporate_overdue_shown"]);
    alerts.section_changed_same_text =
        positive(&today["resilience_24h"]["section_changed_same_text"]);
    let redesigned: Vec<String> = today["resilience_24h"]["redesign_suspected"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|d| d["domain"].as_str().map(str::to_string))
        .collect();
    if !redesigned.is_empty() {
        alerts.redesign_suspected = Some(redesigned);
    }
    // The weekly audit arms after one full cycle and stays armed
    // (`armed_ever`); its alerts read only the current cycle, never a
    // day-over-day count. A coverage dip is its own alert.
    let audit = &today["weekly_audit"];
    let armed = audit["armed_ever"]
        .as_bool()
        .or(audit["armed"].as_bool())
        .unwrap_or(false);
    if armed && audit["enabled"].as_bool() != Some(false) {
        let coverage = audit["established_coverage"].as_f64().unwrap_or(1.0);
        if coverage < super::weekly_audit::COVERAGE_FLOOR {
            alerts.audit_coverage_short = Some(coverage);
        }
    }
    if armed {
        let new: Vec<String> = audit["new_disagreements"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|o| o["title"].as_str().map(str::to_string))
            .collect();
        if !new.is_empty() {
            alerts.audit_new_disagreements = Some(new);
        }
        let visible = audit["visible_disagreements"].as_i64().unwrap_or(0);
        if visible > 0 {
            alerts.audit_visible_disagreements = Some(visible);
        }
    }
    alerts
}

async fn raise(
    pool: &PgPool,
    name: &'static str,
    active: bool,
    severity: Severity,
    message: &'static str,
    details: Value,
) -> Result<()> {
    let fingerprint = format!("corporate_health:{name}");
    if active {
        warn!(event = %format!("corporate_health.{name}"), details = %details, "{message}");
        operations::record_event(
            pool,
            &EventCommand {
                fingerprint: &fingerprint,
                severity,
                component: "corporate_auto",
                event_type: name,
                subject_type: None,
                subject_id: None,
                message,
                details,
            },
        )
        .await?;
    } else {
        operations::resolve_event(
            pool,
            &fingerprint,
            None,
            Some("Automatically resolved: health threshold no longer exceeded"),
        )
        .await?;
    }
    Ok(())
}

/// Writes today's row and raises or clears the threshold events.
pub async fn run_daily(pool: &PgPool, census_enabled: bool) -> Result<Alerts> {
    let today = collect(pool).await?;
    let day = kst_today();
    let previous: Option<Value> = sqlx::query_scalar(
        "SELECT metrics FROM corporate_pipeline_daily WHERE day < $1 ORDER BY day DESC LIMIT 1",
    )
    .bind(day)
    .fetch_optional(pool)
    .await?;
    let alerts = evaluate(&today, previous.as_ref(), census_enabled);
    let mut metrics = today.clone();
    metrics["alerts"] = json!({
        "visible_drop": alerts.visible_drop, "fetch_failure_rate": alerts.fetch_failure_rate,
        "census_stale_days": alerts.census_stale_days, "optout_spike": alerts.optout_spike,
        "overdue_evidence_auto": alerts.overdue_evidence_auto,
        "past_deadline_evidence_auto": alerts.past_deadline_evidence_auto,
        "redesign_suspected": alerts.redesign_suspected,
        "audit_new_disagreements": alerts.audit_new_disagreements,
        "audit_visible_disagreements": alerts.audit_visible_disagreements,
        "audit_coverage_short": alerts.audit_coverage_short,
        "must_have_visible_drop": alerts.must_have_visible_drop,
        "doors_lost_over_7d": alerts.doors_lost_over_7d,
        "human_corporate_overdue_shown": alerts.human_corporate_overdue_shown,
        "section_changed_same_text": alerts.section_changed_same_text,
    });
    sqlx::query(
        "INSERT INTO corporate_pipeline_daily (day, metrics) VALUES ($1, $2)
         ON CONFLICT (day) DO UPDATE SET metrics = EXCLUDED.metrics, updated_at = now()",
    )
    .bind(day)
    .bind(&metrics)
    .execute(pool)
    .await?;
    raise(
        pool,
        "visible_drop",
        alerts.visible_drop.is_some(),
        Severity::Warning,
        "Visible corporate offers dropped more than 10% day over day",
        json!({"drop": alerts.visible_drop}),
    )
    .await?;
    raise(
        pool,
        "fetch_failure_rate",
        alerts.fetch_failure_rate.is_some(),
        Severity::Warning,
        "Corporate evidence fetch failure rate exceeds 30%",
        json!({"rate": alerts.fetch_failure_rate}),
    )
    .await?;
    raise(
        pool,
        "census_stale",
        alerts.census_stale_days.is_some(),
        Severity::Warning,
        "The corporate census has not completed in 8 days",
        json!({"age_days": alerts.census_stale_days}),
    )
    .await?;
    raise(
        pool,
        "optout_spike",
        alerts.optout_spike.is_some(),
        Severity::Warning,
        "More than 5 publisher opt-outs in 24 hours",
        json!({"received": alerts.optout_spike}),
    )
    .await?;
    raise(
        pool,
        "evidence_overdue",
        alerts.overdue_evidence_auto.is_some(),
        Severity::Warning,
        "Evidence-verified monitors were due a check the evidence job did not make; it may not be running",
        json!({"overdue": alerts.overdue_evidence_auto}),
    )
    .await?;
    raise(
        pool,
        "evidence_past_deadline",
        alerts.past_deadline_evidence_auto.is_some(),
        Severity::Error,
        "Evidence-verified offers past their review deadline are still shown; the overdue sweep did not hide them",
        json!({"past_deadline_visible": alerts.past_deadline_evidence_auto}),
    )
    .await?;
    raise(
        pool,
        "redesign_suspected",
        alerts.redesign_suspected.is_some(),
        Severity::Warning,
        "Evidence on a source domain mostly needed relocation today; the site was probably redesigned",
        json!({"domains": alerts.redesign_suspected}),
    )
    .await?;
    raise(
        pool,
        "weekly_audit_new_disagreement",
        alerts.audit_new_disagreements.is_some(),
        Severity::Warning,
        "A corporate offer that agreed with its page on the previous weekly audit now disagrees",
        json!({"offers": alerts.audit_new_disagreements}),
    )
    .await?;
    raise(
        pool,
        "weekly_audit_visible_disagreement",
        alerts.audit_visible_disagreements.is_some(),
        Severity::Warning,
        "Shown corporate offers disagree with a fresh re-extraction of their page",
        json!({
            "visible_disagreements": alerts.audit_visible_disagreements,
            "by_field": today["weekly_audit"]["disagreement_by_field"],
        }),
    )
    .await?;
    raise(
        pool,
        "weekly_audit_coverage_short",
        alerts.audit_coverage_short.is_some(),
        Severity::Warning,
        "The weekly audit gave fewer than 90% of established corporate offers a verdict this cycle",
        json!({
            "coverage": alerts.audit_coverage_short,
            "established": today["weekly_audit"]["established"],
            "established_audited": today["weekly_audit"]["established_audited"],
        }),
    )
    .await?;
    raise(
        pool,
        "must_have_visible_drop",
        alerts.must_have_visible_drop.is_some(),
        Severity::Error,
        "Must-have brands shown fell more than 10% day over day beyond the offers that ended by date",
        json!({"drop": alerts.must_have_visible_drop, "must_have": today["doors"]["must_have"]}),
    )
    .await?;
    raise(
        pool,
        "doors_lost_over_7d",
        alerts.doors_lost_over_7d.is_some(),
        Severity::Warning,
        "Corporate doors have been lost for more than 7 days without a replacement",
        json!({"lost_over_7d": alerts.doors_lost_over_7d}),
    )
    .await?;
    raise(
        pool,
        "human_corporate_overdue_shown",
        alerts.human_corporate_overdue_shown.is_some(),
        Severity::Error,
        "Corporate offers are shown on a manual review timer past its deadline",
        json!({"shown": alerts.human_corporate_overdue_shown}),
    )
    .await?;
    raise(
        pool,
        "section_changed_same_text",
        alerts.section_changed_same_text.is_some(),
        Severity::Warning,
        "Corporate offers were held as section changed while their visible text did not change",
        json!({"holds": alerts.section_changed_same_text}),
    )
    .await?;
    Ok(alerts)
}

/// Worker entry point: runs while the census, auto-verify or the weekly
/// audit is enabled, so
/// the health row cannot be forgotten when the features run.
pub async fn run_scheduled(pool: &PgPool) {
    let Ok(flags) = super::Flags::from_env() else {
        warn!(
            event = "corporate_health.config_invalid",
            "corporate flags are invalid; health row skipped"
        );
        return;
    };
    if !(flags.census_enabled || flags.verify_enabled || flags.weekly_audit_enabled) {
        return;
    }
    if let Err(error) = run_daily(pool, flags.census_enabled).await {
        warn!(event = "corporate_health.failed", error = %crate::error_safety::persisted_error(&error), "corporate health row failed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn census_domain_rows_are_labelled_as_summaries_not_impossible_lanes() {
        let summary = json!({"lanes": {
            "domain": {"impossible": 40, "feed": 2},
            "candidate": {"impossible": 3, "direct_html": 50},
        }});
        assert_eq!(
            census_lanes(&summary),
            json!({
                "domain": {"domain_summary_rows": 40, "feed": 2},
                "candidate": {"impossible": 3, "direct_html": 50},
            })
        );
        assert_eq!(census_lanes(&json!({})), Value::Null);
    }

    fn metrics(visible: i64, decisions: i64, rate: f64, census_age: Option<f64>) -> Value {
        json!({
            "programs": {"visible": visible},
            "evidence_24h": {"decisions": decisions, "fetch_failure_rate": rate},
            "census": census_age.map(|age| json!({"age_days": age})),
            "optouts_24h": {"received": 0},
            "monitors": {"overdue_evidence_auto": 0},
        })
    }

    #[test]
    fn visible_drop_over_ten_percent_alerts() {
        let alerts = evaluate(
            &metrics(40, 0, 0.0, Some(1.0)),
            Some(&metrics(50, 0, 0.0, Some(1.0))),
            true,
        );
        assert!(alerts.visible_drop.is_some_and(|d| (d - 0.2).abs() < 1e-9));
        let calm = evaluate(
            &metrics(46, 0, 0.0, Some(1.0)),
            Some(&metrics(50, 0, 0.0, Some(1.0))),
            true,
        );
        assert_eq!(calm, Alerts::default());
    }

    #[test]
    fn failure_rate_needs_enough_fetches() {
        assert!(evaluate(&metrics(10, 10, 0.5, Some(1.0)), None, true)
            .fetch_failure_rate
            .is_some());
        assert!(evaluate(&metrics(10, 3, 1.0, Some(1.0)), None, true)
            .fetch_failure_rate
            .is_none());
    }

    #[test]
    fn missing_or_old_census_alerts_only_when_enabled() {
        assert!(evaluate(&metrics(10, 0, 0.0, None), None, true)
            .census_stale_days
            .is_some());
        assert!(evaluate(&metrics(10, 0, 0.0, Some(9.0)), None, true)
            .census_stale_days
            .is_some());
        assert!(evaluate(&metrics(10, 0, 0.0, None), None, false)
            .census_stale_days
            .is_none());
    }

    /// 2026-09-23 onwards: 12 evidence_auto offers past their deadline, all
    /// already held by the overdue sweep, kept `evidence_past_deadline` open
    /// through the whole shadow period. Held offers are digest counts; only
    /// a past-deadline offer still shown alarms.
    #[test]
    fn past_deadline_alarms_only_for_offers_still_shown() {
        let mut today = metrics(10, 0, 0.0, Some(1.0));
        today["monitors"] = json!({"overdue_evidence_auto": 0,
            "past_deadline_evidence_auto": 12, "past_deadline_visible_evidence_auto": 0});
        assert_eq!(evaluate(&today, None, true), Alerts::default());
        today["monitors"]["past_deadline_visible_evidence_auto"] = json!(2);
        assert_eq!(
            evaluate(&today, None, true).past_deadline_evidence_auto,
            Some(2)
        );
    }

    #[test]
    fn a_domain_mostly_on_redesign_paths_alerts() {
        let mut today = metrics(10, 0, 0.0, Some(1.0));
        today["resilience_24h"] = json!({"redesign_suspected": [
            {"domain": "shop.example.com", "redesign_decisions": 4, "decisions": 5}
        ]});
        assert_eq!(
            evaluate(&today, None, true).redesign_suspected,
            Some(vec!["shop.example.com".to_string()])
        );
        today["resilience_24h"] = json!({"redesign_suspected": []});
        assert!(evaluate(&today, None, true).redesign_suspected.is_none());
    }

    fn audit(armed: bool, visible: i64, new: &[&str]) -> Value {
        let mut m = metrics(10, 0, 0.0, Some(1.0));
        m["weekly_audit"] = json!({
            "armed": armed,
            "visible_disagreements": visible,
            "new_disagreements": new.iter().map(|t| json!({"title": t})).collect::<Vec<_>>(),
        });
        m
    }

    #[test]
    fn weekly_audit_alerts_read_the_cycle_not_a_day_over_day_count() {
        // A transition and a standing disagreement.
        let alerts = evaluate(&audit(true, 2, &["A 학생 할인"]), None, true);
        assert_eq!(
            alerts.audit_new_disagreements,
            Some(vec!["A 학생 할인".to_string()])
        );
        assert_eq!(alerts.audit_visible_disagreements, Some(2));
        // The next day: no new transition, the standing warning holds,
        // whatever yesterday's row said.
        for previous in [
            audit(true, 2, &["A 학생 할인"]),
            audit(true, 5, &[]),
            metrics(10, 0, 0.0, None),
        ] {
            let alerts = evaluate(&audit(true, 2, &[]), Some(&previous), true);
            assert_eq!(alerts.audit_new_disagreements, None);
            assert_eq!(alerts.audit_visible_disagreements, Some(2));
        }
        // Resolved only when no shown offer disagrees.
        assert_eq!(
            evaluate(&audit(true, 0, &[]), None, true),
            Alerts::default()
        );
    }

    #[test]
    fn once_armed_a_coverage_dip_alerts_and_never_silences_the_standing_warning() {
        let dipped = |enabled: bool| {
            let mut m = audit(false, 2, &[]);
            m["weekly_audit"]["armed_ever"] = json!(true);
            m["weekly_audit"]["established_coverage"] = json!(0.5);
            m["weekly_audit"]["enabled"] = json!(enabled);
            evaluate(&m, None, true)
        };
        let alerts = dipped(true);
        assert_eq!(alerts.audit_visible_disagreements, Some(2), "stays open");
        assert_eq!(alerts.audit_coverage_short, Some(0.5));
        // The audit switched off: no coverage alert, the standing one holds.
        let alerts = dipped(false);
        assert_eq!(alerts.audit_coverage_short, None);
        assert_eq!(alerts.audit_visible_disagreements, Some(2));
        // Coverage at the floor is enough.
        let mut m = audit(true, 0, &[]);
        m["weekly_audit"]["established_coverage"] = json!(0.9);
        assert_eq!(evaluate(&m, None, true), Alerts::default());
    }

    #[test]
    fn weekly_audit_alerts_stay_silent_before_a_full_cycle() {
        assert_eq!(
            evaluate(&audit(false, 3, &["A 학생 할인"]), None, true),
            Alerts::default()
        );
        // An unavailable aggregate (the query failed) alerts nothing.
        let mut m = metrics(10, 0, 0.0, Some(1.0));
        m["weekly_audit"] = json!({"unavailable": true});
        assert_eq!(evaluate(&m, None, true), Alerts::default());
    }

    /// RC4 and RC5 alarms: both counts have a target of 0.
    #[test]
    fn a_manual_timer_past_due_and_a_section_hold_on_unchanged_text_alert() {
        let mut today = json!({"monitors": {"human_corporate_overdue_shown": 0},
                               "resilience_24h": {"section_changed_same_text": 0}});
        let quiet = evaluate(&today, None, false);
        assert_eq!(quiet.human_corporate_overdue_shown, None);
        assert_eq!(quiet.section_changed_same_text, None);
        today["monitors"]["human_corporate_overdue_shown"] = json!(3);
        today["resilience_24h"]["section_changed_same_text"] = json!(2);
        let alerts = evaluate(&today, None, false);
        assert_eq!(alerts.human_corporate_overdue_shown, Some(3));
        assert_eq!(alerts.section_changed_same_text, Some(2));
    }
}
