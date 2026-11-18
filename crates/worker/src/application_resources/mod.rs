//! Bounded discovery/revalidation; never submits forms or auto-approves a link.
//!
//! Observed state (`check_status`, failure counters) is separate from the
//! reviewer's `status`. A probe that cannot see a page (approval, robots,
//! cooldown, network, 5xx, redirect, format) records that and backs off; it
//! never unpublishes a verified row. Only three consecutive HTTP 404/410
//! demote a verified row to `unavailable`.
pub mod apply_entry;
mod discovery;
pub(crate) mod fetch;
pub(crate) mod form_check;
pub(crate) mod form_judge;
mod repository;
pub(crate) mod welfare_forms;

use chrono::{DateTime, Duration, Utc};
use mazimi_core::{
    application_resources::{digest, fingerprint, Resource},
    source_policy::{is_shared_hosting, registrable_domain},
};
use reqwest::Url;
use sqlx::PgPool;

/// Demotion threshold for consecutive 404/410 on a verified row.
pub(crate) const GONE_DEMOTION_THRESHOLD: i32 = 3;
/// The scheduled job has a 25 minute hard cap; probes stop well before it.
const RUN_BUDGET: std::time::Duration = std::time::Duration::from_secs(20 * 60);

#[derive(Debug, Default)]
pub struct Summary {
    pub seeded: usize,
    pub checked: usize,
    pub changed: usize,
    pub failed: usize,
    pub deferred: usize,
    pub demoted: usize,
    /// Candidate templates marked `rejected` as noise this run.
    pub rejected_noise: usize,
    /// Earlier automatic rejections restored because the rules changed.
    pub restored_noise: usize,
    /// Download-only forms published this run ([`FormsPublish::On`]).
    pub forms_published: usize,
    /// Forms the dry run would have published ([`FormsPublish::DryRun`]).
    pub forms_would_publish: usize,
}

/// The download-only form tier ("공식 양식 받기"): a probed template candidate
/// that [`form_check::decide`] accepts is published as a verified resource
/// with no field overlay. `APPLICATION_FORMS_PUBLISH` = `off` (default) |
/// `dry_run` (decide and report in an operational event, change nothing) |
/// `on` (publish, recorded with `APPLICATION_FORMS_ACTOR_USER_ID` as the
/// reviewer and the rule's evidence in `application_resource_reviews`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormsPublish {
    Off,
    DryRun,
    On(uuid::Uuid),
}

impl FormsPublish {
    /// `form_auto_actor` is `FORM_AUTO_ACTOR_USER_ID`: this tier records its
    /// own automatic account, never that one or a person's, so its rows can
    /// be told apart and undone without touching anyone else's reviews.
    pub fn from_values(
        mode: Option<&str>,
        actor: Option<&str>,
        form_auto_actor: Option<&str>,
    ) -> anyhow::Result<Self> {
        match mode.map(str::trim).unwrap_or("off") {
            "" | "off" => Ok(Self::Off),
            "dry_run" => Ok(Self::DryRun),
            "on" => {
                let actor = actor
                    .map(str::trim)
                    .filter(|a| !a.is_empty())
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "APPLICATION_FORMS_PUBLISH=on needs APPLICATION_FORMS_ACTOR_USER_ID"
                        )
                    })?;
                let actor: uuid::Uuid = actor.parse().map_err(|_| {
                    anyhow::anyhow!("APPLICATION_FORMS_ACTOR_USER_ID is not a UUID")
                })?;
                anyhow::ensure!(
                    !actor.is_nil(),
                    "APPLICATION_FORMS_ACTOR_USER_ID must not be nil"
                );
                anyhow::ensure!(
                    form_auto_actor.and_then(|a| a.trim().parse::<uuid::Uuid>().ok()) != Some(actor),
                    "APPLICATION_FORMS_ACTOR_USER_ID must be its own account, not FORM_AUTO_ACTOR_USER_ID"
                );
                Ok(Self::On(actor))
            }
            other => {
                anyhow::bail!("APPLICATION_FORMS_PUBLISH must be off, dry_run or on, not {other:?}")
            }
        }
    }
}

