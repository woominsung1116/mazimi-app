//! No arbitrary outbound client: approval, shared pacing, robots and DNS
//! pinning are checked for EVERY URL. Redirects are observations, not authority.
//! Scope validation, the domain request slot and Retry-After handling come
//! from `sources::source_access`, the same code the benefit-page collector uses.
use crate::sources::{
    benefit_pages::{
        build_pinned_client, fetch_robots_document, read_limited_body, robots_allows,
        BOT_PRODUCT_TOKEN,
    },
    source_access::{self, SlotOutcome},
};
use chrono::{DateTime, Utc};
use mazimi_core::source_policy::RobotsPolicy;
use reqwest::{
    header::{CONTENT_DISPOSITION, CONTENT_TYPE, LOCATION},
    Url,
};
use sqlx::{FromRow, PgPool};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};
use uuid::Uuid;

/// The robots policy for one URL, for the forms collector and the
/// apply-link reader: RFC 9309 on government hosts (the
/// `.go.kr` family the host rule already vouches for), where an HTML answer
/// or a 4xx means robots.txt is unavailable and so no restrictions; the
/// strict default everywhere else. 429, 5xx, timeouts and network failures
/// fail closed under both. The ingestion crawler, the approval preflight and
/// the live pages stay strict on every host, and the corporate crawler
/// (`corporate_auto::policy`) keeps its own strict reading.
pub fn robots_policy_for(url: &Url) -> RobotsPolicy {
    if url
        .host_str()
        .is_some_and(mazimi_core::application_resources::government_host)
    {
        RobotsPolicy::Rfc9309
    } else {
        RobotsPolicy::Strict
    }
}

#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("approval_required")]
    Approval,
    #[error("robots_unavailable_or_disallowed")]
    Robots,
    /// The domain slot is reserved beyond the shared wait cap; retry then.
    #[error("domain_cooldown")]
    Cooldown(DateTime<Utc>),
    #[error("unsafe_url_or_dns")]
    Unsafe,
    #[error("redirect_requires_review")]
    Redirect(Option<String>),
    #[error("source_notice_redirect_requires_review")]
    SourceRedirect,
    #[error("source_http_{0}")]
    Http(u16),
    #[error("network_timeout_or_failure")]
    Network,
    #[error("response_too_large_or_unreadable")]
    Body,
    #[error("unsupported_file_or_page")]
    Format,
    /// A page URL answered with a document: queued as a form candidate.
    #[error("page_is_a_document")]
    IsFile,
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}
impl FetchError {
    pub fn code(&self) -> String {
        // Database/transport details never enter operator or public evidence.
        if matches!(self, Self::Database(_)) {
            "database_error".into()
        } else {
            self.to_string()
        }
    }
}

#[derive(FromRow)]
pub(crate) struct Scope {
    pub(crate) id: Uuid,
    pub(crate) domain: String,
    pub(crate) allow_subdomains: bool,
    pub(crate) allowed_path_prefixes: Vec<String>,
    pub(crate) crawler_contact_url: String,
    pub(crate) min_request_interval_ms: i32,
}

/// An approved, unexpired, direct-collection domain scope (alias `d`). The
/// one definition for seeding, claiming and fetching.
pub(crate) const APPROVED_SCOPE_SQL: &str =
    "d.is_active AND d.verification_status='verified' AND d.crawl_allowed
    AND d.compliance_status='approved' AND d.robots_status='allowed'
    AND d.approval_expires_at>now() AND d.collection_mode='direct_public'";

/// SQL that is true when the host expression `host` is the scope's domain,
/// or one of its subdomains where the scope allows them. A coarse prefilter
/// only: [`scope_for`] makes the decision.
pub(crate) fn scope_host_sql(host: &'static str) -> String {
    format!("(d.domain={host} OR (d.allow_subdomains AND right({host},length(d.domain)+1)='.'||d.domain))")
}

