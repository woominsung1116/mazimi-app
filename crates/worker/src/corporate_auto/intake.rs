//! P2 intake: the one automatic path from a corporate candidate to a
//! programme (`.reports/corporate-p2-design.md`). It replaces the admin
//! `review_candidate` wait for corporate offers and gives human-owned
//! sources facts, so they leave the manual re-verification wait.
//!
//! Sources: a branded candidate with no live monitor (`new_offer`); a
//! candidate whose live monitors are all `human` on corporate programmes
//! with no facts (`human_source`); or a candidate whose live monitors are
//! all `evidence_auto` on held corporate programmes the evidence job queued
//! for fresh reading (`held_source`, [`HELD_FOR_REREADING_SQL`]: the one
//! exit of every hold the page can settle, so the offer returns only
//! through the same whole chain; the in-pass way back stays off and the
//! separate fresh-extraction job is retired). The domain passes
//! [`policy::ELIGIBLE_DOMAIN_SQL`].
//!
//! Every attempt, in order (the first failing gate is recorded):
//! 1. **policy**: [`bootstrap::run_domain_with`] and
//!    [`reextract::gate_decision`], the same gates as a fresh extraction:
//!    precheck (brand, verified domain, no rejected or terms-restricted
//!    review, own https host, no archive/aggregator/third-party, campus out),
//!    opt-out before every request, robots for our agent per path, ToS
//!    (specific clause stops, generic only under G1), the polite session,
//!    same-host redirects through [`policy::fetch_resolving_same_host`],
//!    lane `direct:direct_html`, quotes verbatim and at most 100 characters,
//!    reader B(a) [`super::resilience::extractors_agree`] on every quote, and
//!    the whole-page ending scan;
//! 2. **reader A**: [`bootstrap::extract_offer`] (inside step 1);
//! 3. **scope**: a student or youth audience;
//! 4. **reader B**: (b) the evidence verifier reads every quote back
//!    ([`evidence::evaluate_fact`] must confirm, with the quoted age band),
//!    (c) the display is composed from the facts' own sentences plus every
//!    restricting clause of the offer's section word for word
//!    ([`publish::compose_display`]; a condition that cannot be carried
//!    refuses), and the row as it would be written passes
//!    [`publish::verify_verbatim_fields`] (the §28 content invariant);
//! 5. **dedup** (new offers): no identity for (brand, offer key), no live
//!    programme of the brand on the host, no live programme or published
//!    intake offer on the URL;
//! 6. **stability**: the same offer key seen again at least
//!    [`STABLE_AFTER`] after its first sighting;
//! 7. **publish** (live only, at most [`MAX_PUBLISH_PER_RUN`]): a new offer
//!    becomes a hidden programme with an `evidence_auto` monitor, its quotes
//!    as facts and an `evidence_failed` hold, so it is shown only once the
//!    evidence job confirms the quotes on the live page; a human source moves
//!    to `evidence_auto`, and a human or held source goes through
//!    [`publish::publish`] (facts replaced, every public field against the
//!    page) in a savepoint that is rolled back unless the programme was
//!    restored.
//!
//! Every attempt writes one `corporate_intake_decisions` row. No page body is
//! stored. `CORPORATE_INTAKE_MODE` (`off` | `shadow` | `seeded` | `live`,
//! default shadow) is the kill switch; anything that publishes also needs
//! the evidence job in live. `seeded` publishes only from a reviewed seed
//! URL ([`SEED_VERIFIED_SQL`]) and keeps every other source, every
//! discovered page included, in shadow; each of its first publishes is
//! recorded with its post-check ([`post_check`]).

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Duration, NaiveDate, TimeZone, Utc};
use reqwest::Url;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::{Acquire, FromRow, PgPool, Postgres, Transaction};
use std::collections::{BTreeMap, BTreeSet};
use tracing::{info, warn};
use uuid::Uuid;

use super::bootstrap::{self, CandidateRow, Decision, Offer, Settings};
use super::evidence::{self, Ages, FactOutcome, PageEvidence};
use super::http::PageHttp;
use super::policy::{self, DomainSession, LiveOptOuts, TosSignal};
use super::publish::{self, EndingScan, PublicProgramme, Published};
use super::{reextract, VerifyMode};

/// The kill switch: `off`, `shadow`, `seeded` or `live`.
pub const MODE_FLAG: &str = "CORPORATE_INTAKE_MODE";
pub const MAX_SOURCES_PER_RUN: i64 = 20;
pub const MAX_PUBLISH_PER_RUN: usize = 5;
/// The second sighting of the same offer must come this long after the first.
pub const STABLE_AFTER: Duration = Duration::hours(24);
/// Retry after a policy refusal or a page with no offer.
pub const REFRESH_AFTER: Duration = Duration::days(7);
/// First retry after a transient failure; doubles up to [`REFRESH_AFTER`].
pub const TRANSIENT_RETRY: Duration = Duration::hours(6);
/// A new programme's facts may adopt a baseline this long. The evidence job
/// reads a URL no census rated at its next pass, rating the page by the
/// census's own rule ([`super::policy::rate_lane`]), so adoption no longer
/// waits on the weekly census; the window covers a week of transient
/// failures.
pub const ADOPT_WITHIN_DAYS: i64 = 8;
/// Audience codes in scope: students and youth (an educator-only offer is not).
pub const STUDENT_YOUTH: &[&str] = &["student", "university_student", "graduate", "youth", "teen"];

/// Files a new offer into a browse category from its own text (title,
/// summary, audience), never from a brand list. Wired to
/// `mazimi_core::corporate_categories::classify` when that branch merges;
/// until then the text is stored and the category is computed on read.
pub type Categorize = fn(&str) -> &'static str;
pub const CATEGORIZE: Option<Categorize> = None;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Off,
    Shadow,
    /// Live for reviewed seed URLs only ([`SEED_VERIFIED_SQL`]), shadow
    /// for every other source.
    Seeded,
    Live,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Shadow => "shadow",
            Self::Seeded => "seeded",
            Self::Live => "live",
        }
    }

    /// Whether this mode publishes from `source`. A door's page publishes
    /// only while its lane is a live door kind (it is selected only then,
    /// [`select_sources`]) and intake is live.
    pub(crate) fn publishes(self, source: &Source) -> bool {
        if source.door_id.is_some() {
            return self == Self::Live;
        }
        match self {
            Self::Live => true,
            // A held source was published from this very URL before, and a
            // human source's URL is the one a person reviewed when it was
            // seeded; the chain re-derives either from the page as it reads
            // now (RC5: a person's timer has no renewer in an unattended
            // system).
            Self::Seeded => {
                source.seed_verified || matches!(source.kind(), "held_source" | "human_source")
            }
            Self::Off | Self::Shadow => false,
        }
    }
}

/// A source on a reviewed seed URL: the URL is one of the `seed_urls` of an
/// active brand discovery target whose official domain is verified
/// (`domain_status = 'verified'`, not policy-blocked), the candidate's source
/// domain is that official domain, verified and linked to the same brand,
/// and the candidate carries that brand (the brand gate). A page discovered
/// anywhere else, on the same domain included, is not one. Reads `c`
/// (candidate), `d` (its domain) and `b` (its brand).
pub const SEED_VERIFIED_SQL: &str = "EXISTS (
    SELECT 1 FROM brand_discovery_targets t
    JOIN benefit_source_domain_brands db ON db.brand_id = t.brand_id AND db.domain_id = d.id
    WHERE t.is_active AND t.domain_status = 'verified' AND t.policy_status <> 'blocked'
      AND t.brand_id = b.id
      AND lower(d.domain) = lower(t.official_domain)
      AND d.verification_status = 'verified' AND d.is_active
      AND (c.url = ANY(t.seed_urls) OR c.canonical_url = ANY(t.seed_urls)))";

/// Strict: an unknown value stops the job instead of reading as something.
pub fn parse_mode(raw: &str) -> Result<Mode> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "shadow" => Ok(Mode::Shadow),
        "off" => Ok(Mode::Off),
        "seeded" => Ok(Mode::Seeded),
        "live" => Ok(Mode::Live),
        other => bail!("{MODE_FLAG} must be off, shadow, seeded or live, got {other:?}"),
    }
}

/// Live needs the evidence job in live too: only it shows the offer.
pub fn effective_mode(intake: Mode, verify: VerifyMode) -> Mode {
    match (intake, verify) {
        (Mode::Live, VerifyMode::Live) => Mode::Live,
        (Mode::Seeded, VerifyMode::Live) => Mode::Seeded,
        (Mode::Off, _) => Mode::Off,
        _ => Mode::Shadow,
    }
}

// ── the request-free checks ───────────────────────────────────────────────

/// What the gates read from one page, ready to publish.
#[derive(Debug, Clone)]
pub struct Checked {
    pub offer: Offer,
    pub offer_key: String,
    pub title: String,
    pub summary: String,
    pub page_url: String,
    pub quotes: Vec<String>,
    pub min_age: Option<i32>,
    pub max_age: Option<i32>,
    pub valid_until: Option<DateTime<Utc>>,
}

