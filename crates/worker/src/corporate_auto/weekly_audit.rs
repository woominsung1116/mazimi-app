//! Weekly full re-extraction audit of corporate offers. Measurement only.
//!
//! The evidence job confirms an offer while its stored quote is still on the
//! page; that shows the page did not change, not that the stored values were
//! ever right. This job reads every `corporate_official` programme with an
//! official page once a week with the fresh extractor (the request path and
//! gates of [`super::reextract`]) and compares what the page states with the
//! stored title claim, audience, age, benefit kind and value, end dates and
//! URL. Each programme gets one verdict per week in
//! `corporate_weekly_audit_verdicts`:
//!
//! - `agrees`: every compared field is stated by the page;
//! - `disagrees`: the fields the page contradicts, each with the reason;
//! - `unreadable`: why no offer could be read (policy refusal, transient
//!   failure, no single offer on the page, no source monitor).
//!
//! What each field compares, and in which direction (2026-10-26: one
//! source of truth for "does the page state this value", [`super::values`]):
//!
//! - `title`, `benefit` (summary and amounts): one way. Every value and
//!   claim the stored text shows is a value or claim the page states, in
//!   either language ([`super::values::concepts`]: `1년` is `one-year`,
//!   `무료` is `free`, `₩6,600` is `6,600원`). A page stating more than the
//!   stored text is not a disagreement. The extractor read exactly one offer
//!   on the page, so the offer's section is the page. Deliberately not
//!   checked: which audience or condition the page states a value for (a
//!   page stating `2년` for teachers supports a stored `2년`); the evidence
//!   job's quotes bind values to the offer.
//! - `audience`: one way. Every audience the stored title and summary name
//!   must be among the page offer's audiences.
//! - `age`: both ways. Each stored bound is an age the page states, and a
//!   band the extractor reads must equal the stored one; a band read with
//!   none stored disagrees too.
//! - `dates`: both ways for end dates. Every stored end date must equal the
//!   page's end date (KST), and an end date on the page with none stored
//!   disagrees. A start date cannot be read from a page and is listed as
//!   unchecked.
//! - `url`: equality. The official URL must be the page read (same host,
//!   path and query; `www.`, a trailing slash and a fragment ignored).
//!
//! A page whose offer the extractor cannot read by design (a membership,
//! not a student or youth offer) is `unreadable` with reason class
//! `unchecked`, counted apart from pages that could not be read.
//!
//! Alerts (see [`summarize`]): the latest verdict per programme within one
//! [`CYCLE_DAYS`] cycle counts. A programme that agreed on its previous read
//! and now disagrees raises a transition Warning; any shown offer with a
//! current disagreement keeps a standing Warning open until none has. Both
//! arm once a full cycle has covered every established programme, and stay
//! armed after that ([`health_block`]: `armed_ever`); a later coverage dip
//! raises its own Warning (below [`COVERAGE_FLOOR`]) instead of silencing them.
//!
//! `program_active` on a verdict is the programme's visibility at
//! observation time: an `evidence_page` verdict is written by a task that
//! runs beside the same evidence pass's decision, so it may read the state
//! before or after that decision. The aggregate and the alerts never read
//! it; they read the programme's current `is_active`.
//!
//! The job never hides, publishes or changes a programme, monitor, hold,
//! fact or proposal, in shadow, hide_only or live: it writes verdict rows
//! and nothing else. Letting a disagreement hide an offer is a separate,
//! reviewed change.
//!
//! Requests: the same polite path as everything else (publisher opt-out
//! before every request, robots for our agent, the domain's ToS, the
//! `direct:direct_html` lane, the shared per-domain slot with the 10 s floor
//! or Crawl-delay, a stop on 401/403/429/challenge, the honest user agent).
//! A run handles at most [`MAX_SOURCES_PER_RUN`] sources and
//! [`MAX_REQUESTS_PER_RUN`] requests and runs daily, so a week's audit is
//! spread over the week. A page the evidence job fetched in the last 24 h is
//! not fetched again: the evidence job keeps no page body, so the audit
//! reads that page in memory when the evidence job fetches it, and records
//! the evidence outcome as `unreadable` when the evidence job got no page
//! ([`observe_evidence`]); the standalone run leaves those sources alone.
//! The hook runs in its own task: a panic or error there is logged, counted
//! and recorded as a transient `unreadable` verdict, and never reaches the
//! evidence pass.

use anyhow::{Context, Result};
use chrono::{DateTime, FixedOffset, NaiveDate, Utc};
use reqwest::Url;
use serde_json::{json, Value};
use sqlx::{FromRow, PgPool};
use std::collections::BTreeMap;
use tracing::{info, warn};
use uuid::Uuid;

use super::bootstrap::{self, audience_codes, CandidateRow, Decision, Offer, Settings};
use super::census::LiveOptOuts;
use super::policy::{self, DomainSession};
use super::publish::EndingScan;
use super::reextract::{self, Attempt, OutcomeClass, Target};
use super::values;
use super::{Flags, VerifyMode};
use crate::operations;

/// The switch for the audit (default off; an unknown value stops startup).
pub const ENABLED_FLAG: &str = "CORPORATE_WEEKLY_AUDIT_ENABLED";
/// A programme is due again this long after its last verdict: two hours
/// short of a week, so the daily run re-audits it on day 7, not day 8.
pub const AUDIT_INTERVAL_HOURS: i64 = 7 * 24 - 2;
/// The audit cycle the aggregate and alerts read: a day longer than the
/// interval, so the next verdict lands before the previous one leaves.
pub const CYCLE_DAYS: i64 = 8;
/// A transient failure (network, server, a failed save or audit) is retried
/// after a day instead of a week.
pub const TRANSIENT_RETRY_HOURS: i64 = 24;
/// Share of established programmes with a verdict in the cycle below which,
/// once armed, the `weekly_audit_coverage_short` Warning opens.
pub const COVERAGE_FLOOR: f64 = 0.9;
/// Largest `details` stored, in bytes of JSON text (the column allows 8 KiB).
pub const MAX_DETAILS_BYTES: usize = 4000;
/// A page the evidence job fetched this recently is not fetched again.
pub const EVIDENCE_REUSE_HOURS: i64 = 24;
/// Sources one run reads (77 offers in a week at one run a day).
pub const MAX_SOURCES_PER_RUN: usize = 12;
/// Requests one run may send across all domains.
pub const MAX_REQUESTS_PER_RUN: u32 = 60;
/// Most requests one source can cost: robots, the page, one redirect hop,
/// the terms page and its hop.
pub const REQUESTS_PER_SOURCE: u32 = 5;
/// Disagreeing offers listed in the health row and the digest.
pub const LISTED_DISAGREEMENTS: usize = 100;