/// Approved scopes, most specific domain first; `host` narrows the list.
pub(crate) async fn approved_scopes<'e, E: sqlx::PgExecutor<'e>>(
    db: E,
    host: Option<&str>,
) -> Result<Vec<Scope>, sqlx::Error> {
    sqlx::query_as::<_, Scope>(&format!(
        "SELECT id,domain,allow_subdomains,allowed_path_prefixes,crawler_contact_url,min_request_interval_ms
         FROM benefit_source_domains d WHERE {APPROVED_SCOPE_SQL} AND ($1::text IS NULL OR {host})
         ORDER BY length(d.domain) DESC",
        host = scope_host_sql("$1")
    ))
    .bind(host)
    .fetch_all(db)
    .await
}

/// The approved scope a public URL falls inside (host and path prefix), if
/// any. Nothing outside one is seeded, claimed or fetched.
pub(crate) fn scope_for<'a>(scopes: &'a [Scope], value: &str) -> Option<&'a Scope> {
    if !mazimi_core::application_resources::public_url(value) {
        return None;
    }
    scopes.iter().find(|s| {
        source_access::validate_scope(
            value,
            &s.domain,
            s.allow_subdomains,
            &s.allowed_path_prefixes,
        )
        .is_ok()
    })
}
pub struct Page {
    pub bytes: Vec<u8>,
    pub content_type: String,
    pub content_disposition: Option<String>,
}

/// Per-run state: robots.txt is fetched once per (approved scope, origin)
/// per run, since robots rules apply per authority (RFC 9309) and one
/// subdomain scope covers many hosts. Every slot wait fits inside the run
/// budget so the scheduled job finishes before its hard timeout.
pub struct Session {
    robots: HashMap<(Uuid, String), String>,
    deadline: Instant,
}
impl Session {
    pub fn new(budget: Duration) -> Self {
        Self {
            robots: HashMap::new(),
            deadline: Instant::now() + budget,
        }
    }
    pub fn remaining(&self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }
    fn robots_key(scope: Uuid, url: &Url) -> (Uuid, String) {
        (scope, url.origin().ascii_serialization())
    }
    /// `None` when this host's robots.txt has not been read in this run.
    fn robots_allow(&self, scope: Uuid, url: &Url) -> Option<bool> {
        // Check the actual query as well as the path against robots rules.
        let target = match url.query() {
            Some(query) => format!("{}?{query}", url.path()),
            None => url.path().into(),
        };
        self.robots
            .get(&Self::robots_key(scope, url))
            .map(|document| robots_allows(document, BOT_PRODUCT_TOKEN, &target))
    }
    fn store_robots(&mut self, scope: Uuid, url: &Url, document: String) {
        self.robots.insert(Self::robots_key(scope, url), document);
    }
}

async fn reserve(pool: &PgPool, session: &Session, scope: &Scope) -> Result<(), FetchError> {
    match source_access::reserve_request_slot(
        pool,
        scope.id,
        i64::from(scope.min_request_interval_ms),
        session.remaining(),
    )
    .await
    {
        Ok(SlotOutcome::Reserved) => Ok(()),
        Ok(SlotOutcome::Deferred(until)) => Err(FetchError::Cooldown(until)),
        Err(error) => match error.downcast::<sqlx::Error>() {
            Ok(db) => Err(FetchError::Database(db)),
            // Approval was revoked while waiting for the slot.
            Err(_) => Err(FetchError::Approval),
        },
    }
}

pub async fn get(
    pool: &PgPool,
    session: &mut Session,
    value: &str,
    limit: usize,
) -> Result<Page, FetchError> {
    // Slot waits never exceed the remaining run budget; each request has its
    // own 15 s client timeout. This outer cap covers both.
    match tokio::time::timeout(
        session.remaining() + Duration::from_secs(60),
        fetch(pool, session, value, limit),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Err(FetchError::Network),
    }
}

