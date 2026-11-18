//! P0 census: the collection policy in dry run over every active source
//! domain, every candidate and every monitor (design §6 P0).
//!
//! Read-only toward sources and publication: it fetches robots.txt, the
//! monitored pages, a bounded number of candidate pages and one terms page
//! per domain under the same rules as every other collector (eligible
//! domains only: `policy::ELIGIBLE_DOMAIN_SQL`), then writes
//! only `corporate_census_*`. It never changes crawl permission, a monitor,
//! a candidate or a programme.
//!
//! Shadow extraction runs the deterministic parsers on each fetched
//! monitored page and compares the result with the 2026-10-16 audit:
//! whether each audit quote is still on the page, what P1 adoption would
//! decide, and per fact type how many parsed values the audit text
//! corroborates. Corroboration is a lower bound on precision, not
//! precision: an auditor who did not mention a value did not refute it.

use anyhow::{Context, Result};
use chrono::Utc;
use regex::Regex;
use reqwest::Url;
use serde::Serialize;
use serde_json::{json, Value};
use sqlx::{FromRow, PgPool};
use std::{
    collections::{BTreeMap, HashMap},
    sync::LazyLock,
};
use tracing::{info, warn};
use uuid::Uuid;

use super::evidence::{self, audit, audit_fragments, Ages, AuditItem, PageEvidence};
use super::http::PageHttp;
use super::policy::{
    self, choose_lane, DomainSession, Fetched, Lane, LaneInput, PageState, RobotsState, TosSignal,
};
use super::Flags;
use crate::ai_extraction::crosscheck::parse_age;

#[derive(Debug, Clone)]
pub struct CensusSettings {
    /// Candidate pages probed per domain on top of every monitored page.
    pub max_candidate_probes: usize,
    /// Domains processed concurrently (requests to one domain stay serial).
    pub concurrency: usize,
}

impl Default for CensusSettings {
    fn default() -> Self {
        Self {
            max_candidate_probes: 3,
            concurrency: 4,
        }
    }
}

#[derive(Debug, Clone, FromRow)]
pub struct DomainRow {
    pub domain_id: Uuid,
    pub domain: String,
    pub gray_generic_tos: bool,
    pub gray_robots_missing: bool,
    pub gray_crawl_delay_min: bool,
    pub min_request_interval_ms: i32,
    pub max_pages_per_day: i32,
    pub feed_active: bool,
}

#[derive(Debug, Clone, FromRow)]
pub struct UrlRow {
    pub domain_id: Uuid,
    pub candidate_id: Uuid,
    pub url: String,
    /// Set for monitor rows; candidates without a monitor have none.
    pub program_id: Option<Uuid>,
    pub program_title: Option<String>,
    pub official_url: Option<String>,
    pub min_age: Option<i32>,
    pub max_age: Option<i32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    pub subject_kind: &'static str,
    pub domain_id: Uuid,
    pub candidate_id: Option<Uuid>,
    pub program_id: Option<Uuid>,
    pub url: Option<String>,
    pub host: Option<String>,
    pub lane: Lane,
    pub reason: &'static str,
    pub probed: bool,
    pub robots_state: &'static str,
    pub path_allowed: Option<bool>,
    pub search_engine_allowlist: bool,
    pub crawl_delay_ms: Option<i32>,
    pub tos_signal: TosSignal,
    pub http_status: Option<u16>,
    pub text_chars: Option<usize>,
    pub shadow: Option<Value>,
}

struct Probe {
    state: PageState,
    http_status: Option<u16>,
    text: Option<String>,
}

pub use super::policy::{LiveOptOuts, OptOutCheck};

