//! Shared bounded retry policy for official upstream APIs.
//!
//! API keys are commonly sent as query parameters by Korean public-data
//! providers. Reqwest errors retain the request URL, so all errors are stripped
//! of their URL before they can reach logs or persisted ingestion records.

use anyhow::{bail, Result};
use reqwest::{header::RETRY_AFTER, RequestBuilder, Response, StatusCode};
use std::time::Duration;

const MAX_ATTEMPTS: u32 = 3;

/// Where an operator who sees our requests can read who we are and ask us to
/// stop: the public crawler policy page. Deployments set
/// `CORPORATE_BOT_CONTACT_URL` (the URL the corporate crawler already
/// advertises) to that page on their public host; this placeholder only
/// stands in where it is unset, such as tests and local runs.
pub const DEFAULT_BOT_CONTACT_URL: &str = "https://mazimi.example/crawler-policy";

/// The contact URL for bot user agents: `CORPORATE_BOT_CONTACT_URL` when it
/// is a valid public https URL, else [`DEFAULT_BOT_CONTACT_URL`].
pub fn bot_contact_url() -> String {
    contact_url_or_default(std::env::var("CORPORATE_BOT_CONTACT_URL").ok().as_deref())
}

fn contact_url_or_default(configured: Option<&str>) -> String {
    configured
        .map(str::trim)
        .filter(|value| crate::corporate_auto::validate_contact_url(value).is_ok())
        .unwrap_or(DEFAULT_BOT_CONTACT_URL)
        .to_string()
}

/// User agent of the official public-data API clients (온통청년, 복지로).
pub fn public_api_user_agent() -> String {
    format!(
        "Mozilla/5.0 (compatible; Mazimi-Bot/1.0; +{})",
        bot_contact_url()
    )
}

/// The HTTP client of an official API that takes its key as a query
/// parameter (보조금24, 복지로, 온통청년, 장학재단; finlife builds its own with
/// the same policy): a followed redirect would carry the key to another
/// host, in the request and in the Referer, so redirects are answers (a
/// 3xx [`UpstreamStatus`]) and no Referer is sent. All endpoints are https
/// on the provider's API host.
pub fn key_in_query_client(user_agent: Option<String>) -> reqwest::Client {
    let builder = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .referer(false);
    let builder = match user_agent {
        Some(agent) => builder.user_agent(agent),
        None => builder,
    };
    builder
        .build()
        .expect("reqwest client with timeout must build")
}

/// A non-success HTTP status from the provider, after retries. The message
/// is the status alone (no URL); callers may downcast to tell a redirect,
/// which no key-in-query client follows ([`key_in_query_client`]), from an
/// error.
#[derive(Debug)]
pub struct UpstreamStatus {
    pub source: &'static str,
    pub status: u16,
}

impl std::fmt::Display for UpstreamStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: upstream returned HTTP {}", self.source, self.status)
    }
}

impl std::error::Error for UpstreamStatus {}

pub async fn send(request: RequestBuilder, source: &'static str) -> Result<Response> {
    for attempt in 1..=MAX_ATTEMPTS {
        let current = request
            .try_clone()
            .ok_or_else(|| anyhow::anyhow!("{source}: request cannot be retried safely"))?;

        match current.send().await {
            Ok(response) if response.status().is_success() => return Ok(response),
            Ok(response) => {
                let status = response.status();
                if attempt < MAX_ATTEMPTS && should_retry_status(status) {
                    let delay = retry_delay(&response, attempt);
                    tracing::warn!(
                        source,
                        attempt,
                        max_attempts = MAX_ATTEMPTS,
                        status = status.as_u16(),
                        retry_after_ms = delay.as_millis() as u64,
                        "upstream request failed transiently; retrying"
                    );
                    tokio::time::sleep(delay).await;
                    continue;
                }
                return Err(UpstreamStatus {
                    source,
                    status: status.as_u16(),
                }
                .into());
            }
            Err(error) if attempt < MAX_ATTEMPTS && is_transient_error(&error) => {
                let delay = exponential_delay(attempt);
                tracing::warn!(
                    source,
                    attempt,
                    max_attempts = MAX_ATTEMPTS,
                    retry_after_ms = delay.as_millis() as u64,
                    "upstream connection failed transiently; retrying"
                );
                tokio::time::sleep(delay).await;
            }
            Err(error) => {
                bail!("{source}: upstream request failed: {}", error.without_url());
            }
        }
    }

    unreachable!("MAX_ATTEMPTS is greater than zero")
}

