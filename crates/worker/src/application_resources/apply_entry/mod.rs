//! Automatic "신청" entry points: for each current programme, the page
//! where the user actually applies, read from the official page's own
//! structure and verified before anyone sees it.
//!
//! 1. The method gate: a programme whose application method has no online
//!    channel (visit, post, phone, fax, e-mail, app only, or no application
//!    at all) is recorded as `offline` and gets no link
//!    ([`mazimi_core::application_workflow::Workflow::accepts_online`], the
//!    rule the API uses to offer an application link).
//! 2. A per-host reader ([`readers`]) says where the official page's own
//!    신청 button points. Ties between equally placed elements on one page
//!    may go to the local model as a closed yes/no question ([`judge`]).
//! 3. Verification: the target is on the official page's institution, a
//!    government domain or a listed public institution or bank; it answers
//!    200 (or redirects into its own sign-in page: `login_required`); it is
//!    not a site home; and it is an application step (an application form,
//!    or a sign-in gate). A target that is another notice is read once more
//!    with the structural reader (at most two hops from the official page).
//! 4. Stability: a passing target becomes a `candidate`; the next check at
//!    least `APPLY_LINKS_CONFIRM_AFTER_MINUTES` later that derives the same
//!    target with the same step makes it `verified` (reviewed by
//!    [`mazimi_core::application_resources::AUTOMATED_REVIEWER`], valid 7
//!    days, re-checked every 3). A target that fails is stored `rejected`
//!    with its reason. A human review always wins and is never overridden.
//!
//! GET only, robots.txt obeyed, an honest user agent, one request per
//! origin per second at most with backoff ([`fetch`]). No login, no form
//! submission, nothing about users is read or written.
#[cfg(test)]
mod db_tests;
pub mod fetch;
#[cfg(test)]
mod fixture_tests;
pub mod judge;
pub mod readers;
pub mod repository;
#[cfg(test)]
mod sample_tests;
pub mod structure;

use crate::link_resolver::classify::{is_generic, is_login_url};
use fetch::{Fetched, Session};
use futures_util::StreamExt;
use mazimi_core::{
    application_resources::{automatic_apply_host, digest, public_institution_host},
    application_workflow,
    models::Program,
    source_policy::registrable_domain,
};
use readers::{Entry, Inputs, Reader};
use reqwest::Url;
use serde_json::Value;
use sqlx::PgPool;
use std::{collections::BTreeMap, time::Duration};
use structure::Step;

/// The hourly job has a 25 minute hard cap; work stops well before it.
const RUN_BUDGET: Duration = Duration::from_secs(20 * 60);
/// One programme's derivation and checks never take longer.
const PROGRAMME_LIMIT: Duration = Duration::from_secs(120);
/// Official hosts worked on at once (each still paced per origin).
const LANES: usize = 4;
/// Hops from the official page to the application step.
pub const MAX_DEPTH: i32 = 2;

/// A programme as the job reads it.
#[derive(Debug, Clone)]
pub struct Programme {
    pub program: Program,
    /// 보조금24 `serviceDetail` fields (gov_benefits only).
    pub detail: Option<Value>,
    /// The official URL was typed by an admin: it vouches for no host.
    pub admin_official_url: bool,
    /// Hash of every input the readers use (see [`repository::INPUT_HASH_SQL`]).
    pub input_hash: String,
}

impl Programme {
    fn vouching_official(&self) -> Option<&str> {
        self.program
            .official_url
            .as_deref()
            .filter(|_| !self.admin_official_url)
    }
    fn inputs(&self) -> Inputs<'_> {
        Inputs {
            source_type: &self.program.source_type,
            official_url: self.program.official_url.as_deref(),
            raw: self.program.raw_payload.as_ref(),
            detail: self.detail.as_ref(),
        }
    }
}

/// A target that passed a check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Passed {
    pub reader: Reader,
    pub url: String,
    pub label: String,
    pub source_url: String,
    pub depth: i32,
    pub why: String,
    /// direct | login_required
    pub access: &'static str,
    /// application_form | login_gate | login_form
    pub step: &'static str,
    pub final_url: String,
    /// What the official page pointed to (reader, page, target).
    pub source_hash: String,
    /// What the target is (step, access, final page).
    pub content_hash: String,
}