/// Probes one domain. All requests go through one [`DomainSession`].
pub async fn probe_domain(
    session: &mut DomainSession<'_>,
    domain: &DomainRow,
    urls: &[UrlRow],
    flags: &Flags,
    settings: &CensusSettings,
    opted_out: &dyn OptOutCheck,
) -> Vec<Entry> {
    let gray_tos = flags.gray_generic_tos && domain.gray_generic_tos;
    let mut probes: HashMap<String, Probe> = HashMap::new();
    let mut robots_by_url: HashMap<String, RobotsState> = HashMap::new();
    let mut candidate_probes = 0usize;
    let mut tos: Option<TosSignal> = None;

    // Monitored URLs first, then candidates up to the cap.
    let mut ordered: Vec<&UrlRow> = urls.iter().filter(|u| u.program_id.is_some()).collect();
    ordered.extend(urls.iter().filter(|u| u.program_id.is_none()));
    for row in &ordered {
        let Ok(url) = Url::parse(&row.url) else {
            continue;
        };
        if domain.feed_active || opted_out.covers(&url).await {
            continue;
        }
        let robots = session.robots(&url).await;
        robots_by_url.insert(row.url.clone(), robots.clone());
        if probes.contains_key(&row.url) {
            continue;
        }
        if row.program_id.is_none() {
            if candidate_probes >= settings.max_candidate_probes {
                continue;
            }
            candidate_probes += 1;
        }
        if robots.allows(&policy::robots_target(&url), domain.gray_robots_missing) != Some(true) {
            continue;
        }
        let policy::Resolved {
            fetched,
            state,
            url,
        } = policy::fetch_resolving_same_host(
            session,
            &url,
            domain.gray_robots_missing,
            Some(opted_out),
        )
        .await;
        let (http_status, html) = match &fetched {
            Fetched::Reply(reply) => (
                Some(reply.status),
                matches!(state, PageState::Html { .. })
                    .then(|| {
                        crate::sources::benefit_pages::decode_page(&reply.body, &reply.content_type)
                            .ok()
                    })
                    .flatten(),
            ),
            _ => (None, None),
        };
        if matches!(state, PageState::NotProbed) {
            continue;
        }
        let text = html.as_deref().map(evidence::visible_text);
        if tos.is_none() {
            if let Some(html) = html.as_deref() {
                tos = Some(
                    policy::terms_signal(
                        session,
                        html,
                        &url,
                        domain.gray_robots_missing,
                        opted_out,
                    )
                    .await,
                );
            }
        }
        probes.insert(
            row.url.clone(),
            Probe {
                state,
                http_status,
                text,
            },
        );
    }
    let tos = tos.unwrap_or(TosSignal::Unknown);

    let mut entries = Vec::new();
    let first_robots = robots_by_url.values().next().cloned();
    let domain_robots = first_robots
        .clone()
        .unwrap_or(RobotsState::Unavailable("not_checked"));
    entries.push(Entry {
        subject_kind: "domain",
        domain_id: domain.domain_id,
        candidate_id: None,
        program_id: None,
        url: None,
        host: Some(domain.domain.clone()),
        lane: if domain.feed_active {
            Lane::Feed
        } else {
            Lane::Impossible
        },
        reason: if domain.feed_active {
            "partner_feed"
        } else {
            "domain_summary"
        },
        probed: !probes.is_empty(),
        robots_state: domain_robots.label(),
        path_allowed: None,
        search_engine_allowlist: domain_robots.search_engine_allowlist(),
        crawl_delay_ms: domain_robots.crawl_delay().map(|d| (d * 1000.0) as i32),
        tos_signal: tos,
        http_status: None,
        text_chars: None,
        shadow: Some(json!({"requests": session.requests(), "stopped": session.stopped()})),
    });

    for row in urls {
        let url = Url::parse(&row.url).ok();
        let opted = match url.as_ref() {
            Some(url) => opted_out.covers(url).await,
            None => false,
        };
        let robots = robots_by_url
            .get(&row.url)
            .cloned()
            .unwrap_or(RobotsState::Unavailable(if opted || domain.feed_active {
                "not_checked"
            } else {
                "domain_stopped"
            }));
        let path_allowed = url
            .as_ref()
            .and_then(|u| robots.allows(&policy::robots_target(u), domain.gray_robots_missing));
        let probe = probes.get(&row.url);
        // A URL left unfetched because the domain stopped (A1) carries the
        // stop, not "allowed".
        let not_probed = match session.stopped() {
            Some("rate_limited") => PageState::RateLimited(Utc::now()),
            Some("page_bot_wall") => PageState::BotWall,
            _ => PageState::NotProbed,
        };
        let page = probe.map_or(&not_probed, |p| &p.state);
        let (lane, reason) = choose_lane(&LaneInput {
            opted_out: opted,
            feed_active: domain.feed_active,
            robots: &robots,
            path_allowed,
            page,
            tos,
            gray_generic_tos: gray_tos,
            gray_robots_missing: domain.gray_robots_missing,
        });
        let shadow = match (row.program_id, probe.and_then(|p| p.text.as_deref())) {
            (Some(program_id), Some(text)) => Some(shadow_for(program_id, row, text)),
            _ => None,
        };
        entries.push(Entry {
            subject_kind: if row.program_id.is_some() {
                "monitor"
            } else {
                "candidate"
            },
            domain_id: domain.domain_id,
            candidate_id: Some(row.candidate_id),
            program_id: row.program_id,
            url: Some(row.url.chars().take(2_000).collect()),
            host: url.as_ref().and_then(|u| u.host_str().map(str::to_string)),
            lane,
            reason,
            probed: probe.is_some(),
            robots_state: robots.label(),
            path_allowed,
            search_engine_allowlist: robots.search_engine_allowlist(),
            crawl_delay_ms: robots.crawl_delay().map(|d| (d * 1000.0) as i32),
            tos_signal: tos,
            http_status: probe.and_then(|p| p.http_status),
            text_chars: probe.and_then(|p| match p.state {
                PageState::Html { text_chars } => Some(text_chars),
                _ => None,
            }),
            shadow,
        });
    }
    entries
}

