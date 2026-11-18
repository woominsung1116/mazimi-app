//! Polite liveness checks of navigation links (design 5.6).
//!
//! A check is one GET a user's tap would also make: no form is submitted,
//! nothing beyond the page itself is read. Every hop is re-validated
//! (public HTTPS, DNS pinned to a public address), robots.txt is honoured
//! per origin with a 24 h cache, one origin is asked at most once a second
//! (or its Crawl-delay), and at most four hosts are checked at once.
use super::classify::Observation;
use super::page;
use crate::sources::benefit_pages::{
    build_pinned_client, robots_allows, robots_crawl_delay, robots_response_looks_like_html,
};
use chrono::{DateTime, Utc};
use mazimi_core::source_policy::{RobotsAnswer, RobotsPolicy, RobotsVerdict};
use reqwest::{header::LOCATION, StatusCode, Url};
use sqlx::PgPool;
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;

/// Product token matched against robots.txt groups.
pub const ROBOTS_TOKEN: &str = "Mazimi-Bot";
const MAX_HOPS: usize = 5;
const MAX_ROBOTS_BYTES: usize = 256 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// The least spacing between two requests to one origin.
pub const MIN_HOST_INTERVAL: Duration = Duration::from_millis(1_100);
const MAX_CRAWL_DELAY: Duration = Duration::from_secs(10);
pub const ROBOTS_TTL_HOURS: i64 = 24;
/// An origin whose robots.txt did not answer is not asked again for this long.
pub const UNREACHABLE_TTL_HOURS: i64 = 6;
pub const HOSTS_AT_ONCE: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Robots {
    Allow,
    Rules {
        body: String,
    },
    DisallowAll,
    /// robots.txt did not answer (network failure, 429 or 5xx): RFC 9309
    /// treats the whole origin as disallowed until it answers.
    Unreachable {
        code: String,
    },
}

impl Robots {
    pub(crate) fn allows(&self, url: &Url) -> bool {
        let target = match url.query() {
            Some(q) => format!("{}?{q}", url.path()),
            None => url.path().to_string(),
        };
        match self {
            Self::Allow => true,
            Self::Rules { body } => robots_allows(body, ROBOTS_TOKEN, &target),
            Self::DisallowAll | Self::Unreachable { .. } => false,
        }
    }
    pub(crate) fn interval(&self) -> Duration {
        let delay = match self {
            Self::Rules { body } => robots_crawl_delay(body, ROBOTS_TOKEN)
                .filter(|d| d.is_finite() && *d > 0.0)
                .map(|d| Duration::from_secs_f64(d).min(MAX_CRAWL_DELAY)),
            _ => None,
        };
        delay.unwrap_or_default().max(MIN_HOST_INTERVAL)
    }
}

/// Request spacing per origin, shared by every task of one run.
#[derive(Default, Clone)]
pub struct Pacer {
    next: Arc<Mutex<HashMap<String, Instant>>>,
}

impl Pacer {
    /// Waits until `origin` may be asked again and reserves that slot.
    pub async fn wait(&self, origin: &str, interval: Duration) {
        let at = {
            let mut next = self.next.lock().await;
            let now = Instant::now();
            let at = next.get(origin).copied().unwrap_or(now).max(now);
            next.insert(origin.to_string(), at + interval);
            at
        };
        tokio::time::sleep_until(at.into()).await;
    }
}

pub(crate) fn origin_of(url: &Url) -> String {
    url.origin().ascii_serialization()
}

/// A transport failure as a recorded soft-failure code: `timeout`, `tls`,
/// `refused` or `connect`. None of them breaks a link (users' browsers
/// may well connect); only hard failures do.
pub(crate) fn transport_code(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        return "timeout".into();
    }
    let mut chain = String::new();
    let mut source: Option<&dyn std::error::Error> = Some(error);
    while let Some(e) = source {
        chain.push_str(&e.to_string().to_ascii_lowercase());
        chain.push(' ');
        source = e.source();
    }
    if ["certificate", "tls", "handshake", "ssl"]
        .iter()
        .any(|w| chain.contains(w))
    {
        "tls".into()
    } else if chain.contains("refused") {
        "refused".into()
    } else {
        "connect".into()
    }
}

pub(crate) async fn client(url: &Url, user_agent: &str) -> Result<reqwest::Client, String> {
    build_pinned_client(url, user_agent, REQUEST_TIMEOUT)
        .await
        .map_err(|error| {
            let text = format!("{error:#}");
            if text.contains("non-public") {
                "unsafe".to_string()
            } else if text.contains("DNS") {
                "dns".to_string()
            } else {
                "connect".to_string()
            }
        })
}

