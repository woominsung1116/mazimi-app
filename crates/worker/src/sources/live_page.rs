//! Live official-page fetch for the agent's `check_official_page`.
//!
//! Two halves, so the fetching process can run without database access:
//! - [`LiveFetcher`] (no database, no user data) runs inside the isolated
//!   fetch service. It takes the API's list of stored programme URLs with
//!   their approved scopes and fetches one with the collector's guards: the
//!   scope re-check, a DNS-pinned public-address client with no redirects
//!   and no proxy (`benefit_pages::build_pinned_client`), robots.txt
//!   (`fetch_robots_document`, `robots_allows`), a per-host interval, a
//!   body size cap (`read_limited_body`), a content-type check, the page
//!   decoder (`decode_page`) and the collector's text extraction
//!   (`extract_review_text`). Redirects are followed only on the same host
//!   inside the same scope (at most two hops).
//! - [`approved_targets`] and [`reserve_slot`] (database) run in the API:
//!   they read `benefit_source_domains` approvals and reserve the shared
//!   per-domain request slot the collectors use, so a live check counts
//!   against the same politeness budget.
use super::{
    benefit_pages::{
        build_pinned_client, decode_page, extract_review_text, fetch_robots_document,
        read_limited_body, retry_at, robots_allows, BOT_PRODUCT_TOKEN,
    },
    source_access::{self, SlotOutcome},
};
use crate::application_resources::fetch::{approved_scopes, scope_for};
use chrono::{NaiveDate, Utc};
use mazimi_core::official_page::{
    analyze, CheckRequest, CheckResponse, Outcome, Target, MAX_TARGETS,
};
use reqwest::{
    header::{ACCEPT, CONTENT_TYPE, LOCATION},
    Client, StatusCode, Url,
};
use sqlx::PgPool;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::Semaphore;
use uuid::Uuid;

/// Builds the HTTP client for one approved HTTPS URL and says which URL to
/// request with it. Production pins DNS to a public address; a test
/// connector may point at a loopback mock server.
#[async_trait::async_trait]
pub trait Connector: Send + Sync {
    async fn connect(
        &self,
        url: &Url,
        user_agent: &str,
        timeout: Duration,
    ) -> anyhow::Result<(Client, Url)>;
}

/// Official-page reads for the assistant keep the strict robots policy.
pub(crate) const LIVE_PAGE_ROBOTS: mazimi_core::source_policy::RobotsPolicy =
    mazimi_core::source_policy::RobotsPolicy::Strict;

/// DNS-pinned, public addresses only, no redirects, no proxy: the
/// collector's client.
pub struct PinnedConnector;

#[async_trait::async_trait]
impl Connector for PinnedConnector {
    async fn connect(
        &self,
        url: &Url,
        user_agent: &str,
        timeout: Duration,
    ) -> anyhow::Result<(Client, Url)> {
        Ok((
            build_pinned_client(url, user_agent, timeout).await?,
            url.clone(),
        ))
    }
}

#[derive(Debug, Clone)]
pub struct Limits {
    /// Largest page body read.
    pub max_bytes: usize,
    /// One whole visit (robots, page, redirects).
    pub visit_timeout: Duration,
    /// Longest wait for a host's interval before answering `cooldown`.
    pub max_wait: Duration,
    /// Visits at once in this process.
    pub concurrency: usize,
    /// How long a robots.txt document is reused (RFC 9309 allows 24 h).
    pub robots_ttl: Duration,
    /// Floor under a scope's own interval.
    pub min_interval: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_bytes: 512 * 1024,
            // max_wait + visit_timeout stays under the API's 11 s call timeout.
            visit_timeout: Duration::from_secs(7),
            max_wait: Duration::from_secs(2),
            concurrency: 4,
            robots_ttl: Duration::from_secs(3600),
            min_interval: Duration::from_secs(1),
        }
    }
}

pub struct LiveFetcher {
    connector: Arc<dyn Connector>,
    limits: Limits,
    robots: Mutex<HashMap<String, (Instant, String)>>,
    next_visit: Mutex<HashMap<String, Instant>>,
    slots: Semaphore,
}

/// A visit failure, before it becomes an [`Outcome`].
struct Stop(Outcome, Option<chrono::DateTime<Utc>>);

fn stop(outcome: Outcome) -> Stop {
    Stop(outcome, None)
}

/// One same-host redirect (a trailing-slash form): every hop is one more
/// request inside the same reserved slot.
const MAX_REDIRECTS: usize = 1;
const HOST_MAP_CAP: usize = 10_000;

/// The scope a target's URL must fall in; `None` = not a public HTTPS URL
/// inside the stated scope.
fn checked_url(target: &Target) -> Option<Url> {
    if !mazimi_core::application_resources::public_url(&target.url) {
        return None;
    }
    source_access::validate_scope(
        &target.url,
        &target.domain,
        target.allow_subdomains,
        &target.path_prefixes,
    )
    .ok()
}