// ── shadow extraction (deterministic pass B only) ─────────────────────────

static SENTENCE_SPLIT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[.!?。]\s+|\n").expect("valid"));
/// Whole-number percentages only ("1.5%" is not 5%; review M11).
static PERCENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:^|[^\d.,])(\d{1,2})\s*%\s*(할인|off|discount|적립)").expect("valid")
});
static KRW: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(\d{1,3}(?:,\d{3})+|\d{3,8})\s*원").expect("valid"));
static KRW_CUE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"할인|무료|적립|혜택|요금|월|결제|쿠폰").expect("valid"));
static FREE_PERIOD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(\d{1,2})\s*(개월|년|months?|years?)\s*(무료|free)|(?i)free\s+for\s+(\d{1,2})\s*(months?|years?)").expect("valid")
});
const VERIFICATION: &[(&str, &[&str])] = &[
    ("sheerid", &["sheerid"]),
    ("unidays", &["unidays"]),
    (
        "github_student",
        &["github student", "student developer pack"],
    ),
    (
        "school_email",
        &[
            "학교 이메일",
            "학교 메일",
            "school email",
            "university email",
            ".ac.kr",
            ".edu",
        ],
    ),
    ("student_id", &["학생증", "재학증명서", "student id"]),
];
const AUDIENCE: &[(&str, &[&str])] = &[
    (
        "university_student",
        &["대학생", "재학생", "university student", "college student"],
    ),
    ("graduate", &["대학원생", "graduate student"]),
    (
        "educator",
        &["교직원", "교사", "교육자", "educator", "teacher", "faculty"],
    ),
    ("high_school", &["고등학생", "high school"]),
];

fn clip(text: &str) -> String {
    text.chars().take(100).collect()
}