pub(crate) async fn read_prefix(
    mut response: reqwest::Response,
    limit: usize,
) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    while body.len() < limit {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                let take = chunk.len().min(limit - body.len());
                body.extend_from_slice(&chunk[..take]);
            }
            Ok(None) => break,
            Err(error) => return Err(transport_code(&error)),
        }
    }
    Ok(body)
}

/// What `/robots.txt` of `origin` answers, following up to five redirects
/// (RFC 9309 2.3.1.2). Policy is applied by the caller ([`robots`]).
async fn fetch_robots(origin: &Url, user_agent: &str, pacer: &Pacer) -> RobotsAnswer {
    let unavailable = |code: &str| RobotsAnswer::Unavailable(code.into());
    let mut url = origin.join("/robots.txt").expect("robots path joins");
    for _ in 0..=MAX_HOPS {
        if !mazimi_core::application_resources::public_url(url.as_str()) {
            // Fail closed: a robots.txt redirected off the public web.
            return unavailable("robots_redirect");
        }
        let http = match client(&url, user_agent).await {
            Ok(http) => http,
            Err(code) => return unavailable(&code),
        };
        pacer.wait(&origin_of(&url), MIN_HOST_INTERVAL).await;
        let response = match http.get(url.clone()).send().await {
            Ok(r) => r,
            Err(error) => return unavailable(&transport_code(&error)),
        };
        let status = response.status();
        if status.is_redirection() {
            match response
                .headers()
                .get(LOCATION)
                .and_then(|h| h.to_str().ok())
                .and_then(|l| url.join(l).ok())
            {
                Some(next) => {
                    url = next;
                    continue;
                }
                None => return unavailable("robots_redirect"),
            }
        }
        return match status {
            StatusCode::OK => match read_prefix(response, MAX_ROBOTS_BYTES).await {
                Ok(bytes) => answer_from_body(&bytes),
                Err(code) => unavailable(&code),
            },
            s if s == StatusCode::TOO_MANY_REQUESTS || s.is_server_error() => {
                unavailable(&format!("robots_http_{}", s.as_u16()))
            }
            s if s.is_client_error() => RobotsAnswer::ClientError(s.as_u16()),
            // 1xx/2xx other than 200: no robots.txt to read.
            s => RobotsAnswer::ClientError(s.as_u16()),
        };
    }
    unavailable("robots_redirect")
}

/// A 200 robots.txt body: rules, or an HTML page where robots.txt should be
/// (a bot-challenge interstitial, or the error page a redirect landed on).
pub(crate) fn answer_from_body(bytes: &[u8]) -> RobotsAnswer {
    if robots_response_looks_like_html(bytes) {
        RobotsAnswer::Html
    } else {
        RobotsAnswer::Rules(String::from_utf8_lossy(bytes).into_owned())
    }
}

impl From<RobotsVerdict> for Robots {
    fn from(verdict: RobotsVerdict) -> Self {
        match verdict {
            RobotsVerdict::Allow => Self::Allow,
            RobotsVerdict::Rules(body) => Self::Rules { body },
            RobotsVerdict::DisallowAll => Self::DisallowAll,
            RobotsVerdict::Unavailable(code) => Self::Unreachable { code },
        }
    }
}

/// The answer as stored in `link_check_hosts` (status, body, failure code).
/// The table's status vocabulary predates [`RobotsAnswer`]: the failure
/// code keeps the fact apart (`robots_html`, `robots_http_403`).
fn cache_row(answer: &RobotsAnswer) -> (&'static str, Option<String>, Option<String>) {
    match answer {
        RobotsAnswer::Rules(body) => ("rules", Some(body.clone()), None),
        RobotsAnswer::Html => ("unreachable", None, Some("robots_html".into())),
        RobotsAnswer::ClientError(s @ (401 | 403)) => {
            ("disallow_all", None, Some(format!("robots_http_{s}")))
        }
        RobotsAnswer::ClientError(s) => ("allow", None, Some(format!("robots_http_{s}"))),
        RobotsAnswer::Unavailable(code) => ("unreachable", None, Some(code.clone())),
    }
}

/// A cached row back as the answer. Rows written before the answer was
/// kept read as what the old code meant (`allow` = no robots.txt,
/// `disallow_all` = 403).
fn cached_answer(status: &str, body: Option<String>, code: Option<String>) -> RobotsAnswer {
    let status_of = |code: &Option<String>, default: u16| {
        code.as_deref()
            .and_then(|c| c.strip_prefix("robots_http_"))
            .and_then(|n| n.parse().ok())
            .unwrap_or(default)
    };
    match status {
        "rules" => RobotsAnswer::Rules(body.unwrap_or_default()),
        "disallow_all" => RobotsAnswer::ClientError(status_of(&code, 403)),
        "unreachable" if code.as_deref() == Some("robots_html") => RobotsAnswer::Html,
        "unreachable" => RobotsAnswer::Unavailable(code.unwrap_or_else(|| "timeout".into())),
        _ => RobotsAnswer::ClientError(status_of(&code, 404)),
    }
}

