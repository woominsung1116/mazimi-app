//! Fresh extraction for sources the evidence job could no longer confirm.
//!
//! **Retired as a scheduled job (2026-11-11).** The evidence job's queue
//! (`reextract_requested_at`) has one consumer, intake's held-source lane
//! ([`super::evidence::Exit::Reread`]); `CORPORATE_REEXTRACT_ENABLED` no
//! longer does anything. The gates below ([`gate_decision`],
//! [`gate_offer`], [`attempt_domain`]) are what intake and the weekly audit
//! run, and [`run`] stays a library entry point for one-off tools.
//!
//! The resilience layer sets `corporate_evidence_targets.reextract_requested_at`
//! when a stored fact can no longer be shown as the same offer (identity
//! changed, quote gone, ending context). This job consumes that flag: it
//! fetches the source page again **as if it were a new candidate** and
//! writes what it finds as a proposal. Nothing is re-derived from the old
//! fact and nothing is published.
//!
//! Gates, all of them on every attempt, in this order:
//! 1. the source still has a non-retired `evidence_auto` monitor (a monitor
//!    returned to `human` belongs to a person again);
//! 2. the domain passes [`policy::ELIGIBLE_DOMAIN_SQL`];
//! 3. [`bootstrap::precheck`]: brand, verified domain, no rejected or
//!    terms-restricted review, https on the brand's own host, no archive,
//!    aggregator or third-party content;
//! 4. publisher opt-out before every request (robots, page, redirect hop,
//!    terms page);
//! 5. robots for our agent on the exact path, the domain's ToS (a specific
//!    collection clause stops it, a generic one only while G1 is on), the
//!    polite [`DomainSession`] (10 s floor or Crawl-delay, shared domain
//!    slot, stop on 401/403/429/challenge);
//! 6. the lane must be `direct:direct_html` ([`policy::choose_lane`]);
//! 7. the deterministic extractor ([`bootstrap::extract_offer`]) with every
//!    quote verbatim visible text of at most 100 characters.
//!
//! What it writes: `corporate_reextract_state` (which request was handled,
//! retry clock) and `corporate_reextract_proposals`, plus one operational
//! event per source that keeps failing. Shadow and live write the same
//! rows; publishing a proposal is a separate, reviewed step that this job
//! does not have. The flag itself belongs to the evidence job and is never
//! written here: a handled request is recorded as `handled_request_at`, so
//! the same request is consumed once, and the evidence job clears the flag
//! once the offer confirms again.

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use reqwest::Url;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::{FromRow, PgPool, Postgres, Transaction};
use std::collections::BTreeMap;
use tracing::{info, warn};
use uuid::Uuid;

use super::bootstrap::{self, CandidateRow, Decision, Settings, MAX_QUOTE_CHARS};
use super::census::LiveOptOuts;
use super::http::PageHttp;
use super::policy::{self, robots_target, DomainSession, Lane, LaneInput, PageState, TosSignal};
use super::publish::{EndingScan, Published, ScanVerdict};
use super::values::value_signature;
use super::{Flags, VerifyMode};
use crate::operations;

/// Sources handled per run. One attempt costs at most robots, the page, one
/// redirect hop and the terms page (≤ 2 more) on top of the domain's floor.
pub const MAX_SOURCES_PER_RUN: i64 = 10;
/// Consecutive failures after which an operational event is raised.
pub const FAILURE_EVENT_THRESHOLD: i32 = 3;
/// A handled request is read again after this long while it stays flagged,
/// so a proposal follows the page.
pub const REFRESH_AFTER: Duration = Duration::days(7);
/// Retry delay after a policy refusal (robots, ToS, opt-out, bot wall).
pub const POLICY_RETRY: Duration = Duration::days(7);
/// First retry delay after a transient failure; doubles up to [`POLICY_RETRY`].
pub const TRANSIENT_RETRY: Duration = Duration::hours(6);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutcomeClass {
    /// A proposal was written.
    Proposal,
    /// The page was read and holds no qualifying offer (ended, none, several):
    /// a completed extraction, nothing to propose.
    NoOffer,
    /// Policy refused the request or the page (robots, ToS, opt-out, lane,
    /// bot wall, out of scope).
    Policy,
    /// Network, server or budget trouble: try again later.
    Transient,
}