/// Deterministic shadow facts from a page's visible text.
pub fn shadow_facts(text: &str) -> Vec<Value> {
    let mut facts = Vec::new();
    let lower = text.to_lowercase();
    for sentence in SENTENCE_SPLIT.split(text) {
        let sentence = sentence.trim();
        if sentence.is_empty() || sentence.chars().count() > 400 {
            continue;
        }
        for c in PERCENT.captures_iter(sentence) {
            facts.push(json!({"type": "discount_percent", "value": format!("{}%", &c[1]), "quote": clip(sentence)}));
        }
        if KRW_CUE.is_match(sentence) {
            for c in KRW.captures_iter(sentence) {
                let value = c[1].replace(',', "");
                if value
                    .parse::<u64>()
                    .is_ok_and(|v| (100..=10_000_000).contains(&v))
                {
                    facts.push(
                        json!({"type": "price_krw", "value": value, "quote": clip(sentence)}),
                    );
                }
            }
        }
        for c in FREE_PERIOD.captures_iter(sentence) {
            let n = c.get(1).or(c.get(4)).map_or("", |m| m.as_str());
            let unit = c.get(2).or(c.get(5)).map_or("", |m| m.as_str());
            facts.push(json!({"type": "free_period", "value": format!("{n}{unit}"), "quote": clip(sentence)}));
        }
        if sentence.contains('세') {
            if let Ok(age) = parse_age(sentence) {
                facts.push(json!({"type": "age", "value": {"min": age.min, "max": age.max}, "quote": clip(sentence)}));
            }
        }
        if facts.len() >= 40 {
            break;
        }
    }
    for (code, words) in VERIFICATION {
        if words.iter().any(|w| lower.contains(w)) {
            facts.push(json!({"type": "verification", "value": code}));
        }
    }
    for (code, words) in AUDIENCE {
        if words.iter().any(|w| lower.contains(w)) {
            facts.push(json!({"type": "audience", "value": code}));
        }
    }
    facts
}

/// Whether the audit text mentions a shadow fact's value.
fn corroborated(fact: &Value, audit_text: &str) -> bool {
    let compact_audit = evidence::compact(audit_text).to_lowercase();
    match (fact["type"].as_str(), &fact["value"]) {
        (Some("age"), value) => {
            let min = value["min"].as_i64();
            let max = value["max"].as_i64();
            [min, max]
                .iter()
                .flatten()
                .all(|n| compact_audit.contains(&format!("{n}세")))
        }
        (Some("price_krw"), Value::String(v)) => {
            compact_audit.contains(v.as_str()) || compact_audit.contains(&with_commas(v))
        }
        (Some("verification" | "audience"), Value::String(code)) => {
            let words = VERIFICATION
                .iter()
                .chain(AUDIENCE)
                .find(|(c, _)| c == code)
                .map_or(&[][..], |(_, w)| *w);
            words
                .iter()
                .any(|w| compact_audit.contains(&evidence::compact(w).to_lowercase()))
        }
        (_, Value::String(v)) => compact_audit.contains(&evidence::compact(v).to_lowercase()),
        _ => false,
    }
}

fn with_commas(digits: &str) -> String {
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn audit_item_for(program_id: Uuid, row: &UrlRow) -> Option<&'static AuditItem> {
    audit().items.iter().find(|item| {
        item.id == program_id
            || (row.program_title.as_deref() == Some(item.title.as_str())
                && row.official_url.is_some()
                && row.official_url == item.official_url)
    })
}

fn shadow_for(program_id: Uuid, row: &UrlRow, text: &str) -> Value {
    let facts = shadow_facts(text);
    let Some(item) = audit_item_for(program_id, row) else {
        return json!({"facts": facts});
    };
    let audit_text = format!(
        "{} {}",
        item.quote.as_deref().unwrap_or(""),
        item.page_says.as_deref().unwrap_or("")
    );
    let facts: Vec<Value> = facts
        .into_iter()
        .map(|mut fact| {
            fact["audit_corroborated"] = Value::Bool(corroborated(&fact, &audit_text));
            fact
        })
        .collect();
    let page = PageEvidence::from_visible_text(text);
    let (evidence_present, would_adopt) = match audit_fragments(item) {
        Ok(fragments) => {
            let present: Vec<bool> = fragments.iter().map(|f| page.contains(f)).collect();
            let outcome = fragments
                .iter()
                .map(|f| {
                    evidence::evaluate_fact(
                        &page,
                        f,
                        None,
                        Ages {
                            min: row.min_age,
                            max: row.max_age,
                        },
                        true,
                    )
                })
                .find(|o| !matches!(o, evidence::FactOutcome::Confirmed { .. }))
                .map_or("confirmed", |o| o.label());
            (Value::from(present), Value::from(outcome))
        }
        Err(reason) => (Value::Null, Value::from(reason)),
    };
    json!({
        "facts": facts,
        "audit": {"verdict": item.verdict, "visible": item.visible,
                  "evidence_present": evidence_present, "would_adopt": would_adopt}
    })
}