/// A refusal: the gate and its reason.
pub type Refusal = (&'static str, String);

fn end_of_kst_day(day: NaiveDate) -> Option<DateTime<Utc>> {
    let kst = chrono::FixedOffset::east_opt(9 * 3600)?;
    kst.from_local_datetime(&day.and_hms_opt(23, 59, 59)?)
        .single()
        .map(|at| at.with_timezone(&Utc))
}

/// The offer key: brand, the facts' kinds and values, and the audience.
/// The rest of the page does not enter it.
pub fn offer_key(brand_key: &str, offer: &Offer) -> String {
    let facts: BTreeSet<String> = offer
        .facts
        .iter()
        .map(|f| format!("{}={}", f.kind, f.value))
        .collect();
    let audience: BTreeSet<&str> = offer.audience.iter().copied().collect();
    let mut hasher = Sha256::new();
    hasher.update(brand_key.trim().to_lowercase().as_bytes());
    hasher.update(b"\x1f");
    hasher.update(
        facts
            .into_iter()
            .collect::<Vec<_>>()
            .join("\x1e")
            .as_bytes(),
    );
    hasher.update(b"\x1f");
    hasher.update(
        audience
            .into_iter()
            .collect::<Vec<_>>()
            .join(",")
            .as_bytes(),
    );
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A URL as the dedup gate compares it: host without `www.` (with a
/// non-default port), path without a trailing slash, query kept, fragment
/// and scheme dropped, lowercased.
pub fn url_key(url: &str) -> String {
    let Ok(parsed) = Url::parse(url.trim()) else {
        return url.trim().to_lowercase();
    };
    let mut host = parsed
        .host_str()
        .unwrap_or("")
        .trim_start_matches("www.")
        .to_lowercase();
    if let Some(port) = parsed.port() {
        host = format!("{host}:{port}");
    }
    let path = parsed.path().trim_end_matches('/').to_lowercase();
    match parsed.query() {
        Some(query) => format!("{host}{path}?{}", query.to_lowercase()),
        None => format!("{host}{path}"),
    }
}

/// Steps 3–4 on a decision that passed every policy gate (`proposal`).
pub fn check_offer(decision: &Decision, brand: &str, brand_key: &str) -> Result<Checked, Refusal> {
    let Some(offer) = decision.offer.as_ref() else {
        return Err(("reader_a", "no offer".into()));
    };
    let (Some(text), Some(html)) = (decision.page_text.as_deref(), decision.page_html.as_deref())
    else {
        return Err(("reader_a", "page not kept in memory".into()));
    };
    let lines = publish::offer_section(html).lines;
    let mut offer = offer.clone();
    for fact in offer.facts.iter_mut().chain(offer.verification.iter_mut()) {
        // A quote no single line holds stays whole; the display takes the
        // whole sentences of the lines it spans.
        let quote =
            publish::own_line_quote(&fact.quote, &lines).unwrap_or_else(|| fact.quote.clone());
        if !super::resilience::extractors_agree(&quote) {
            return Err(("reader_b", "extractors_disagree".into()));
        }
        fact.quote = quote;
    }
    let offer = &offer;
    // A brand-scope offer (a door's own page, review H5) is in scope by its
    // door; every other offer needs a student or youth audience.
    if offer.scope_brand.is_none()
        && !offer
            .audience
            .iter()
            .any(|code| STUDENT_YOUTH.contains(code))
    {
        return Err((
            "scope",
            format!("audience_out_of_scope:{}", offer.audience.join(",")),
        ));
    }
    let page_url = decision
        .final_url
        .clone()
        .unwrap_or_else(|| decision.url.clone());
    // Rule 6: an offer the page restricts to another country is out.
    let fact_quotes: Vec<&str> = offer.facts.iter().map(|f| f.quote.as_str()).collect();
    if let Some(reason) = bootstrap::region_restriction(&lines, &page_url, &fact_quotes) {
        return Err(("scope", format!("region_out_of_scope:{reason}")));
    }
    let mut quotes: Vec<String> = Vec::new();
    for fact in offer.facts.iter().chain(offer.verification.iter()) {
        if !quotes.contains(&fact.quote) {
            quotes.push(fact.quote.clone());
        }
    }
    // Reader B(b): the evidence verifier reads every quote back.
    let (min_age, max_age) = offer.age.unwrap_or((None, None));
    let page = PageEvidence::from_visible_text(text);
    for quote in &quotes {
        let outcome = evidence::evaluate_fact(
            &page,
            quote,
            None,
            Ages {
                min: min_age,
                max: max_age,
            },
            true,
        );
        if !matches!(outcome, FactOutcome::Confirmed { .. }) {
            return Err(("reader_b", format!("quote_readback:{}", outcome.label())));
        }
    }
    // Reader B(c): one own-heading section holds every fact with its notes.
    let summary = publish::compose_display(html, offer)
        .map_err(|reason| ("reader_b", format!("section:{reason}")))?;
    let title =
        publish::offer_title(brand, offer).map_err(|reason| ("display", reason.to_string()))?;
    let valid_until = offer.end_date.and_then(end_of_kst_day);
    let shown = PublicProgramme {
        program_type: "corporate_benefit".into(),
        title: title.clone(),
        summary: Some(summary.clone()),
        official_url: Some(page_url.clone()),
        application_method: None,
        eligibility_condition: None,
        submission_documents: None,
        screening_method: None,
        application_steps: None,
        benefit_conditions: None,
        application_start_at: None,
        application_end_at: None,
        deadline_at: None,
        benefit_valid_until: valid_until,
        benefit_amount_monthly: None,
        benefit_amount_semester: None,
        benefit_amount_once: None,
        min_age,
        max_age,
        regions: None,
        region_districts: None,
        region_scope: None,
        school_scope: None,
        tags: None,
    };
    let quote_refs: Vec<&str> = quotes.iter().map(String::as_str).collect();
    publish::verify_verbatim_fields(&shown, offer, &quote_refs, html, text, &page_url)
        .map_err(|reason| ("display", reason))?;
    Ok(Checked {
        offer_key: offer_key(brand_key, offer),
        offer: offer.clone(),
        title,
        summary,
        page_url,
        quotes,
        min_age,
        max_age,
        valid_until,
    })
}

/// The gate a non-proposal outcome of steps 1–2 belongs to.
pub fn gate_of(outcome: &str) -> &'static str {
    match outcome {
        "no_student_youth_offer"
        | "offer_ended"
        | "multi_offer_page"
        | "conflicting_ages"
        | "stale_page"
        | "conflicting_terms"
        | "short_free_term"
        | "unbound_term"
        | "template_shell"
        | "product_listing"
        | "js_shell" => "reader_a",
        "quote_not_verbatim" | "extractors_disagree" => "reader_b",
        "offer_ended_scan" => "ending_scan",
        "page_unreachable"
        | "rate_limited"
        | "request_budget"
        | "robots_unavailable"
        | "domain_stopped_rate_limited"
        | "unexpected_reply"
        | "redirect_loop"
        | "page_gone"
        | "unsupported_content"
        | "visit_window"
        | "not_attempted" => "transient",
        _ => "policy",
    }
}

// ── the job ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, FromRow)]
pub(crate) struct Source {
    pub(crate) candidate_id: Uuid,
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
    pub(crate) brand_id: Option<Uuid>,
    pub(crate) brand_name: Option<String>,
    pub(crate) campus_programme: bool,
    pub(crate) university_domain: bool,
    pub(crate) human_programs: Vec<Uuid>,
    pub(crate) held_programs: Vec<Uuid>,
    /// The domain's shared request slot is closed until then (a pause after
    /// a wall, a 401/403 or a 429 to our agent).
    pub(crate) paused_until: Option<DateTime<Utc>>,
    pub(crate) consecutive_failures: i32,
    /// The outcome of the last attempt, for [`failure_streak`].
    pub(crate) last_outcome: Option<String>,
    /// On a reviewed seed URL ([`SEED_VERIFIED_SQL`]).
    pub(crate) seed_verified: bool,
    /// The door owning the candidate, if any.
    pub(crate) door_id: Option<Uuid>,
    /// The candidate's read scope ([`super::doors::job::scopes_for`]).
    #[sqlx(skip)]
    pub(crate) scope: bootstrap::Scope,
}

impl Source {
    fn kind(&self) -> &'static str {
        if !self.human_programs.is_empty() {
            "human_source"
        } else if !self.held_programs.is_empty() {
            "held_source"
        } else {
            "new_offer"
        }
    }

    fn brand_key(&self) -> String {
        self.brand_id.map_or_else(
            || {
                self.brand_name
                    .as_deref()
                    .unwrap_or("")
                    .trim()
                    .to_lowercase()
            },
            |id| id.to_string(),
        )
    }

    fn row(&self) -> CandidateRow {
        CandidateRow {
            id: self.candidate_id.to_string(),
            url: self.url.clone(),
            domain_id: Some(self.domain_id.to_string()),
            domain: self.domain.clone(),
            organization_name: self.organization_name.clone(),
            brand_id: self.brand_id.map(|id| id.to_string()),
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

#[derive(Debug, Default, Clone)]
pub struct IntakeSummary {
    pub sources: usize,
    pub outcomes: BTreeMap<String, usize>,
    pub published: usize,
    pub requests: u32,
}

/// Production entry point.
pub async fn run(
    pool: &PgPool,
    mode: Mode,
    gray_generic_tos: bool,
    http: &dyn PageHttp,
) -> Result<IntakeSummary> {
    run_with(
        pool,
        mode,
        gray_generic_tos,
        http,
        None,
        MAX_SOURCES_PER_RUN,
        None,
        bootstrap::precheck,
        publish::PAGE_ENDING_SCAN,
    )
    .await
}

/// `only` limits the pass to these candidates; `test_spacing` replaces the
/// 10 s floor and the shared slot; `check` is [`bootstrap::precheck`] outside
/// tests; `scan` is [`publish::PAGE_ENDING_SCAN`] outside tests.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_with(
    pool: &PgPool,
    mode: Mode,
    gray_generic_tos: bool,
    http: &dyn PageHttp,
    only: Option<Vec<Uuid>>,
    cap: i64,
    test_spacing: Option<std::time::Duration>,
    check: fn(&CandidateRow) -> Result<Url, &'static str>,
    scan: Option<EndingScan>,
) -> Result<IntakeSummary> {
    anyhow::ensure!(
        http.user_agent().starts_with("MazimiBenefitBot/"),
        "intake only sends the MazimiBenefitBot user agent"
    );
    if mode == Mode::Off {
        return Ok(IntakeSummary::default());
    }
    let mut sources = select_sources(pool, only, cap).await?;
    sources.sort_by_key(|s| s.domain_id);
    let today = super::health::kst_today();
    let optouts = LiveOptOuts(pool);
    let mut summary = IntakeSummary {
        sources: sources.len(),
        ..IntakeSummary::default()
    };
    let mut index = 0;
    while index < sources.len() {
        let domain_id = sources[index].domain_id;
        let end = sources[index..]
            .iter()
            .position(|s| s.domain_id != domain_id)
            .map_or(sources.len(), |offset| index + offset);
        let group = &sources[index..end];
        index = end;
        let first = &group[0];
        // A domain paused beyond a polite wait is decided without a request
        // and retried when the pause ends.
        let pause =
            chrono::Duration::from_std(policy::MAX_SLOT_WAIT).unwrap_or(Duration::minutes(10));
        if let Some(until) = first
            .paused_until
            .filter(|until| *until > Utc::now() + pause)
        {
            for source in group {
                let verdict = Verdict::refused(
                    "policy",
                    "domain_paused",
                    &format!("the domain is paused until {until}"),
                );
                let outcome = match settle(pool, mode, source, verdict, &mut summary).await {
                    Ok(outcome) => outcome,
                    Err(error) => {
                        warn!(event = "corporate_intake.record_failed", candidate_id = %source.candidate_id,
                              error = %crate::error_safety::persisted_error(&error), "intake decision could not be saved");
                        "record_failed".to_string()
                    }
                };
                *summary.outcomes.entry(outcome).or_default() += 1;
            }
            continue;
        }
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
        let rows: Vec<CandidateRow> = group.iter().map(Source::row).collect();
        let (report, decisions) = bootstrap::run_domain_with(
            &mut session,
            &rows,
            &settings,
            today,
            check,
            Some(&optouts),
        )
        .await;
        let tos = report.tos_signal;
        for source in group {
            let id = source.candidate_id.to_string();
            let decision = decisions
                .iter()
                .find(|d| d.candidate_ids.contains(&id))
                .cloned();
            let verdict = match decision {
                None => Verdict::refused("transient", "not_attempted", "no decision"),
                Some(decision) => {
                    judge(
                        &mut session,
                        decision,
                        tos,
                        &settings,
                        &report,
                        scan,
                        source,
                    )
                    .await
                }
            };
            let outcome = match settle(pool, mode, source, verdict, &mut summary).await {
                Ok(outcome) => outcome,
                Err(error) => {
                    warn!(event = "corporate_intake.record_failed", candidate_id = %source.candidate_id,
                          error = %crate::error_safety::persisted_error(&error), "intake decision could not be saved");
                    "record_failed".to_string()
                }
            };
            *summary.outcomes.entry(outcome).or_default() += 1;
        }
        summary.requests += session.requests();
    }
    Ok(summary)
}

/// A page that passed steps 1–4, kept in memory for the publish step.
#[derive(Debug, Clone)]
pub(crate) struct PassedPage {
    checked: Checked,
    page_hash: Option<String>,
    page_text: Option<String>,
    page_html: Option<String>,
}

/// What steps 1–4 concluded for one source.
#[derive(Debug, Clone)]
pub(crate) enum Verdict {
    Refused {
        gate: &'static str,
        outcome: String,
        detail: String,
    },
    Passed(Box<PassedPage>),
}

impl Verdict {
    fn refused(gate: &'static str, outcome: &str, detail: &str) -> Self {
        Self::Refused {
            gate,
            outcome: outcome.to_string(),
            detail: detail.chars().take(300).collect(),
        }
    }
}

async fn judge(
    session: &mut DomainSession<'_>,
    decision: Decision,
    tos: TosSignal,
    settings: &Settings,
    report: &bootstrap::DomainReport,
    scan: Option<EndingScan>,
    source: &Source,
) -> Verdict {
    if decision.outcome != "proposal" {
        let outcome = decision.outcome.clone();
        return Verdict::refused(gate_of(&outcome), &outcome, &outcome);
    }
    let attempt =
        reextract::gate_decision(session, decision, tos, settings, Some(report), scan).await;
    let Some(decision) = attempt.decision.filter(|_| attempt.outcome == "proposal") else {
        return Verdict::refused(gate_of(&attempt.outcome), &attempt.outcome, &attempt.detail);
    };
    let brand = source.brand_name.clone().unwrap_or_default();
    match check_offer(&decision, &brand, &source.brand_key()) {
        Ok(checked) => Verdict::Passed(Box::new(PassedPage {
            checked,
            page_hash: decision.page_hash.clone(),
            page_text: decision.page_text.clone(),
            page_html: decision.page_html.clone(),
        })),
        Err((gate, detail)) => {
            let outcome = detail.split(':').next().unwrap_or(gate).to_string();
            Verdict::refused(gate, &outcome, &detail)
        }
    }
}

/// A live monitor `m` is a held source's: `evidence_auto` on a corporate
/// programme held by an open hold, on a source the evidence job queued for
/// fresh reading (`corporate_evidence_targets.reextract_requested_at`, the
/// one queue of [`super::evidence::Exit::Reread`]: every class of held
/// fact the page could settle, a changed value, an ending or a date
/// included). Such an offer is re-derived from the page as it reads now,
/// through the whole chain, never confirmed from the old fact; the evidence
/// job withdraws the request once the offer confirms again.
const HELD_FOR_REREADING_SQL: &str = "m.verification_mode = 'evidence_auto'
    AND EXISTS (SELECT 1 FROM programs p WHERE p.id = m.program_id
                AND p.program_type = 'corporate_benefit' AND p.retirement_reason IS NULL)
    AND EXISTS (SELECT 1 FROM program_source_holds h
                WHERE h.program_id = m.program_id AND h.resolved_at IS NULL)
    AND EXISTS (SELECT 1 FROM corporate_evidence_targets t
                WHERE t.candidate_id = m.candidate_id AND t.reextract_requested_at IS NOT NULL)";

async fn select_sources(pool: &PgPool, only: Option<Vec<Uuid>>, cap: i64) -> Result<Vec<Source>> {
    let mut sources = sqlx::query_as::<_, Source>(&format!(
        "SELECT c.id AS candidate_id, c.url, d.id AS domain_id, d.domain, d.organization_name,
                d.compliance_status, d.policy_notes, d.allow_subdomains, d.verification_status,
                d.is_active AS domain_active, d.gray_generic_tos, d.gray_robots_missing,
                d.gray_crawl_delay_min, d.min_request_interval_ms, d.max_pages_per_day,
                COALESCE(b.id, dr.grantor_brand_id) AS brand_id,
                COALESCE(b.name, drb.name,
                    (SELECT p.provider_name FROM program_source_monitors m JOIN programs p ON p.id = m.program_id
                     WHERE m.candidate_id = c.id AND m.verification_state <> 'retired'
                       AND p.program_type = 'corporate_benefit' ORDER BY p.provider_name LIMIT 1)) AS brand_name,
                EXISTS (SELECT 1 FROM program_source_monitors m JOIN programs p ON p.id = m.program_id
                        WHERE m.candidate_id = c.id AND m.verification_state <> 'retired'
                          AND p.program_type = 'campus_benefit') AS campus_programme,
                EXISTS (SELECT 1 FROM universities u,
                               LATERAL (SELECT regexp_replace(lower(regexp_replace(u.official_url,
                                            '^[a-z]+://([^/:?#]+).*$', '\\1', 'i')), '^www\\.', '') AS host) uh,
                               LATERAL (SELECT regexp_replace(lower(d.domain), '^www\\.', '') AS host) dh
                        WHERE uh.host <> '' AND (dh.host = uh.host OR dh.host LIKE '%.' || uh.host)) AS university_domain,
                ARRAY(SELECT m.program_id FROM program_source_monitors m JOIN programs p ON p.id = m.program_id
                      WHERE m.candidate_id = c.id AND m.verification_state <> 'retired'
                        AND m.verification_mode = 'human' AND p.program_type = 'corporate_benefit'
                      ORDER BY m.program_id) AS human_programs,
                ARRAY(SELECT m.program_id FROM program_source_monitors m
                      WHERE m.candidate_id = c.id AND m.verification_state <> 'retired'
                        AND ({held})
                      ORDER BY m.program_id) AS held_programs,
                d.next_request_at AS paused_until,
                COALESCE(s.consecutive_failures, 0) AS consecutive_failures,
                s.last_outcome,
                {seed} AS seed_verified,
                dr.id AS door_id
         FROM benefit_source_candidates c
         JOIN benefit_source_domains d ON d.id = c.domain_id
         LEFT JOIN LATERAL (SELECT b.id, b.name FROM benefit_candidate_brands cb JOIN brands b ON b.id = cb.brand_id
                            WHERE cb.candidate_id = c.id ORDER BY b.name, b.id LIMIT 1) b ON true
         LEFT JOIN corporate_doors dr ON dr.candidate_id = c.id AND dr.kind <> 'hub_rows'
         LEFT JOIN corporate_doors pdr ON pdr.id = dr.parent_door_id
         LEFT JOIN brands drb ON drb.id = dr.grantor_brand_id
         LEFT JOIN corporate_intake_state s ON s.candidate_id = c.id
         WHERE ({eligible} OR (dr.id IS NOT NULL AND {policy}))
           -- A door's page only while its lane is a live door kind ($3).
           AND (dr.id IS NULL OR (COALESCE(pdr.kind, dr.kind) = ANY($3) AND dr.state = 'active'))
           AND NOT EXISTS (SELECT 1 FROM corporate_doors hub WHERE hub.candidate_id = c.id AND hub.kind = 'hub_rows')
           AND c.status <> 'rejected'
           AND ($2::uuid[] IS NULL OR c.id = ANY($2))
           AND (s.candidate_id IS NULL OR s.next_attempt_at <= now())
           AND (
               ((b.id IS NOT NULL OR dr.id IS NOT NULL) AND NOT EXISTS (
                    SELECT 1 FROM program_source_monitors m
                    WHERE m.candidate_id = c.id AND m.verification_state <> 'retired'))
               OR (EXISTS (SELECT 1 FROM program_source_monitors m JOIN programs p ON p.id = m.program_id
                           WHERE m.candidate_id = c.id AND m.verification_state <> 'retired'
                             AND m.verification_mode = 'human' AND p.program_type = 'corporate_benefit')
                   AND NOT EXISTS (SELECT 1 FROM program_source_monitors m
                                   WHERE m.candidate_id = c.id AND m.verification_state <> 'retired'
                                     AND m.verification_mode <> 'human')
                   AND NOT EXISTS (SELECT 1 FROM program_source_monitors m
                                   JOIN corporate_offer_facts f ON f.program_id = m.program_id AND f.status <> 'retired'
                                   WHERE m.candidate_id = c.id AND m.verification_state <> 'retired'))
               OR (EXISTS (SELECT 1 FROM program_source_monitors m
                           WHERE m.candidate_id = c.id AND m.verification_state <> 'retired')
                   AND NOT EXISTS (SELECT 1 FROM program_source_monitors m
                                   WHERE m.candidate_id = c.id AND m.verification_state <> 'retired'
                                     AND NOT ({held})))
           )
         ORDER BY s.next_attempt_at NULLS FIRST, c.discovered_at, c.id
         LIMIT $1",
        eligible = policy::ELIGIBLE_DOMAIN_SQL,
        policy = policy::DOMAIN_POLICY_SQL,
        held = HELD_FOR_REREADING_SQL,
        seed = SEED_VERIFIED_SQL,
    ))
    .bind(cap.clamp(0, 200))
    .bind(only)
    .bind(live_door_lanes())
    .fetch_all(pool)
    .await
    .context("failed to select intake sources")?;
    let ids: Vec<Uuid> = sources
        .iter()
        .filter(|s| s.door_id.is_some())
        .map(|s| s.candidate_id)
        .collect();
    if !ids.is_empty() {
        let scopes = super::doors::job::scopes_for(pool, &ids).await?;
        for source in sources.iter_mut() {
            if let Some(scope) = scopes.get(&source.candidate_id) {
                source.scope = scope.clone();
            }
        }
    }
    Ok(sources)
}

#[cfg(test)]
thread_local! {
    /// Tests only: the live door lanes, instead of the environment.
    pub(crate) static TEST_DOOR_LANES: std::cell::RefCell<Option<Vec<String>>> =
        const { std::cell::RefCell::new(None) };
}

/// The door lanes whose pages intake may read and publish now: the live
/// door kinds while `CORPORATE_DOORS_MODE` is live, none otherwise (an
/// invalid door configuration reads as none).
fn live_door_lanes() -> Vec<String> {
    #[cfg(test)]
    if let Some(lanes) = TEST_DOOR_LANES.with(|lanes| lanes.borrow().clone()) {
        return lanes;
    }
    match super::doors::DoorFlags::from_env() {
        Ok(flags) if flags.mode == super::doors::DoorsMode::Live => flags
            .live_kinds
            .iter()
            .filter(|k| **k != super::doors::Kind::HubRows)
            .map(|k| k.as_str().to_string())
            .collect(),
        _ => Vec::new(),
    }
}

/// A robots.txt that never answers (no reply, a timeout, a reset) is a
/// transient failure at first. With the backoff of [`retry_after`], the
/// [`ROBOTS_SILENT_AFTER`]th failure in a row comes more than a day after the
/// first, on another day; from then on the domain is in a policy state,
/// [`ROBOTS_SILENT`], retried weekly. Either way no page is requested: a
/// robots file we could not read never reads as allowed.
pub const ROBOTS_SILENT_AFTER: i32 = 4;
pub const ROBOTS_SILENT: &str = "robots_unreachable";

/// The consecutive failures of this outcome, counting this attempt: a
/// transient failure extends the streak only when it repeats the last
/// outcome (the robots policy state counts as robots failing), so a run of
/// one failure never brings another's escalation early (review L1). A
/// decision is no failure.
pub fn failure_streak(last: Option<&str>, outcome: &str, failures: i32, transient: bool) -> i32 {
    fn kind(outcome: &str) -> &str {
        if outcome == ROBOTS_SILENT {
            "robots_unavailable"
        } else {
            outcome
        }
    }
    if !transient {
        return 0;
    }
    if last.map(kind) == Some(kind(outcome)) {
        failures + 1
    } else {
        1
    }
}

/// The gate and outcome a refusal is recorded under after `failures`
/// consecutive failures (this one included).
pub fn escalate(gate: &'static str, outcome: String, failures: i32) -> (&'static str, String) {
    if outcome == "robots_unavailable" && failures >= ROBOTS_SILENT_AFTER {
        ("policy", ROBOTS_SILENT.to_string())
    } else {
        (gate, outcome)
    }
}

fn retry_after(gate: &str, failures: i32) -> Duration {
    if gate == "transient" {
        let doublings = (failures.max(1) - 1).clamp(0, 10) as u32;
        (TRANSIENT_RETRY * 2i32.pow(doublings)).min(REFRESH_AFTER)
    } else {
        REFRESH_AFTER
    }
}

/// Steps 5–7 and the audit row, in one transaction. Returns the outcome.
async fn settle(
    pool: &PgPool,
    mode: Mode,
    source: &Source,
    verdict: Verdict,
    summary: &mut IntakeSummary,
) -> Result<String> {
    let mut tx = pool.begin().await?;
    let (gate, outcome, detail, offer_id, program_id, next) = match verdict {
        Verdict::Refused {
            gate,
            outcome,
            detail,
        } => {
            let failures = failure_streak(
                source.last_outcome.as_deref(),
                &outcome,
                source.consecutive_failures,
                gate == "transient",
            );
            let (gate, outcome) = escalate(gate, outcome, failures);
            // A paused domain is read again when its pause ends.
            let next = match source.paused_until {
                Some(until) if outcome == "domain_paused" => {
                    (until - Utc::now()).max(Duration::minutes(1))
                }
                _ => retry_after(gate, failures),
            };
            (gate, outcome, detail, None, None, next)
        }
        Verdict::Passed(passed) => {
            let PassedPage {
                checked,
                page_hash,
                page_text,
                page_html,
            } = *passed;
            let step = observe_and_publish(
                &mut tx,
                mode,
                source,
                &checked,
                (
                    page_hash.as_deref(),
                    page_text.as_deref(),
                    page_html.as_deref(),
                ),
                summary.published < MAX_PUBLISH_PER_RUN,
            )
            .await?;
            if step.published {
                summary.published += 1;
            }
            (
                step.gate,
                step.outcome,
                step.detail,
                step.offer_id,
                step.program_id,
                step.next,
            )
        }
    };
    let failures = failure_streak(
        source.last_outcome.as_deref(),
        &outcome,
        source.consecutive_failures,
        gate == "transient" || outcome == ROBOTS_SILENT,
    );
    sqlx::query(
        "INSERT INTO corporate_intake_state (candidate_id, attempts, consecutive_failures, next_attempt_at,
                                             last_attempt_at, last_outcome, last_detail, updated_at)
         VALUES ($1, 1, $2, now() + make_interval(secs => $3), now(), $4, $5, now())
         ON CONFLICT (candidate_id) DO UPDATE SET
             attempts = corporate_intake_state.attempts + 1,
             consecutive_failures = EXCLUDED.consecutive_failures,
             next_attempt_at = EXCLUDED.next_attempt_at, last_attempt_at = now(),
             last_outcome = EXCLUDED.last_outcome, last_detail = EXCLUDED.last_detail, updated_at = now()",
    )
    .bind(source.candidate_id)
    .bind(failures.min(100))
    .bind(next.num_seconds() as f64)
    .bind(outcome.chars().take(60).collect::<String>())
    .bind(detail.chars().take(300).collect::<String>())
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO corporate_intake_decisions (mode, candidate_id, outcome, gate, detail, offer_id, program_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(if mode.publishes(source) { "live" } else { "shadow" })
    .bind(source.candidate_id)
    .bind(outcome.chars().take(80).collect::<String>())
    .bind(gate)
    .bind(detail.chars().take(300).collect::<String>())
    .bind(offer_id)
    .bind(program_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(outcome)
}

struct Step {
    gate: &'static str,
    outcome: String,
    detail: String,
    offer_id: Option<Uuid>,
    program_id: Option<Uuid>,
    next: Duration,
    published: bool,
}

impl Step {
    fn refused(gate: &'static str, outcome: &str, offer_id: Option<Uuid>) -> Self {
        Self {
            gate,
            outcome: outcome.to_string(),
            detail: outcome.to_string(),
            offer_id,
            program_id: None,
            next: REFRESH_AFTER,
            published: false,
        }
    }
}

/// The dedup gate for a new offer. `Some(reason)` refuses.
async fn duplicate(
    tx: &mut Transaction<'_, Postgres>,
    source: &Source,
    checked: &Checked,
) -> Result<Option<&'static str>> {
    let brand_key = source.brand_key();
    let identity: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM corporate_offer_identities WHERE brand_key = $1 AND offer_key = $2)",
    )
    .bind(&brand_key)
    .bind(&checked.offer_key)
    .fetch_one(&mut **tx)
    .await?;
    if identity {
        return Ok(Some("duplicate_offer"));
    }
    let host = Url::parse(&checked.page_url)
        .ok()
        .and_then(|u| {
            u.host_str()
                .map(|h| h.trim_start_matches("www.").to_lowercase())
        })
        .unwrap_or_default();
    let brand_host: bool = sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1 FROM programs p JOIN program_brands pb ON pb.program_id = p.id
             WHERE pb.brand_id = $1 AND p.retirement_reason IS NULL
               AND p.program_type = 'corporate_benefit'
               AND regexp_replace(lower(substring(p.official_url FROM '^https?://([^/?#:]+)')), '^www\\.', '') = $2)",
    )
    .bind(source.brand_id)
    .bind(&host)
    .fetch_one(&mut **tx)
    .await?;
    if brand_host {
        return Ok(Some("duplicate_brand_host"));
    }
    let urls = [url_key(&checked.page_url), url_key(&source.url)];
    let url_taken: bool = sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1 FROM programs p
             WHERE p.retirement_reason IS NULL AND p.official_url IS NOT NULL
               AND rtrim(regexp_replace(lower(split_part(p.official_url, '#', 1)), '^https?://(www\\.)?', ''), '/') = ANY($1))
         OR EXISTS (SELECT 1 FROM corporate_intake_offers o WHERE o.status = 'published' AND o.url_key = ANY($1))",
    )
    .bind(&urls[..])
    .fetch_one(&mut **tx)
    .await?;
    Ok(url_taken.then_some("duplicate_url"))
}