pub async fn text(mut response: Response, source: &'static str) -> Result<String> {
    // Bound the decompressed body as well as declared length: an upstream
    // outage or schema change must not exhaust the worker's memory.
    const MAX_BODY_BYTES: usize = 10 * 1024 * 1024;
    anyhow::ensure!(
        response.content_length().unwrap_or(0) <= MAX_BODY_BYTES as u64,
        "{source}: response too large"
    );
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| anyhow::anyhow!("{source}: response read failed: {}", e.without_url()))?
    {
        anyhow::ensure!(
            body.len().saturating_add(chunk.len()) <= MAX_BODY_BYTES,
            "{source}: response too large"
        );
        body.extend_from_slice(&chunk);
    }
    String::from_utf8(body).map_err(|_| anyhow::anyhow!("{source}: invalid UTF-8 response"))
}

fn should_retry_status(status: StatusCode) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS
        || status == StatusCode::REQUEST_TIMEOUT
        || status.is_server_error()
}

fn is_transient_error(error: &reqwest::Error) -> bool {
    error.is_timeout() || error.is_connect()
}

fn retry_delay(response: &Response, attempt: u32) -> Duration {
    response
        .headers()
        .get(RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .map(|seconds| Duration::from_secs(seconds.min(10)))
        .unwrap_or_else(|| exponential_delay(attempt))
}

fn exponential_delay(attempt: u32) -> Duration {
    Duration::from_millis(250_u64.saturating_mul(1_u64 << attempt.saturating_sub(1).min(4)))
}

#[cfg(test)]
mod tests {
    use super::should_retry_status;
    use reqwest::StatusCode;

    #[test]
    fn bot_contact_is_the_configured_policy_url_or_the_live_default() {
        use super::{contact_url_or_default, DEFAULT_BOT_CONTACT_URL};
        assert_eq!(
            contact_url_or_default(Some(" https://mazimi.kr/crawler-policy ")),
            "https://mazimi.kr/crawler-policy"
        );
        for unusable in [
            None,
            Some(""),
            Some("http://plain.example/x"),
            Some("https://a.example/(x)"),
        ] {
            assert_eq!(contact_url_or_default(unusable), DEFAULT_BOT_CONTACT_URL);
        }
        assert!(DEFAULT_BOT_CONTACT_URL.ends_with("/crawler-policy"));
        assert!(crate::corporate_auto::validate_contact_url(DEFAULT_BOT_CONTACT_URL).is_ok());
    }

    /// review2-fss L9: a key-in-query client answers a redirect as a status
    /// and never follows it to the other location (local server, no network).
    #[tokio::test]
    async fn a_key_in_query_client_never_follows_a_redirect() {
        use axum::{response::Redirect, routing::get, Router};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = Router::new()
            .route("/api", get(|| async { Redirect::temporary("/elsewhere") }))
            .route("/elsewhere", get(|| async { "followed" }));
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let client = super::key_in_query_client(None);
        let request = client.get(format!("http://{addr}/api?serviceKey=secret"));
        let error = super::send(request, "test").await.unwrap_err();
        assert_eq!(
            error
                .downcast_ref::<super::UpstreamStatus>()
                .map(|s| s.status),
            Some(307)
        );
    }

    #[test]
    fn retries_only_transient_http_statuses() {
        assert!(should_retry_status(StatusCode::TOO_MANY_REQUESTS));
        assert!(should_retry_status(StatusCode::REQUEST_TIMEOUT));
        assert!(should_retry_status(StatusCode::BAD_GATEWAY));
        assert!(!should_retry_status(StatusCode::BAD_REQUEST));
        assert!(!should_retry_status(StatusCode::UNAUTHORIZED));
        assert!(!should_retry_status(StatusCode::NOT_FOUND));
    }
}