pub struct Job {
    limit: i64,
    seed_limit: i64,
    forms: FormsPublish,
    /// The local model for ambiguous programme matches (`form_judge`).
    judge: Option<form_judge::FormJudge>,
}
impl Job {
    pub fn from_env() -> anyhow::Result<Option<Self>> {
        if !matches!(
            std::env::var("APPLICATION_RESOURCES_ENABLED")
                .unwrap_or_default()
                .as_str(),
            "true" | "1"
        ) {
            return Ok(None);
        }
        fn bound(key: &str, default: i64, max: i64) -> anyhow::Result<i64> {
            let value = std::env::var(key)
                .ok()
                .map(|v| v.parse::<i64>())
                .transpose()?
                .unwrap_or(default);
            anyhow::ensure!((1..=max).contains(&value), "{key} out of bounds");
            Ok(value)
        }
        let forms = FormsPublish::from_values(
            std::env::var("APPLICATION_FORMS_PUBLISH").ok().as_deref(),
            std::env::var("APPLICATION_FORMS_ACTOR_USER_ID")
                .ok()
                .as_deref(),
            std::env::var("FORM_AUTO_ACTOR_USER_ID").ok().as_deref(),
        )?;
        Ok(Some(Self {
            limit: bound("APPLICATION_RESOURCES_MAX_CHECKS", 20, 100)?,
            seed_limit: bound("APPLICATION_RESOURCES_SEED_LIMIT", 200, 1000)?,
            judge: if forms == FormsPublish::Off {
                None
            } else {
                form_judge::FormJudge::from_env()?
            },
            forms,
        }))
    }
    pub async fn run(&self, pool: &PgPool) -> anyhow::Result<Summary> {
        let mut result = Summary {
            seeded: repository::seed(pool, self.seed_limit).await?,
            ..Default::default()
        };
        // Form files the 복지로 detail API lists (the legal source of 복지로
        // forms). A failure is logged; the probes below still run.
        for api in [welfare_forms::Api::National, welfare_forms::Api::Local] {
            match welfare_forms::seed(pool, api).await {
                Ok(n) => result.seeded += n,
                Err(error) => {
                    tracing::error!(source = api.source_type(), error = %error, "welfare form listing failed")
                }
            }
        }
        // Noise is rejected before the claim, so no check budget goes to it.
        // A sweep failure must not stop the renewals that keep verified
        // templates visible (review M5): log it, the next run retries.
        match repository::reject_noise(pool, true).await {
            Ok(sweep) => {
                result.rejected_noise = sweep.rejected.len();
                result.restored_noise = sweep.restored.len();
            }
            Err(error) => {
                tracing::error!(error = %error, "application resource noise sweep failed")
            }
        }
        // The catalogue the programme match reads (qualifiers, places). If it
        // cannot be read, no form is decided this run (fail closed).
        let vocabulary = if self.forms == FormsPublish::Off {
            None
        } else {
            match repository::catalogue(pool).await {
                Ok(v) => Some(v),
                Err(error) => {
                    tracing::error!(error = %error, "form catalogue not read; no form decided this run");
                    None
                }
            }
        };
        let forms = vocabulary.as_ref().map(|v| (v, self.judge.as_ref()));
        let mut session = fetch::Session::new(RUN_BUDGET);
        let mut decisions: Vec<(uuid::Uuid, uuid::Uuid, String, String, form_check::Decision)> =
            vec![];
        for row in repository::claim(pool, self.limit).await? {
            if session.remaining().is_zero() {
                // Leased rows come back after the lease; nothing is recorded.
                break;
            }
            let Some(p) = repository::available_program(pool, row.program_id).await? else {
                continue;
            };
            result.checked += 1;
            match probe(pool, &mut session, &row, &p, forms).await {
                Ok(observation) => {
                    let changed = needs_review(&row, &observation, &fingerprint(&p));
                    let form = observation.form.clone();
                    let hash = observation.hash.clone();
                    if changed {
                        result.changed += 1;
                    }
                    let recorded =
                        repository::success(pool, &row, &p, observation, changed).await?;
                    if recorded.newly_changed {
                        report(pool, &row, "changed", "content_changed").await;
                    }
                    if recorded.grace_renewed {
                        record_grace_renewal(pool, &row).await;
                    }
                    if let (true, Some(form)) = (recorded.updated, form) {
                        // The event lists what was actually published (or, in a
                        // dry run, would be) and the refusals by reason; a
                        // decision whose publish did not happen is not listed.
                        let reported = match (self.forms, form.publish) {
                            (FormsPublish::On(actor), true) => {
                                match repository::publish_form(pool, &row, &hash, actor, &form)
                                    .await
                                {
                                    Ok(true) => {
                                        result.forms_published += 1;
                                        true
                                    }
                                    Ok(false) => false,
                                    Err(error) => {
                                        tracing::error!(resource_id = %row.id, error = %error, "download form not published");
                                        false
                                    }
                                }
                            }
                            (FormsPublish::DryRun, true) => {
                                result.forms_would_publish += 1;
                                true
                            }
                            _ => true,
                        };
                        if reported {
                            decisions.push((
                                row.id,
                                row.program_id,
                                p.title.clone(),
                                row.label.clone(),
                                form,
                            ));
                        }
                    }
                }
                Err(fetch::FetchError::Database(error)) => return Err(error.into()),
                Err(fetch::FetchError::Cooldown(until)) => {
                    result.deferred += 1;
                    repository::defer(pool, &row, until).await?;
                }
                Err(fetch::FetchError::Redirect(Some(target))) if login_gate(&row, &target) => {
                    // A login_required destination answering with its own
                    // sign-in page is the expected, healthy observation.
                    repository::login_gate(pool, &row).await?;
                }
                Err(error) => {
                    result.failed += 1;
                    if matches!(error, fetch::FetchError::IsFile) {
                        // Like every other side step of the loop: log, never stop
                        // the renewals that keep verified templates visible.
                        if let Err(error) = repository::file_candidate(pool, &row, &p).await {
                            tracing::error!(resource_id = %row.id, error = %error, "file candidate not queued");
                        }
                    }
                    if let fetch::FetchError::Redirect(Some(target)) = &error {
                        repository::redirect_candidate(pool, &row, &p, target).await?;
                        // Redirects never demote (C1), so a verified entry
                        // that now leaves its site must reach an operator.
                        if row.status == "verified" {
                            report(pool, &row, "redirected", "redirect_requires_review").await;
                        }
                    }
                    let failure = Failure::of(&error, &row, Utc::now());
                    if repository::failure(pool, &row, &failure).await? {
                        result.demoted += 1;
                        report(pool, &row, "demoted", &failure.code).await;
                    }
                }
            }
        }
        if self.forms != FormsPublish::Off && !decisions.is_empty() {
            report_forms(pool, self.forms, &decisions).await;
        }
        // Self-reporting never fails the probe run: a database hiccup here
        // is logged and the next hourly run reports again.
        if let Err(error) = report_health(pool).await {
            tracing::error!(error = %error, "application resource health report failed");
        }
        Ok(result)
    }
}

/// A verified template with no successful re-check for this long is at
/// risk, whatever its window (a 168-hour approval still warns early).
pub(crate) const AT_RISK_HOURS: i64 = 48;
/// A verified template whose window ends within this many hours is at risk:
/// the warning comes before users lose it, not after (10-18: measured from
/// `verified_at` alone, a 24-hour window had expired a day before the alert).
/// A healthy row is renewed every six hours, so it keeps at least
/// `window - 6h` ahead of it and never trips this for a 24-hour or longer window.
pub(crate) const EXPIRY_WARNING_HOURS: i64 = 12;

/// Opens one warning per verified template that is at risk (no successful
/// re-check for [`AT_RISK_HOURS`], or its window ends within
/// [`EXPIRY_WARNING_HOURS`]), resolves the warnings of rows that recovered,
/// and once per UTC day records a
/// summary of the queue (found, rejected by reason, verified, expired,
/// unsupported) as an info event that replaces the previous day's.
pub async fn report_health(pool: &PgPool) -> anyhow::Result<()> {
    use crate::operations::{record_event, resolve_event, EventCommand, Severity};
    let risks = repository::at_risk_templates(pool, AT_RISK_HOURS, EXPIRY_WARNING_HOURS).await?;
    let open: std::collections::HashMap<String, bool> = repository::open_at_risk_events(pool)
        .await?
        .into_iter()
        .collect();
    let current: std::collections::HashSet<String> = risks
        .iter()
        .map(|r| format!("application_resource:at_risk:{}", r.id))
        .collect();
    for risk in risks {
        let fingerprint = format!("application_resource:at_risk:{}", risk.id);
        let subject = risk.id.to_string();
        let expired = risk.valid_until <= Utc::now();
        // An open warning is left alone (an operator's acknowledgement
        // stays, review M6) unless it has to escalate to "expired".
        if open
            .get(&fingerprint)
            .is_some_and(|said_expired| *said_expired || !expired)
        {
            continue;
        }
        let event = EventCommand {
            fingerprint: &fingerprint,
            severity: Severity::Warning,
            component: "application_resources",
            event_type: "application_resource_at_risk",
            subject_type: Some("program_application_resource"),
            subject_id: Some(&subject),
            message: if expired {
                "A verified form template expired without a successful re-check; users no longer see it. An unchanged file renews automatically within 72 hours of expiry; after that it needs review"
            } else {
                "A verified form template is close to expiry (or has had no successful re-check for 48 hours) and will expire unless a re-check succeeds"
            },
            details: serde_json::json!({
                "program_id": risk.program_id,
                "last_success_at": risk.verified_at,
                "valid_until": risk.valid_until,
                "expired": expired,
                "hours_until_expiry": (risk.valid_until - Utc::now()).num_minutes() as f64 / 60.0,
                "check_status": risk.check_status,
                "last_failure_code": risk.last_failure_code,
                "next_check_at": risk.next_check_at,
            }),
        };
        if let Err(error) = record_event(pool, &event).await {
            tracing::error!(resource_id = %risk.id, error = %error, "at-risk event not recorded");
        }
    }
    // Renewed, no longer verified, or its programme closed: nothing at risk.
    for fingerprint in open.into_keys().filter(|f| !current.contains(f)) {
        resolve_event(
            pool,
            &fingerprint,
            None,
            Some("re-checked successfully, no longer verified, or programme closed"),
        )
        .await?;
    }
    let day = Utc::now().format("%Y-%m-%d").to_string();
    let fingerprint = format!("application_resources:daily:{day}");
    if !repository::event_exists(pool, &fingerprint).await? {
        let details = repository::daily_counts(pool).await?;
        tracing::info!(%details, "application resources daily summary");
        record_event(
            pool,
            &EventCommand {
                fingerprint: &fingerprint,
                severity: Severity::Info,
                component: "application_resources",
                event_type: "application_resources_daily_summary",
                subject_type: None,
                subject_id: None,
                message: "Application-resource collector: last 24 hours and current queue",
                details,
            },
        )
        .await?;
        for old in repository::open_daily_summaries(pool, &fingerprint).await? {
            resolve_event(
                pool,
                &old,
                None,
                Some("superseded by the next daily summary"),
            )
            .await?;
        }
    }
    Ok(())
}