pub fn classify(outcome: &str) -> OutcomeClass {
    match outcome {
        "proposal" => OutcomeClass::Proposal,
        "no_student_youth_offer"
        | "offer_ended"
        | "multi_offer_page"
        | "conflicting_ages"
        | "product_listing"
        | "quote_not_verbatim"
        | "offer_ended_scan"
        | "extractors_disagree" => OutcomeClass::NoOffer,
        "page_unreachable"
        | "rate_limited"
        | "request_budget"
        | "robots_unavailable"
        | "domain_stopped_rate_limited"
        | "unexpected_reply"
        | "redirect_loop"
        | "page_gone"
        | "unsupported_content"
        | "not_attempted" => OutcomeClass::Transient,
        _ => OutcomeClass::Policy,
    }
}

/// When to try a failing source again. `failures` counts this failure.
pub fn retry_delay(class: OutcomeClass, failures: i32) -> Duration {
    match class {
        OutcomeClass::Proposal | OutcomeClass::NoOffer => REFRESH_AFTER,
        OutcomeClass::Policy => POLICY_RETRY,
        OutcomeClass::Transient => {
            let doublings = (failures.max(1) - 1).clamp(0, 10) as u32;
            (TRANSIENT_RETRY * 2i32.pow(doublings)).min(POLICY_RETRY)
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct ReextractSummary {
    pub sources: usize,
    pub outcomes: BTreeMap<String, usize>,
    pub proposals: usize,
    pub requests: u32,
    pub events_raised: usize,
    /// Programmes restored by the publish step (live only).
    pub published: usize,
    /// Proposals the whole-page scan stopped, by the `hidden_*` name health
    /// reports for the same `resilience` reason.
    pub hidden_by_reason: BTreeMap<String, usize>,
}

#[derive(Debug, Clone, FromRow)]
pub(crate) struct Target {
    pub(crate) candidate_id: Uuid,
    pub(crate) requested_at: DateTime<Utc>,
    pub(crate) reason: Option<String>,
    pub(crate) url: String,
    pub(crate) domain_id: Uuid,
    pub(crate) domain: String,
    pub(crate) organization_name: Option<String>,
    pub(crate) compliance_status: Option<String>,
    pub(crate) policy_notes: Option<String>,
    pub(crate) allow_subdomains: Option<bool>,
    pub(crate) verification_status: Option<String>,
    pub(crate) domain_active: Option<bool>,
    pub(crate) gray_generic_tos: bool,
    pub(crate) gray_robots_missing: bool,
    pub(crate) gray_crawl_delay_min: bool,
    pub(crate) min_request_interval_ms: i32,
    pub(crate) max_pages_per_day: i32,
    pub(crate) domain_eligible: bool,
    pub(crate) consecutive_failures: i32,
    pub(crate) brand_name: Option<String>,
    pub(crate) campus_programme: bool,
    pub(crate) university_domain: bool,
    pub(crate) program_ids: Vec<Uuid>,
    /// The candidate's read scope ([`super::doors::job::scopes_for`]).
    #[sqlx(skip)]
    pub(crate) scope: bootstrap::Scope,
}

/// Sets each target's read scope from its door, if one owns it.
pub(crate) async fn attach_scopes(pool: &PgPool, targets: &mut [Target]) -> Result<()> {
    let ids: Vec<Uuid> = targets.iter().map(|t| t.candidate_id).collect();
    let scopes = super::doors::job::scopes_for(pool, &ids).await?;
    for target in targets.iter_mut() {
        if let Some(scope) = scopes.get(&target.candidate_id) {
            target.scope = scope.clone();
        }
    }
    Ok(())
}

impl Target {
    pub(crate) fn candidate_row(&self) -> CandidateRow {
        CandidateRow {
            id: self.candidate_id.to_string(),
            url: self.url.clone(),
            domain_id: Some(self.domain_id.to_string()),
            domain: self.domain.clone(),
            organization_name: self.organization_name.clone(),
            brand_id: None,
            brand_name: self.brand_name.clone(),
            compliance_status: self.compliance_status.clone(),
            policy_notes: self.policy_notes.clone(),
            allow_subdomains: self.allow_subdomains,
            verification_status: self.verification_status.clone(),
            domain_active: self.domain_active,
            page_title: None,
            campus_programme: Some(self.campus_programme),
            university_domain: Some(self.university_domain),
            scope: self.scope.clone(),
        }
    }
}

/// What one attempt concluded, before it is written.
#[derive(Debug, Clone)]
pub(crate) struct Attempt {
    pub(crate) outcome: String,
    pub(crate) detail: String,
    pub(crate) decision: Option<Decision>,
    pub(crate) gates: Value,
}

/// Production entry point: the shared domain slot, real precheck.
pub async fn run(pool: &PgPool, flags: &Flags, http: &dyn PageHttp) -> Result<ReextractSummary> {
    run_with(
        pool,
        flags.verify_mode,
        flags.verify_mode == VerifyMode::Live && flags.publish_enabled,
        flags.gray_generic_tos,
        http,
        None,
        MAX_SOURCES_PER_RUN,
        None,
        bootstrap::precheck,
        super::publish::PAGE_ENDING_SCAN,
    )
    .await
}

/// `only` limits the pass to these sources; `test_spacing` replaces the
/// 10 s floor and the shared slot; `check` is the request-free policy
/// ([`bootstrap::precheck`] outside tests); `scan` is the whole-page ending
/// and date scan ([`super::publish::PAGE_ENDING_SCAN`] outside tests).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_with(
    pool: &PgPool,
    mode: VerifyMode,
    publish: bool,
    gray_generic_tos: bool,
    http: &dyn PageHttp,
    only: Option<Vec<Uuid>>,
    cap: i64,
    test_spacing: Option<std::time::Duration>,
    check: fn(&CandidateRow) -> Result<Url, &'static str>,
    scan: Option<EndingScan>,
) -> Result<ReextractSummary> {
    anyhow::ensure!(
        http.user_agent().starts_with("MazimiBenefitBot/"),
        "fresh extraction only sends the MazimiBenefitBot user agent"
    );
    let settled = settle_withdrawn(pool).await?;
    if settled > 0 {
        info!(
            event = "corporate_reextract.withdrawn",
            settled, "failure events of withdrawn fresh-extraction requests resolved"
        );
    }
    let mut targets = select_targets(pool, only, cap).await?;
    // One polite session per domain; the cap was applied oldest request first.
    targets.sort_by_key(|t| t.domain_id);
    let today = super::health::kst_today();
    let mut summary = ReextractSummary {
        sources: targets.len(),
        ..ReextractSummary::default()
    };
    let optouts = LiveOptOuts(pool);
    let mut index = 0;
    while index < targets.len() {
        let domain_id = targets[index].domain_id;
        let group_end = targets[index..]
            .iter()
            .position(|t| t.domain_id != domain_id)
            .map_or(targets.len(), |offset| index + offset);
        let group = &targets[index..group_end];
        index = group_end;
        let first = &group[0];
        let shared_slot = test_spacing.is_none();
        let mut session = DomainSession::new(
            http,
            shared_slot.then_some(pool),
            shared_slot.then_some(domain_id),
            first.gray_crawl_delay_min,
            first.min_request_interval_ms as i64,
            first.max_pages_per_day.clamp(1, 200) as u32,
        );
        if let Some(spacing) = test_spacing {
            session = session.with_test_spacing(spacing);
        }
        let settings = Settings {
            gray_generic_tos: gray_generic_tos && first.gray_generic_tos,
            gray_robots_missing: first.gray_robots_missing,
            concurrency: 1,
            test_spacing,
        };
        let attempts =
            attempt_domain(&mut session, group, &settings, today, check, &optouts, scan).await;
        summary.requests += session.requests();
        for (target, attempt) in group.iter().zip(attempts) {
            let class = classify(&attempt.outcome);
            // One source that cannot be saved never stops the run.
            let (label, raised) = match record(pool, mode, publish, target, &attempt, class).await {
                Ok((raised, published)) => {
                    summary.proposals += usize::from(class == OutcomeClass::Proposal);
                    summary.published += usize::from(published);
                    (attempt.outcome.clone(), raised)
                }
                Err(error) => {
                    warn!(event = "corporate_reextract.record_failed", candidate_id = %target.candidate_id,
                          outcome = %attempt.outcome, error = %crate::error_safety::persisted_error(&error),
                          "fresh extraction attempt could not be saved; backing off");
                    let raised = match record_save_failure(pool, target).await {
                        Ok(raised) => raised,
                        Err(error) => {
                            warn!(event = "corporate_reextract.record_failed", candidate_id = %target.candidate_id,
                                  error = %crate::error_safety::persisted_error(&error),
                                  "fresh extraction failure could not be saved either");
                            false
                        }
                    };
                    ("record_failed".to_string(), raised)
                }
            };
            if let Some(metric) = attempt.gates["hidden_by"].as_str() {
                *summary
                    .hidden_by_reason
                    .entry(metric.to_string())
                    .or_default() += 1;
            }
            *summary.outcomes.entry(label).or_default() += 1;
            summary.events_raised += usize::from(raised);
        }
    }
    Ok(summary)
}

async fn select_targets(pool: &PgPool, only: Option<Vec<Uuid>>, cap: i64) -> Result<Vec<Target>> {
    let targets = sqlx::query_as::<_, Target>(&format!(
        "SELECT t.candidate_id, t.reextract_requested_at AS requested_at, t.reextract_reason AS reason,
                c.url, d.id AS domain_id, d.domain, d.organization_name, d.compliance_status,
                d.policy_notes, d.allow_subdomains, d.verification_status, d.is_active AS domain_active,
                d.gray_generic_tos, d.gray_robots_missing, d.gray_crawl_delay_min,
                d.min_request_interval_ms, d.max_pages_per_day,
                {eligible} AS domain_eligible,
                COALESCE(s.consecutive_failures, 0) AS consecutive_failures,
                COALESCE(
                    (SELECT b.name FROM benefit_candidate_brands cb JOIN brands b ON b.id = cb.brand_id
                     WHERE cb.candidate_id = c.id ORDER BY b.name LIMIT 1),
                    (SELECT p.provider_name FROM program_source_monitors m JOIN programs p ON p.id = m.program_id
                     WHERE m.candidate_id = c.id AND m.verification_mode = 'evidence_auto'
                       AND m.verification_state <> 'retired' AND p.program_type = 'corporate_benefit'
                     ORDER BY p.provider_name LIMIT 1)
                ) AS brand_name,
                EXISTS (SELECT 1 FROM program_source_monitors m JOIN programs p ON p.id = m.program_id
                        WHERE m.candidate_id = c.id AND m.verification_state <> 'retired'
                          AND p.program_type = 'campus_benefit') AS campus_programme,
                EXISTS (SELECT 1 FROM universities u,
                               LATERAL (SELECT regexp_replace(lower(regexp_replace(u.official_url,
                                            '^[a-z]+://([^/:?#]+).*$', '\\1', 'i')), '^www\\.', '') AS host) uh,
                               LATERAL (SELECT regexp_replace(lower(d.domain), '^www\\.', '') AS host) dh
                        WHERE uh.host <> '' AND (dh.host = uh.host OR dh.host LIKE '%.' || uh.host)) AS university_domain,
                ARRAY(SELECT m.program_id FROM program_source_monitors m
                      WHERE m.candidate_id = c.id AND m.verification_mode = 'evidence_auto'
                        AND m.verification_state <> 'retired'
                      ORDER BY m.program_id) AS program_ids
         FROM corporate_evidence_targets t
         JOIN benefit_source_candidates c ON c.id = t.candidate_id
         JOIN benefit_source_domains d ON d.id = c.domain_id
         LEFT JOIN corporate_reextract_state s ON s.candidate_id = t.candidate_id
         WHERE t.reextract_requested_at IS NOT NULL
           AND ($2::uuid[] IS NULL OR t.candidate_id = ANY($2))
           AND (s.candidate_id IS NULL
                OR s.next_attempt_at <= now()
                OR (s.handled_request_at IS DISTINCT FROM t.reextract_requested_at
                    AND s.consecutive_failures = 0))
         ORDER BY t.reextract_requested_at, t.candidate_id
         LIMIT $1",
        eligible = policy::ELIGIBLE_DOMAIN_SQL
    ))
    .bind(cap.clamp(0, 100))
    .bind(only)
    .fetch_all(pool)
    .await
    .context("failed to select fresh-extraction targets")?;
    let mut targets = targets;
    attach_scopes(pool, &mut targets).await?;
    Ok(targets)
}

/// Gates 1–7 for one domain's sources. Requests only for sources that pass
/// the request-free gates; one extraction run per domain so its terms page
/// is read once.
pub(crate) async fn attempt_domain(
    session: &mut DomainSession<'_>,
    group: &[Target],
    settings: &Settings,
    today: NaiveDate,
    check: fn(&CandidateRow) -> Result<Url, &'static str>,
    optouts: &LiveOptOuts<'_>,
    scan: Option<EndingScan>,
) -> Vec<Attempt> {
    let refused = |outcome: &str, detail: &str| Attempt {
        outcome: outcome.to_string(),
        detail: detail.to_string(),
        decision: None,
        gates: json!({}),
    };
    let mut attempts: Vec<Option<Attempt>> = Vec::with_capacity(group.len());
    let mut rows = Vec::new();
    for target in group {
        attempts.push(if target.program_ids.is_empty() {
            Some(refused(
                "not_evidence_auto",
                "no non-retired evidence_auto monitor; a person owns this source",
            ))
        } else if !target.domain_eligible {
            Some(refused(
                "policy_blocked",
                "the domain is rejected, blocked, restricted or serves no corporate benefit",
            ))
        } else {
            rows.push(target.candidate_row());
            None
        });
    }
    let (report, decisions) = if rows.is_empty() {
        Default::default()
    } else {
        let (report, decisions) =
            bootstrap::run_domain_with(session, &rows, settings, today, check, Some(optouts)).await;
        (Some(report), decisions)
    };
    let tos = report.as_ref().map_or(TosSignal::Unknown, |r| r.tos_signal);
    let mut out = Vec::with_capacity(group.len());
    for (target, attempt) in group.iter().zip(attempts) {
        if let Some(attempt) = attempt {
            out.push(attempt);
            continue;
        }
        let id = target.candidate_id.to_string();
        let Some(decision) = decisions
            .iter()
            .find(|d| d.candidate_ids.contains(&id))
            .cloned()
        else {
            out.push(refused("not_attempted", "no decision for this source"));
            continue;
        };
        out.push(gate_decision(session, decision, tos, settings, report.as_ref(), scan).await);
    }
    out
}

/// Gates 6 and 7 on a decision the extractor proposed.
pub(crate) async fn gate_decision(
    session: &mut DomainSession<'_>,
    decision: Decision,
    tos: TosSignal,
    settings: &Settings,
    report: Option<&bootstrap::DomainReport>,
    scan: Option<EndingScan>,
) -> Attempt {
    let robots_label = report.map_or("not_checked", |r| r.robots.as_str());
    let mut gates = json!({
        "robots": robots_label,
        "tos": tos.as_str(),
        "gray_generic_tos": settings.gray_generic_tos,
        "gray_robots_missing": settings.gray_robots_missing,
        "verification_mode": "evidence_auto",
    });
    if decision.outcome != "proposal" {
        return Attempt {
            detail: format!("extraction outcome {}", decision.outcome),
            outcome: decision.outcome.clone(),
            decision: Some(decision),
            gates,
        };
    }
    let page_url = decision
        .final_url
        .as_deref()
        .unwrap_or(&decision.url)
        .to_string();
    let Ok(page_url) = Url::parse(&page_url) else {
        return Attempt {
            outcome: "invalid_url".into(),
            detail: "unparseable page URL".into(),
            decision: None,
            gates,
        };
    };
    // Cached for this origin: no request.
    let robots = session.robots(&page_url).await;
    let page = PageState::Html {
        text_chars: decision.visible_chars.unwrap_or(0),
    };
    let (lane, lane_reason) = policy::choose_lane(&LaneInput {
        opted_out: false,
        feed_active: false,
        robots: &robots,
        path_allowed: robots.allows(&robots_target(&page_url), settings.gray_robots_missing),
        page: &page,
        tos,
        gray_generic_tos: settings.gray_generic_tos,
        gray_robots_missing: settings.gray_robots_missing,
    });
    gates["lane"] = json!(lane.as_str());
    gates["lane_reason"] = json!(lane_reason);
    if lane != Lane::Direct || lane_reason != "direct_html" {
        return Attempt {
            outcome: format!("lane_{lane_reason}"),
            detail: format!("lane {}:{lane_reason}", lane.as_str()),
            decision: None,
            gates,
        };
    }
    gate_offer(decision, gates, scan)
}

/// Gate 7 and the checks after it, on a `proposal` decision whose lane is
/// `direct:direct_html`: verbatim quotes of at most 100 characters, both
/// value extractors agreeing on every quote, and the whole-page ending scan.
/// Request-free.
pub(crate) fn gate_offer(
    decision: Decision,
    mut gates: Value,
    scan: Option<EndingScan>,
) -> Attempt {
    let Some(offer) = decision.offer.as_ref() else {
        return Attempt {
            outcome: "no_student_youth_offer".into(),
            detail: "proposal without an offer".into(),
            decision: None,
            gates,
        };
    };
    // Evidence quote requirement: every fact carries a verbatim quote of at
    // most 100 characters (the extractor already checked presence on the
    // page; length and non-emptiness are checked again here).
    let quotes_ok = !offer.facts.is_empty()
        && offer
            .facts
            .iter()
            .chain(offer.verification.iter())
            .all(|f| !f.quote.trim().is_empty() && f.quote.chars().count() <= MAX_QUOTE_CHARS);
    gates["quotes_verbatim"] = json!(quotes_ok);
    gates["max_quote_chars"] = json!(MAX_QUOTE_CHARS);
    if !quotes_ok {
        return Attempt {
            outcome: "quote_not_verbatim".into(),
            detail: "a quote is empty or longer than 100 characters".into(),
            decision: None,
            gates,
        };
    }
    let quotes: Vec<&str> = offer
        .facts
        .iter()
        .chain(offer.verification.iter())
        .map(|f| f.quote.as_str())
        .collect();
    // Contract §16: both value extractors read the same values on every
    // quote, or nothing is proposed.
    if let Some(quote) = quotes
        .iter()
        .find(|q| !super::resilience::extractors_agree(q))
    {
        gates["extractors_agree"] = json!(false);
        return Attempt {
            outcome: "extractors_disagree".into(),
            detail: format!("the value extractors read {quote:?} differently"),
            decision: None,
            gates,
        };
    }
    gates["extractors_agree"] = json!(true);
    // The whole-page ending and date scan (`resilience::page_blocked`) runs
    // before anything is proposed. Without a scan the proposal is kept but
    // can never publish.
    match (scan, decision.page_html.as_deref()) {
        (Some(scan), Some(html)) => match scan(html, &quotes) {
            ScanVerdict::Clean => gates["ending_scan"] = json!("clean"),
            ScanVerdict::Ended(reason) => {
                // Recorded as the evidence job records it: the reason
                // verbatim, and its `hidden_by_reason` name.
                gates["ending_scan"] = json!("ended");
                gates["ending_reason"] = json!(reason);
                gates["hidden_by"] = json!(super::publish::hidden_metric(&reason));
                return Attempt {
                    outcome: "offer_ended_scan".into(),
                    detail: reason,
                    decision: None,
                    gates,
                };
            }
        },
        _ => gates["ending_scan"] = json!("unavailable"),
    }
    Attempt {
        outcome: "proposal".into(),
        detail: format!("{} facts", offer.facts.len()),
        decision: Some(decision),
        gates,
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Content key of a proposal: the offer as read and the page it came from.
/// The page hash is left out, so unrelated page churn does not make a new
/// proposal.
fn proposal_key(candidate_id: Uuid, page_url: &str, offer: &Value) -> String {
    let mut hasher = Sha256::new();
    hasher.update(candidate_id.as_bytes());
    hasher.update(page_url.as_bytes());
    hasher.update(offer.to_string().as_bytes());
    hex(&hasher.finalize())
}

fn event_fingerprint(candidate_id: Uuid) -> String {
    format!("corporate_reextract:{candidate_id}")
}

/// Writes one attempt in a single transaction. Returns whether the failure
/// event was raised and whether a programme was published (live only).
async fn record(
    pool: &PgPool,
    mode: VerifyMode,
    publish: bool,
    target: &Target,
    attempt: &Attempt,
    class: OutcomeClass,
) -> Result<(bool, bool)> {
    let mut tx = pool.begin().await?;
    let detail: String = attempt.detail.chars().take(300).collect();
    let outcome: String = attempt.outcome.chars().take(60).collect();
    let mut raised = false;
    let mut published = false;
    match class {
        OutcomeClass::Proposal | OutcomeClass::NoOffer => {
            if class == OutcomeClass::Proposal {
                let key = write_proposal(&mut tx, mode, target, attempt).await?;
                // Publishing new values is its own switch; without it a live
                // pass records the proposal as measurement only.
                if mode == VerifyMode::Live && publish {
                    published = publish_proposal(&mut tx, target, attempt, &key).await?;
                }
            } else {
                // The page no longer states an offer: earlier proposals are stale.
                supersede(&mut tx, target.candidate_id, None).await?;
            }
            sqlx::query(
                "INSERT INTO corporate_reextract_state (candidate_id, handled_request_at, attempts,
                     consecutive_failures, next_attempt_at, last_attempt_at, last_success_at,
                     last_outcome, last_detail, updated_at)
                 VALUES ($1, $2, 1, 0, now() + make_interval(secs => $3), now(), now(), $4, $5, now())
                 ON CONFLICT (candidate_id) DO UPDATE SET
                     handled_request_at = EXCLUDED.handled_request_at,
                     attempts = corporate_reextract_state.attempts + 1,
                     consecutive_failures = 0,
                     next_attempt_at = EXCLUDED.next_attempt_at,
                     last_attempt_at = now(), last_success_at = now(),
                     last_outcome = EXCLUDED.last_outcome, last_detail = EXCLUDED.last_detail,
                     updated_at = now()",
            )
            .bind(target.candidate_id)
            .bind(target.requested_at)
            .bind(REFRESH_AFTER.num_seconds() as f64)
            .bind(&outcome)
            .bind(&detail)
            .execute(&mut *tx)
            .await?;
            operations::resolve_event_tx(
                &mut tx,
                &event_fingerprint(target.candidate_id),
                None,
                Some("Fresh extraction completed"),
            )
            .await?;
        }
        OutcomeClass::Policy | OutcomeClass::Transient => {
            raised = record_failure_tx(&mut tx, target, &outcome, &detail, class).await?;
        }
    }
    tx.commit().await?;
    Ok((raised, published))
}

/// Live only: the publish gates and restore ([`super::publish`]); the
/// decision is stored on the proposal (`gates.publish`, status `accepted`
/// when a programme was restored).
async fn publish_proposal(
    tx: &mut Transaction<'_, Postgres>,
    target: &Target,
    attempt: &Attempt,
    key: &str,
) -> Result<bool> {
    let decision = attempt
        .decision
        .as_ref()
        .context("a proposal carries its decision")?;
    let offer = decision
        .offer
        .as_ref()
        .context("a proposal carries its offer")?;
    let result = if attempt.gates["ending_scan"] != "clean" {
        Published::Refused("ending_scan_unavailable".into())
    } else {
        super::publish::publish(
            tx,
            target.candidate_id,
            offer,
            decision.page_hash.as_deref(),
            decision.page_text.as_deref(),
            decision.page_html.as_deref(),
            decision.final_url.as_deref().unwrap_or(&decision.url),
        )
        .await?
    };
    let restored = matches!(result, Published::Restored { .. });
    sqlx::query(
        "UPDATE corporate_reextract_proposals
         SET gates = jsonb_set(gates, '{publish}', $3),
             status = CASE WHEN $4 THEN 'accepted' ELSE status END,
             decided_at = CASE WHEN $4 THEN now() ELSE decided_at END
         WHERE candidate_id = $1 AND proposal_key = $2 AND status = 'proposed'",
    )
    .bind(target.candidate_id)
    .bind(key)
    .bind(result.to_json())
    .bind(restored)
    .execute(&mut **tx)
    .await?;
    Ok(restored)
}

/// The failure half of [`record`]: counter, backoff, and the event once the
/// source has failed [`FAILURE_EVENT_THRESHOLD`] times in a row. Returns
/// whether the event was raised.
async fn record_failure_tx(
    tx: &mut Transaction<'_, Postgres>,
    target: &Target,
    outcome: &str,
    detail: &str,
    class: OutcomeClass,
) -> Result<bool> {
    let failures = (target.consecutive_failures + 1).min(100);
    let delay = retry_delay(class, failures);
    sqlx::query(
        "INSERT INTO corporate_reextract_state (candidate_id, attempts, consecutive_failures,
             next_attempt_at, last_attempt_at, last_outcome, last_detail, updated_at)
         VALUES ($1, 1, $2, now() + make_interval(secs => $3), now(), $4, $5, now())
         ON CONFLICT (candidate_id) DO UPDATE SET
             attempts = corporate_reextract_state.attempts + 1,
             consecutive_failures = EXCLUDED.consecutive_failures,
             next_attempt_at = EXCLUDED.next_attempt_at,
             last_attempt_at = now(),
             last_outcome = EXCLUDED.last_outcome, last_detail = EXCLUDED.last_detail,
             updated_at = now()",
    )
    .bind(target.candidate_id)
    .bind(failures)
    .bind(delay.num_seconds() as f64)
    .bind(outcome)
    .bind(detail)
    .execute(&mut **tx)
    .await?;
    if failures >= FAILURE_EVENT_THRESHOLD {
        let subject = target.candidate_id.to_string();
        operations::record_event_tx(
            tx,
            &operations::EventCommand {
                fingerprint: &event_fingerprint(target.candidate_id),
                severity: operations::Severity::Warning,
                component: "corporate_auto",
                event_type: "corporate_reextract_failing",
                subject_type: Some("benefit_source_candidate"),
                subject_id: Some(&subject),
                message: "Fresh extraction keeps failing for a flagged corporate source; its offer stays hidden",
                details: json!({
                    "consecutive_failures": failures,
                    "last_outcome": outcome,
                    "class": if class == OutcomeClass::Policy { "policy" } else { "transient" },
                    "next_attempt_in_hours": delay.num_hours(),
                }),
            },
        )
        .await?;
        return Ok(true);
    }
    Ok(false)
}

/// When an attempt could not be saved (a value the tables refuse, a lost
/// connection), the source still gets a failure and its backoff, written
/// with fixed values only, so it is neither refetched every run nor able
/// to stop the sources after it.
async fn record_save_failure(pool: &PgPool, target: &Target) -> Result<bool> {
    let mut tx = pool.begin().await?;
    let raised = record_failure_tx(
        &mut tx,
        target,
        "record_failed",
        "the attempt could not be saved",
        OutcomeClass::Transient,
    )
    .await?;
    tx.commit().await?;
    Ok(raised)
}

/// Requests the evidence job withdrew (flag cleared, or its target row gone)
/// leave no open failure event and no failure count behind.
async fn settle_withdrawn(pool: &PgPool) -> Result<u64> {
    let fingerprints: Vec<String> = sqlx::query_scalar(
        "SELECT e.fingerprint FROM operational_events e
         WHERE starts_with(e.fingerprint, 'corporate_reextract:') AND e.status <> 'resolved'
           AND NOT EXISTS (SELECT 1 FROM corporate_evidence_targets t
                           WHERE e.fingerprint = 'corporate_reextract:' || t.candidate_id::text
                             AND t.reextract_requested_at IS NOT NULL)",
    )
    .fetch_all(pool)
    .await?;
    let mut settled = 0;
    for fingerprint in &fingerprints {
        if operations::resolve_event(
            pool,
            fingerprint,
            None,
            Some("Fresh extraction is no longer requested"),
        )
        .await?
        {
            settled += 1;
        }
    }
    sqlx::query(
        "UPDATE corporate_reextract_state s
         SET consecutive_failures = 0, next_attempt_at = now(), updated_at = now()
         WHERE s.consecutive_failures > 0
           AND NOT EXISTS (SELECT 1 FROM corporate_evidence_targets t
                           WHERE t.candidate_id = s.candidate_id AND t.reextract_requested_at IS NOT NULL)",
    )
    .execute(pool)
    .await?;
    Ok(settled)
}

async fn supersede(
    tx: &mut Transaction<'_, Postgres>,
    candidate_id: Uuid,
    keep: Option<&str>,
) -> Result<()> {
    sqlx::query(
        "UPDATE corporate_reextract_proposals SET status = 'superseded', decided_at = now()
         WHERE candidate_id = $1 AND status = 'proposed' AND ($2::text IS NULL OR proposal_key <> $2)",
    )
    .bind(candidate_id)
    .bind(keep)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn write_proposal(
    tx: &mut Transaction<'_, Postgres>,
    mode: VerifyMode,
    target: &Target,
    attempt: &Attempt,
) -> Result<String> {
    let decision = attempt
        .decision
        .as_ref()
        .context("a proposal carries its decision")?;
    let offer = decision
        .offer
        .as_ref()
        .context("a proposal carries its offer")?;
    let page_url = decision.final_url.as_deref().unwrap_or(&decision.url);
    let offer_value = serde_json::to_value(offer)?;
    let key = proposal_key(target.candidate_id, page_url, &offer_value);
    // For the reviewer: the value signatures of the facts now stored for
    // these programmes next to the new ones (ids and signatures only).
    let previous: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT id, quote FROM corporate_offer_facts
         WHERE program_id = ANY($1) AND status <> 'retired' ORDER BY id",
    )
    .bind(&target.program_ids)
    .fetch_all(&mut **tx)
    .await?;
    let stored = json!({
        "offer": offer_value,
        "brand": target.brand_name,
        "page_title": decision.page_title,
        "new_values": offer
            .facts
            .iter()
            .map(|f| value_signature(&f.quote))
            .collect::<Vec<_>>(),
        "previous_values": previous
            .iter()
            .map(|(id, quote)| json!({"fact_id": id, "values": value_signature(quote)}))
            .collect::<Vec<_>>(),
    });
    supersede(tx, target.candidate_id, Some(&key)).await?;
    sqlx::query(
        "INSERT INTO corporate_reextract_proposals (candidate_id, program_ids, request_at,
             request_reason, mode, proposal_key, url, final_url, page_hash, offer, gates)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
         ON CONFLICT (candidate_id, proposal_key) WHERE status = 'proposed' DO UPDATE SET
             sightings = corporate_reextract_proposals.sightings + 1,
             last_seen_at = now(),
             program_ids = EXCLUDED.program_ids,
             request_at = EXCLUDED.request_at,
             request_reason = EXCLUDED.request_reason,
             mode = EXCLUDED.mode,
             page_hash = EXCLUDED.page_hash,
             gates = EXCLUDED.gates",
    )
    .bind(target.candidate_id)
    .bind(&target.program_ids)
    .bind(target.requested_at)
    .bind(
        target
            .reason
            .as_deref()
            .map(|r| r.chars().take(60).collect::<String>()),
    )
    .bind(mode.as_str())
    .bind(&key)
    .bind(&decision.url)
    .bind(decision.final_url.as_deref())
    .bind(decision.page_hash.as_deref())
    .bind(&stored)
    .bind(&attempt.gates)
    .execute(&mut **tx)
    .await?;
    Ok(key)
}

#[cfg(test)]
#[path = "reextract_tests.rs"]
mod tests;
