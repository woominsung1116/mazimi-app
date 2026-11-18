//! The single network seam for the census and the evidence job.
//!
//! Production requests go through [`PinnedHttp`]: HTTPS on port 443 only,
//! DNS pinned to public addresses (the shared `build_pinned_client`), no
//! redirects followed, no proxy, bounded bodies and the honest bot user
//! agent. Tests substitute a loopback implementation of [`PageHttp`].

use anyhow::{bail, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use reqwest::{
    header::{ACCEPT, ACCEPT_LANGUAGE, CONTENT_TYPE, LOCATION},
    Url,
};
use std::time::Duration;

use crate::sources::benefit_pages::{build_pinned_client, read_limited_body, retry_at};

pub const MAX_PAGE_BYTES: usize = 1_048_576;
pub const MAX_ROBOTS_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub struct HttpReply {
    pub status: u16,
    pub content_type: String,
    pub location: Option<String>,
    /// Retry-After resolved to an instant (1 day when absent).
    pub retry_at: DateTime<Utc>,
    pub body: Vec<u8>,
}

#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    /// Timeouts, connection resets, TLS and DNS failures: retry later.
    #[error("transient fetch failure: {0}")]
    Transient(String),
    /// The request was refused by our own guards (scheme, address, size).
    #[error("request refused by fetch guard: {0}")]
    Refused(String),
}

#[async_trait]
pub trait PageHttp: Send + Sync {
    async fn get(
        &self,
        url: &Url,
        accept: &'static str,
        max_bytes: usize,
    ) -> Result<HttpReply, FetchError>;
    fn user_agent(&self) -> &str;
}

pub struct PinnedHttp {
    user_agent: String,
    timeout: Duration,
}

impl PinnedHttp {
    pub fn new(user_agent: String, timeout: Duration) -> Result<Self> {
        if !user_agent.starts_with("MazimiBenefitBot/") {
            bail!("the corporate fetcher only sends the MazimiBenefitBot user agent");
        }
        Ok(Self {
            user_agent,
            timeout,
        })
    }
}

#[async_trait]
impl PageHttp for PinnedHttp {
    async fn get(
        &self,
        url: &Url,
        accept: &'static str,
        max_bytes: usize,
    ) -> Result<HttpReply, FetchError> {
        if url.scheme() != "https"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.port_or_known_default() != Some(443)
        {
            return Err(FetchError::Refused(
                "only credential-free https URLs on port 443 are fetched".into(),
            ));
        }
        let client = build_pinned_client(url, &self.user_agent, self.timeout)
            .await
            .map_err(|error| {
                let message = error.to_string();
                if message.contains("non-public") {
                    FetchError::Refused(message)
                } else {
                    FetchError::Transient(message)
                }
            })?;
        send(&client, url, accept, max_bytes).await
    }

    fn user_agent(&self) -> &str {
        &self.user_agent
    }
}

pub(crate) async fn send(
    client: &reqwest::Client,
    url: &Url,
    accept: &'static str,
    max_bytes: usize,
) -> Result<HttpReply, FetchError> {
    let response = client
        .get(url.clone())
        .header(ACCEPT, accept)
        .header(ACCEPT_LANGUAGE, "ko-KR,ko;q=0.9,en;q=0.5")
        .send()
        .await
        .map_err(|error| FetchError::Transient(error.without_url().to_string()))?;
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    if response
        .content_length()
        .is_some_and(|size| size > max_bytes as u64)
    {
        return Err(FetchError::Refused("body exceeds the size limit".into()));
    }
    let body = read_limited_body(response, max_bytes)
        .await
        .map_err(|error| FetchError::Refused(error.to_string()))?;
    Ok(HttpReply {
        status,
        content_type: headers
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase(),
        location: headers
            .get(LOCATION)
            .and_then(|value| value.to_str().ok())
            .map(|value| value.chars().take(2_000).collect()),
        retry_at: retry_at(&headers, Utc::now()),
        body,
    })
}

/// Loopback transport for tests: same request shape, no pinning.
#[cfg(test)]
pub(crate) struct LoopbackHttp {
    client: reqwest::Client,
    user_agent: String,
}

#[cfg(test)]
impl LoopbackHttp {
    pub(crate) fn new(user_agent: &str) -> Self {
        Self {
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .no_proxy()
                .user_agent(user_agent)
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap(),
            user_agent: user_agent.to_string(),
        }
    }
}

#[cfg(test)]
#[async_trait]
impl PageHttp for LoopbackHttp {
    async fn get(
        &self,
        url: &Url,
        accept: &'static str,
        max_bytes: usize,
    ) -> Result<HttpReply, FetchError> {
        send(&self.client, url, accept, max_bytes).await
    }

    fn user_agent(&self) -> &str {
        &self.user_agent
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_transport_refuses_a_disguised_user_agent() {
        assert!(PinnedHttp::new("Mozilla/5.0".into(), Duration::from_secs(5)).is_err());
        assert!(PinnedHttp::new("Googlebot/2.1".into(), Duration::from_secs(5)).is_err());
        assert!(PinnedHttp::new(
            "MazimiBenefitBot/1.0 (+https://mazimi.kr/bot)".into(),
            Duration::from_secs(5)
        )
        .is_ok());
    }

    #[tokio::test]
    async fn production_transport_refuses_plain_http_and_other_ports() {
        let http = PinnedHttp::new(
            "MazimiBenefitBot/1.0 (+https://mazimi.kr/bot)".into(),
            Duration::from_secs(5),
        )
        .unwrap();
        for url in [
            "http://example.com/",
            "https://example.com:8443/",
            "https://u:p@example.com/",
        ] {
            let result = http
                .get(&Url::parse(url).unwrap(), "text/html", MAX_PAGE_BYTES)
                .await;
            assert!(matches!(result, Err(FetchError::Refused(_))), "{url}");
        }
    }
}