/// Why a candidate template is noise, or `None` to keep it. First match
/// wins; the rules are the twins of `scripts/forms/auto/choose.py` and
/// the discovery rule [`discovery::page_may_offer_templates`].
pub(crate) fn noise_reason(row: &NoiseRow) -> Option<&'static str> {
    if !discovery::template_label_allowed(&row.label, &row.program_title) {
        Some("recruitment_document")
    } else if row.discovery_depth >= 2 && row.programs_sharing_url >= SHARED_ATTACHMENT_LIMIT {
        // Found through a link (not on the programme's own page) and on 3+
        // programmes: a board attachment. A form a city links from each of
        // its own programme pages (depth 1) is kept.
        Some("shared_attachment")
    } else if row.discovery_depth >= 2
        // Found through a link, and the same file belongs to an ended round
        // of this same programme (review 2). A stable yearly form URL on the
        // programme's own page, or a form sibling programmes share, is kept.
        && row
            .ended_titles
            .iter()
            .any(|title| discovery::related(title, &row.program_title) == Some(true))
    {
        Some("earlier_round")
    } else if row.discovery_depth >= 2
        // Found on a page reached through a link: that page must be about
        // the programme. Only the caption is stored; a caption that names
        // nothing ("모집공고") or an umbrella notice leaves the row alone,
        // and so does a form whose own title names the programme.
        && row.source_label.as_deref().is_some_and(|caption| {
            discovery::related(caption, &row.program_title) == Some(false)
                && !discovery::umbrella_caption(caption)
        })
        && discovery::related(&row.label, &row.program_title) != Some(true)
    {
        Some("unrelated_source_page")
    } else {
        None
    }
}

/// One file on this many programmes is a board-wide attachment.
pub(crate) const SHARED_ATTACHMENT_LIMIT: i64 = 3;

/// A candidate template with the facts [`noise_reason`] needs.
#[derive(Debug, Clone, sqlx::FromRow, serde::Deserialize)]
pub struct NoiseRow {
    pub id: uuid::Uuid,
    pub revision: i64,
    pub label: String,
    pub discovery_depth: i32,
    pub program_title: String,
    /// Caption of the page the file was found on (its own resource row).
    pub source_label: Option<String>,
    pub programs_sharing_url: i64,
    /// Titles of other programmes whose window ended (or that are closed) and
    /// carry the same file.
    #[serde(default)]
    pub ended_titles: Vec<String>,
    /// Rejected by an earlier sweep (re-evaluated, restored if no rule fires).
    #[serde(default)]
    pub auto_rejected: bool,
}

pub use repository::reject_noise;
pub use repository::undo_download_forms;

/// A verified destination that users currently see was demoted, redirects
/// outside its reviewed site, or no longer matches its reviewed version: log it and open an operator event so a
/// reviewer looks at it. Event recording failure is logged, never fatal.
async fn report(pool: &PgPool, row: &Resource, what: &str, code: &str) {
    tracing::warn!(
        program_id = %row.program_id,
        resource_id = %row.id,
        kind = %row.kind,
        code,
        "verified application resource {what}"
    );
    let fingerprint = format!("application_resource:{what}:{}", row.id);
    let subject = row.id.to_string();
    let message = match what {
        "demoted" => "A verified application resource returned 404/410 three times in a row and was unpublished",
        "redirected" => "A verified application resource redirects outside its reviewed site or sign-in gate; review it again",
        _ => "A verified application resource no longer matches its reviewed version; review it again",
    };
    let event = crate::operations::EventCommand {
        fingerprint: &fingerprint,
        severity: crate::operations::Severity::Warning,
        component: "application_resources",
        event_type: match what {
            "demoted" => "application_resource_demoted",
            "redirected" => "application_resource_redirected",
            _ => "application_resource_changed",
        },
        subject_type: Some("program_application_resource"),
        subject_id: Some(&subject),
        message,
        details: serde_json::json!({
            "program_id": row.program_id,
            "kind": row.kind,
            "code": code,
        }),
    };
    if let Err(error) = crate::operations::record_event(pool, &event).await {
        tracing::error!(resource_id = %row.id, error = %error, "operational event not recorded");
    }
}

/// One failed probe, classified. `gone` is the only class that can demote.
pub(crate) struct Failure {
    pub code: String,
    pub check_status: &'static str,
    pub gone: bool,
    /// Seconds until the next probe (see [`retry_after`]).
    pub retry_after_secs: i64,
}
impl Failure {
    fn of(error: &fetch::FetchError, row: &Resource, now: DateTime<Utc>) -> Self {
        use fetch::FetchError as E;
        let check_status = match error {
            E::Http(_) | E::Network | E::Body => "unreachable",
            E::Format | E::IsFile => "unsupported",
            _ => "blocked",
        };
        // A redirect off the reviewed site (or to nowhere public) hides the
        // row until a clean probe; a same-site one is only reported.
        let code = match error {
            E::Redirect(target)
                if !target
                    .as_deref()
                    .is_some_and(|target| same_site(&row.url, target)) =>
            {
                mazimi_core::application_resources::REDIRECTED_OFF_SITE.to_string()
            }
            _ => error.code(),
        };
        Self {
            code,
            check_status,
            gone: matches!(error, E::Http(404 | 410)),
            retry_after_secs: retry_after(error, row, now).num_seconds(),
        }
    }
}

/// Exponential backoff from the number of failures before this one:
/// 1, 2, 4, 8 days, then the 14 day cap.
pub(crate) fn backoff_days(previous_failures: i32) -> i32 {
    (1_i32 << previous_failures.clamp(0, 4)).min(14)
}