async fn observe_and_publish(
    tx: &mut Transaction<'_, Postgres>,
    mode: Mode,
    source: &Source,
    checked: &Checked,
    page: (Option<&str>, Option<&str>, Option<&str>),
    may_publish: bool,
) -> Result<Step> {
    let kind = source.kind();
    if kind == "new_offer" {
        if let Some(reason) = duplicate(tx, source, checked).await? {
            return Ok(Step::refused("dedup", reason, None));
        }
    }
    let offer_json = serde_json::to_value(&checked.offer)?;
    let display = json!({"title": checked.title, "summary": checked.summary,
                         "min_age": checked.min_age, "max_age": checked.max_age,
                         "valid_until": checked.valid_until, "quotes": checked.quotes});
    // A different offer read from the same source replaces the earlier one.
    sqlx::query(
        "UPDATE corporate_intake_offers SET status = 'superseded', decided_at = now()
         WHERE candidate_id = $1 AND status = 'observed' AND offer_key <> $2",
    )
    .bind(source.candidate_id)
    .bind(&checked.offer_key)
    .execute(&mut **tx)
    .await?;
    let (offer_id, first_seen_at): (Uuid, DateTime<Utc>) = sqlx::query_as(
        "INSERT INTO corporate_intake_offers (candidate_id, kind, offer_key, brand_key, url_key, page_url, offer, display)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
         ON CONFLICT (candidate_id, offer_key) WHERE status = 'observed' DO UPDATE SET
             sightings = corporate_intake_offers.sightings + 1, last_seen_at = now(),
             page_url = EXCLUDED.page_url, url_key = EXCLUDED.url_key,
             offer = EXCLUDED.offer, display = EXCLUDED.display
         RETURNING id, first_seen_at",
    )
    .bind(source.candidate_id)
    .bind(kind)
    .bind(&checked.offer_key)
    .bind(source.brand_key())
    .bind(url_key(&checked.page_url))
    .bind(&checked.page_url)
    .bind(&offer_json)
    .bind(&display)
    .fetch_one(&mut **tx)
    .await?;
    let stable_at = first_seen_at + STABLE_AFTER;
    let now: DateTime<Utc> = sqlx::query_scalar("SELECT now()")
        .fetch_one(&mut **tx)
        .await?;
    if now < stable_at {
        return Ok(Step {
            gate: "stability",
            outcome: "awaiting_stability".into(),
            detail: format!("first seen {first_seen_at}; stable from {stable_at}"),
            offer_id: Some(offer_id),
            program_id: None,
            next: (stable_at - now).max(Duration::minutes(1)),
            published: false,
        });
    }
    if !mode.publishes(source) {
        return Ok(Step {
            gate: "publish",
            outcome: "would_publish".into(),
            detail: format!("{kind}: {}", checked.title),
            offer_id: Some(offer_id),
            program_id: None,
            next: REFRESH_AFTER,
            published: false,
        });
    }
    if !may_publish {
        return Ok(Step {
            gate: "publish",
            outcome: "publish_cap".into(),
            detail: format!("at most {MAX_PUBLISH_PER_RUN} publishes per run"),
            offer_id: Some(offer_id),
            program_id: None,
            next: Duration::hours(1),
            published: false,
        });
    }
    // The recorded post-check of a seeded first publish: what it rests on,
    // read again on the page just fetched.
    let post_checked = if mode == Mode::Seeded {
        match post_check(checked, page.1.unwrap_or_default(), Utc::now()) {
            Ok(record) => Some(record),
            Err(reason) => {
                let mut step = Step::refused("publish", "post_check_failed", Some(offer_id));
                step.detail = reason;
                return Ok(step);
            }
        }
    } else {
        None
    };
    let mut savepoint = tx.begin().await?;
    let result = match kind {
        "new_offer" => create_programme(&mut savepoint, source, checked).await?,
        "human_source" => adopt_human_source(&mut savepoint, source, checked, page).await?,
        _ => republish(&mut savepoint, source, checked, page).await?,
    };
    match result {
        Ok(program_id) => {
            sqlx::query(
                "UPDATE corporate_intake_offers SET status = 'published', program_id = $2, decided_at = now()
                 WHERE id = $1",
            )
            .bind(offer_id)
            .bind(program_id)
            .execute(&mut *savepoint)
            .await?;
            savepoint.commit().await?;
            Ok(Step {
                gate: "publish",
                outcome: "published".into(),
                detail: match &post_checked {
                    Some(record) => format!("{kind}: {}; {record}", checked.title),
                    None => format!("{kind}: {}", checked.title),
                },
                offer_id: Some(offer_id),
                program_id: Some(program_id),
                next: REFRESH_AFTER,
                published: true,
            })
        }
        Err(reason) => {
            savepoint.rollback().await?;
            let mut step = Step::refused("publish", "publish_refused", Some(offer_id));
            step.detail = reason;
            Ok(step)
        }
    }
}