// ── runner ────────────────────────────────────────────────────────────────

pub struct CensusRun {
    pub run_id: Uuid,
    pub entries: Vec<Entry>,
    pub summary: Value,
}

async fn load(pool: &PgPool) -> Result<(Vec<DomainRow>, Vec<UrlRow>)> {
    let domains = sqlx::query_as::<_, DomainRow>(&format!(
        "SELECT d.id AS domain_id, d.domain, d.gray_generic_tos, d.gray_robots_missing,
                d.gray_crawl_delay_min, d.min_request_interval_ms, d.max_pages_per_day,
                EXISTS (SELECT 1 FROM benefit_feeds f WHERE f.domain_id = d.id AND f.is_active
                        AND f.authorized_until > now()) AS feed_active
         FROM benefit_source_domains d
         WHERE {eligible}
         ORDER BY d.domain",
        eligible = policy::ELIGIBLE_DOMAIN_SQL
    ))
    .fetch_all(pool)
    .await
    .context("failed to load census domains")?;
    let urls = sqlx::query_as::<_, UrlRow>(&format!(
        "SELECT c.domain_id, c.id AS candidate_id, c.url, m.program_id, p.title AS program_title,
                p.official_url, p.min_age, p.max_age
         FROM benefit_source_candidates c
         JOIN program_source_monitors m ON m.candidate_id = c.id AND m.verification_state <> 'retired'
         JOIN programs p ON p.id = m.program_id
         WHERE NOT {hub}
         UNION ALL
         SELECT c.domain_id, c.id, c.url, NULL, NULL, NULL, NULL, NULL
         FROM benefit_source_candidates c
         WHERE c.status <> 'rejected'
           AND NOT {hub}
           AND NOT EXISTS (SELECT 1 FROM program_source_monitors m
                           WHERE m.candidate_id = c.id AND m.verification_state <> 'retired')
         ORDER BY 1, 4 NULLS LAST, 2",
        hub = policy::hub_owned("c")
    ))
    .fetch_all(pool)
    .await
    .context("failed to load census URLs")?;
    Ok((domains, urls))
}

/// Runs a full census and stores it. Returns the stored run.
pub async fn run(
    pool: &PgPool,
    flags: &Flags,
    http: &dyn PageHttp,
    settings: &CensusSettings,
) -> Result<CensusRun> {
    sqlx::query(
        "UPDATE corporate_census_runs SET status = 'failed', finished_at = now()
         WHERE status = 'running' AND started_at < now() - interval '12 hours'",
    )
    .execute(pool)
    .await?;
    let run_id: Uuid = sqlx::query_scalar("INSERT INTO corporate_census_runs (settings) VALUES ($1) RETURNING id")
        .bind(json!({"max_candidate_probes": settings.max_candidate_probes, "concurrency": settings.concurrency,
                     "gray_generic_tos": flags.gray_generic_tos, "user_agent": http.user_agent()}))
        .fetch_one(pool)
        .await?;
    let result = run_inner(pool, flags, http, settings, run_id).await;
    match result {
        Ok(run) => Ok(run),
        Err(error) => {
            let _ = sqlx::query("UPDATE corporate_census_runs SET status = 'failed', finished_at = now() WHERE id = $1")
                .bind(run_id)
                .execute(pool)
                .await;
            Err(error)
        }
    }
}