fn robots_target(url: &Url) -> String {
    match url.query() {
        Some(query) => format!("{}?{query}", url.path()),
        None => url.path().to_owned(),
    }
}

impl LiveFetcher {
    pub fn new(connector: Arc<dyn Connector>, limits: Limits) -> Self {
        let slots = Semaphore::new(limits.concurrency.max(1));
        Self {
            connector,
            limits,
            robots: Mutex::new(HashMap::new()),
            next_visit: Mutex::new(HashMap::new()),
            slots,
        }
    }

    /// Fetch the first target that passes its scope check and read it.
    /// Never panics on page content; every failure is an [`Outcome`].
    pub async fn check(&self, request: &CheckRequest, today: NaiveDate) -> CheckResponse {
        let respond = |outcome, url: Option<String>, findings| CheckResponse {
            outcome,
            url,
            fetched_at: Utc::now(),
            findings,
            retry_after: None,
        };
        let candidates: Vec<(&Target, Url)> = request
            .targets
            .iter()
            .take(MAX_TARGETS)
            .filter_map(|t| checked_url(t).map(|u| (t, u)))
            .collect();
        if candidates.is_empty() {
            return respond(Outcome::NotApproved, None, None);
        }
        let Ok(_slot) = self.slots.try_acquire() else {
            return respond(Outcome::Busy, Some(candidates[0].0.url.clone()), None);
        };
        // A dead or robots-blocked stored URL falls back to the next stored
        // URL (code review M1), inside one overall bound.
        let attempts = async {
            let mut last = respond(Outcome::NotApproved, None, None);
            for (target, url) in candidates {
                if let Err(Stop(outcome, _)) = self.pace(&url, target).await {
                    return respond(outcome, Some(target.url.clone()), None);
                }
                let visit =
                    tokio::time::timeout(self.limits.visit_timeout, self.visit(target, url));
                let response = match visit.await {
                    Ok(Ok(text)) => {
                        return respond(
                            Outcome::Fetched,
                            Some(target.url.clone()),
                            Some(analyze(&text, today)),
                        )
                    }
                    // A rate-limited host: the API pushes the shared domain slot back.
                    Ok(Err(Stop(outcome, retry_after))) => CheckResponse {
                        retry_after,
                        ..respond(outcome, Some(target.url.clone()), None)
                    },
                    Err(_) => return respond(Outcome::Timeout, Some(target.url.clone()), None),
                };
                let next = matches!(response.outcome, Outcome::Http | Outcome::Robots)
                    && response.retry_after.is_none();
                if !next {
                    return response;
                }
                last = response;
            }
            last
        };
        match tokio::time::timeout(self.limits.visit_timeout + self.limits.max_wait, attempts).await
        {
            Ok(response) => response,
            Err(_) => respond(Outcome::Timeout, None, None),
        }
    }

    /// Wait for the host's interval (at most `max_wait`) and book the next.
    async fn pace(&self, url: &Url, target: &Target) -> Result<(), Stop> {
        let host = url.host_str().unwrap_or_default().to_owned();
        let interval =
            Duration::from_millis(u64::from(target.min_interval_ms)).max(self.limits.min_interval);
        let wait = {
            let mut next = self
                .next_visit
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let now = Instant::now();
            if next.len() >= HOST_MAP_CAP {
                next.retain(|_, at| *at > now);
            }
            let at = next.get(&host).copied().unwrap_or(now).max(now);
            let wait = at - now;
            if wait > self.limits.max_wait {
                return Err(stop(Outcome::Cooldown));
            }
            next.insert(host, at + interval);
            wait
        };
        tokio::time::sleep(wait).await;
        Ok(())
    }

    fn defer_host(&self, url: &Url, until: Instant) {
        if let Some(host) = url.host_str() {
            let mut next = self
                .next_visit
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let at = next.entry(host.to_owned()).or_insert(until);
            *at = (*at).max(until);
        }
    }

