//! One polite GET per URL per run. robots.txt is honoured per origin with
//! the link resolver's 24 h cache (`link_check_hosts`), one origin is asked
//! at most once per [`crate::link_resolver::check::MIN_HOST_INTERVAL`] (or
//! its Crawl-delay), and the spacing doubles after each refusal or server
//! error from that origin; after [`ORIGIN_FAILURE_LIMIT`] of them the
//! origin is left alone for the rest of the run. Redirects are followed by
//! hand, each hop re-checked (public HTTPS, robots under
//! [`robots_policy_for`]), and a redirect into a
//! sign-in page is recorded, never followed: no login, no form submission.
pub use crate::application_resources::fetch::robots_policy_for;
use crate::link_resolver::{
    check::{self, Pacer, Robots},
    classify::is_login_url,
};
use reqwest::{header::LOCATION, Url};
use sqlx::PgPool;
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::sync::Mutex;

const MAX_HOPS: usize = 5;
/// Bytes read from one page (복지로 detail pages are ~100 KB, 온통청년 ~360 KB).
pub const PAGE_BYTE_LIMIT: usize = 800_000;
/// Refusals (403/429), server errors and timeouts from one origin before
/// the run stops asking it.
pub const ORIGIN_FAILURE_LIMIT: u32 = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fetched {
    /// An HTTP response. `html` only for a successful HTML page.
    Page {
        status: u16,
        final_url: String,
        html: Option<String>,
    },
    /// A redirect into a sign-in page (`at`), not followed.
    LoginGate { from: String, at: String },
    /// robots.txt disallows it.
    Blocked,
    /// No usable response: `dns`, `timeout`, `tls`, `refused`, `connect`,
    /// `unsafe`, `redirect_loop`, `robots_*`, `origin_backoff`.
    Error { code: String },
}

impl Fetched {
    /// A failure that says nothing about the link (the network, the
    /// origin's load, robots): retried later, never a rejection.
    pub fn soft(&self) -> bool {
        match self {
            Self::Blocked | Self::Error { .. } => true,
            Self::Page { status, .. } => matches!(status, 403 | 408 | 429) || *status >= 500,
            Self::LoginGate { .. } => false,
        }
    }
}

/// Shared by every lane of one run.
#[derive(Clone)]
pub struct Session {
    pool: PgPool,
    user_agent: String,
    pacer: Pacer,
    cache: Arc<Mutex<HashMap<String, Fetched>>>,
    failures: Arc<Mutex<HashMap<String, u32>>>,
    /// Approved source domains (`benefit_source_domains`), matched exactly:
    /// an operator's approval admits a host the structural rule does not.
    approved: Arc<Vec<String>>,
}

impl Session {
    pub fn new(pool: PgPool, user_agent: String) -> Self {
        Self {
            pool,
            user_agent,
            pacer: Pacer::default(),
            cache: Arc::default(),
            failures: Arc::default(),
            approved: Arc::default(),
        }
    }

    pub fn with_approved(mut self, domains: Vec<String>) -> Self {
        self.approved = Arc::new(domains);
        self
    }