/// A failure that says nothing about the resource itself: the network, a
/// truncated body, or a server error. 404/410, a redirect, an unsupported
/// format and a changed hash are evidence and keep the plain backoff.
fn transient(error: &fetch::FetchError) -> bool {
    use fetch::FetchError as E;
    matches!(error, E::Network | E::Body | E::Http(500..=599))
}

/// Retry steps for a verified row after transient failures: 1h, 3h, then 6h.
const TRANSIENT_STEPS_HOURS: [i64; 3] = [1, 3, 6];
/// The last retry before expiry is planned at least this long before it.
const EXPIRY_MARGIN: Duration = Duration::hours(1);
/// Never retry sooner than this (the job runs hourly; the lease is 2 hours).
const MIN_RETRY: Duration = Duration::minutes(30);
/// After expiry, an unchanged template may still renew itself for this long
/// (see [`may_grace_renew`]).
pub(crate) const GRACE_HOURS: i64 = 72;

/// When to probe again after a failure. A transient failure of a verified
/// row that users see (or of an expired template still inside its grace
/// period) is retried in 1h, 3h, then 6h steps, and before expiry never
/// later than `valid_until - EXPIRY_MARGIN`, so one network blip cannot put
/// the next probe past the window (10-18: a 1-day backoff on a 24-hour
/// window). Everything else keeps the exponential day backoff.
pub(crate) fn retry_after(
    error: &fetch::FetchError,
    row: &Resource,
    now: DateTime<Utc>,
) -> Duration {
    let backoff = Duration::days(backoff_days(row.consecutive_failures).into());
    if row.status != "verified" || !transient(error) {
        return backoff;
    }
    let Some(valid_until) = row.valid_until else {
        return backoff;
    };
    let in_grace = row.kind == "template"
        && valid_until <= now
        && valid_until > now - Duration::hours(GRACE_HOURS);
    if valid_until <= now && !in_grace {
        return backoff;
    }
    let step = TRANSIENT_STEPS_HOURS[row.consecutive_failures.clamp(0, 2) as usize];
    let mut next = backoff.min(Duration::hours(step));
    if valid_until > now {
        next = next.min(valid_until - EXPIRY_MARGIN - now);
    }
    next.max(MIN_RETRY)
}

/// Sign-in path words, compared per path segment (and per `.`/`_`/`-` part
/// of a segment), never as substrings: `/association`, `/author` and
/// `/lesson` are not sign-in pages.
const LOGIN_PATH_WORDS: &[&str] = &[
    "login", "logon", "signin", "sign-in", "sso", "auth", "oauth", "oauth2", "nidlogin", "nlogin",
    "lgn",
];

fn is_sign_in_path(path: &str) -> bool {
    path.to_ascii_lowercase()
        .split('/')
        .filter(|segment| !segment.is_empty())
        .any(|segment| {
            LOGIN_PATH_WORDS.contains(&segment)
                || segment
                    .split(['.', '_', '-'])
                    .any(|part| LOGIN_PATH_WORDS.contains(&part))
        })
}

/// The login gate the reviewer accepted for a `login_required` destination:
/// a redirect to a sign-in path on the destination's own registrable domain.
/// Shared-hosting platforms are never "the same site", and any cross-site
/// redirect (including a third-party SSO) is a failure the reviewer must
/// look at again (security M2).
fn login_gate(row: &Resource, target: &str) -> bool {
    if row.access != "login_required" || !same_site(&row.url, target) {
        return false;
    }
    Url::parse(target).is_ok_and(|to| {
        to.host_str().is_some_and(|b| !is_shared_hosting(b)) && is_sign_in_path(to.path())
    })
}

/// `to` is on the same site as `from`: the same registrable domain, or the
/// very same host on shared hosting, over https.
fn same_site(from: &str, to: &str) -> bool {
    let (Ok(from), Ok(to)) = (Url::parse(from), Url::parse(to)) else {
        return false;
    };
    if to.scheme() != "https" {
        return false;
    }
    match (from.host_str(), to.host_str()) {
        (Some(a), Some(b)) if is_shared_hosting(a) => a == b,
        (Some(a), Some(b)) => {
            registrable_domain(a).is_some() && registrable_domain(a) == registrable_domain(b)
        }
        _ => false,
    }
}

pub(super) struct Observation {
    hash: String,
    source_hash: String,
    file_format: Option<String>,
    size: Option<i64>,
    links: Vec<discovery::Candidate>,
    /// The download-only form decision, for an unreviewed template candidate.
    form: Option<form_check::Decision>,
}

/// A file a 복지로 detail API lists for this very service: queued at depth 0
/// as its own source, on bokjiro.go.kr, for a 복지로 programme.
fn listed_by_api(row: &Resource, program: &mazimi_core::models::Program) -> bool {
    matches!(
        program.source_type.as_str(),
        "national_welfare" | "local_welfare"
    ) && row.discovery_depth == 0
        && row.source_url == row.url
        && Url::parse(&row.url).is_ok_and(|u| {
            u.host_str().and_then(registrable_domain).as_deref() == Some("bokjiro.go.kr")
        })
}

/// This year in Korea, where the forms are dated.
fn korea_year() -> i32 {
    use chrono::Datelike;
    (Utc::now() + Duration::hours(9)).year()
}