    async fn robots_allows(
        &self,
        client: &Client,
        url: &Url,
        request_url: &Url,
        user_agent: &str,
        timeout: Duration,
    ) -> Result<bool, Stop> {
        let origin = url.origin().ascii_serialization();
        let cached = self
            .robots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&origin)
            .filter(|(at, _)| at.elapsed() < self.limits.robots_ttl)
            .map(|(_, body)| body.clone());
        let body = match cached {
            Some(body) => body,
            None => {
                let document = fetch_robots_document(
                    client,
                    request_url,
                    user_agent,
                    timeout,
                    LIVE_PAGE_ROBOTS,
                )
                .await
                .map_err(|_| stop(Outcome::Robots))?;
                let body = String::from_utf8_lossy(&document.bytes).into_owned();
                let mut robots = self
                    .robots
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if robots.len() >= HOST_MAP_CAP {
                    robots.clear();
                }
                robots.insert(origin, (Instant::now(), body.clone()));
                body
            }
        };
        Ok(robots_allows(&body, BOT_PRODUCT_TOKEN, &robots_target(url)))
    }

    async fn visit(&self, target: &Target, start: Url) -> Result<String, Stop> {
        let user_agent = format!("{BOT_PRODUCT_TOKEN}/1.0 (+{})", target.contact_url);
        let client_timeout = self.limits.visit_timeout;
        let mut url = start;
        for hop in 0..=MAX_REDIRECTS {
            let (client, request_url) = self
                .connector
                .connect(&url, &user_agent, client_timeout)
                .await
                .map_err(|_| stop(Outcome::Unsafe))?;
            if !self
                .robots_allows(&client, &url, &request_url, &user_agent, client_timeout)
                .await?
            {
                return Err(stop(Outcome::Robots));
            }
            let response = client
                .get(request_url.clone())
                .header(ACCEPT, "text/html,application/xhtml+xml,text/plain;q=0.8")
                .send()
                .await
                .map_err(|_| stop(Outcome::Timeout))?;
            let status = response.status();
            if status.is_redirection() {
                // Resolved against the approved URL, never the transport's.
                let next = response
                    .headers()
                    .get(LOCATION)
                    .and_then(|h| h.to_str().ok())
                    .and_then(|location| url.join(location).ok());
                let same_scope = next.as_ref().is_some_and(|next| {
                    next.host_str() == url.host_str()
                        && checked_url(&Target {
                            url: next.to_string(),
                            ..target.clone()
                        })
                        .is_some()
                });
                match next {
                    Some(next) if same_scope && hop < MAX_REDIRECTS => {
                        url = next;
                        continue;
                    }
                    _ => return Err(stop(Outcome::RedirectOffHost)),
                }
            }
            if matches!(
                status,
                StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE
            ) {
                let until = retry_at(response.headers(), Utc::now());
                let wait = (until - Utc::now()).to_std().unwrap_or_default();
                self.defer_host(&url, Instant::now() + wait.min(Duration::from_secs(86_400)));
                return Err(Stop(Outcome::Http, Some(until)));
            }
            if status != StatusCode::OK {
                return Err(stop(Outcome::Http));
            }
            if response
                .content_length()
                .is_some_and(|n| n > self.limits.max_bytes as u64)
            {
                return Err(stop(Outcome::TooLarge));
            }
            let content_type = response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|h| h.to_str().ok())
                .unwrap_or("")
                .to_ascii_lowercase();
            let html = content_type.starts_with("text/html")
                || content_type.starts_with("application/xhtml+xml");
            if !html && !content_type.starts_with("text/plain") {
                return Err(stop(Outcome::ContentType));
            }
            let bytes = read_limited_body(response, self.limits.max_bytes)
                .await
                .map_err(|_| stop(Outcome::TooLarge))?;
            let decoded =
                decode_page(&bytes, &content_type).map_err(|_| stop(Outcome::Encoding))?;
            return Ok(if html {
                extract_review_text(&decoded).1
            } else {
                decoded.split_whitespace().collect::<Vec<_>>().join(" ")
            });
        }
        Err(stop(Outcome::RedirectOffHost))
    }
}

/// Stored programme URLs that fall inside an approved, unexpired
/// direct-collection scope, as targets for the fetch service, with the
/// scope id each one reserves its request slot on. Order is kept.
pub async fn approved_targets(
    pool: &PgPool,
    urls: &[String],
) -> Result<Vec<(Uuid, Target)>, sqlx::Error> {
    let mut out = Vec::new();
    for value in urls {
        if out.len() >= MAX_TARGETS {
            break;
        }
        let Some(host) = Url::parse(value)
            .ok()
            .and_then(|u| u.host_str().map(str::to_owned))
        else {
            continue;
        };
        let scopes = approved_scopes(pool, Some(&host)).await?;
        if let Some(scope) = scope_for(&scopes, value) {
            out.push((
                scope.id,
                Target {
                    url: value.clone(),
                    domain: scope.domain.clone(),
                    allow_subdomains: scope.allow_subdomains,
                    path_prefixes: scope.allowed_path_prefixes.clone(),
                    contact_url: scope.crawler_contact_url.clone(),
                    min_interval_ms: u32::try_from(scope.min_request_interval_ms.max(0))
                        .unwrap_or(u32::MAX),
                },
            ));
        }
    }
    Ok(out)
}

/// Push the shared per-domain slot back after a 429/503 the fetch service
/// saw, so the collectors and later live checks wait too.
pub async fn defer_domain(
    pool: &PgPool,
    scope: Uuid,
    until: chrono::DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    source_access::defer_domain(pool, scope, until).await
}