/// The post-check a seeded publish records: every quote verbatim on the
/// page just read (the evidence verifier's own reading), an audience, a
/// value the template states, and a window that is open or still ahead.
/// `Err` names the first part that fails. Pure.
pub fn post_check(
    checked: &Checked,
    page_text: &str,
    now: DateTime<Utc>,
) -> Result<String, String> {
    let page = PageEvidence::from_visible_text(page_text);
    let ages = Ages {
        min: checked.min_age,
        max: checked.max_age,
    };
    let verbatim = checked
        .quotes
        .iter()
        .filter(|q| {
            matches!(
                evidence::evaluate_fact(&page, q, None, ages, true),
                FactOutcome::Confirmed { .. }
            )
        })
        .count();
    if checked.quotes.is_empty() || verbatim < checked.quotes.len() {
        return Err(format!(
            "quote: {verbatim}/{} verbatim on the page",
            checked.quotes.len()
        ));
    }
    if checked.offer.audience.is_empty() {
        return Err("audience: none read".into());
    }
    let values: Vec<String> = checked
        .offer
        .facts
        .iter()
        .filter(|f| f.kind != "age_band")
        .map(|f| format!("{}={}", f.kind, f.value))
        .collect();
    if values.is_empty() {
        return Err("value: none read".into());
    }
    let window = match checked.valid_until {
        Some(until) if until <= now => return Err(format!("window: ended {until}")),
        Some(until) => format!("until {}", until.date_naive()),
        None => "open".to_string(),
    };
    Ok(format!(
        "post-check: quotes {verbatim}/{verbatim} verbatim; audience {}; value {}; window {window}",
        checked.offer.audience.join("+"),
        values.join(",")
    ))
}