/// One operational event per run with the form decisions: in a dry run the
/// operator reads what would be published; when on, what was.
async fn report_forms(
    pool: &PgPool,
    mode: FormsPublish,
    decisions: &[(uuid::Uuid, uuid::Uuid, String, String, form_check::Decision)],
) {
    let mut refused: std::collections::BTreeMap<&str, usize> = Default::default();
    for (.., d) in decisions.iter().filter(|(.., d)| !d.publish) {
        *refused.entry(d.reason).or_default() += 1;
    }
    let published: Vec<_> = decisions
        .iter()
        .filter(|(.., d)| d.publish)
        .take(100)
        .map(|(id, program, title, label, d)| {
            serde_json::json!({"resource_id": id, "program_id": program, "program": title.chars().take(80).collect::<String>(),
                "label": label.chars().take(80).collect::<String>(), "form_title": d.title.chars().take(80).collect::<String>(), "tier": d.reason})
        })
        .collect();
    let (event_type, verb) = match mode {
        FormsPublish::DryRun => ("application_forms_dry_run", "would publish"),
        _ => ("application_forms_published", "published"),
    };
    let fingerprint = format!("{event_type}:{}", Utc::now().format("%Y-%m-%dT%H"));
    let message = format!(
        "download-only forms: {verb} {}, refused {}",
        published.len(),
        decisions.len() - published.len()
    );
    let event = crate::operations::EventCommand {
        fingerprint: &fingerprint,
        severity: crate::operations::Severity::Info,
        component: "application_resources",
        event_type,
        subject_type: None,
        subject_id: None,
        message: &message,
        details: serde_json::json!({"forms": published, "refused": refused}),
    };
    if let Err(error) = crate::operations::record_event(pool, &event).await {
        tracing::error!(error = %error, "download form report not recorded");
    }
}
fn needs_review(row: &Resource, observed: &Observation, fp: &str) -> bool {
    row.program_fingerprint != fp
        || row
            .content_hash
            .as_ref()
            .is_some_and(|hash| hash != &observed.hash)
        || row
            .source_hash
            .as_ref()
            .is_some_and(|hash| hash != &observed.source_hash)
}
fn is_html(content_type: &str) -> bool {
    content_type.starts_with("text/html") || content_type.starts_with("application/xhtml+xml")
}
/// The model's answer moves an ambiguous file only: in `judge` mode a yes
/// publishes and a no refuses; in `shadow` mode the answer is recorded and
/// the file stays unpublished. Every question is logged with its evidence.
async fn ask_judge(
    judge: &form_judge::FormJudge,
    row: &Resource,
    program: &mazimi_core::models::Program,
    decision: &mut form_check::Decision,
) {
    let answer = judge
        .is_form_of(
            &program.title,
            program.summary.as_deref().unwrap_or(""),
            &decision.title,
            &decision.excerpt,
        )
        .await;
    decision.model = answer;
    tracing::info!(
        resource_id = %row.id,
        program_id = %row.program_id,
        model = judge.model(),
        mode = ?judge.mode(),
        answer = ?answer,
        form_title = %decision.title.chars().take(60).collect::<String>(),
        "form programme match asked of the local model"
    );
    if judge.mode() == form_judge::Mode::Judge {
        match answer {
            Some(true) => {
                decision.publish = true;
                decision.reason = "model_confirmed";
            }
            Some(false) => decision.reason = "model_other_programme",
            None => {}
        }
    }
}

async fn probe(
    pool: &PgPool,
    session: &mut fetch::Session,
    row: &Resource,
    program: &mazimi_core::models::Program,
    forms: Option<(&form_check::Vocabulary, Option<&form_judge::FormJudge>)>,
) -> Result<Observation, fetch::FetchError> {
    let program_title = program.title.as_str();
    let page = fetch::get(
        pool,
        session,
        &row.url,
        // A notice URL may serve the form itself (FetchError::IsFile), so it
        // gets the file cap too; HTML is decoded from the same bytes.
        8 * 1024 * 1024,
    )
    .await?;
    let (hash, file_format, size, links, form) = if row.kind == "template" {
        let path = Url::parse(&row.url)
            .map(|u| u.path().to_string())
            .unwrap_or_default();
        let format = discovery::file_format(
            &page.bytes,
            &path,
            page.content_disposition.as_deref(),
            &row.label,
        )
        .ok_or(fetch::FetchError::Format)?;
        // Only a candidate nobody has reviewed is decided; a reviewer's
        // decision (verified or rejected) is never replaced by the rule.
        let form = match forms {
            Some((vocabulary, judge)) if row.status == "candidate" && row.reviewed_by.is_none() => {
                let pdf_text = if format == "pdf" {
                    form_check::pdf_first_page_text(&page.bytes).await
                } else {
                    None
                };
                let programme = form_check::Programme {
                    title: program_title,
                    provider: program.provider_name.as_deref().unwrap_or(""),
                    regions: program.regions.as_deref().unwrap_or(&[]),
                    districts: program.region_districts.as_deref().unwrap_or(&[]),
                };
                let mut decision = form_check::decide(
                    &page.bytes,
                    format,
                    pdf_text.as_deref(),
                    &programme,
                    listed_by_api(row, program),
                    korea_year(),
                    vocabulary,
                );
                if decision.reason == "ambiguous_programme" {
                    if let Some(judge) = judge {
                        ask_judge(judge, row, program, &mut decision).await;
                    }
                }
                Some(decision)
            }
            _ => None,
        };
        (
            digest(&page.bytes),
            Some(format.into()),
            Some(page.bytes.len() as i64),
            vec![],
            form,
        )
    } else {
        if !is_html(&page.content_type) {
            // A notice URL that serves a document (a download link given as
            // the programme's URL) becomes a form candidate of its own.
            let path = Url::parse(&row.url)
                .map(|u| u.path().to_string())
                .unwrap_or_default();
            return Err(
                match discovery::file_format(
                    &page.bytes,
                    &path,
                    page.content_disposition.as_deref(),
                    &row.label,
                ) {
                    Some(_) => fetch::FetchError::IsFile,
                    _ => fetch::FetchError::Format,
                },
            );
        }
        let html = crate::sources::benefit_pages::decode_page(&page.bytes, &page.content_type)
            .map_err(|_| fetch::FetchError::Format)?;
        let links = if row.discovery_depth < 2 {
            let title = discovery::page_title(&html);
            // Board lists are followed one level, from the programme's own page.
            let program = (row.discovery_depth == 0).then_some(program_title);
            discovery::links(&row.url, &html, program)
                .into_iter()
                .filter(|c| {
                    c.kind != "template"
                        || discovery::template_candidate_allowed(
                            row.discovery_depth,
                            &row.label,
                            &title,
                            &c.label,
                            program_title,
                        )
                })
                .collect()
        } else {
            vec![]
        };
        (discovery::semantic_hash(&html), None, None, links, None)
    };
    let source_hash = if row.source_url == row.url {
        hash.clone()
    } else {
        let source = fetch::get(pool, session, &row.source_url, 1024 * 1024)
            .await
            .map_err(|error| {
                // The source notice moved, not the application/file itself.
                // Never turn a notice redirect into a new application candidate.
                if matches!(error, fetch::FetchError::Redirect(_)) {
                    fetch::FetchError::SourceRedirect
                } else {
                    error
                }
            })?;
        if !is_html(&source.content_type) {
            return Err(fetch::FetchError::Format);
        }
        let html = crate::sources::benefit_pages::decode_page(&source.bytes, &source.content_type)
            .map_err(|_| fetch::FetchError::Format)?;
        discovery::semantic_hash(&html)
    };
    Ok(Observation {
        hash,
        source_hash,
        file_format,
        size,
        links,
        form,
    })
}

/// A successful crawl renews only an unchanged, still-live reviewed record.
/// Expired evidence must be reviewed again; a job outage cannot self-certify it
/// (except the narrow template grace in [`may_grace_renew`]).
fn may_renew(row: &Resource, changed: bool, now: DateTime<Utc>) -> bool {
    !changed
        // A 200 HTML shell is not evidence that a JS/login-only form still
        // accepts this intake. Do not extend manual application verification.
        && row.kind != "application"
        && row.status == "verified"
        && row.check_status.as_deref() != Some("changed")
        && row.valid_until.is_some_and(|d| d > now)
        && row.content_hash.is_some()
        && row.source_hash.is_some()
        && row.checked_at.is_some_and(|d| d > now - Duration::days(7))
}