async fn fetch(
    pool: &PgPool,
    session: &mut Session,
    value: &str,
    limit: usize,
) -> Result<Page, FetchError> {
    if !mazimi_core::application_resources::public_url(value) {
        return Err(FetchError::Unsafe);
    }
    let url = Url::parse(value).map_err(|_| FetchError::Unsafe)?;
    let host = url.host_str().ok_or(FetchError::Unsafe)?;
    // Approval is re-read here, not trusted from the claim: it can be
    // revoked or expire between the two.
    let scopes = approved_scopes(pool, Some(host)).await?;
    let scope = scope_for(&scopes, value).ok_or(FetchError::Approval)?;
    let user_agent = format!("{BOT_PRODUCT_TOKEN}/1.0 (+{})", scope.crawler_contact_url);
    let client = build_pinned_client(&url, &user_agent, Duration::from_secs(15))
        .await
        .map_err(|_| FetchError::Unsafe)?;
    if session.robots_allow(scope.id, &url).is_none() {
        reserve(pool, session, scope).await?;
        let document = fetch_robots_document(
            &client,
            &url,
            &user_agent,
            Duration::from_secs(15),
            robots_policy_for(&url),
        )
        .await
        .map_err(|_| FetchError::Robots)?;
        session.store_robots(
            scope.id,
            &url,
            String::from_utf8_lossy(&document.bytes).into_owned(),
        );
    }
    if session.robots_allow(scope.id, &url) != Some(true) {
        return Err(FetchError::Robots);
    }
    reserve(pool, session, scope).await?;
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|_| FetchError::Network)?;
    let status = response.status();
    if matches!(status.as_u16(), 429 | 503) {
        source_access::record_retry_after(pool, scope.id, response.headers())
            .await
            .map_err(|error| match error.downcast::<sqlx::Error>() {
                Ok(db) => FetchError::Database(db),
                Err(_) => FetchError::Network,
            })?;
    }
    if status.is_redirection() {
        let target = response
            .headers()
            .get(LOCATION)
            .and_then(|h| h.to_str().ok())
            .and_then(|value| response.url().join(value).ok())
            .filter(|url| mazimi_core::application_resources::public_url(url.as_str()))
            .map(|url| url.to_string());
        return Err(FetchError::Redirect(target));
    }
    if status.as_u16() != 200 {
        return Err(FetchError::Http(status.as_u16()));
    }
    if response.content_length().is_some_and(|n| n > limit as u64) {
        return Err(FetchError::Body);
    }
    let header = |name| {
        response
            .headers()
            .get(name)
            .and_then(|h| h.to_str().ok())
            .map(str::to_owned)
    };
    let content_type = header(CONTENT_TYPE).unwrap_or_default().to_lowercase();
    let content_disposition = header(CONTENT_DISPOSITION);
    let bytes = read_limited_body(response, limit)
        .await
        .map_err(|_| FetchError::Body)?;
    Ok(Page {
        bytes,
        content_type,
        content_disposition,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Read-only dry run of the A3 seeding rule over an export of production
    /// rows (approved scopes, active programs' URLs, queued resources; query
    /// in the handoff report `stale-intake-signals.md`). Writes nothing.
    /// `SEED_DRY_RUN=/path/seed.jsonl [SEED_DRY_RUN_CLOSED=/path/ids]
    /// cargo test -p mazimi-worker --lib seed_dry_run -- --ignored --nocapture`
    #[test]
    #[ignore = "needs SEED_DRY_RUN export"]
    fn seed_dry_run() {
        use std::collections::{BTreeMap, HashSet};
        let Ok(path) = std::env::var("SEED_DRY_RUN") else {
            return;
        };
        let closing: HashSet<String> = std::env::var("SEED_DRY_RUN_CLOSED")
            .ok()
            .map(|p| {
                std::fs::read_to_string(p)
                    .unwrap()
                    .lines()
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        let rows: Vec<serde_json::Value> = std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).expect("export line is JSON"))
            .collect();
        let mut scopes: Vec<Scope> = rows
            .iter()
            .filter(|r| r["kind"] == "scope")
            .map(|r| Scope {
                id: Uuid::new_v4(),
                domain: r["domain"].as_str().unwrap().into(),
                allow_subdomains: r["allow_subdomains"].as_bool().unwrap(),
                allowed_path_prefixes: serde_json::from_value(r["prefixes"].clone()).unwrap(),
                crawler_contact_url: String::new(),
                min_request_interval_ms: 0,
            })
            .collect();
        scopes.sort_by_key(|s| std::cmp::Reverse(s.domain.len()));
        let mut counts: BTreeMap<String, i64> = BTreeMap::new();
        let mut bump = |key: &str, n: i64| *counts.entry(key.into()).or_default() += n;
        bump("approved scopes", scopes.len() as i64);
        let mut hosts: BTreeMap<String, i64> = BTreeMap::new();
        for r in rows.iter().filter(|r| r["kind"] == "program") {
            let p: mazimi_core::models::Program = serde_json::from_value(serde_json::json!({
                "id": r["id"], "program_type": "policy", "source_type": r["source_type"],
                "title": "dry run", "program_status": "active", "is_active": true,
                "created_at": Utc::now(), "updated_at": Utc::now(),
                "official_url": r["official_url"],
                "raw_payload": {"aplyUrlAddr": r["aplyUrlAddr"], "refUrlAddr1": r["refUrlAddr1"], "refUrlAddr2": r["refUrlAddr2"]},
            }))
            .unwrap();
            let available = r["available"].as_bool().unwrap();
            let after = available && !closing.contains(r["id"].as_str().unwrap());
            let seeds = super::super::discovery::seeds(&p);
            let eligible: Vec<_> = seeds
                .iter()
                .filter(|c| scope_for(&scopes, &c.url).is_some())
                .collect();
            for (label, on) in [
                ("available now", available),
                ("available after intake fixes", after),
            ] {
                if !on {
                    continue;
                }
                bump(&format!("{label}: programs"), 1);
                bump(
                    &format!("{label}: candidate URLs, old rule (all)"),
                    seeds.len() as i64,
                );
                bump(
                    &format!("{label}: candidate URLs, new rule (approved scope)"),
                    eligible.len() as i64,
                );
                if !eligible.is_empty() {
                    bump(&format!("{label}: programs with an eligible URL"), 1);
                }
            }
            if after {
                for c in &eligible {
                    let host = Url::parse(&c.url).unwrap().host_str().unwrap().to_string();
                    *hosts.entry(host).or_default() += 1;
                }
            }
        }
        for r in rows.iter().filter(|r| r["kind"] == "resource") {
            let due = r["due"].as_bool().unwrap()
                && r["program_available"].as_bool().unwrap()
                && r["status"] != "rejected";
            bump("queued resources", 1);
            if due {
                bump("queued resources due now, old claim rule", 1);
                if scope_for(&scopes, r["url"].as_str().unwrap()).is_some() {
                    bump("queued resources due now, new claim rule", 1);
                }
            }
            if scope_for(&scopes, r["url"].as_str().unwrap()).is_some() {
                bump("queued resources inside an approved scope", 1);
            }
        }
        for (key, value) in &counts {
            println!("{value:>7}  {key}");
        }
        for (host, n) in &hosts {
            println!("{n:>7}  eligible after: {host}");
        }
    }

    #[test]
    fn robots_are_cached_per_host_inside_one_subdomain_scope() {
        let scope = Uuid::new_v4();
        let www = Url::parse("https://www.busan.go.kr/notice/1").unwrap();
        let apply = Url::parse("https://apply.busan.go.kr/form?id=3").unwrap();
        let mut session = Session::new(Duration::from_secs(60));
        session.store_robots(scope, &www, "User-agent: *\nAllow: /\n".into());
        assert_eq!(session.robots_allow(scope, &www), Some(true));
        // Another host of the same scope is unknown until its own robots.txt
        // is read; www's rules never stand in for it.
        assert_eq!(session.robots_allow(scope, &apply), None);
        session.store_robots(scope, &apply, "User-agent: *\nDisallow: /\n".into());
        assert_eq!(session.robots_allow(scope, &apply), Some(false));
        assert_eq!(session.robots_allow(scope, &www), Some(true));
        // Same host, other path: cached, evaluated against the full target.
        let other = Url::parse("https://www.busan.go.kr/board?q=1").unwrap();
        assert_eq!(session.robots_allow(scope, &other), Some(true));
    }

    /// A resource on a site whose robots.txt lists the crawlers that may
    /// come, without us, is refused before its request (`fetch` asks
    /// `robots_allow` first).
    #[test]
    fn a_named_agent_allowlist_refuses_the_resource_request() {
        let scope = Uuid::new_v4();
        let form = Url::parse("https://www.example.or.kr/form/apply.hwp").unwrap();
        let mut session = Session::new(Duration::from_secs(60));
        session.store_robots(
            scope,
            &form,
            "User-agent: Yeti\nAllow: /\n\nUser-agent: Googlebot\nAllow: /\n".into(),
        );
        assert_eq!(session.robots_allow(scope, &form), Some(false));
    }
}