/// Same key as the evidence job writes (`evidence::quote_key`).
fn quote_key(program_id: Uuid, quote: &str) -> String {
    Sha256::digest(format!("{program_id}:{}", evidence::compact(quote)).as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A new offer: a hidden programme the evidence job shows once it confirms
/// the quotes on the live page. The inner `Err` refuses (rolled back).
async fn create_programme(
    tx: &mut Transaction<'_, Postgres>,
    source: &Source,
    checked: &Checked,
) -> Result<std::result::Result<Uuid, String>> {
    let Some(brand_id) = source.brand_id else {
        return Ok(Err("no_brand".into()));
    };
    let brand = source.brand_name.clone().unwrap_or_default();
    // An issuer the offer names pays for it (review H6): it is the sponsor,
    // provider and company (H3), the door brand is where it is redeemed,
    // and the condition is the issuer's holder template plus the line that
    // names it, verbatim (C2).
    let issuer = checked.offer.issuer.as_ref();
    let issuer_holder = issuer.and_then(|i| mazimi_core::holder::Holder::parse(&i.holder));
    if issuer.is_some() && issuer_holder.is_none() {
        return Ok(Err("issuer_without_holder_key".into()));
    }
    let provider = issuer.map_or(brand.clone(), |i| i.name.clone());
    let condition = issuer
        .zip(issuer_holder)
        .map(|(i, holder)| super::doors::lifecycle::condition(&holder, &i.name, Some(&i.line)));
    let category_text = format!(
        "{} {} {}",
        checked.title,
        checked.summary,
        checked.offer.audience.join(" ")
    );
    let mut payload = json!({
        "display": "verbatim",
        "audience_scope": if checked.offer.scope_brand.is_some() { "general" } else { "higher_education" },
        "category_text": category_text,
        "intake": {"offer_key": checked.offer_key, "candidate_id": source.candidate_id},
    });
    if let Some(categorize) = CATEGORIZE {
        payload["corporate_category"] = json!(categorize(&category_text));
    }
    let raw = json!({"curation": {"status": "automated", "source": "corporate_intake",
                                  "official_url": checked.page_url}});
    let Some(program_id): Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO programs (program_type, source_type, source_id, title, summary, provider_name, company_name,
                               official_url, program_status, provider_kind, normalized_payload, raw_payload,
                               min_age, max_age, benefit_valid_until, regions, is_active, source_health,
                               source_verified_at, source_review_due_at, last_synced_at, eligibility_condition)
         VALUES ('corporate_benefit', 'corporate_official', $1, $2, $3, $4, $4, $5, 'review_required', 'corporate',
                 $6, $7, $8, $9, $10, ARRAY[]::text[], false, 'review_due', now(), now(), now(), $11)
         ON CONFLICT (source_type, source_id) DO NOTHING
         RETURNING id",
    )
    .bind(format!("intake:{}", source.candidate_id))
    .bind(&checked.title)
    .bind(&checked.summary)
    .bind(&provider)
    .bind(&checked.page_url)
    .bind(&payload)
    .bind(&raw)
    .bind(checked.min_age)
    .bind(checked.max_age)
    .bind(checked.valid_until)
    .bind(&condition)
    .fetch_optional(&mut **tx)
    .await?
    else {
        return Ok(Err("programme_exists".into()));
    };
    let identity = sqlx::query(
        "INSERT INTO corporate_offer_identities (brand_key, offer_key, program_id) VALUES ($1, $2, $3)
         ON CONFLICT DO NOTHING",
    )
    .bind(source.brand_key())
    .bind(&checked.offer_key)
    .bind(program_id)
    .execute(&mut **tx)
    .await?
    .rows_affected();
    if identity != 1 {
        return Ok(Err("duplicate_offer".into()));
    }
    // A brand-scope offer (a door's page) claims the one identity of a
    // shown corporate benefit, shared with the hub doors (code review H-3).
    if checked.offer.scope_brand.is_some() {
        let kind = checked
            .offer
            .facts
            .iter()
            .find(|f| matches!(f.kind, "discount_percent" | "krw"))
            .map(|f| f.kind);
        let Some(kind) = kind else {
            return Ok(Err("no_displayed_value".into()));
        };
        let issuer_id = issuer.map_or(brand_id, |i| i.brand_id);
        if !super::doors::lifecycle::claim_identity(tx, issuer_id, brand_id, kind, program_id)
            .await?
        {
            return Ok(Err("duplicate_identity".into()));
        }
    }
    match issuer {
        Some(issuer) => {
            sqlx::query(
                "INSERT INTO program_brands (program_id, brand_id, relationship_type, is_primary)
                 VALUES ($1, $2, 'sponsor', true), ($1, $3, 'redeemable_at', false) ON CONFLICT DO NOTHING",
            )
            .bind(program_id)
            .bind(issuer.brand_id)
            .bind(brand_id)
            .execute(&mut **tx)
            .await?;
        }
        None => {
            sqlx::query("INSERT INTO program_brands (program_id, brand_id, is_primary) VALUES ($1, $2, true) ON CONFLICT DO NOTHING")
                .bind(program_id)
                .bind(brand_id)
                .execute(&mut **tx)
                .await?;
        }
    }
    sqlx::query(
        "INSERT INTO program_source_monitors (program_id, candidate_id, verification_state, verified_at,
                                              review_due_at, verification_mode)
         VALUES ($1, $2, 'review_due', now(), now(), 'evidence_auto')",
    )
    .bind(program_id)
    .bind(source.candidate_id)
    .execute(&mut **tx)
    .await?;
    for quote in &checked.quotes {
        let (fact_type, value) = evidence::read_quote(quote);
        sqlx::query(
            "INSERT INTO corporate_offer_facts (program_id, fact_type, value, quote, quote_key, channel,
                 evidence_source, pass_b, status, adopt_before, anchors)
             VALUES ($1, $2, $3, $4, $5, 'page', 'intake', $6, 'published',
                     now() + make_interval(days => $7), '{}'::jsonb)
             ON CONFLICT (program_id, quote_key) DO NOTHING",
        )
        .bind(program_id)
        .bind(fact_type)
        .bind(&value)
        .bind(quote)
        .bind(quote_key(program_id, quote))
        .bind(json!({"source": "intake", "reading": value}))
        .bind(ADOPT_WITHIN_DAYS as i32)
        .execute(&mut **tx)
        .await?;
    }
    // Scheduled for the evidence job now, as it schedules every
    // `evidence_auto` source (otherwise health counts a missed check).
    sqlx::query("INSERT INTO corporate_evidence_targets (candidate_id) VALUES ($1) ON CONFLICT (candidate_id) DO NOTHING")
        .bind(source.candidate_id)
        .execute(&mut **tx)
        .await?;
    // Hidden until the evidence job confirms; its restore lifts this hold.
    sqlx::query(
        "INSERT INTO program_source_holds (program_id, candidate_id, hold_reason, previous_is_active, previous_program_status)
         VALUES ($1, $2, 'evidence_failed', true, 'always_open')",
    )
    .bind(program_id)
    .bind(source.candidate_id)
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "UPDATE benefit_source_candidates SET status = 'approved', program_id = $2, updated_at = now()
         WHERE id = $1",
    )
    .bind(source.candidate_id)
    .bind(program_id)
    .execute(&mut **tx)
    .await?;
    Ok(Ok(program_id))
}