/// Grace renewal: a verified template that expired less than [`GRACE_HOURS`]
/// ago, re-fetched with exactly the reviewed file bytes and source page, may
/// renew itself to the reviewer's approved window. The repository adds the
/// rest of the condition in the same statement: the latest verified review
/// must carry this exact `content_hash` and a `validity_hours`, and the row's
/// stored hashes (the reviewed baseline; any difference is sticky
/// `changed`) must equal the observation. Beyond the grace period, or on any
/// difference, a reviewer must look again. While expired the row stays
/// hidden (`Resource::usable`).
///
/// Templates only: their hash is a digest of the exact file a user
/// downloads, so equality is the whole claim. A notice or other page has a
/// lossy semantic hash of HTML whose meaning can change around the same
/// text, and an application never auto-renews at all.
fn may_grace_renew(
    row: &Resource,
    observed: &Observation,
    changed: bool,
    now: DateTime<Utc>,
) -> bool {
    !changed
        && row.kind == "template"
        && row.status == "verified"
        && row.check_status.as_deref() != Some("changed")
        && row
            .valid_until
            .is_some_and(|d| d <= now && d > now - Duration::hours(GRACE_HOURS))
        && row.content_hash.as_deref() == Some(observed.hash.as_str())
        && row.source_hash.as_deref() == Some(observed.source_hash.as_str())
}