async fn run_inner(
    pool: &PgPool,
    flags: &Flags,
    http: &dyn PageHttp,
    settings: &CensusSettings,
    run_id: Uuid,
) -> Result<CensusRun> {
    use futures_util::stream::{self, StreamExt};
    let (domains, urls) = load(pool).await?;
    let mut by_domain: HashMap<Uuid, Vec<UrlRow>> = HashMap::new();
    for row in urls {
        by_domain.entry(row.domain_id).or_default().push(row);
    }
    let started = Utc::now();
    let mut jobs: Vec<
        std::pin::Pin<Box<dyn std::future::Future<Output = Vec<Entry>> + Send + '_>>,
    > = Vec::new();
    for domain in &domains {
        let rows = by_domain.remove(&domain.domain_id).unwrap_or_default();
        jobs.push(Box::pin(async move {
            let mut session = DomainSession::new(
                http,
                Some(pool),
                Some(domain.domain_id),
                domain.gray_crawl_delay_min,
                domain.min_request_interval_ms as i64,
                domain.max_pages_per_day.clamp(1, 200) as u32,
            );
            probe_domain(
                &mut session,
                domain,
                &rows,
                flags,
                settings,
                &LiveOptOuts(pool),
            )
            .await
        }));
    }
    let entries: Vec<Entry> = stream::iter(jobs)
        .buffer_unordered(settings.concurrency.clamp(1, 8))
        .collect::<Vec<Vec<Entry>>>()
        .await
        .into_iter()
        .flatten()
        .collect();

    store_entries(pool, run_id, &entries).await?;
    let mut summary = summarize(&entries);
    summary["run_id"] = json!(run_id);
    summary["started_at"] = json!(started);
    summary["finished_at"] = json!(Utc::now());
    sqlx::query("UPDATE corporate_census_runs SET status = 'completed', finished_at = now(), summary = $2 WHERE id = $1")
        .bind(run_id)
        .bind(&summary)
        .execute(pool)
        .await?;
    info!(event = "corporate_census.completed", run_id = %run_id, entries = entries.len(), "corporate census completed");
    Ok(CensusRun {
        run_id,
        entries,
        summary,
    })
}