/// A human source: its monitors move to `evidence_auto` and the fresh offer
/// goes through the fresh-extraction publish (every public field against the
/// page, side tables, facts replaced, restore). Anything short of a restore
/// is rolled back by the caller and the source stays with its person.
async fn adopt_human_source(
    tx: &mut Transaction<'_, Postgres>,
    source: &Source,
    checked: &Checked,
    page: (Option<&str>, Option<&str>, Option<&str>),
) -> Result<std::result::Result<Uuid, String>> {
    sqlx::query(
        "UPDATE program_source_monitors SET verification_mode = 'evidence_auto', updated_at = now()
         WHERE candidate_id = $1 AND verification_state <> 'retired' AND verification_mode = 'human'",
    )
    .bind(source.candidate_id)
    .execute(&mut **tx)
    .await?;
    republish(tx, source, checked, page).await
}

/// The fresh-extraction publish of a source's one programme
/// ([`publish::publish`]): every public field against the page, the facts
/// replaced, and a restore. Anything short of a restore is an `Err`, which
/// the caller rolls back.
async fn republish(
    tx: &mut Transaction<'_, Postgres>,
    source: &Source,
    checked: &Checked,
    (page_hash, page_text, page_html): (Option<&str>, Option<&str>, Option<&str>),
) -> Result<std::result::Result<Uuid, String>> {
    sqlx::query("INSERT INTO corporate_evidence_targets (candidate_id) VALUES ($1) ON CONFLICT (candidate_id) DO NOTHING")
        .bind(source.candidate_id)
        .execute(&mut **tx)
        .await?;
    let mut inner = tx.begin().await?;
    let published = publish::publish(
        &mut inner,
        source.candidate_id,
        &checked.offer,
        page_hash,
        page_text,
        page_html,
        &checked.page_url,
    )
    .await?;
    inner.commit().await?;
    Ok(match published {
        Published::Restored { program_id, .. } => Ok(program_id),
        other => Err(other.to_json()["reason"]
            .as_str()
            .unwrap_or("refused")
            .chars()
            .take(300)
            .collect()),
    })
}

// ── projection (no database) ──────────────────────────────────────────────

/// One live programme or published offer the dedup gate compares with.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct Existing {
    pub brand_id: Option<String>,
    pub official_url: String,
}

/// One projected decision.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Projected {
    pub candidate_id: String,
    pub url: String,
    pub brand: Option<String>,
    pub gate: &'static str,
    pub outcome: String,
    pub detail: String,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub facts: Vec<Value>,
    pub audience: Vec<&'static str>,
}

/// Steps 1–5 over exported candidates without a database: what a shadow
/// run would decide on the first sighting (stability needs a second one a
/// day later). Opt-outs are not consulted (the caller exports none).
pub async fn project(
    http: &dyn PageHttp,
    rows: Vec<CandidateRow>,
    existing: &[Existing],
    gray_generic_tos: bool,
    today: NaiveDate,
    concurrency: usize,
) -> Result<Vec<Projected>> {
    use futures_util::stream::{self, StreamExt};
    anyhow::ensure!(
        http.user_agent().starts_with("MazimiBenefitBot/"),
        "the projection only sends the MazimiBenefitBot user agent"
    );
    let mut by_domain: BTreeMap<String, Vec<CandidateRow>> = BTreeMap::new();
    for row in rows {
        by_domain
            .entry(row.domain.trim().to_ascii_lowercase())
            .or_default()
            .push(row);
    }
    let settings = Settings {
        gray_generic_tos,
        gray_robots_missing: true,
        concurrency: 1,
        test_spacing: None,
    };
    let settings = &settings;
    let jobs = by_domain.into_values().map(|rows| async move {
        let mut session = DomainSession::new(
            http,
            None,
            None,
            true,
            0,
            bootstrap::MAX_REQUESTS_PER_DOMAIN,
        );
        let (report, decisions) = bootstrap::run_domain_with(
            &mut session,
            &rows,
            settings,
            today,
            bootstrap::precheck,
            None,
        )
        .await;
        let mut out = Vec::new();
        for decision in decisions {
            let row = rows
                .iter()
                .find(|r| decision.candidate_ids.contains(&r.id))
                .cloned();
            let Some(row) = row else { continue };
            let brand = row.brand_name.clone().unwrap_or_default();
            let brand_key = row.brand_id.clone().unwrap_or_else(|| brand.to_lowercase());
            let base = Projected {
                candidate_id: decision.candidate_ids.join(","),
                url: decision.url.clone(),
                brand: row.brand_name.clone(),
                gate: "policy",
                outcome: String::new(),
                detail: String::new(),
                title: None,
                summary: None,
                facts: Vec::new(),
                audience: Vec::new(),
            };
            if decision.outcome != "proposal" {
                out.push(Projected {
                    gate: gate_of(&decision.outcome),
                    outcome: decision.outcome.clone(),
                    ..base
                });
                continue;
            }
            let attempt = reextract::gate_decision(
                &mut session,
                decision,
                report.tos_signal,
                settings,
                Some(&report),
                publish::PAGE_ENDING_SCAN,
            )
            .await;
            let Some(decision) = attempt.decision.filter(|_| attempt.outcome == "proposal") else {
                out.push(Projected {
                    gate: gate_of(&attempt.outcome),
                    outcome: attempt.outcome.clone(),
                    detail: attempt.detail.clone(),
                    ..base
                });
                continue;
            };
            let facts: Vec<Value> = decision
                .offer
                .iter()
                .flat_map(|o| o.facts.iter())
                .map(|f| json!({"kind": f.kind, "value": f.value, "quote": f.quote}))
                .collect();
            let audience = decision
                .offer
                .as_ref()
                .map(|o| o.audience.clone())
                .unwrap_or_default();
            match check_offer(&decision, &brand, &brand_key) {
                Err((gate, detail)) => out.push(Projected {
                    gate,
                    outcome: detail.split(':').next().unwrap_or(gate).to_string(),
                    detail,
                    facts,
                    audience,
                    ..base
                }),
                Ok(checked) => {
                    let page = url_key(&checked.page_url);
                    let host = page.split(['/', '?']).next().unwrap_or("").to_string();
                    let dup = existing.iter().find_map(|e| {
                        let key = url_key(&e.official_url);
                        if key == page || key == url_key(&row.url) {
                            Some("duplicate_url")
                        } else if e.brand_id.is_some()
                            && e.brand_id == row.brand_id
                            && key.split(['/', '?']).next() == Some(host.as_str())
                        {
                            Some("duplicate_brand_host")
                        } else {
                            None
                        }
                    });
                    let (gate, outcome) = match dup {
                        Some(reason) => ("dedup", reason.to_string()),
                        None => ("stability", "awaiting_stability".to_string()),
                    };
                    out.push(Projected {
                        gate,
                        outcome,
                        detail: checked.offer_key.clone(),
                        title: Some(checked.title),
                        summary: Some(checked.summary),
                        facts,
                        audience,
                        ..base
                    });
                }
            }
        }
        out
    });
    let results: Vec<Vec<Projected>> = stream::iter(jobs)
        .buffer_unordered(concurrency.clamp(1, 16))
        .collect()
        .await;
    Ok(results.into_iter().flatten().collect())
}