/// A grace renewal is an automatic action: logged and recorded once per
/// resource per Korean day. Failure to record is logged, never fatal.
async fn record_grace_renewal(pool: &PgPool, row: &Resource) {
    tracing::warn!(
        program_id = %row.program_id,
        resource_id = %row.id,
        expired_at = ?row.valid_until,
        "expired verified template renewed: re-fetched file and source match the review exactly"
    );
    let fingerprint = format!("self_healing:application_resource_grace:{}", row.id);
    if let Err(error) = crate::storage::record_self_healing(
        pool,
        &fingerprint,
        "application_resource_grace_renewed",
        "만료된 검증 양식을 다시 받아 검토 당시 파일·공고와 해시가 정확히 같아 승인된 유효기간으로 자동 갱신했습니다.",
        serde_json::json!({
            "resource_id": row.id,
            "program_id": row.program_id,
            "expired_at": row.valid_until,
            "grace_hours": GRACE_HOURS,
        }),
    )
    .await
    {
        tracing::error!(resource_id = %row.id, error = %error, "grace renewal not recorded");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(access: &str, url: &str) -> Resource {
        Resource {
            id: uuid::Uuid::new_v4(),
            program_id: uuid::Uuid::new_v4(),
            kind: "application".into(),
            url: url.into(),
            source_url: "https://www.wonju.go.kr/notice/1".into(),
            label: "신청하기".into(),
            status: "verified".into(),
            access: access.into(),
            program_fingerprint: "fp".into(),
            discovery_depth: 0,
            content_hash: None,
            source_hash: None,
            file_format: None,
            byte_size: None,
            checked_at: None,
            verified_at: None,
            valid_until: None,
            reviewed_by: None,
            review_note: None,
            check_error: None,
            check_status: None,
            consecutive_failures: 0,
            consecutive_gone: 0,
            last_failure_code: None,
            revision: 1,
        }
    }

    #[test]
    fn login_gate_needs_same_non_shared_site_and_a_sign_in_path_segment() {
        let city = row("login_required", "https://apply.busan.go.kr/form/12");
        for ok in [
            "https://www.busan.go.kr/member/login.do?return=x",
            "https://apply.busan.go.kr/sso/start",
            "https://apply.busan.go.kr/oauth2/authorize",
            "https://www.busan.go.kr/member/lgn",
            "https://www.busan.go.kr/user/sign-in",
        ] {
            assert!(login_gate(&city, ok), "{ok}");
        }
        for bad in [
            // Cross-site, even on a sign-in path (security M2).
            "https://evil.example/login",
            "https://sso.gov.kr/login?return=x",
            // Same site but not a sign-in page.
            "https://www.busan.go.kr/notice/landing",
            // Substrings of ordinary words are not sign-in paths (code L5).
            "https://www.busan.go.kr/association/apply",
            "https://www.busan.go.kr/author/1",
            "https://www.busan.go.kr/lesson/2",
            "https://www.busan.go.kr/processor",
        ] {
            assert!(!login_gate(&city, bad), "{bad}");
        }
        // Shared hosting is never "the same site".
        let form = row("login_required", "https://form.naver.com/response/abc");
        assert!(!login_gate(
            &form,
            "https://nid.naver.com/nidlogin.login?url=x"
        ));
        let direct = row("direct", "https://apply.busan.go.kr/form/12");
        assert!(!login_gate(
            &direct,
            "https://www.busan.go.kr/member/login.do"
        ));
    }

    #[test]
    fn failures_are_classified_and_only_404_410_count_as_gone() {
        use fetch::FetchError as E;
        let r = row("direct", "https://www.wonju.go.kr/apply");
        for (error, status, gone) in [
            (E::Approval, "blocked", false),
            (E::Robots, "blocked", false),
            (E::Redirect(None), "blocked", false),
            (E::Network, "unreachable", false),
            (E::Http(503), "unreachable", false),
            (E::Http(404), "unreachable", true),
            (E::Http(410), "unreachable", true),
            (E::Format, "unsupported", false),
        ] {
            let failure = Failure::of(&error, &r, Utc::now());
            assert_eq!(failure.check_status, status, "{error:?}");
            assert_eq!(failure.gone, gone, "{error:?}");
        }
        assert_eq!(
            Failure::of(&E::Http(404), &r, Utc::now()).code,
            "source_http_404"
        );
    }

    /// Review M7: a redirect that leaves the reviewed site (or leads to no
    /// public https address at all) is recorded under the code that hides
    /// the row; a same-site redirect is only reported.
    #[test]
    fn a_redirect_off_the_reviewed_site_gets_the_hiding_code() {
        use fetch::FetchError as E;
        use mazimi_core::application_resources::REDIRECTED_OFF_SITE;
        let r = row("direct", "https://apply.busan.go.kr/form/12");
        let code = |target: Option<&str>| {
            Failure::of(&E::Redirect(target.map(Into::into)), &r, Utc::now()).code
        };
        for elsewhere in [
            Some("https://evil.example/apply"),
            Some("https://busan.go.kr.evil.example/apply"),
            None,
        ] {
            assert_eq!(code(elsewhere), REDIRECTED_OFF_SITE, "{elsewhere:?}");
        }
        for same_site in [
            "https://apply.busan.go.kr/form/12/",
            "https://www.busan.go.kr/apply/12",
        ] {
            assert_eq!(
                code(Some(same_site)),
                "redirect_requires_review",
                "{same_site}"
            );
        }
        // Shared hosting: only the very same host is the reviewed site.
        let form = row("direct", "https://form.naver.com/response/abc");
        let shared =
            |target: &str| Failure::of(&E::Redirect(Some(target.into())), &form, Utc::now()).code;
        assert_eq!(shared("https://blog.naver.com/x"), REDIRECTED_OFF_SITE);
        assert_eq!(
            shared("https://form.naver.com/response/abc?x=1"),
            "redirect_requires_review"
        );
    }

    #[test]
    fn backoff_doubles_from_one_day_and_caps_at_fourteen() {
        let days: Vec<i32> = (0..7).map(backoff_days).collect();
        assert_eq!(days, vec![1, 2, 4, 8, 14, 14, 14]);
        assert_eq!(backoff_days(-3), 1);
    }

    fn template(valid_for: Duration, failures: i32, now: DateTime<Utc>) -> Resource {
        Resource {
            kind: "template".into(),
            content_hash: Some("file".into()),
            source_hash: Some("source".into()),
            check_status: Some("ok".into()),
            verified_at: Some(now - Duration::hours(1)),
            valid_until: Some(now + valid_for),
            checked_at: Some(now - Duration::hours(1)),
            consecutive_failures: failures,
            ..row("direct", "https://www.wonju.go.kr/form.hwp")
        }
    }

    /// 10-18: one network failure on a 24-hour window must not push the
    /// next probe past expiry. Steps are 1h, 3h, 6h, capped one hour before
    /// `valid_until`, so several attempts happen before users lose it.
    #[test]
    fn transient_failures_of_a_verified_row_retry_before_its_window_ends() {
        use fetch::FetchError as E;
        let now = Utc::now();
        for error in [E::Network, E::Body, E::Http(500), E::Http(503)] {
            let mut at = now;
            let mut attempts = 0;
            let mut r = template(Duration::hours(24), 0, now);
            let expiry = r.valid_until.unwrap();
            while at < expiry - EXPIRY_MARGIN {
                let next = retry_after(&error, &r, at);
                assert!(
                    next >= MIN_RETRY && next <= Duration::hours(6),
                    "{error:?} {next}"
                );
                at += next;
                attempts += 1;
                r.consecutive_failures += 1;
            }
            assert!(at <= expiry - EXPIRY_MARGIN + MIN_RETRY, "{error:?}");
            assert!(attempts >= 4, "{error:?}: {attempts} attempts");
            assert_eq!(
                retry_after(&error, &template(Duration::hours(24), 0, now), now),
                Duration::hours(1)
            );
        }
        // Close to expiry the retry is pulled in before it.
        let late = template(Duration::hours(3), 2, now);
        assert_eq!(retry_after(&E::Network, &late, now), Duration::hours(2));
        // Negative evidence keeps the plain day backoff, as before.
        let r = template(Duration::hours(24), 0, now);
        for error in [
            E::Http(404),
            E::Http(410),
            E::Redirect(None),
            E::Format,
            E::Http(403),
        ] {
            assert_eq!(retry_after(&error, &r, now), Duration::days(1), "{error:?}");
        }
        // A candidate (nothing visible to protect) keeps the day backoff.
        let candidate = Resource {
            status: "candidate".into(),
            valid_until: None,
            ..r.clone()
        };
        assert_eq!(retry_after(&E::Network, &candidate, now), Duration::days(1));
        // An expired template inside the grace period is retried in steps
        // (it can still recover); past the grace period it is not.
        let expired = template(-Duration::hours(10), 1, now);
        assert_eq!(retry_after(&E::Network, &expired, now), Duration::hours(3));
        let old = template(-Duration::hours(GRACE_HOURS + 1), 1, now);
        assert_eq!(retry_after(&E::Network, &old, now), Duration::days(2));
    }

    #[test]
    fn grace_renewal_needs_an_unchanged_template_expired_under_72_hours() {
        let now = Utc::now();
        let same = Observation {
            hash: "file".into(),
            source_hash: "source".into(),
            file_format: Some("hwp".into()),
            size: Some(10),
            links: vec![],
            form: None,
        };
        let expired = template(-Duration::hours(30), 0, now);
        assert!(may_grace_renew(&expired, &same, false, now));
        assert!(
            !may_renew(&expired, false, now),
            "regular renewal still refuses"
        );
        // Live rows use the regular renewal, not the grace path.
        assert!(!may_grace_renew(
            &template(Duration::hours(5), 0, now),
            &same,
            false,
            now
        ));
        // Past 72 hours: human review.
        let old = template(-Duration::hours(GRACE_HOURS), 0, now);
        assert!(!may_grace_renew(&old, &same, false, now));
        // Any hash difference, or a sticky changed flag, never renews.
        for observed in [
            Observation {
                hash: "other".into(),
                ..same_clone(&same)
            },
            Observation {
                source_hash: "other".into(),
                ..same_clone(&same)
            },
        ] {
            assert!(!may_grace_renew(&expired, &observed, false, now));
        }
        assert!(!may_grace_renew(&expired, &same, true, now));
        let flagged = Resource {
            check_status: Some("changed".into()),
            ..expired.clone()
        };
        assert!(!may_grace_renew(&flagged, &same, false, now));
        let no_hash = Resource {
            source_hash: None,
            ..expired.clone()
        };
        assert!(!may_grace_renew(&no_hash, &same, false, now));
        // Templates only.
        for kind in ["notice", "application"] {
            let page = Resource {
                kind: kind.into(),
                ..expired.clone()
            };
            assert!(!may_grace_renew(&page, &same, false, now), "{kind}");
        }
    }

    fn same_clone(o: &Observation) -> Observation {
        Observation {
            hash: o.hash.clone(),
            source_hash: o.source_hash.clone(),
            file_format: o.file_format.clone(),
            size: o.size,
            links: vec![],
            form: None,
        }
    }

    /// Read-only dry run of [`noise_reason`] over a production export
    /// (`scripts/forms/auto/noise-export.sql`, one JSON array of candidate
    /// template rows). Writes nothing.
    /// `NOISE_DRY_RUN=/path/noise.json cargo test -p mazimi-worker --lib noise_dry_run -- --ignored --nocapture`
    #[test]
    #[ignore = "needs NOISE_DRY_RUN export"]
    fn noise_dry_run() {
        use std::collections::BTreeMap;
        let Ok(path) = std::env::var("NOISE_DRY_RUN") else {
            return;
        };
        let text = std::fs::read_to_string(path).unwrap();
        let start = text
            .find("[{")
            .or_else(|| text.find("[]"))
            .expect("JSON array");
        let end = text.rfind(']').unwrap();
        let rows: Vec<serde_json::Value> = serde_json::from_str(&text[start..=end]).unwrap();
        let mut counts: BTreeMap<String, i64> = BTreeMap::new();
        for value in &rows {
            let row: NoiseRow = serde_json::from_value(value.clone()).unwrap();
            let reason = noise_reason(&row).unwrap_or("kept");
            let hashed = if value["hashed"].as_bool().unwrap_or(false) {
                "hashed"
            } else {
                "no hash"
            };
            *counts.entry(format!("{reason} ({hashed})")).or_default() += 1;
            *counts.entry(format!("{reason} (all)")).or_default() += 1;
            println!("{} {reason} {hashed}", &row.id.to_string()[..8]);
        }
        for (key, n) in &counts {
            println!("{n:>5}  {key}");
        }
        println!("{:>5}  candidate templates", rows.len());
    }

    #[test]
    fn noise_rules_match_the_pipeline_selection() {
        let row =
            |label: &str, depth: i32, source: Option<&str>, shared: i64, ended: &[&str]| NoiseRow {
                id: uuid::Uuid::nil(),
                revision: 1,
                label: label.into(),
                discovery_depth: depth,
                program_title: "청년 우울증 치료비 지원".into(),
                source_label: source.map(Into::into),
                programs_sharing_url: shared,
                ended_titles: ended.iter().map(|t| t.to_string()).collect(),
                auto_rejected: false,
            };
        assert_eq!(
            noise_reason(&row("응시원서.hwp", 1, None, 1, &[])),
            Some("recruitment_document")
        );
        assert_eq!(
            noise_reason(&row("신청서.hwp", 2, None, 3, &[])),
            Some("shared_attachment")
        );
        // On the programme's own page (depth 1) a shared city form is kept.
        assert_eq!(noise_reason(&row("신청서.hwp", 1, None, 3, &[])), None);
        assert_eq!(noise_reason(&row("신청서.hwp", 1, None, 2, &[])), None);
        // Earlier round (review 2): only through a link, and only an ended
        // round of the same programme.
        let round1 = ["청년 우울증 치료비 지원 1차"];
        assert_eq!(
            noise_reason(&row("신청서.hwp", 2, None, 2, &round1)),
            Some("earlier_round")
        );
        // A stable yearly form URL on the programme's own page is kept.
        assert_eq!(
            noise_reason(&row(
                "신청서.hwp",
                1,
                None,
                2,
                &["2025년 청년 우울증 치료비 지원"]
            )),
            None
        );
        // Sibling programmes sharing one city form: an ended sibling is not
        // an earlier round of this programme.
        assert_eq!(
            noise_reason(&row("신청서.hwp", 2, None, 2, &["결혼장려금 지원"])),
            None
        );
        // Existing rows (only the caption is stored): an umbrella caption, or
        // a form whose own title names the programme, keeps the row.
        let mind = |label: &str, caption: &str| NoiseRow {
            program_title: "청년 마음건강 지원사업".into(),
            ..row(label, 2, Some(caption), 1, &[])
        };
        assert_eq!(
            noise_reason(&mind(
                "신청서.hwp",
                "2026년 상반기 청년 지원사업 통합 모집공고"
            )),
            None
        );
        assert_eq!(
            noise_reason(&mind(
                "청년 마음건강 지원사업 신청서.hwp",
                "청년 심리상담 바우처 참여자 모집"
            )),
            None
        );
        assert_eq!(
            noise_reason(&mind("신청서.hwp", "청년 심리상담 바우처 참여자 모집")),
            Some("unrelated_source_page")
        );
        for caption in [
            "최종합격자 발표",
            "하반기 지방공무원 임용시험 시행계획 공고",
            "상반기 기간제근로자 채용 공고",
            "OO종합사회복지관 프로그램 참여자 모집",
        ] {
            assert_eq!(
                noise_reason(&mind("신청서.hwp", caption)),
                Some("unrelated_source_page"),
                "{caption}"
            );
        }
        let unrelated = Some("2026년 제5회 임기제공무원 채용시험 시행계획 공고");
        assert_eq!(
            noise_reason(&row("신청서.hwp", 2, unrelated, 1, &[])),
            Some("unrelated_source_page")
        );
        // Found on the programme's own page: the page rule does not apply.
        assert_eq!(noise_reason(&row("신청서.hwp", 1, unrelated, 1, &[])), None);
        assert_eq!(
            noise_reason(&row(
                "신청서.hwp",
                2,
                Some("청년 우울증 치료비 추가모집 공고"),
                1,
                &[]
            )),
            None
        );
        // A caption that names nothing, or no stored page: kept.
        assert_eq!(
            noise_reason(&row("신청서.hwp", 2, Some("모집공고"), 1, &[])),
            None
        );
        assert_eq!(noise_reason(&row("신청서.hwp", 2, None, 1, &[])), None);
    }

    #[test]
    fn templates_come_only_from_the_programmes_own_or_related_pages() {
        use discovery::page_may_offer_templates as may;
        let program = "청년 면접 정장 지원";
        assert!(may(0, program, "", program));
        assert!(!may(
            1,
            "new 행정지원과 채용시험 최종합격자 발표",
            "공지사항 | 원주시",
            program
        ));
        assert!(may(1, "2026 청년 면접정장 무료대여 모집", "", program));
        // A caption that names nothing falls back to the page title.
        assert!(may(
            1,
            "모집공고",
            "청년 면접정장 무료대여 안내 | 원주시",
            program
        ));
        assert!(!may(1, "모집공고", "공지사항 | 원주시청", program));
        // Either the caption or the title may show the page is the programme's.
        assert!(may(
            1,
            "2026년 상반기 청년 지원사업 통합 공고",
            "청년 면접정장 무료대여 | 원주시",
            program
        ));
        assert!(may(
            1,
            "2027 지역대기질 개선 시범사업 공고",
            "청년 면접정장 | 원주시",
            program
        ));
        assert!(!may(
            1,
            "2027 지역대기질 개선 시범사업 공고",
            "공지사항 | 원주시",
            program
        ));
    }

    #[test]
    fn the_forms_flag_is_off_unless_set_and_on_needs_a_reviewer_id() {
        assert_eq!(
            FormsPublish::from_values(None, None, None).unwrap(),
            FormsPublish::Off
        );
        assert_eq!(
            FormsPublish::from_values(Some("dry_run"), None, None).unwrap(),
            FormsPublish::DryRun
        );
        assert!(FormsPublish::from_values(Some("on"), None, None).is_err());
        assert!(FormsPublish::from_values(Some("on"), Some("not-a-uuid"), None).is_err());
        let id = uuid::Uuid::new_v4();
        assert_eq!(
            FormsPublish::from_values(Some("on"), Some(&id.to_string()), None).unwrap(),
            FormsPublish::On(id)
        );
        assert!(FormsPublish::from_values(Some("yes"), None, None).is_err());
        // Its own account: never nil, never the form-auto importer's.
        let nil = uuid::Uuid::nil().to_string();
        assert!(FormsPublish::from_values(Some("on"), Some(&nil), None).is_err());
        assert!(FormsPublish::from_values(
            Some("on"),
            Some(&id.to_string()),
            Some(&format!(" {id} "))
        )
        .is_err());
        assert!(FormsPublish::from_values(
            Some("on"),
            Some(&id.to_string()),
            Some(&uuid::Uuid::new_v4().to_string())
        )
        .is_ok());
    }

    #[test]
    fn xhtml_is_html() {
        assert!(is_html("application/xhtml+xml; charset=utf-8"));
        assert!(is_html("text/html"));
        assert!(!is_html("application/pdf"));
    }
}