/// robots.txt with the persistent 24 h cache (6 h for an origin that did
/// not answer), read under `policy`. The cache holds the answer, not a
/// verdict, so callers with different policies share it.
pub async fn robots(
    pool: &PgPool,
    origin: &Url,
    user_agent: &str,
    pacer: &Pacer,
    policy: RobotsPolicy,
) -> Robots {
    let key = origin_of(origin);
    type Cached = (String, Option<String>, Option<String>, DateTime<Utc>);
    let cached: Option<Cached> = sqlx::query_as(
        "SELECT robots_status, robots_body, failure_code, fetched_at FROM link_check_hosts WHERE origin=$1",
    )
    .bind(&key)
    .fetch_optional(pool)
    .await
    .unwrap_or(None);
    if let Some((status, body, code, fetched_at)) = cached {
        let ttl = if status == "unreachable" {
            UNREACHABLE_TTL_HOURS
        } else {
            ROBOTS_TTL_HOURS
        };
        if Utc::now() - fetched_at < chrono::Duration::hours(ttl) {
            return policy.verdict(&cached_answer(&status, body, code)).into();
        }
    }
    let answer = fetch_robots(origin, user_agent, pacer).await;
    let (status, body, code) = cache_row(&answer);
    if let Err(error) = sqlx::query(
        "INSERT INTO link_check_hosts(origin,robots_status,robots_body,failure_code,fetched_at)
         VALUES($1,$2,$3,$4,now())
         ON CONFLICT(origin) DO UPDATE SET robots_status=excluded.robots_status,
           robots_body=excluded.robots_body,failure_code=excluded.failure_code,fetched_at=now()",
    )
    .bind(&key)
    .bind(status)
    .bind(body)
    .bind(code)
    .execute(pool)
    .await
    {
        tracing::warn!(error = %crate::error_safety::persisted_error(anyhow::Error::from(error)), "robots cache write failed");
    }
    policy.verdict(&answer).into()
}