// ── link discovery: offer pages on allowed brands ─────────────────────────

/// Links followed from a seed page (the seed is depth 0).
pub const DISCOVERY_MAX_DEPTH: usize = 2;
/// Page requests per domain per discovery pass (robots not counted).
pub const DISCOVERY_MAX_FETCHES: u32 = 12;
/// A domain is explored again this long after its last pass.
pub const DISCOVERY_EVERY: Duration = Duration::days(7);
/// Domains per discovery pass.
pub const DISCOVERY_DOMAINS_PER_RUN: i64 = 5;

/// A page found by link discovery whose own content names a student or
/// youth offer.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Found {
    pub url: String,
    /// The title or heading that names the offer.
    pub signal: String,
    pub depth: usize,
    pub parent: String,
}

/// The page's title or a heading (h1–h3) that names a student or youth
/// audience as the target of a benefit. Page content only: no URL words.
pub fn offer_page_signal(html: &str) -> Option<String> {
    use scraper::{Html, Selector};
    let document = Html::parse_document(&evidence::strip_comments(html));
    let selector = Selector::parse("title, h1, h2, h3").ok()?;
    document.select(&selector).find_map(|element| {
        // A heading inside navigation or hidden text names no offer here.
        let outside = element.ancestors().all(|a| {
            a.value().as_element().is_none_or(|el| {
                !evidence::element_hidden(el)
                    && !matches!(el.name(), "nav" | "footer")
                    && !matches!(el.attr("role"), Some("navigation" | "contentinfo"))
            })
        }) || element.value().name() == "title";
        let text = element
            .text()
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        static FOR_AUDIENCE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
            regex::Regex::new(r"(?i)\bfor\s+(?:students?|educators?|teachers?|education)\b")
                .expect("valid audience pattern")
        });
        let named = (bootstrap::find_target(&text).is_some() || FOR_AUDIENCE.is_match(&text))
            && bootstrap::audience_codes(&text)
                .iter()
                .any(|code| STUDENT_YOUTH.contains(code));
        (outside && named && !text.is_empty()).then(|| text.chars().take(200).collect())
    })
}

/// Same-host, same-scheme links of a page, fragment dropped, account, checkout
/// and file links left out, each once; links whose own text names a
/// student or youth audience first.
pub fn same_host_links(html: &str, page: &Url) -> Vec<Url> {
    use scraper::{Html, Selector};
    const FILES: &[&str] = &[
        ".pdf", ".jpg", ".jpeg", ".png", ".gif", ".zip", ".dmg", ".exe", ".mp4", ".svg", ".webp",
    ];
    let document = Html::parse_document(&evidence::strip_comments(html));
    let Ok(selector) = Selector::parse("a[href]") else {
        return Vec::new();
    };
    let mut first: Vec<Url> = Vec::new();
    let mut rest: Vec<Url> = Vec::new();
    for anchor in document.select(&selector) {
        let Some(mut url) = anchor
            .value()
            .attr("href")
            .and_then(|href| page.join(href.trim()).ok())
        else {
            continue;
        };
        url.set_fragment(None);
        let path = url.path().to_ascii_lowercase();
        if url.scheme() != page.scheme()
            || url.host_str() != page.host_str()
            || mazimi_core::source_policy::is_interactive_or_purchase_url(&url)
            || FILES.iter().any(|ext| path.ends_with(ext))
            || url == *page
            || first.contains(&url)
            || rest.contains(&url)
        {
            continue;
        }
        let text = anchor.text().collect::<String>();
        let offer_words = bootstrap::audience_codes(&text)
            .iter()
            .any(|code| STUDENT_YOUTH.contains(code));
        if offer_words {
            first.push(url);
        } else {
            rest.push(url);
        }
    }
    first.extend(rest);
    first.truncate(60);
    first
}

/// Breadth-first from the seeds: every fetch through the polite session and
/// the shared redirect resolver (robots per path, opt-out before every
/// request), at most [`DISCOVERY_MAX_FETCHES`] pages and
/// [`DISCOVERY_MAX_DEPTH`] links deep. A page past the seeds whose content
/// names an offer, and that is not `known`, is found.
pub async fn discover_from(
    session: &mut DomainSession<'_>,
    seeds: &[Url],
    known: &std::collections::HashSet<String>,
    gray_robots_missing: bool,
    opted_out: Option<&dyn policy::OptOutCheck>,
) -> Vec<Found> {
    discover_from_excluding(
        session,
        seeds,
        known,
        &Default::default(),
        gray_robots_missing,
        opted_out,
    )
    .await
}

/// [`discover_from`] that never requests a URL in `never` ([`url_key`]s:
/// an issuer hub's list page).
pub async fn discover_from_excluding(
    session: &mut DomainSession<'_>,
    seeds: &[Url],
    known: &std::collections::HashSet<String>,
    never: &std::collections::HashSet<String>,
    gray_robots_missing: bool,
    opted_out: Option<&dyn policy::OptOutCheck>,
) -> Vec<Found> {
    use std::collections::{HashSet, VecDeque};
    let mut queue: VecDeque<(Url, usize, String)> = seeds
        .iter()
        .filter(|u| !never.contains(&url_key(u.as_str())))
        .map(|u| (u.clone(), 0, String::new()))
        .collect();
    let mut seen: HashSet<String> = seeds
        .iter()
        .map(|u| url_key(u.as_str()))
        .chain(never.iter().cloned())
        .collect();
    let mut found = Vec::new();
    let mut fetches = 0u32;
    while let Some((url, depth, parent)) = queue.pop_front() {
        if fetches >= DISCOVERY_MAX_FETCHES || session.stopped().is_some() {
            break;
        }
        if let Some(check) = opted_out {
            if check.covers(&url).await {
                continue;
            }
        }
        // robots for this path first: a disallowed link costs no page request.
        let robots = session.robots(&url).await;
        if robots.allows(&policy::robots_target(&url), gray_robots_missing) != Some(true) {
            continue;
        }
        fetches += 1;
        let resolved =
            policy::fetch_resolving_same_host(session, &url, gray_robots_missing, opted_out).await;
        let (policy::Fetched::Reply(reply), policy::PageState::Html { .. }) =
            (&resolved.fetched, &resolved.state)
        else {
            continue;
        };
        let Ok(html) = crate::sources::benefit_pages::decode_page(&reply.body, &reply.content_type)
        else {
            continue;
        };
        let final_key = url_key(resolved.url.as_str());
        if depth > 0 && !known.contains(&final_key) {
            if let Some(signal) = offer_page_signal(&html) {
                found.push(Found {
                    url: resolved.url.to_string(),
                    signal,
                    depth,
                    parent: parent.clone(),
                });
            }
        }
        if depth < DISCOVERY_MAX_DEPTH {
            for link in same_host_links(&html, &resolved.url) {
                let key = url_key(link.as_str());
                if known.contains(&key) || !seen.insert(key) {
                    continue;
                }
                queue.push_back((link, depth + 1, resolved.url.to_string()));
            }
        }
    }
    found
}

#[derive(Debug, Default, Clone)]
pub struct DiscoverySummary {
    pub domains: usize,
    pub found: usize,
    pub requests: u32,
}

#[derive(Debug, FromRow)]
struct DiscoveryDomain {
    domain_id: Uuid,
    domain: String,
    gray_robots_missing: bool,
    gray_crawl_delay_min: bool,
    min_request_interval_ms: i32,
    brand_id: Option<Uuid>,
    seeds: Vec<String>,
    known: Vec<String>,
    hub_urls: Vec<String>,
}

/// Production entry point of link discovery.
pub async fn discover(pool: &PgPool, mode: Mode, http: &dyn PageHttp) -> Result<DiscoverySummary> {
    discover_with(pool, mode, http, None, None, bootstrap::precheck).await
}