/// Every field the audit compares, in report order.
pub const FIELDS: &[&str] = &["title", "audience", "age", "benefit", "dates", "url"];

/// Evidence outcomes after which no request was sent: the standalone run
/// may still handle those sources (its own gates refuse them request-free).
const EVIDENCE_NO_REQUEST_OUTCOMES: &[&str] = &["policy_blocked", "domain_withdrawn", "opted_out"];

/// Whether the audit is on. Startup already refused an unknown value
/// ([`Flags::from_env`]).
pub fn enabled() -> Result<bool> {
    super::strict_flag(ENABLED_FLAG)
}

#[derive(Debug, Clone, Copy)]
pub struct Budget {
    pub max_sources: usize,
    pub max_requests: u32,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            max_sources: MAX_SOURCES_PER_RUN,
            max_requests: MAX_REQUESTS_PER_RUN,
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct AuditSummary {
    /// Programmes due this run.
    pub due: usize,
    pub agrees: usize,
    pub disagrees: usize,
    pub unreadable: usize,
    pub requests: u32,
    /// Programmes left for a later run: the source cap or request budget.
    pub deferred_budget: usize,
    /// Programmes whose page the evidence job fetched within 24 h; they are
    /// audited from the evidence job's own read.
    pub deferred_evidence_recent: usize,
    pub by_field: BTreeMap<String, usize>,
}

/// The stored values of one programme the audit compares, plus its source.
#[derive(Debug, Clone, FromRow)]
pub struct AuditedProgramme {
    pub id: Uuid,
    pub title: String,
    pub summary: Option<String>,
    pub official_url: Option<String>,
    pub is_active: bool,
    pub application_start_at: Option<DateTime<Utc>>,
    pub application_end_at: Option<DateTime<Utc>>,
    pub deadline_at: Option<DateTime<Utc>>,
    pub benefit_valid_until: Option<DateTime<Utc>>,
    pub benefit_amount_monthly: Option<i32>,
    pub benefit_amount_semester: Option<i32>,
    pub benefit_amount_once: Option<i32>,
    pub min_age: Option<i32>,
    pub max_age: Option<i32>,
    /// The source read for it: a non-retired monitor, `evidence_auto` first.
    pub candidate_id: Option<Uuid>,
}

/// One compared field.
#[derive(Debug, Clone, PartialEq)]
pub struct FieldCheck {
    pub field: &'static str,
    pub agrees: bool,
    pub detail: String,
}

fn check(field: &'static str, result: std::result::Result<String, String>) -> FieldCheck {
    match result {
        Ok(detail) => FieldCheck {
            field,
            agrees: true,
            detail,
        },
        Err(detail) => FieldCheck {
            field,
            agrees: false,
            detail: detail.chars().take(200).collect(),
        },
    }
}

/// A stored title or summary against the page: every value and claim it
/// shows is one the page states ([`values::concepts`]). `stated` holds the
/// page's concepts.
fn supported(text: &str, stated: &std::collections::BTreeSet<String>) -> Result<String, String> {
    let missing: Vec<String> = values::concepts(text)
        .into_iter()
        .filter(|concept| !stated.contains(concept))
        .collect();
    if missing.is_empty() {
        Ok("stated".into())
    } else {
        Err(format!("not_stated:{}", missing.join(",")))
    }
}

/// A stored age band against the page: each stored bound is an age the
/// page states, and a band the extractor reads on the page is the stored
/// one; a band read with none stored disagrees too.
fn age_check(
    stored: (Option<i32>, Option<i32>),
    read: Option<(Option<i32>, Option<i32>)>,
    stated: &std::collections::BTreeSet<String>,
) -> Result<String, String> {
    if let Some(read) = read.filter(|read| *read != stored) {
        return Err(format!("stored {stored:?}, page {read:?}"));
    }
    match [stored.0, stored.1]
        .into_iter()
        .flatten()
        .find(|age| !stated.contains(&format!("{age}age")))
    {
        Some(age) => Err(format!("stored {stored:?}, page states no {age}세")),
        None => Ok(format!("{stored:?}")),
    }
}

fn kst_date(at: DateTime<Utc>) -> NaiveDate {
    at.with_timezone(&FixedOffset::east_opt(9 * 3600).expect("KST offset"))
        .date_naive()
}

fn host_of(url: &str) -> Option<String> {
    let host = Url::parse(url).ok()?.host_str()?.to_ascii_lowercase();
    Some(host.trim_start_matches("www.").to_string())
}

/// Host, port, path without a trailing slash, and query; no fragment.
fn page_key(url: &str) -> Option<String> {
    let url = Url::parse(url.trim()).ok()?;
    let host = url.host_str()?.to_ascii_lowercase();
    let host = host.trim_start_matches("www.");
    let path = url.path().trim_end_matches('/');
    let query = url.query().map(|q| format!("?{q}")).unwrap_or_default();
    Some(format!(
        "{host}:{}{path}{query}",
        url.port_or_known_default().unwrap_or(0)
    ))
}

/// Compares one programme's stored values with the offer read from its
/// page. Pure. `page_text` is the page's visible text, `page_url` the URL
/// the page was read from (after a same-host redirect).
pub fn compare(
    programme: &AuditedProgramme,
    offer: &Offer,
    page_text: &str,
    page_url: &str,
) -> Vec<FieldCheck> {
    // The extractor read one offer on this page (a page with several is
    // `multi_offer_page`, never compared), so the offer's section is the
    // page: every value is checked against what the whole page states.
    let stated = values::concepts(page_text);
    let summary = programme.summary.as_deref().unwrap_or("");
    let mut checks = vec![check("title", supported(&programme.title, &stated))];

    let listed: Vec<&str> = offer.audience.clone();
    let shown = audience_codes(&format!("{} {summary}", programme.title));
    checks.push(check(
        "audience",
        match shown.iter().find(|code| !listed.contains(code)) {
            Some(code) => Err(format!("not_stated:{code}; page:{}", listed.join(","))),
            None => Ok(format!("stated:{}", shown.join(","))),
        },
    ));

    checks.push(check(
        "age",
        age_check((programme.min_age, programme.max_age), offer.age, &stated),
    ));

    let benefit = supported(summary, &stated).and_then(|detail| {
        for (field, amount) in [
            ("benefit_amount_monthly", programme.benefit_amount_monthly),
            ("benefit_amount_semester", programme.benefit_amount_semester),
            ("benefit_amount_once", programme.benefit_amount_once),
        ] {
            if let Some(amount) = amount.filter(|a| !stated.contains(&format!("{a}won"))) {
                return Err(format!("{field}:{amount}원 not stated"));
            }
        }
        Ok(detail)
    });
    checks.push(check("benefit", benefit));

    let mut dates = Ok(match offer.end_date {
        Some(end) => format!("end {end}"),
        None => "no end date on page or stored".into(),
    });
    let mut any_end = false;
    for (field, at) in [
        ("application_end_at", programme.application_end_at),
        ("deadline_at", programme.deadline_at),
        ("benefit_valid_until", programme.benefit_valid_until),
    ] {
        if let Some(at) = at {
            any_end = true;
            let stored = kst_date(at);
            if offer.end_date != Some(stored) && dates.is_ok() {
                dates = Err(match offer.end_date {
                    Some(end) => format!("{field}: stored {stored}, page {end}"),
                    None => format!("{field}: stored {stored}, page states none"),
                });
            }
        }
    }
    if let (Some(end), false, true) = (offer.end_date, any_end, dates.is_ok()) {
        dates = Err(format!("page ends {end}, none stored"));
    }
    checks.push(check("dates", dates));

    checks.push(check(
        "url",
        match programme.official_url.as_deref() {
            None => Err("no official_url".into()),
            Some(official)
                if host_of(official).is_none() || host_of(official) != host_of(page_url) =>
            {
                Err("host_differs_from_page".into())
            }
            Some(official) if page_key(official) != page_key(page_url) => {
                Err("official_url_differs_from_page".into())
            }
            Some(_) => Ok("same_page".into()),
        },
    ));
    checks
}

// ── selection ────────────────────────────────────────────────────────────

/// Programmes in scope: `corporate_official`, not retired, with an official
/// page. Shared by the due selection and the aggregate.
const SCOPE_SQL: &str = "p.source_type = 'corporate_official' AND p.retirement_reason IS NULL
     AND p.official_url IS NOT NULL AND btrim(p.official_url) <> ''
     AND NOT EXISTS (SELECT 1 FROM program_source_monitors door_monitor
                     WHERE door_monitor.program_id = p.id AND door_monitor.verification_mode = 'door')";

/// Programmes due an audit, never-audited first, then oldest verdict.
/// `only` limits to these programmes, `candidate` to one source.
async fn due_programmes(
    pool: &PgPool,
    only: Option<&[Uuid]>,
    candidate: Option<Uuid>,
) -> Result<Vec<AuditedProgramme>> {
    sqlx::query_as::<_, AuditedProgramme>(&format!(
        "WITH primary_monitor AS (
             SELECT DISTINCT ON (m.program_id) m.program_id, m.candidate_id
             FROM program_source_monitors m
             WHERE m.verification_state <> 'retired'
             ORDER BY m.program_id, (m.verification_mode = 'evidence_auto') DESC, m.candidate_id
         ), latest AS (
             SELECT DISTINCT ON (v.program_id) v.program_id, v.audited_at, v.reason_class
             FROM corporate_weekly_audit_verdicts v
             ORDER BY v.program_id, v.audited_at DESC
         )
         SELECT p.id, p.title, p.summary, p.official_url, p.is_active,
                p.application_start_at, p.application_end_at, p.deadline_at, p.benefit_valid_until,
                p.benefit_amount_monthly, p.benefit_amount_semester, p.benefit_amount_once,
                p.min_age, p.max_age, pm.candidate_id
         FROM programs p
         LEFT JOIN primary_monitor pm ON pm.program_id = p.id
         LEFT JOIN latest ON latest.program_id = p.id
         WHERE {SCOPE_SQL}
           AND (latest.program_id IS NULL
                OR latest.audited_at <= now() - make_interval(hours => $1)
                OR (latest.reason_class = 'transient'
                    AND latest.audited_at <= now() - make_interval(hours => $2)))
           AND ($3::uuid[] IS NULL OR p.id = ANY($3))
           AND ($4::uuid IS NULL OR pm.candidate_id = $4)
         ORDER BY latest.audited_at NULLS FIRST, p.id"
    ))
    .bind(AUDIT_INTERVAL_HOURS as i32)
    .bind(TRANSIENT_RETRY_HOURS as i32)
    .bind(only)
    .bind(candidate)
    .fetch_all(pool)
    .await
    .context("failed to select programmes due a weekly audit")
}