/// A derived target that failed verification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failed {
    pub reader: Reader,
    pub url: String,
    pub label: String,
    pub source_url: String,
    pub depth: i32,
    pub why: String,
    pub code: String,
}

/// One programme's result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The method has no online channel: no link.
    Offline {
        reason: &'static str,
    },
    /// The official page offers no 신청 element (reason code).
    NotFound {
        reader: Reader,
        reason: String,
    },
    /// Several equally placed elements and no decision.
    Ambiguous {
        reader: Reader,
        source_url: String,
    },
    /// The official page or the target did not answer usably (robots,
    /// network, refusal): nothing changes, retried later.
    Unreachable {
        reader: Reader,
        code: String,
    },
    Passed(Passed),
    Failed(Failed),
}

impl Outcome {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Offline { .. } => "offline",
            Self::NotFound { .. } => "not_found",
            Self::Ambiguous { .. } => "ambiguous",
            Self::Unreachable { .. } => "unreachable",
            Self::Passed(_) => "passed",
            Self::Failed(_) => "failed",
        }
    }
}

fn same_site(a: &str, b: &str) -> bool {
    let site = |u: &str| {
        Url::parse(u)
            .ok()
            .and_then(|u| u.host_str().and_then(registrable_domain))
    };
    site(a).is_some() && site(a) == site(b)
}

fn host_of(url: &str) -> String {
    Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
        .unwrap_or_default()
}

/// A sign-in URL without its query (it carries return paths and nonces).
fn gate_identity(url: &str) -> String {
    Url::parse(url)
        .map(|mut u| {
            u.set_query(None);
            u.set_fragment(None);
            u.to_string()
        })
        .unwrap_or_else(|_| url.to_string())
}

struct Link {
    url: String,
    label: String,
    source_url: String,
    why: String,
}

/// A tie on one page: the local model may name one; else ambiguous.
async fn settle(entry: Entry, judge: Option<&judge::Judge>, title: &str) -> Result<Link, Entry> {
    match entry {
        Entry::Link {
            url,
            label,
            source_url,
            why,
        } => Ok(Link {
            url,
            label,
            source_url,
            why,
        }),
        Entry::Tied { source_url, found } => {
            let Some(judge) = judge else {
                return Err(Entry::Tied { source_url, found });
            };
            let pairs: Vec<(String, String)> = found
                .iter()
                .map(|f| (f.label.clone(), f.url.clone()))
                .collect();
            match judge.choose(title, &pairs).await {
                Some(i) => Ok(Link {
                    url: found[i].url.clone(),
                    label: found[i].label.clone(),
                    source_url,
                    why: format!("{}+judge", found[i].why),
                }),
                None => Err(Entry::Tied { source_url, found }),
            }
        }
        none => Err(none),
    }
}

/// One programme, end to end (no database writes).
pub async fn evaluate(session: &Session, judge: Option<&judge::Judge>, p: &Programme) -> Outcome {
    let workflow = application_workflow::classify(p.program.application_method.as_deref());
    if !workflow.accepts_online() {
        return Outcome::Offline {
            reason: if workflow.no_application_required {
                "no_application_required"
            } else {
                "offline_method"
            },
        };
    }
    let inputs = p.inputs();
    let reader = readers::reader_for(&inputs);
    let entry = match reader {
        Reader::Gov24 => readers::gov24(&inputs),
        Reader::YouthCenter => readers::youthcenter(&inputs),
        Reader::Bokjiro | Reader::Generic => {
            let Some(official) = p.program.official_url.as_deref() else {
                return Outcome::NotFound {
                    reader,
                    reason: "no_official_url".into(),
                };
            };
            if reader == Reader::Generic && is_generic(official) {
                // A site home's 신청 links are the site's, not the programme's.
                Entry::None("official_is_home")
            } else {
                match session.get(official).await {
                    Fetched::Page {
                        status,
                        final_url,
                        html: Some(html),
                    } if (200..300).contains(&status) => {
                        if reader == Reader::Bokjiro {
                            readers::bokjiro(&final_url, &html)
                        } else if is_generic(&final_url) {
                            Entry::None("official_redirects_home")
                        } else {
                            readers::generic(&final_url, &html)
                        }
                    }
                    fetched if fetched.soft() => {
                        return Outcome::Unreachable {
                            reader,
                            code: code_of(&fetched),
                        }
                    }
                    Fetched::LoginGate { .. } => Entry::None("official_behind_login"),
                    Fetched::Page {
                        html: None, status, ..
                    } if status < 400 => Entry::None("official_not_html"),
                    _ => Entry::None("official_page_gone"),
                }
            }
        }
    };
    match settle(entry, judge, &p.program.title).await {
        Ok(link) => verify(session, judge, p, reader, link, 1).await,
        Err(Entry::Tied { source_url, .. }) => Outcome::Ambiguous { reader, source_url },
        Err(Entry::None(reason)) => Outcome::NotFound {
            reader,
            reason: reason.into(),
        },
        Err(Entry::Link { .. }) => unreachable!("settle passes links through"),
    }
}