async fn store_entries(pool: &PgPool, run_id: Uuid, entries: &[Entry]) -> Result<()> {
    let mut tx = pool.begin().await?;
    for entry in entries {
        sqlx::query(
            "INSERT INTO corporate_census_entries (
                 run_id, subject_kind, domain_id, candidate_id, program_id, url, host, lane, reason,
                 probed, robots_state, path_allowed, search_engine_allowlist, crawl_delay_ms,
                 tos_signal, http_status, text_chars, shadow
             ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18)",
        )
        .bind(run_id)
        .bind(entry.subject_kind)
        .bind(entry.domain_id)
        .bind(entry.candidate_id)
        .bind(entry.program_id)
        .bind(entry.url.as_deref())
        .bind(
            entry
                .host
                .as_deref()
                .map(|h| h.chars().take(253).collect::<String>()),
        )
        .bind(entry.lane.as_str())
        .bind(entry.reason)
        .bind(entry.probed)
        .bind(entry.robots_state)
        .bind(entry.path_allowed)
        .bind(entry.search_engine_allowlist)
        .bind(entry.crawl_delay_ms)
        .bind(entry.tos_signal.as_str())
        .bind(entry.http_status.map(i32::from))
        .bind(entry.text_chars.map(|c| c.min(i32::MAX as usize) as i32))
        .bind(&entry.shadow)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

fn bump(map: &mut BTreeMap<String, usize>, key: impl Into<String>) {
    *map.entry(key.into()).or_default() += 1;
}

/// The gate-readable summary of one census.
pub fn summarize(entries: &[Entry]) -> Value {
    let mut lanes: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    let mut reasons: BTreeMap<String, usize> = BTreeMap::new();
    let mut robots: BTreeMap<String, usize> = BTreeMap::new();
    let mut tos: BTreeMap<String, usize> = BTreeMap::new();
    let mut allowlist = 0usize;
    let mut audit_lanes: BTreeMap<String, usize> = BTreeMap::new();
    let mut adoption: BTreeMap<String, usize> = BTreeMap::new();
    let mut shadow_types: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for entry in entries {
        bump(
            lanes.entry(entry.subject_kind.to_string()).or_default(),
            entry.lane.as_str(),
        );
        if entry.subject_kind == "domain" {
            bump(&mut robots, entry.robots_state);
            bump(&mut tos, entry.tos_signal.as_str());
            allowlist += usize::from(entry.search_engine_allowlist);
            continue;
        }
        bump(
            &mut reasons,
            format!("{}:{}", entry.lane.as_str(), entry.reason),
        );
        if entry.subject_kind != "monitor" {
            continue;
        }
        let audit_item = entry
            .program_id
            .and_then(|id| audit().items.iter().find(|i| i.id == id));
        let shadow_audit = entry.shadow.as_ref().and_then(|s| s.get("audit"));
        let verdict = audit_item
            .map(|i| i.verdict.clone())
            .or_else(|| shadow_audit.and_then(|a| a["verdict"].as_str().map(str::to_string)));
        if let Some(verdict) = verdict {
            bump(
                &mut audit_lanes,
                format!("{verdict}:{}", entry.lane.as_str()),
            );
        }
        if let Some(audit) = shadow_audit {
            bump(
                &mut adoption,
                format!(
                    "{}:{}",
                    audit["verdict"].as_str().unwrap_or("?"),
                    audit["would_adopt"].as_str().unwrap_or("?")
                ),
            );
        }
        if let Some(facts) = entry.shadow.as_ref().and_then(|s| s["facts"].as_array()) {
            for fact in facts {
                if let Some(corroborated) = fact.get("audit_corroborated").and_then(Value::as_bool)
                {
                    let slot = shadow_types
                        .entry(fact["type"].as_str().unwrap_or("?").to_string())
                        .or_default();
                    slot.0 += 1;
                    slot.1 += usize::from(corroborated);
                }
            }
        }
    }
    let shadow: BTreeMap<String, Value> = shadow_types
        .into_iter()
        .map(|(kind, (support, corroborated))| {
            (kind, json!({"support": support, "audit_corroborated": corroborated,
                          "corroboration_rate": if support > 0 { corroborated as f64 / support as f64 } else { 0.0 },
                          "meets_support_20": support >= 20}))
        })
        .collect();
    json!({
        "lanes": lanes,
        "reasons": reasons,
        "domain_robots": robots,
        "domain_tos": tos,
        "domains_search_engine_allowlist": allowlist,
        "audited_monitor_lanes": audit_lanes,
        "p1_adoption_preview": adoption,
        "shadow_fact_types": shadow,
        "notes": [
            "audit_corroborated is a lower bound on precision: the audit text not mentioning a value does not refute it",
            "p1_adoption_preview runs the P1 evidence rule with no baseline on the live page fetched by this census"
        ]
    })
}

/// Human-readable lines for the summary file.
pub fn summary_text(summary: &Value) -> String {
    let mut out = format!(
        "Corporate census {}\nstarted {} finished {}\n\n",
        summary["run_id"], summary["started_at"], summary["finished_at"]
    );
    for key in [
        "lanes",
        "reasons",
        "domain_robots",
        "domain_tos",
        "audited_monitor_lanes",
        "p1_adoption_preview",
        "shadow_fact_types",
    ] {
        out.push_str(&format!("## {key}\n"));
        if let Some(map) = summary[key].as_object() {
            for (k, v) in map {
                out.push_str(&format!("  {k}: {v}\n"));
            }
        }
        out.push('\n');
    }
    out.push_str(&format!(
        "domains with a search-engine-only robots allowlist: {}\n",
        summary["domains_search_engine_allowlist"]
    ));
    out
}

/// Worker entry point: weekly, skipped while a recent run exists.
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
    run_scheduled_once(pool).await;
}

async fn run_scheduled_once(pool: &PgPool) {
    let flags = match Flags::from_env() {
        Ok(flags) if flags.census_enabled => flags,
        Ok(_) => return,
        Err(error) => {
            warn!(event = "corporate_census.config_invalid", error = %error, "corporate census misconfigured");
            return;
        }
    };
    let recent: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM corporate_census_runs
                        WHERE (status = 'completed' AND finished_at > now() - interval '6 days')
                           OR (status = 'running' AND started_at > now() - interval '12 hours'))",
    )
    .fetch_one(pool)
    .await
    .unwrap_or(true);
    if recent {
        return;
    }
    let Some(user_agent) = flags.user_agent() else {
        return;
    };
    let Ok(http) = super::http::PinnedHttp::new(user_agent, std::time::Duration::from_secs(20))
    else {
        return;
    };
    if let Err(error) = run(pool, &flags, &http, &CensusSettings::default()).await {
        warn!(event = "corporate_census.failed", error = %crate::error_safety::persisted_error(&error), "corporate census failed");
    }
}

#[cfg(test)]
#[path = "census_tests.rs"]
mod tests;