/// Reserve the shared per-domain request slot (the collectors' own), waiting
/// at most `max_wait`. `false`: the domain is busy longer, or approval
/// changed meanwhile.
pub async fn reserve_slot(
    pool: &PgPool,
    scope: Uuid,
    min_interval_ms: u32,
    max_wait: Duration,
) -> bool {
    matches!(
        source_access::reserve_request_slot(pool, scope, i64::from(min_interval_ms), max_wait)
            .await,
        Ok(SlotOutcome::Reserved)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(url: &str) -> Target {
        Target {
            url: url.into(),
            domain: "www.city.go.kr".into(),
            allow_subdomains: false,
            path_prefixes: vec!["/youth".into()],
            contact_url: "https://mazimi.example/crawler-policy".into(),
            min_interval_ms: 0,
        }
    }

    #[test]
    fn targets_outside_their_scope_are_never_fetched() {
        assert!(checked_url(&target("https://www.city.go.kr/youth/1")).is_some());
        for url in [
            "http://www.city.go.kr/youth/1",
            "https://www.city.go.kr/admin/1",
            "https://evil.example/youth/1",
            "https://user:pw@www.city.go.kr/youth/1",
            "https://www.city.go.kr:8443/youth/1",
            "https://127.0.0.1/youth/1",
        ] {
            assert!(checked_url(&target(url)).is_none(), "{url}");
        }
    }

    /// The production connector refuses non-public addresses (SSRF).
    #[tokio::test]
    async fn pinned_connector_refuses_loopback_and_private_hosts() {
        for url in ["https://localhost/youth", "https://127.0.0.1/youth"] {
            let url = Url::parse(url).unwrap();
            assert!(
                PinnedConnector
                    .connect(&url, "test", Duration::from_secs(2))
                    .await
                    .is_err(),
                "{url}"
            );
        }
    }

    #[tokio::test]
    async fn no_approved_target_means_no_request() {
        let fetcher = LiveFetcher::new(Arc::new(PinnedConnector), Limits::default());
        let response = fetcher
            .check(
                &CheckRequest {
                    program_id: "x".into(),
                    targets: vec![target("https://evil.example/youth/1")],
                },
                NaiveDate::from_ymd_opt(2026, 10, 16).unwrap(),
            )
            .await;
        assert_eq!(response.outcome, Outcome::NotApproved);
        assert!(response.findings.is_none());
    }

    /// Maps the approved HTTPS URL to a loopback site; every other guard
    /// runs as in production.
    struct Loopback(std::net::SocketAddr);

    #[async_trait::async_trait]
    impl Connector for Loopback {
        async fn connect(
            &self,
            url: &Url,
            user_agent: &str,
            timeout: Duration,
        ) -> anyhow::Result<(Client, Url)> {
            let mut local = Url::parse(&format!("http://{}", self.0))?;
            local.set_path(url.path());
            local.set_query(url.query());
            let client = Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .no_proxy()
                .user_agent(user_agent)
                .timeout(timeout)
                .build()?;
            Ok((client, local))
        }
    }

    /// A robots.txt that lists the crawlers that may come, without us, is
    /// the shared robots decision's refusal: no page request is made.
    #[tokio::test]
    async fn a_named_agent_allowlist_sends_no_page_request() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        for (robots, fetched) in [
            (
                "User-agent: Yeti\nAllow: /\n\nUser-agent: Googlebot\nAllow: /\n",
                false,
            ),
            ("User-agent: *\nDisallow: /admin\n", true),
        ] {
            let pages = Arc::new(AtomicUsize::new(0));
            let counted = pages.clone();
            let app = axum::Router::new()
                .route(
                    "/robots.txt",
                    axum::routing::get(move || async move { robots }),
                )
                .route(
                    "/youth/open",
                    axum::routing::get(move || {
                        counted.fetch_add(1, Ordering::SeqCst);
                        async { axum::response::Html("<main>청년 지원 접수 중</main>") }
                    }),
                );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let fetcher = LiveFetcher::new(
                Arc::new(Loopback(addr)),
                Limits {
                    min_interval: Duration::ZERO,
                    ..Limits::default()
                },
            );
            let response = fetcher
                .check(
                    &CheckRequest {
                        program_id: "x".into(),
                        targets: vec![target("https://www.city.go.kr/youth/open")],
                    },
                    NaiveDate::from_ymd_opt(2026, 11, 10).unwrap(),
                )
                .await;
            if fetched {
                assert_eq!(response.outcome, Outcome::Fetched, "{robots}");
                assert_eq!(pages.load(Ordering::SeqCst), 1, "{robots}");
            } else {
                assert_eq!(response.outcome, Outcome::Robots, "{robots}");
                assert_eq!(pages.load(Ordering::SeqCst), 0, "no page request");
            }
        }
    }
}