    /// `url`'s host is an approved source domain (exact host).
    pub fn approved_host(&self, url: &str) -> bool {
        Url::parse(url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
            .is_some_and(|h| self.approved.iter().any(|d| d.eq_ignore_ascii_case(&h)))
    }

    /// Requests actually sent and answers cached this run.
    pub async fn cached(&self) -> usize {
        self.cache.lock().await.len()
    }

    pub async fn get(&self, value: &str) -> Fetched {
        if let Some(hit) = self.cache.lock().await.get(value) {
            return hit.clone();
        }
        let result = self.fetch(value).await;
        self.cache
            .lock()
            .await
            .insert(value.to_string(), result.clone());
        result
    }

    async fn note_failure(&self, origin: &str) {
        *self.failures.lock().await.entry(origin.into()).or_default() += 1;
    }

    async fn fetch(&self, value: &str) -> Fetched {
        let error = |code: &str| Fetched::Error { code: code.into() };
        let Ok(mut url) = Url::parse(value) else {
            return error("unsafe");
        };
        for _ in 0..=MAX_HOPS {
            if !mazimi_core::application_resources::public_url(url.as_str()) {
                return error("unsafe");
            }
            let origin = check::origin_of(&url);
            let failures = self
                .failures
                .lock()
                .await
                .get(&origin)
                .copied()
                .unwrap_or(0);
            if failures >= ORIGIN_FAILURE_LIMIT {
                return error("origin_backoff");
            }
            let robots = check::robots(
                &self.pool,
                &url,
                &self.user_agent,
                &self.pacer,
                robots_policy_for(&url),
            )
            .await;
            if let Robots::Unreachable { code } = &robots {
                return error(code);
            }
            if !robots.allows(&url) {
                return Fetched::Blocked;
            }
            let http = match check::client(&url, &self.user_agent).await {
                Ok(http) => http,
                Err(code) => return error(&code),
            };
            // Backoff: the spacing doubles with each failure of this origin.
            let interval = robots.interval() * 2u32.pow(failures);
            self.pacer.wait(&origin, interval).await;
            let response = match http
                .get(url.clone())
                .header("Accept", "text/html,application/xhtml+xml,*/*;q=0.8")
                .header("Accept-Language", "ko-KR,ko;q=0.9")
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    self.note_failure(&origin).await;
                    return error(&check::transport_code(&e));
                }
            };
            let status = response.status();
            // A refusal (403 from a bot wall, 429) or a server error slows
            // this origin down and, after a few, stops the run asking it.
            if matches!(status.as_u16(), 403 | 429) || status.is_server_error() {
                self.note_failure(&origin).await;
            }
            if status.is_redirection() {
                let Some(next) = response
                    .headers()
                    .get(LOCATION)
                    .and_then(|h| h.to_str().ok())
                    .and_then(|l| url.join(l).ok())
                else {
                    return error("redirect_without_location");
                };
                if is_login_url(next.as_str()) {
                    return Fetched::LoginGate {
                        from: url.to_string(),
                        at: next.to_string(),
                    };
                }
                url = next;
                continue;
            }
            let content_type = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|h| h.to_str().ok())
                .unwrap_or("text/html")
                .to_string();
            let html = if status.is_success() && content_type.contains("html") {
                match check::read_prefix(response, PAGE_BYTE_LIMIT).await {
                    Ok(bytes) => Some(crate::link_resolver::page::decode(&bytes, &content_type)),
                    Err(code) => return error(&code),
                }
            } else {
                None
            };
            return Fetched::Page {
                status: status.as_u16(),
                final_url: url.to_string(),
                html,
            };
        }
        error("redirect_loop")
    }
}

/// How long a programme whose pages could not be read waits before the
/// next scan, after `previous_failures` such scans in a row: 1, 2, 4, then
/// 7 days.
pub fn soft_retry(previous_failures: i32) -> Duration {
    let days = (1u64 << previous_failures.clamp(0, 3)).min(7);
    Duration::from_secs(days * 86_400)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mazimi_core::source_policy::RobotsPolicy;

    #[test]
    fn soft_failures_are_the_ones_users_might_not_see() {
        for soft in [
            Fetched::Blocked,
            Fetched::Error {
                code: "timeout".into(),
            },
            Fetched::Page {
                status: 503,
                final_url: "https://a.go.kr/".into(),
                html: None,
            },
            Fetched::Page {
                status: 429,
                final_url: "https://a.go.kr/".into(),
                html: None,
            },
        ] {
            assert!(soft.soft(), "{soft:?}");
        }
        for hard in [
            Fetched::Page {
                status: 404,
                final_url: "https://a.go.kr/".into(),
                html: None,
            },
            Fetched::Page {
                status: 200,
                final_url: "https://a.go.kr/".into(),
                html: None,
            },
            Fetched::LoginGate {
                from: "https://a.go.kr/apply".into(),
                at: "https://a.go.kr/login".into(),
            },
        ] {
            assert!(!hard.soft(), "{hard:?}");
        }
    }

    #[test]
    fn government_hosts_get_rfc9309_and_every_other_host_stays_strict() {
        let policy = |u: &str| robots_policy_for(&Url::parse(u).unwrap());
        for gov in [
            "https://www.bokjiro.go.kr/ssis-tbu/x",
            "https://youth.seoul.go.kr/a",
            "https://www.work24.go.kr/a",
            "https://www.army.mil.kr/a",
        ] {
            assert_eq!(policy(gov), RobotsPolicy::Rfc9309, "{gov}");
        }
        for other in [
            "https://plus.gov.kr/portal",
            "https://apply.jobaba.net/a",
            "https://www.ch2030youth.kr/",
            "https://www.work24.go.kr.evil.example/a",
            "https://xn--3e0b707e.go.kr/a",
        ] {
            assert_eq!(policy(other), RobotsPolicy::Strict, "{other}");
        }
    }

    #[test]
    fn soft_retries_back_off_to_a_week() {
        let days: Vec<u64> = (0..6).map(|n| soft_retry(n).as_secs() / 86_400).collect();
        assert_eq!(days, vec![1, 2, 4, 7, 7, 7]);
    }
}