/// The sources, as [`reextract`] reads them (same policy columns). Every
/// non-retired monitor counts: the audit only reads.
async fn load_targets(pool: &PgPool, candidates: &[Uuid]) -> Result<Vec<Target>> {
    let mut targets = sqlx::query_as::<_, Target>(&format!(
        "SELECT c.id AS candidate_id, now() AS requested_at, NULL::text AS reason,
                c.url, d.id AS domain_id, d.domain, d.organization_name, d.compliance_status,
                d.policy_notes, d.allow_subdomains, d.verification_status, d.is_active AS domain_active,
                d.gray_generic_tos, d.gray_robots_missing, d.gray_crawl_delay_min,
                d.min_request_interval_ms, d.max_pages_per_day,
                {eligible} AS domain_eligible,
                0 AS consecutive_failures,
                COALESCE(
                    (SELECT b.name FROM benefit_candidate_brands cb JOIN brands b ON b.id = cb.brand_id
                     WHERE cb.candidate_id = c.id ORDER BY b.name LIMIT 1),
                    (SELECT p.provider_name FROM program_source_monitors m JOIN programs p ON p.id = m.program_id
                     WHERE m.candidate_id = c.id AND m.verification_state <> 'retired'
                       AND p.program_type = 'corporate_benefit'
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
                      WHERE m.candidate_id = c.id AND m.verification_state <> 'retired'
                      ORDER BY m.program_id) AS program_ids
         FROM benefit_source_candidates c
         JOIN benefit_source_domains d ON d.id = c.domain_id
         WHERE c.id = ANY($1)",
        eligible = policy::ELIGIBLE_DOMAIN_SQL
    ))
    .bind(candidates)
    .fetch_all(pool)
    .await
    .context("failed to load weekly audit sources")?;
    // A brand-scope offer is audited under the scope it was read in (H5).
    super::reextract::attach_scopes(pool, &mut targets).await?;
    Ok(targets)
}

/// Sources whose page the evidence job requested within
/// [`EVIDENCE_REUSE_HOURS`].
async fn evidence_fetched_recently(pool: &PgPool, candidates: &[Uuid]) -> Result<Vec<Uuid>> {
    Ok(sqlx::query_scalar(
        "SELECT candidate_id FROM corporate_evidence_targets
         WHERE candidate_id = ANY($1)
           AND last_checked_at > now() - make_interval(hours => $2)
           AND NOT (COALESCE(last_outcome, '') = ANY($3))",
    )
    .bind(candidates)
    .bind(EVIDENCE_REUSE_HOURS as i32)
    .bind(EVIDENCE_NO_REQUEST_OUTCOMES)
    .fetch_all(pool)
    .await?)
}

// ── recording ────────────────────────────────────────────────────────────

/// The extractor's outcome for a page that states no student or youth
/// offer: outside what it reads, so the programme is unchecked.
const OUT_OF_SCOPE: &str = "no_student_youth_offer";

fn reason_class(outcome: &str) -> &'static str {
    if outcome == OUT_OF_SCOPE {
        return "unchecked";
    }
    if let Some(evidence) = outcome.strip_prefix("evidence_") {
        return match evidence {
            "fetch_failed" | "rate_limited" => "transient",
            "robots_disallowed" | "blocked" | "opted_out" | "policy_blocked"
            | "domain_withdrawn" => "policy",
            _ => "no_offer",
        };
    }
    if matches!(outcome, "record_failed" | "audit_failed") {
        return "transient";
    }
    match reextract::classify(outcome) {
        OutcomeClass::NoOffer | OutcomeClass::Proposal => "no_offer",
        OutcomeClass::Policy => "policy",
        OutcomeClass::Transient => "transient",
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct Counted {
    agrees: usize,
    disagrees: usize,
    unreadable: usize,
}

impl AuditSummary {
    fn add(&mut self, counted: Counted, fields: &[&'static str]) {
        self.agrees += counted.agrees;
        self.disagrees += counted.disagrees;
        self.unreadable += counted.unreadable;
        for field in fields {
            *self.by_field.entry(field.to_string()).or_default() += 1;
        }
    }
}

/// `details` within [`MAX_DETAILS_BYTES`]: the page reading and gates go
/// first, then every per-field detail text; the verdict and fields are
/// columns of their own and never cut.
pub(crate) fn cap_details(details: Value) -> Value {
    let fits = |v: &Value| v.to_string().len() <= MAX_DETAILS_BYTES;
    if fits(&details) {
        return details;
    }
    let mut cut = details;
    if let Some(map) = cut.as_object_mut() {
        map.remove("read");
        map.remove("gates");
        map.insert("truncated".into(), json!(true));
    }
    if fits(&cut) {
        return cut;
    }
    if let Some(checks) = cut.get_mut("checks").and_then(Value::as_object_mut) {
        for check in checks.values_mut() {
            if let Some(map) = check.as_object_mut() {
                map.remove("detail");
            }
        }
    }
    if let Some(map) = cut.as_object_mut() {
        map.remove("detail");
    }
    if fits(&cut) {
        cut
    } else {
        json!({"truncated": true})
    }
}

/// Writes a verdict; when the row cannot be saved, writes a transient
/// `record_failed` verdict with fixed values instead, so the programme is
/// retried after [`TRANSIENT_RETRY_HOURS`] rather than on every run.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn save_verdict(
    pool: &PgPool,
    mode: VerifyMode,
    programme: &AuditedProgramme,
    via: &str,
    fields: &[&str],
    unreadable: Option<(&str, &str)>,
    page: Option<(&str, Option<&str>)>,
    details: Value,
) -> Result<()> {
    let details = cap_details(details);
    match insert_verdict(
        pool, mode, programme, via, fields, unreadable, page, &details,
    )
    .await
    {
        Ok(()) => Ok(()),
        Err(error) => {
            warn!(event = "corporate_weekly_audit.record_failed", program_id = %programme.id,
                  error = %crate::error_safety::persisted_error(&error),
                  "weekly audit verdict could not be saved; retried in a day");
            insert_verdict(
                pool,
                mode,
                programme,
                via,
                &[],
                Some(("record_failed", "transient")),
                None,
                &json!({"detail": "the verdict could not be saved"}),
            )
            .await
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn insert_verdict(
    pool: &PgPool,
    mode: VerifyMode,
    programme: &AuditedProgramme,
    via: &str,
    fields: &[&str],
    unreadable: Option<(&str, &str)>,
    page: Option<(&str, Option<&str>)>,
    details: &Value,
) -> Result<()> {
    let verdict = match (unreadable.is_some(), fields.is_empty()) {
        (true, _) => "unreadable",
        (false, true) => "agrees",
        (false, false) => "disagrees",
    };
    sqlx::query(
        "INSERT INTO corporate_weekly_audit_verdicts (program_id, candidate_id, mode, via, verdict,
             fields, reason, reason_class, program_active, final_url, page_hash, details)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
    )
    .bind(programme.id)
    .bind(programme.candidate_id)
    .bind(mode.as_str())
    .bind(via)
    .bind(verdict)
    .bind(fields)
    .bind(unreadable.map(|(reason, _)| reason.chars().take(60).collect::<String>()))
    .bind(unreadable.map(|(_, class)| class))
    .bind(programme.is_active)
    .bind(page.map(|(url, _)| url.chars().take(2000).collect::<String>()))
    .bind(page.and_then(|(_, hash)| hash))
    .bind(details)
    .execute(pool)
    .await?;
    Ok(())
}

/// Records one verdict per programme read from `attempt`'s source. Returns
/// the counts and the disagreeing fields.
async fn record_attempt(
    pool: &PgPool,
    mode: VerifyMode,
    attempt: &Attempt,
    programmes: &[&AuditedProgramme],
    via: &str,
) -> Result<(Counted, Vec<&'static str>)> {
    let mut counted = Counted::default();
    let mut all_fields = Vec::new();
    let read = attempt
        .decision
        .as_ref()
        .filter(|_| attempt.outcome == "proposal")
        .and_then(|d| Some((d, d.offer.as_ref()?, d.page_text.as_deref()?)));
    for programme in programmes {
        match read {
            Some((decision, offer, page_text)) => {
                let page_url = decision.final_url.as_deref().unwrap_or(&decision.url);
                let checks = compare(programme, offer, page_text, page_url);
                let fields: Vec<&'static str> = checks
                    .iter()
                    .filter(|c| !c.agrees)
                    .map(|c| c.field)
                    .collect();
                let mut unchecked = Vec::new();
                if programme.application_start_at.is_some() {
                    unchecked.push("application_start_at");
                }
                let details = json!({
                    "checks": checks.iter().map(|c| (c.field.to_string(), json!({
                        "agrees": c.agrees, "detail": c.detail,
                    }))).collect::<serde_json::Map<_, _>>(),
                    "unchecked": unchecked,
                    "read": {
                        "facts": offer.facts.iter()
                            .map(|f| json!({"kind": f.kind, "value": f.value}))
                            .collect::<Vec<_>>(),
                        "audience": offer.audience,
                        "end_date": offer.end_date,
                        "age": offer.age,
                    },
                    "gates": attempt.gates,
                });
                save_verdict(
                    pool,
                    mode,
                    programme,
                    via,
                    &fields,
                    None,
                    Some((page_url, decision.page_hash.as_deref())),
                    details,
                )
                .await?;
                if fields.is_empty() {
                    counted.agrees += 1;
                } else {
                    counted.disagrees += 1;
                }
                all_fields.extend(fields);
            }
            None => {
                let outcome = attempt.outcome.as_str();
                let details = json!({
                    "detail": attempt.detail.chars().take(200).collect::<String>(),
                    "gates": attempt.gates,
                });
                save_verdict(
                    pool,
                    mode,
                    programme,
                    via,
                    &[],
                    Some((outcome, reason_class(outcome))),
                    None,
                    details,
                )
                .await?;
                counted.unreadable += 1;
            }
        }
    }
    Ok((counted, all_fields))
}

// ── the standalone run ───────────────────────────────────────────────────

/// Production entry point: the shared domain slot, real precheck and scan.
pub async fn run(
    pool: &PgPool,
    flags: &Flags,
    http: &dyn super::http::PageHttp,
) -> Result<AuditSummary> {
    run_with(
        pool,
        flags.verify_mode,
        flags.gray_generic_tos,
        http,
        None,
        Budget::default(),
        None,
        bootstrap::precheck,
        super::publish::PAGE_ENDING_SCAN,
    )
    .await
}

/// `only` limits the run to these programmes; `test_spacing` replaces the
/// 10 s floor and the shared slot (tests only); `check` is the request-free
/// policy ([`bootstrap::precheck`] outside tests); `scan` the whole-page
/// ending scan. `mode` is recorded only: no mode changes what is written.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_with(
    pool: &PgPool,
    mode: VerifyMode,
    gray_generic_tos: bool,
    http: &dyn super::http::PageHttp,
    only: Option<Vec<Uuid>>,
    budget: Budget,
    test_spacing: Option<std::time::Duration>,
    check: fn(&CandidateRow) -> Result<Url, &'static str>,
    scan: Option<EndingScan>,
) -> Result<AuditSummary> {
    anyhow::ensure!(
        http.user_agent().starts_with("MazimiBenefitBot/"),
        "the weekly audit only sends the MazimiBenefitBot user agent"
    );
    let due = due_programmes(pool, only.as_deref(), None).await?;
    let mut summary = AuditSummary {
        due: due.len(),
        ..AuditSummary::default()
    };
    // No source to read: recorded without a request.
    for programme in due.iter().filter(|p| p.candidate_id.is_none()) {
        save_verdict(
            pool,
            mode,
            programme,
            "no_request",
            &[],
            Some(("no_source_monitor", "no_source")),
            None,
            json!({"detail": "no non-retired source monitor"}),
        )
        .await?;
        summary.unreadable += 1;
    }
    // Sources in due order (the first due programme decides).
    let mut order: Vec<Uuid> = Vec::new();
    for candidate in due.iter().filter_map(|p| p.candidate_id) {
        if !order.contains(&candidate) {
            order.push(candidate);
        }
    }
    let programmes_of = |candidate: Uuid| -> Vec<&AuditedProgramme> {
        due.iter()
            .filter(|p| p.candidate_id == Some(candidate))
            .collect()
    };
    let recent = evidence_fetched_recently(pool, &order).await?;
    for candidate in order.iter().filter(|c| recent.contains(c)) {
        summary.deferred_evidence_recent += programmes_of(*candidate).len();
    }
    order.retain(|c| !recent.contains(c));
    for candidate in order.iter().skip(budget.max_sources) {
        summary.deferred_budget += programmes_of(*candidate).len();
    }
    order.truncate(budget.max_sources);
    let loaded = load_targets(pool, &order).await?;
    let mut targets: Vec<Target> = order
        .iter()
        .filter_map(|id| loaded.iter().find(|t| t.candidate_id == *id).cloned())
        .collect();
    // One polite session per domain; the cap was applied in due order.
    targets.sort_by_key(|t| t.domain_id);

    let today = super::health::kst_today();
    let optouts = LiveOptOuts(pool);
    let mut remaining = budget.max_requests;
    let mut index = 0;
    while index < targets.len() {
        let domain_id = targets[index].domain_id;
        let group_end = targets[index..]
            .iter()
            .position(|t| t.domain_id != domain_id)
            .map_or(targets.len(), |offset| index + offset);
        let fit = (remaining / REQUESTS_PER_SOURCE) as usize;
        let take_end = group_end.min(index + fit);
        if take_end == index {
            for target in &targets[index..] {
                summary.deferred_budget += programmes_of(target.candidate_id).len();
            }
            break;
        }
        for target in &targets[take_end..group_end] {
            summary.deferred_budget += programmes_of(target.candidate_id).len();
        }
        let group = &targets[index..take_end];
        index = group_end;
        let first = &group[0];
        let shared_slot = test_spacing.is_none();
        let mut session = DomainSession::new(
            http,
            shared_slot.then_some(pool),
            shared_slot.then_some(domain_id),
            first.gray_crawl_delay_min,
            first.min_request_interval_ms as i64,
            (first.max_pages_per_day.clamp(1, 200) as u32).min(remaining),
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
            reextract::attempt_domain(&mut session, group, &settings, today, check, &optouts, scan)
                .await;
        summary.requests += session.requests();
        remaining = remaining.saturating_sub(session.requests());
        for (target, attempt) in group.iter().zip(attempts) {
            let programmes = programmes_of(target.candidate_id);
            if attempt.outcome == "request_budget" {
                summary.deferred_budget += programmes.len();
                continue;
            }
            match record_attempt(pool, mode, &attempt, &programmes, "audit_fetch").await {
                Ok((counted, fields)) => summary.add(counted, &fields),
                Err(error) => warn!(event = "corporate_weekly_audit.record_failed",
                    candidate_id = %target.candidate_id,
                    error = %crate::error_safety::persisted_error(&error),
                    "weekly audit verdict could not be saved; the source stays due"),
            }
        }
    }
    Ok(summary)
}

// ── the evidence job's read ──────────────────────────────────────────────

/// What the evidence job read from a source: the page, or the outcome
/// (gone, blocked, a transient failure …) when it got none.
#[derive(Debug, Clone)]
pub enum EvidenceRead {
    Page { html: String, final_url: String },
    Outcome(String),
}

/// Hook failures (panic or error) since the worker started; each one is
/// also an occurrence of the `corporate_weekly_audit:hook` event.
pub static HOOK_FAILURES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Whether the evidence job should hand its reads to the audit.
pub fn hook_enabled() -> bool {
    match enabled() {
        Ok(on) => on,
        Err(error) => {
            warn!(event = "corporate_weekly_audit.config_invalid", error = %error, "weekly audit flag invalid");
            false
        }
    }
}

/// Called by the evidence job right after it read a source under its own
/// policy (opt-out, census lane `direct:direct_html`, robots, the shared
/// domain slot). When a programme on that source is due, the page in memory
/// is audited instead of being fetched again, or the evidence outcome is
/// recorded as `unreadable`. Runs in its own task and returns at once: the
/// evidence result and timing never depend on it.
pub fn observe_evidence(pool: &PgPool, mode: VerifyMode, candidate_id: Uuid, read: EvidenceRead) {
    let worker = pool.clone();
    isolate(pool.clone(), mode, candidate_id, async move {
        observe_with(
            &worker,
            mode,
            candidate_id,
            &read,
            bootstrap::precheck,
            super::publish::PAGE_ENDING_SCAN,
        )
        .await
    });
}

/// Runs `work` in a task of its own and supervises it from another: a
/// panic or error is logged, counted ([`HOOK_FAILURES`], the
/// `corporate_weekly_audit:hook` event) and recorded as a transient
/// `audit_failed` verdict for the source's due programmes, so they are
/// retried in a day and still get a verdict this cycle.
pub(crate) fn isolate<F>(
    pool: PgPool,
    mode: VerifyMode,
    candidate_id: Uuid,
    work: F,
) -> tokio::task::JoinHandle<()>
where
    F: std::future::Future<Output = Result<usize>> + Send + 'static,
{
    let task = tokio::spawn(work);
    tokio::spawn(async move {
        let failure = match task.await {
            Ok(Ok(_)) => return,
            Ok(Err(error)) => crate::error_safety::persisted_error(&error).to_string(),
            Err(join) if join.is_panic() => "panic".to_string(),
            Err(_) => "cancelled".to_string(),
        };
        HOOK_FAILURES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        warn!(event = "corporate_weekly_audit.hook_failed", candidate_id = %candidate_id,
              failure = %failure, "weekly audit of an evidence read failed; the evidence job is unaffected");
        if let Err(error) = record_hook_failure(&pool, mode, candidate_id, &failure).await {
            warn!(event = "corporate_weekly_audit.hook_failed", candidate_id = %candidate_id,
                  error = %crate::error_safety::persisted_error(&error),
                  "the weekly audit hook failure could not be recorded either");
        }
    })
}

async fn record_hook_failure(
    pool: &PgPool,
    mode: VerifyMode,
    candidate_id: Uuid,
    failure: &str,
) -> Result<()> {
    let subject = candidate_id.to_string();
    operations::record_event(
        pool,
        &operations::EventCommand {
            fingerprint: "corporate_weekly_audit:hook",
            severity: operations::Severity::Warning,
            component: "corporate_auto",
            event_type: "corporate_weekly_audit_hook_failed",
            subject_type: Some("benefit_source_candidate"),
            subject_id: Some(&subject),
            message: "The weekly audit of an evidence read failed; the source is retried in a day",
            details: json!({"failure": failure.chars().take(200).collect::<String>()}),
        },
    )
    .await?;
    for programme in due_programmes(pool, None, Some(candidate_id)).await? {
        save_verdict(
            pool,
            mode,
            &programme,
            "evidence_page",
            &[],
            Some(("audit_failed", "transient")),
            None,
            json!({"detail": failure.chars().take(200).collect::<String>()}),
        )
        .await?;
    }
    Ok(())
}

/// The audit of one evidence read, without the flag or the task; returns
/// the verdicts written.
pub(crate) async fn observe_with(
    pool: &PgPool,
    mode: VerifyMode,
    candidate_id: Uuid,
    read: &EvidenceRead,
    check: fn(&CandidateRow) -> Result<Url, &'static str>,
    scan: Option<EndingScan>,
) -> Result<usize> {
    let due = due_programmes(pool, None, Some(candidate_id)).await?;
    if due.is_empty() {
        return Ok(0);
    }
    let attempt = match read {
        EvidenceRead::Outcome(outcome) => Attempt {
            outcome: format!("evidence_{outcome}").chars().take(60).collect(),
            detail: "the evidence job read no page from this source".into(),
            decision: None,
            gates: json!({"evidence_outcome": outcome}),
        },
        EvidenceRead::Page { html, final_url } => {
            let Some(target) = load_targets(pool, &[candidate_id])
                .await?
                .into_iter()
                .next()
            else {
                return Ok(0);
            };
            attempt_from_page(
                &target,
                html,
                final_url,
                super::health::kst_today(),
                check,
                scan,
            )
        }
    };
    let programmes: Vec<&AuditedProgramme> = due.iter().collect();
    let (counted, _) = record_attempt(pool, mode, &attempt, &programmes, "evidence_page").await?;
    Ok(counted.agrees + counted.disagrees + counted.unreadable)
}

/// The fresh-extraction gates on a page another job read under policy. The
/// lane is the census's `direct:direct_html` (the evidence job reads only
/// such pages); the request-free gates, the extractor and the offer gates
/// are [`reextract`]'s own.
fn attempt_from_page(
    target: &Target,
    html: &str,
    final_url: &str,
    today: NaiveDate,
    check: fn(&CandidateRow) -> Result<Url, &'static str>,
    scan: Option<EndingScan>,
) -> Attempt {
    let refused = |outcome: &str, detail: &str| Attempt {
        outcome: outcome.to_string(),
        detail: detail.to_string(),
        decision: None,
        gates: json!({"lane_from": "census"}),
    };
    if !target.domain_eligible {
        return refused(
            "policy_blocked",
            "the domain is rejected, blocked, restricted or serves no corporate benefit",
        );
    }
    let row = target.candidate_row();
    if let Err(reason) = check(&row) {
        return refused(reason, "request-free policy refused the source");
    }
    let mut decision = Decision {
        candidate_ids: vec![target.candidate_id.to_string()],
        url: target.url.clone(),
        final_url: (final_url != target.url).then(|| final_url.to_string()),
        domain: target.domain.clone(),
        brand: target.brand_name.clone(),
        brand_id: None,
        domain_id: Some(target.domain_id.to_string()),
        outcome: String::new(),
        http_status: None,
        page_title: None,
        offer: None,
        visible_chars: None,
        page_hash: None,
        page_text: None,
        page_html: None,
        scope: row.scope.clone(),
    };
    bootstrap::read_page(&mut decision, html, today);
    let gates = json!({
        "lane": "direct",
        "lane_reason": "direct_html",
        "lane_from": "census",
    });
    if decision.outcome != "proposal" {
        return Attempt {
            detail: format!("extraction outcome {}", decision.outcome),
            outcome: decision.outcome.clone(),
            decision: None,
            gates,
        };
    }
    reextract::gate_offer(decision, gates, scan)
}

// ── aggregate ────────────────────────────────────────────────────────────

/// The latest verdict of one programme within the cycle.
#[derive(Debug, Clone, FromRow)]
pub struct LatestVerdict {
    pub program_id: Uuid,
    pub title: String,
    pub verdict: String,
    pub fields: Vec<String>,
    pub reason: Option<String>,
    pub audited_at: DateTime<Utc>,
    /// Whether the offer is shown now.
    pub visible: bool,
    /// The read verdict (agrees or disagrees) before this one, if any.
    pub previous_read: Option<String>,
}

/// How much of the scope the cycle covered.
#[derive(Debug, Clone, Copy, Default)]
pub struct Scope {
    pub in_scope: i64,
    /// In scope for at least one full cycle (created before it began).
    pub established: i64,
    /// Established programmes with a verdict in the cycle.
    pub established_audited: i64,
}

/// Aggregate of the latest verdict per programme in the last [`CYCLE_DAYS`]
/// days. Pure.
///
/// - `agree_rate`: agreeing over read (agreeing + disagreeing), `None` when
///   none was read; `coverage`: audited over in scope.
/// - `new_disagreements`: programmes whose verdict in the 24 h before `now`
///   disagrees while their previous read agreed (a transition).
/// - `visible_disagreements`: shown offers whose current verdict disagrees.
/// - `armed`: every established programme has a verdict in the cycle; the
///   alerts read nothing before (the first cycle is silent).
pub fn summarize(rows: &[LatestVerdict], scope: Scope, now: DateTime<Utc>) -> Value {
    let count = |verdict: &str| rows.iter().filter(|r| r.verdict == verdict).count() as i64;
    let unchecked_row =
        |r: &&LatestVerdict| r.verdict == "unreadable" && r.reason.as_deref() == Some(OUT_OF_SCOPE);
    let unchecked = rows.iter().filter(unchecked_row).count() as i64;
    let (agrees, disagrees, unreadable) = (
        count("agrees"),
        count("disagrees"),
        count("unreadable") - unchecked,
    );
    let mut by_field = serde_json::Map::new();
    for field in FIELDS {
        let n = rows
            .iter()
            .filter(|r| r.verdict == "disagrees" && r.fields.iter().any(|f| f == field))
            .count();
        by_field.insert(field.to_string(), json!(n));
    }
    let mut by_reason: BTreeMap<&str, i64> = BTreeMap::new();
    for row in rows
        .iter()
        .filter(|r| r.verdict == "unreadable" && !unchecked_row(r))
    {
        *by_reason
            .entry(row.reason.as_deref().unwrap_or("unknown"))
            .or_default() += 1;
    }
    let mut disagreeing: Vec<&LatestVerdict> =
        rows.iter().filter(|r| r.verdict == "disagrees").collect();
    disagreeing.sort_by(|a, b| a.title.cmp(&b.title).then(a.program_id.cmp(&b.program_id)));
    let since = now - chrono::Duration::hours(24);
    let transitions: Vec<&&LatestVerdict> = disagreeing
        .iter()
        .filter(|r| r.audited_at > since && r.previous_read.as_deref() == Some("agrees"))
        .collect();
    let visible_disagreements = disagreeing.iter().filter(|r| r.visible).count();
    let read = agrees + disagrees;
    let audited = rows.len() as i64;
    let listed = |r: &LatestVerdict| {
        json!({
            "program_id": r.program_id,
            "title": r.title,
            "fields": r.fields,
            "visible": r.visible,
            "audited_at": r.audited_at,
        })
    };
    json!({
        "window_days": CYCLE_DAYS,
        "programmes_in_scope": scope.in_scope,
        "established": scope.established,
        "established_audited": scope.established_audited,
        "armed": scope.established > 0 && scope.established_audited >= scope.established,
        "established_coverage": (scope.established > 0)
            .then(|| scope.established_audited as f64 / scope.established as f64),
        "audited": audited,
        "agrees": agrees,
        "disagrees": disagrees,
        "unreadable": unreadable,
        "unchecked": unchecked,
        "agree_rate": (read > 0).then(|| agrees as f64 / read as f64),
        "coverage": (scope.in_scope > 0).then(|| audited as f64 / scope.in_scope as f64),
        "disagreement_by_field": by_field,
        "unreadable_by_reason": by_reason,
        "visible_disagreements": visible_disagreements,
        "new_disagreements": transitions.iter().map(|r| listed(r)).collect::<Vec<_>>(),
        "disagreeing": disagreeing.iter().take(LISTED_DISAGREEMENTS).map(|r| listed(r)).collect::<Vec<_>>(),
    })
}

/// [`summarize`] over the database. `only` limits to these programmes.
pub async fn aggregate(pool: &PgPool, only: Option<&[Uuid]>) -> Result<Value> {
    let (in_scope, established, established_audited): (i64, i64, i64) = sqlx::query_as(&format!(
        "SELECT count(*),
                    count(*) FILTER (WHERE p.created_at <= now() - make_interval(days => $2)),
                    count(*) FILTER (WHERE p.created_at <= now() - make_interval(days => $2)
                        AND EXISTS (SELECT 1 FROM corporate_weekly_audit_verdicts v
                                    WHERE v.program_id = p.id
                                      AND v.audited_at > now() - make_interval(days => $2)))
             FROM programs p WHERE {SCOPE_SQL} AND ($1::uuid[] IS NULL OR p.id = ANY($1))"
    ))
    .bind(only)
    .bind(CYCLE_DAYS as i32)
    .fetch_one(pool)
    .await?;
    let rows = sqlx::query_as::<_, LatestVerdict>(&format!(
        "SELECT DISTINCT ON (v.program_id) v.program_id, p.title, v.verdict, v.fields, v.reason,
                v.audited_at, p.is_active AS visible,
                (SELECT w.verdict FROM corporate_weekly_audit_verdicts w
                 WHERE w.program_id = v.program_id AND w.audited_at < v.audited_at
                   AND w.verdict <> 'unreadable'
                 ORDER BY w.audited_at DESC LIMIT 1) AS previous_read
         FROM corporate_weekly_audit_verdicts v JOIN programs p ON p.id = v.program_id
         WHERE {SCOPE_SQL}
           AND v.audited_at > now() - make_interval(days => $1)
           AND ($2::uuid[] IS NULL OR v.program_id = ANY($2))
         ORDER BY v.program_id, v.audited_at DESC"
    ))
    .bind(CYCLE_DAYS as i32)
    .bind(only)
    .fetch_all(pool)
    .await?;
    let scope = Scope {
        in_scope,
        established,
        established_audited,
    };
    // The 24 h window of new disagreements is read on the database clock,
    // the one `audited_at` is written with: a worker clock a few seconds
    // off moved a verdict exactly a day old into or out of the window.
    let now: DateTime<Utc> = sqlx::query_scalar("SELECT now()").fetch_one(pool).await?;
    Ok(summarize(&rows, scope, now))
}

/// The health row's block: [`aggregate`] plus `armed_ever` (armed now, or
/// in any earlier health row: the alerts stay armed once a full cycle has
/// completed) and `enabled` (the flag, so a coverage dip alerts only while
/// the audit runs).
pub async fn health_block(pool: &PgPool) -> Result<Value> {
    let mut block = aggregate(pool, None).await?;
    let armed_before: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM corporate_pipeline_daily
                        WHERE metrics->'weekly_audit'->>'armed_ever' = 'true'
                           OR metrics->'weekly_audit'->>'armed' = 'true')",
    )
    .fetch_one(pool)
    .await?;
    block["armed_ever"] = json!(block["armed"].as_bool() == Some(true) || armed_before);
    block["enabled"] = json!(enabled().unwrap_or(false));
    Ok(block)
}

/// The digest section (Korean, like the rest of the digest). Pure.
pub fn digest_text(aggregate: &Value) -> String {
    let n = |key: &str| aggregate[key].as_i64().unwrap_or(0);
    let mut text = String::from("\n■ 기업 혜택 주간 재추출 감사 (측정 전용, 숨김·변경 없음)\n");
    if n("audited") == 0 {
        text.push_str(&format!(
            "- 최근 {}일 감사 없음 (대상 {}건)\n",
            CYCLE_DAYS,
            n("programmes_in_scope")
        ));
        return text;
    }
    let rate = aggregate["agree_rate"]
        .as_f64()
        .map_or("-".to_string(), |r| format!("{:.1}%", r * 100.0));
    text.push_str(&format!(
        "- 최근 {}일: 감사 {} / 대상 {} · 일치 {} · 불일치 {} (노출 중 {}) · 판독 불가 {}{} · 일치율 {rate}\n",
        CYCLE_DAYS,
        n("audited"),
        n("programmes_in_scope"),
        n("agrees"),
        n("disagrees"),
        n("visible_disagreements"),
        n("unreadable"),
        match n("unchecked") {
            0 => String::new(),
            unchecked => format!(" · 확인 범위 밖 {unchecked}"),
        },
    ));
    if aggregate["armed"].as_bool() != Some(true) {
        text.push_str(&format!(
            "- 경보 대기: 첫 주기 진행 중 ({} / {}건 감사)\n",
            n("established_audited"),
            n("established")
        ));
    }
    let new: Vec<&str> = aggregate["new_disagreements"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|o| o["title"].as_str())
        .collect();
    if !new.is_empty() {
        text.push_str(&format!("- 새로 불일치 (24시간): {}\n", new.join(", ")));
    }
    let fields: Vec<String> = FIELDS
        .iter()
        .filter_map(|field| {
            let count = aggregate["disagreement_by_field"][*field]
                .as_i64()
                .unwrap_or(0);
            (count > 0).then(|| format!("{field} {count}"))
        })
        .collect();
    if !fields.is_empty() {
        text.push_str(&format!("- 필드별 불일치: {}\n", fields.join(" · ")));
    }
    if let Some(reasons) = aggregate["unreadable_by_reason"].as_object() {
        if !reasons.is_empty() {
            let list: Vec<String> = reasons.iter().map(|(r, c)| format!("{r} {c}")).collect();
            text.push_str(&format!("- 판독 불가 사유: {}\n", list.join(" · ")));
        }
    }
    for offer in aggregate["disagreeing"]
        .as_array()
        .into_iter()
        .flatten()
        .take(20)
    {
        let fields: Vec<&str> = offer["fields"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        text.push_str(&format!(
            "- 불일치: {} ({}){}\n",
            offer["title"].as_str().unwrap_or(""),
            fields.join(", "),
            if offer["visible"].as_bool() == Some(true) {
                " · 노출 중"
            } else {
                ""
            }
        ));
    }
    text
}

// ── scheduling ───────────────────────────────────────────────────────────

/// Worker entry point (daily); logs instead of failing the scheduler.
pub async fn run_scheduled(pool: &PgPool) {
    static RUNNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    match enabled() {
        Ok(true) => {}
        Ok(false) => return,
        Err(error) => {
            warn!(event = "corporate_weekly_audit.config_invalid", error = %error, "weekly audit flag invalid");
            return;
        }
    }
    if RUNNING.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    struct Release;
    impl Drop for Release {
        fn drop(&mut self) {
            RUNNING.store(false, std::sync::atomic::Ordering::SeqCst);
        }
    }
    let _release = Release;
    let flags = match Flags::from_env() {
        Ok(flags) => flags,
        Err(error) => {
            warn!(event = "corporate_weekly_audit.config_invalid", error = %error, "corporate flags misconfigured");
            return;
        }
    };
    let Some(user_agent) = flags.user_agent() else {
        warn!(
            event = "corporate_weekly_audit.config_invalid",
            "CORPORATE_BOT_CONTACT_URL is required for the weekly audit"
        );
        return;
    };
    let http = match super::http::PinnedHttp::new(user_agent, std::time::Duration::from_secs(20)) {
        Ok(http) => http,
        Err(error) => {
            warn!(event = "corporate_weekly_audit.config_invalid", error = %error, "weekly audit transport rejected");
            return;
        }
    };
    match run(pool, &flags, &http).await {
        Ok(summary) => {
            info!(event = "corporate_weekly_audit.run", mode = flags.verify_mode.as_str(),
                  due = summary.due, agrees = summary.agrees, disagrees = summary.disagrees,
                  unreadable = summary.unreadable, requests = summary.requests,
                  deferred_budget = summary.deferred_budget,
                  deferred_evidence_recent = summary.deferred_evidence_recent,
                  by_field = ?summary.by_field, "corporate weekly audit pass complete");
            let _ = operations::resolve_event(
                pool,
                "job:corporate_weekly_audit",
                None,
                Some("Run completed"),
            )
            .await;
        }
        Err(error) => {
            warn!(event = "corporate_weekly_audit.run_failed", error = %crate::error_safety::persisted_error(&error), "corporate weekly audit failed");
            let _ = operations::record_event(
                pool,
                &operations::EventCommand {
                    fingerprint: "job:corporate_weekly_audit",
                    severity: operations::Severity::Error,
                    component: "corporate_auto",
                    event_type: "corporate_weekly_audit_failed",
                    subject_type: None,
                    subject_id: None,
                    message: "Corporate weekly re-extraction audit run failed",
                    details: json!({}),
                },
            )
            .await;
        }
    }
}

#[cfg(test)]
#[path = "weekly_audit_tests.rs"]
mod tests;