/// Link discovery over due allowed brand domains: from each domain's
/// homepage and up to three of its candidates, follow same-host links to
/// pages whose own content names a student or youth offer
/// ([`discover_from`]) and queue them as `review` candidates of the
/// domain's brand, the existing intake path; one audit row each. A domain
/// is explored again after [`DISCOVERY_EVERY`]. `check` is
/// [`bootstrap::precheck`] outside tests (a seed must pass it).
pub(crate) async fn discover_with(
    pool: &PgPool,
    mode: Mode,
    http: &dyn PageHttp,
    only: Option<Vec<Uuid>>,
    test_spacing: Option<std::time::Duration>,
    check: fn(&CandidateRow) -> Result<Url, &'static str>,
) -> Result<DiscoverySummary> {
    anyhow::ensure!(
        http.user_agent().starts_with("MazimiBenefitBot/"),
        "discovery only sends the MazimiBenefitBot user agent"
    );
    let mut summary = DiscoverySummary::default();
    if mode == Mode::Off {
        return Ok(summary);
    }
    let domains = sqlx::query_as::<_, DiscoveryDomain>(&format!(
        "SELECT d.id AS domain_id, d.domain, d.gray_robots_missing, d.gray_crawl_delay_min,
                d.min_request_interval_ms,
                (SELECT cb.brand_id FROM benefit_candidate_brands cb JOIN benefit_source_candidates c2 ON c2.id = cb.candidate_id
                 WHERE c2.domain_id = d.id GROUP BY cb.brand_id ORDER BY count(*) DESC, cb.brand_id LIMIT 1) AS brand_id,
                ARRAY(SELECT c3.url FROM benefit_source_candidates c3
                      WHERE c3.domain_id = d.id AND c3.status <> 'rejected' AND NOT {hub}
                      ORDER BY c3.discovered_at, c3.id LIMIT 3) AS seeds,
                ARRAY(SELECT c4.url FROM benefit_source_candidates c4 WHERE c4.domain_id = d.id) AS known,
                ARRAY(SELECT hd.entry_url FROM corporate_doors hd
                      WHERE hd.domain_id = d.id AND hd.kind = 'hub_rows') AS hub_urls
         FROM benefit_source_domains d
         LEFT JOIN corporate_discovery_state s ON s.domain_id = d.id
         WHERE {eligible}
           AND ($1::uuid[] IS NULL OR d.id = ANY($1))
           AND (s.domain_id IS NULL OR s.next_run_at <= now())
           AND EXISTS (SELECT 1 FROM benefit_source_candidates c JOIN benefit_candidate_brands cb ON cb.candidate_id = c.id
                       WHERE c.domain_id = d.id)
         ORDER BY s.next_run_at NULLS FIRST, d.domain
         LIMIT $2",
        eligible = policy::ELIGIBLE_DOMAIN_SQL,
        hub = policy::hub_owned("c3"),
    ))
    .bind(only)
    .bind(DISCOVERY_DOMAINS_PER_RUN)
    .fetch_all(pool)
    .await
    .context("failed to select discovery domains")?;
    let optouts = LiveOptOuts(pool);
    for domain in domains {
        let Some(brand_id) = domain.brand_id else {
            continue;
        };
        if bootstrap::campus_host(&domain.domain) {
            continue;
        }
        summary.domains += 1;
        let seed_row = |url: &str| CandidateRow {
            id: String::new(),
            url: url.to_string(),
            domain_id: Some(domain.domain_id.to_string()),
            domain: domain.domain.clone(),
            organization_name: None,
            brand_id: Some(brand_id.to_string()),
            brand_name: Some("brand".into()),
            compliance_status: Some("pending".into()),
            policy_notes: None,
            allow_subdomains: Some(false),
            verification_status: Some("verified".into()),
            domain_active: Some(true),
            page_title: None,
            campus_programme: Some(false),
            university_domain: Some(false),
            scope: Default::default(),
        };
        let mut seeds: Vec<Url> = Vec::new();
        for raw in std::iter::once(format!("https://{}/", domain.domain))
            .chain(domain.seeds.iter().cloned())
        {
            if let Ok(url) = check(&seed_row(&raw)) {
                if !seeds.contains(&url) {
                    seeds.push(url);
                }
            }
        }
        let known: std::collections::HashSet<String> =
            domain.known.iter().map(|u| url_key(u)).collect();
        let shared_slot = test_spacing.is_none();
        let mut session = DomainSession::new(
            http,
            shared_slot.then_some(pool),
            shared_slot.then_some(domain.domain_id),
            domain.gray_crawl_delay_min,
            domain.min_request_interval_ms as i64,
            DISCOVERY_MAX_FETCHES * 2 + 4,
        );
        if let Some(spacing) = test_spacing {
            session = session.with_test_spacing(spacing);
        }
        // An issuer hub's list page is the door job's alone: never fetched here.
        let never: std::collections::HashSet<String> =
            domain.hub_urls.iter().map(|u| url_key(u)).collect();
        let found = discover_from_excluding(
            &mut session,
            &seeds,
            &known,
            &never,
            domain.gray_robots_missing,
            Some(&optouts),
        )
        .await;
        summary.requests += session.requests();
        let mut tx = pool.begin().await?;
        let mut queued = 0;
        for page in &found {
            let canonical = mazimi_core::source_policy::canonicalize_public_https_url(&page.url)
                .map(|c| c.canonical_url)
                .unwrap_or_else(|_| page.url.clone());
            let Some(candidate_id): Option<Uuid> = sqlx::query_scalar(
                "INSERT INTO benefit_source_candidates (domain_id, url, canonical_url, discovered_via, page_title,
                                                        status, candidate_kind, discovery_depth)
                 VALUES ($1, $2, $3, 'official_page', $4, 'review', 'detail', $5)
                 ON CONFLICT (canonical_url) DO NOTHING
                 RETURNING id",
            )
            .bind(domain.domain_id)
            .bind(&page.url)
            .bind(&canonical)
            .bind(page.signal.chars().take(200).collect::<String>())
            .bind(page.depth as i32)
            .fetch_optional(&mut *tx)
            .await?
            else {
                continue;
            };
            sqlx::query("INSERT INTO benefit_candidate_brands (candidate_id, brand_id) VALUES ($1, $2) ON CONFLICT DO NOTHING")
                .bind(candidate_id)
                .bind(brand_id)
                .execute(&mut *tx)
                .await?;
            sqlx::query(
                "INSERT INTO corporate_intake_decisions (mode, candidate_id, outcome, gate, detail)
                 VALUES ($1, $2, 'discovered', 'discovery', $3)",
            )
            .bind(if mode == Mode::Live { "live" } else { "shadow" })
            .bind(candidate_id)
            .bind(
                format!("depth {} from {}: {}", page.depth, page.parent, page.signal)
                    .chars()
                    .take(300)
                    .collect::<String>(),
            )
            .execute(&mut *tx)
            .await?;
            queued += 1;
        }
        sqlx::query(
            "INSERT INTO corporate_discovery_state (domain_id, last_run_at, next_run_at, pages_fetched, found, last_outcome, updated_at)
             VALUES ($1, now(), now() + make_interval(secs => $2), $3, $4, $5, now())
             ON CONFLICT (domain_id) DO UPDATE SET last_run_at = now(), next_run_at = EXCLUDED.next_run_at,
                 pages_fetched = EXCLUDED.pages_fetched, found = EXCLUDED.found,
                 last_outcome = EXCLUDED.last_outcome, updated_at = now()",
        )
        .bind(domain.domain_id)
        .bind(DISCOVERY_EVERY.num_seconds() as f64)
        .bind(session.requests() as i32)
        .bind(queued)
        .bind(session.stopped().unwrap_or("completed"))
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        summary.found += queued as usize;
    }
    Ok(summary)
}

/// Link discovery without a database, then [`project`] on the pages found:
/// for each exported domain, its homepage and up to three exported
/// candidates are the seeds and every exported URL is known.
pub async fn project_discovery(
    http: &dyn PageHttp,
    rows: Vec<CandidateRow>,
    existing: &[Existing],
    gray_generic_tos: bool,
    today: NaiveDate,
    concurrency: usize,
) -> Result<(Vec<(String, Found)>, Vec<Projected>)> {
    use futures_util::stream::{self, StreamExt};
    let mut by_domain: BTreeMap<String, Vec<CandidateRow>> = BTreeMap::new();
    for row in rows {
        by_domain
            .entry(row.domain.trim().to_ascii_lowercase())
            .or_default()
            .push(row);
    }
    let jobs = by_domain.into_iter().map(|(domain, rows)| async move {
        if bootstrap::campus_host(&domain) {
            return Vec::new();
        }
        let Some(template) = rows
            .iter()
            .find(|r| bootstrap::precheck(r).is_ok())
            .cloned()
        else {
            return Vec::new();
        };
        let mut seeds: Vec<Url> = Vec::new();
        let homepage = CandidateRow {
            url: format!("https://{domain}/"),
            ..template.clone()
        };
        for row in std::iter::once(&homepage).chain(rows.iter().take(3)) {
            if let Ok(url) = bootstrap::precheck(row) {
                if !seeds.contains(&url) {
                    seeds.push(url);
                }
            }
        }
        let known: std::collections::HashSet<String> =
            rows.iter().map(|r| url_key(&r.url)).collect();
        let mut session =
            DomainSession::new(http, None, None, true, 0, DISCOVERY_MAX_FETCHES * 2 + 4);
        let found = discover_from(&mut session, &seeds, &known, true, None).await;
        found
            .into_iter()
            .map(|page| {
                let row = CandidateRow {
                    id: format!("discovered:{}", page.url),
                    url: page.url.clone(),
                    page_title: Some(page.signal.clone()),
                    ..template.clone()
                };
                (row, page)
            })
            .collect::<Vec<_>>()
    });
    let found: Vec<(CandidateRow, Found)> = stream::iter(jobs)
        .buffer_unordered(concurrency.clamp(1, 16))
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .flatten()
        .collect();
    let rows: Vec<CandidateRow> = found.iter().map(|(row, _)| row.clone()).collect();
    let projected = project(http, rows, existing, gray_generic_tos, today, concurrency).await?;
    Ok((
        found
            .into_iter()
            .map(|(row, page)| (row.domain, page))
            .collect(),
        projected,
    ))
}

// ── schedule ──────────────────────────────────────────────────────────────

/// Worker entry point; logs instead of failing the scheduler.
pub async fn run_scheduled(pool: &PgPool) {
    static RUNNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
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
    let intake = match parse_mode(&std::env::var(MODE_FLAG).unwrap_or_default()) {
        Ok(Mode::Off) => return,
        Ok(mode) => mode,
        Err(error) => {
            warn!(event = "corporate_intake.config_invalid", error = %error, "corporate intake misconfigured");
            return;
        }
    };
    let flags = match super::Flags::from_env() {
        Ok(flags) => flags,
        Err(error) => {
            warn!(event = "corporate_intake.config_invalid", error = %error, "corporate intake misconfigured");
            return;
        }
    };
    let Some(user_agent) = flags.user_agent() else {
        return;
    };
    let http = match super::http::PinnedHttp::new(user_agent, std::time::Duration::from_secs(20)) {
        Ok(http) => http,
        Err(error) => {
            warn!(event = "corporate_intake.config_invalid", error = %error, "corporate intake transport rejected");
            return;
        }
    };
    let mode = effective_mode(intake, flags.verify_mode);
    match discover(pool, mode, &http).await {
        Ok(found) => info!(
            event = "corporate_intake.discovery",
            domains = found.domains,
            found = found.found,
            requests = found.requests,
            "corporate offer-page discovery pass complete"
        ),
        Err(error) => warn!(event = "corporate_intake.discovery_failed",
                            error = %crate::error_safety::persisted_error(&error), "corporate offer-page discovery failed"),
    }
    match run(pool, mode, flags.gray_generic_tos, &http).await {
        Ok(summary) => {
            info!(event = "corporate_intake.run", mode = mode.as_str(), sources = summary.sources,
                             published = summary.published, requests = summary.requests,
                             outcomes = ?summary.outcomes, "corporate intake pass complete")
        }
        Err(error) => warn!(event = "corporate_intake.run_failed",
                            error = %crate::error_safety::persisted_error(&error), "corporate intake failed"),
    }
}

#[cfg(test)]
#[path = "intake_tests.rs"]
mod tests;