fn code_of(fetched: &Fetched) -> String {
    match fetched {
        Fetched::Blocked => "robots".into(),
        Fetched::Error { code } => code.clone(),
        Fetched::Page { status, .. } => format!("http_{status}"),
        Fetched::LoginGate { .. } => "login_gate".into(),
    }
}

/// Is `link` an application step this programme may show?
fn verify<'a>(
    session: &'a Session,
    judge: Option<&'a judge::Judge>,
    p: &'a Programme,
    reader: Reader,
    link: Link,
    depth: i32,
) -> futures_util::future::BoxFuture<'a, Outcome> {
    Box::pin(async move {
        let official = p.vouching_official();
        let fail = |code: &str| {
            Outcome::Failed(Failed {
                reader,
                url: link.url.clone(),
                label: link.label.clone(),
                source_url: link.source_url.clone(),
                depth,
                why: link.why.clone(),
                code: code.into(),
            })
        };
        if !automatic_apply_host(official, &link.url) && !session.approved_host(&link.url) {
            return fail("off_domain");
        }
        if is_generic(&link.url) {
            return fail("site_home");
        }
        if is_login_url(&link.url) {
            // A sign-in page itself leads to no particular application.
            return fail("login_page");
        }
        let pass = |access: &'static str, step: &'static str, final_url: String| {
            Outcome::Passed(Passed {
                reader,
                source_hash: digest(
                    serde_json::json!([reader.as_str(), link.source_url, link.url])
                        .to_string()
                        .as_bytes(),
                ),
                content_hash: digest(
                    serde_json::json!([step, access, final_url])
                        .to_string()
                        .as_bytes(),
                ),
                url: link.url.clone(),
                label: link.label.clone(),
                source_url: link.source_url.clone(),
                depth,
                why: link.why.clone(),
                access,
                step,
                final_url,
            })
        };
        let fetched = session.get(&link.url).await;
        match fetched {
            Fetched::LoginGate { from, at } => {
                if same_site(&from, &at) || public_institution_host(&host_of(&at)) {
                    pass("login_required", "login_gate", gate_identity(&at))
                } else {
                    fail("login_off_site")
                }
            }
            ref f if f.soft() => Outcome::Unreachable {
                reader,
                code: code_of(f),
            },
            Fetched::Page { status, .. } if status >= 400 => fail(&format!("http_{status}")),
            Fetched::Page {
                final_url, html, ..
            } => {
                if !automatic_apply_host(official, &final_url) && !session.approved_host(&final_url)
                {
                    return fail("redirect_off_domain");
                }
                if is_generic(&final_url) {
                    return fail("redirect_to_home");
                }
                let Some(html) = html else {
                    return fail("not_a_page");
                };
                let final_identity = super::discovery::normalize_link(&final_url);
                match structure::step(&html) {
                    Step::ApplicationForm => pass("direct", "application_form", final_identity),
                    Step::LoginForm => pass("login_required", "login_form", final_identity),
                    Step::Other if depth < MAX_DEPTH => {
                        match settle(readers::generic(&final_url, &html), judge, &p.program.title)
                            .await
                        {
                            Ok(next) => verify(session, judge, p, reader, next, depth + 1).await,
                            Err(Entry::Tied { source_url, .. }) => {
                                Outcome::Ambiguous { reader, source_url }
                            }
                            Err(_) => fail("no_apply_step"),
                        }
                    }
                    Step::Other => fail("no_apply_step"),
                }
            }
            // Blocked and transport errors are soft (handled above).
            _ => fail("unexpected"),
        }
    })
}