/// One check of `value`: robots first, then GET with manual redirects.
pub async fn check(pool: &PgPool, value: &str, user_agent: &str, pacer: &Pacer) -> Observation {
    let error = |code: &str| Observation::Error { code: code.into() };
    let Ok(mut url) = Url::parse(value) else {
        return error("unsafe");
    };
    for _ in 0..=MAX_HOPS {
        if !mazimi_core::application_resources::public_url(url.as_str()) {
            return error("unsafe");
        }
        let robots = robots(pool, &url, user_agent, pacer, RobotsPolicy::Strict).await;
        if let Robots::Unreachable { code } = &robots {
            // The origin does not answer from here: a soft failure, not a block.
            return error(if code == "dns" { "dns" } else { code });
        }
        if !robots.allows(&url) {
            return Observation::Blocked;
        }
        let http = match client(&url, user_agent).await {
            Ok(http) => http,
            Err(code) => return error(&code),
        };
        pacer.wait(&origin_of(&url), robots.interval()).await;
        let response = match http
            .get(url.clone())
            .header("Accept", "text/html,application/xhtml+xml,*/*;q=0.8")
            .header("Accept-Language", "ko-KR,ko;q=0.9")
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => return error(&transport_code(&e)),
        };
        let status = response.status();
        if status.is_redirection() {
            if let Some(next) = response
                .headers()
                .get(LOCATION)
                .and_then(|h| h.to_str().ok())
                .and_then(|l| url.join(l).ok())
            {
                url = next;
                continue;
            }
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|h| h.to_str().ok())
            .unwrap_or("text/html")
            .to_string();
        let facts = if status.is_success() && content_type.contains("html") {
            match read_prefix(response, page::PAGE_BYTE_LIMIT).await {
                Ok(bytes) => Some(page::analyze(&page::decode(&bytes, &content_type))),
                Err(code) => return error(&code),
            }
        } else {
            None
        };
        return Observation::Fetched {
            status: status.as_u16(),
            final_url: url.to_string(),
            page: facts,
        };
    }
    error("redirect_loop")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A link on a site whose robots.txt lists the crawlers that may come,
    /// without ours, is `Blocked` before any request (`check` asks
    /// `allows` first).
    #[test]
    fn a_named_agent_allowlist_blocks_the_link_before_its_request() {
        let allowlist = Robots::Rules {
            body: "User-agent: Yeti\nAllow: /\n\nUser-agent: Googlebot\nAllow: /\n".into(),
        };
        assert!(!allowlist.allows(&Url::parse("https://www.example.or.kr/apply?id=1").unwrap()));
        // A file naming only other agents refuses us too (security review H3).
        let others = Robots::Rules {
            body: "User-agent: Googlebot-Image\nDisallow: /images\n".into(),
        };
        assert!(!others.allows(&Url::parse("https://www.example.or.kr/apply?id=1").unwrap()));
        let star = Robots::Rules {
            body: "User-agent: Googlebot-Image\nDisallow: /images\n\nUser-agent: *\nAllow: /\n"
                .into(),
        };
        assert!(star.allows(&Url::parse("https://www.example.or.kr/apply?id=1").unwrap()));
    }

    #[test]
    fn robots_rules_and_crawl_delay_apply_to_our_token() {
        let rules = Robots::Rules {
            body: "User-agent: Mazimi-Bot\nDisallow: /portal/rcvfvrSvc\nCrawl-delay: 3\n".into(),
        };
        assert!(!rules.allows(&Url::parse("https://www.gov.kr/portal/rcvfvrSvc/dtlEx/1").unwrap()));
        assert!(rules.allows(&Url::parse("https://www.gov.kr/other").unwrap()));
        assert_eq!(rules.interval(), Duration::from_secs(3));
        assert_eq!(Robots::Allow.interval(), MIN_HOST_INTERVAL);
        assert!(!Robots::DisallowAll.allows(&Url::parse("https://x.go.kr/").unwrap()));
        let star = Robots::Rules {
            body: "User-agent: *\nDisallow: /\n".into(),
        };
        assert!(!star.allows(&Url::parse("https://x.go.kr/a?b=1").unwrap()));
        let slow = Robots::Rules {
            body: "User-agent: *\nCrawl-delay: 600\n".into(),
        };
        assert_eq!(slow.interval(), MAX_CRAWL_DELAY);
    }

    #[test]
    fn the_cache_keeps_the_answer_so_each_policy_reads_it() {
        let bokjiro_error_page =
            b"<!DOCTYPE html><html><head><title>\xeb\xb3\xb5\xec\xa7\x80\xeb\xa1\x9c</title></head><body><h1>ERROR</h1></body></html>";
        assert_eq!(answer_from_body(bokjiro_error_page), RobotsAnswer::Html);
        assert_eq!(
            answer_from_body(b"<html><head><title>Just a moment...</title></head></html>"),
            RobotsAnswer::Html
        );
        for answer in [
            RobotsAnswer::Rules("User-agent: *\nDisallow: /x\n".into()),
            RobotsAnswer::Html,
            RobotsAnswer::ClientError(403),
            RobotsAnswer::ClientError(401),
            RobotsAnswer::ClientError(404),
            RobotsAnswer::ClientError(410),
            RobotsAnswer::Unavailable("robots_http_503".into()),
            RobotsAnswer::Unavailable("timeout".into()),
        ] {
            let (status, body, code) = cache_row(&answer);
            assert_eq!(cached_answer(status, body, code), answer, "{answer:?}");
        }
        // Rows written before: allow = no robots.txt, disallow_all = 403.
        assert_eq!(
            cached_answer("allow", None, None),
            RobotsAnswer::ClientError(404)
        );
        assert_eq!(
            cached_answer("disallow_all", None, None),
            RobotsAnswer::ClientError(403)
        );
        // The strict resolver still reads HTML as a wall.
        let url = Url::parse("https://www.bokjiro.go.kr/a").unwrap();
        let strict: Robots = RobotsPolicy::Strict.verdict(&RobotsAnswer::Html).into();
        assert!(!strict.allows(&url));
        let rfc: Robots = RobotsPolicy::Rfc9309.verdict(&RobotsAnswer::Html).into();
        assert!(rfc.allows(&url));
    }

    #[tokio::test]
    async fn the_pacer_spaces_requests_to_one_origin() {
        let pacer = Pacer::default();
        let start = Instant::now();
        pacer
            .wait("https://a.go.kr", Duration::from_millis(200))
            .await;
        pacer
            .wait("https://b.go.kr", Duration::from_millis(200))
            .await;
        assert!(start.elapsed() < Duration::from_millis(150));
        pacer
            .wait("https://a.go.kr", Duration::from_millis(200))
            .await;
        assert!(start.elapsed() >= Duration::from_millis(190));
    }
}