#[derive(Debug, Default, serde::Serialize)]
pub struct Summary {
    pub programmes: usize,
    pub deferred: usize,
    pub outcomes: BTreeMap<String, usize>,
    pub recorded: repository::Recorded,
    pub unrecorded: usize,
    pub requests_cached: usize,
}

pub struct Job {
    max_programmes: i64,
    pub confirm_after: chrono::Duration,
    judge: Option<judge::Judge>,
}

impl Job {
    /// `None` unless `APPLY_LINKS_ENABLED` is `true` or `1`.
    pub fn from_env() -> anyhow::Result<Option<Self>> {
        if !matches!(
            std::env::var("APPLY_LINKS_ENABLED")
                .unwrap_or_default()
                .as_str(),
            "true" | "1"
        ) {
            return Ok(None);
        }
        fn bound(key: &str, default: i64, min: i64, max: i64) -> anyhow::Result<i64> {
            let value = std::env::var(key)
                .ok()
                .map(|v| v.parse::<i64>())
                .transpose()?
                .unwrap_or(default);
            anyhow::ensure!((min..=max).contains(&value), "{key} out of bounds");
            Ok(value)
        }
        Ok(Some(Self {
            max_programmes: bound("APPLY_LINKS_MAX_PROGRAMMES", 150, 1, 2_000)?,
            confirm_after: chrono::Duration::minutes(bound(
                "APPLY_LINKS_CONFIRM_AFTER_MINUTES",
                60,
                0,
                24 * 60,
            )?),
            judge: judge::Judge::from_env()?,
        }))
    }

    pub fn new(
        max_programmes: i64,
        confirm_after: chrono::Duration,
        judge: Option<judge::Judge>,
    ) -> Self {
        Self {
            max_programmes,
            confirm_after,
            judge,
        }
    }

    /// One bounded pass over due programmes, four official hosts at a time.
    pub async fn run(&self, pool: &PgPool) -> anyhow::Result<Summary> {
        let deadline = tokio::time::Instant::now() + RUN_BUDGET;
        let due = repository::due(pool, self.max_programmes).await?;
        let session = Session::new(pool.clone(), crate::sources::http::public_api_user_agent())
            .with_approved(crate::link_resolver::repository::approved_domains(pool).await?);
        let mut lanes: BTreeMap<String, Vec<Programme>> = BTreeMap::new();
        for p in due {
            let host = p
                .program
                .official_url
                .as_deref()
                .map(host_of)
                .unwrap_or_default();
            lanes.entry(host).or_default().push(p);
        }
        let mut summary = Summary::default();
        let results = futures_util::stream::iter(lanes.into_values())
            .map(|programmes| {
                let (session, pool) = (session.clone(), pool.clone());
                async move {
                    let mut local: Vec<Result<&'static str, (&'static str, anyhow::Error)>> =
                        vec![];
                    let mut recorded = repository::Recorded::default();
                    let mut deferred = 0;
                    for p in programmes {
                        if tokio::time::Instant::now() >= deadline {
                            deferred += 1;
                            continue;
                        }
                        let outcome = tokio::time::timeout(
                            PROGRAMME_LIMIT,
                            evaluate(&session, self.judge.as_ref(), &p),
                        )
                        .await
                        .unwrap_or_else(|_| Outcome::Unreachable {
                            reader: readers::reader_for(&p.inputs()),
                            code: "programme_timeout".into(),
                        });
                        let name = outcome.name();
                        match repository::record(
                            &pool,
                            &p,
                            &outcome,
                            self.confirm_after,
                            &mut recorded,
                        )
                        .await
                        {
                            Ok(()) => local.push(Ok(name)),
                            Err(error) => local.push(Err((name, error))),
                        }
                    }
                    (local, recorded, deferred)
                }
            })
            .buffer_unordered(LANES)
            .collect::<Vec<_>>()
            .await;
        for (local, recorded, deferred) in results {
            summary.deferred += deferred;
            summary.recorded.add(&recorded);
            for result in local {
                summary.programmes += 1;
                match result {
                    Ok(name) => *summary.outcomes.entry(name.into()).or_default() += 1,
                    Err((name, error)) => {
                        summary.unrecorded += 1;
                        *summary.outcomes.entry(name.into()).or_default() += 1;
                        tracing::warn!(error = %crate::error_safety::persisted_error(&error), "apply link not recorded");
                    }
                }
            }
        }
        summary.requests_cached = session.cached().await;
        Ok(summary)
    }
}
