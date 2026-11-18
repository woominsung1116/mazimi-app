//! Compliance-gated benefit-page fetcher.
//!
//! This worker never discovers arbitrary URLs and never publishes a program.
//! It only turns an admin-queued URL into a small, plain-text review excerpt
//! when its exact domain and path have a current compliance approval. HTML,
//! images, scripts and social-platform pages are not retained.

use crate::source_monitoring::{self, ContentChange, PageCheck, UnavailableKind};
use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Utc};
use mazimi_core::source_policy::{
    canonical_url_string, classify_candidate_kind, contains_benefit_signal, host_is_within,
    is_interactive_or_purchase_url, path_is_allowed, strip_tracking_query_parameters,
    CandidateKind,
};
use mazimi_core::source_policy::{RobotsAnswer, RobotsPolicy, RobotsVerdict};
use regex::Regex;
use reqwest::{
    header::{
        ACCEPT, ACCEPT_LANGUAGE, CONTENT_TYPE, ETAG, IF_MODIFIED_SINCE, IF_NONE_MATCH,
        LAST_MODIFIED, LOCATION, RETRY_AFTER,
    },
    redirect::Policy,
    Client, StatusCode, Url,
};
use scraper::{Html, Selector};
use sqlx::{FromRow, PgPool};
use std::{
    collections::HashSet,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::LazyLock,
    time::Duration,
};
use tokio::net::lookup_host;
use tracing::{info, warn};
use uuid::Uuid;

use super::{fnv_hash_hex, source_access};

pub(crate) const BOT_PRODUCT_TOKEN: &str = "MazimiBenefitBot";
const MAX_ROBOTS_BYTES: usize = 256 * 1024;
const MAX_REVIEW_TEXT_CHARS: usize = 12_000;
/// Below this many characters a page is most likely a script-rendered shell
/// or an error page. Such text never becomes a verified baseline on its own.
const MIN_REVIEW_TEXT_CHARS: usize = 200;

#[derive(Debug, Clone)]
pub struct RobotsPreflightResult {
    pub robots_url: String,
    pub allowed: bool,
    pub checked_at: DateTime<Utc>,
    pub content_hash: String,
}

#[derive(Debug, thiserror::Error)]
#[error("robots.txt returned HTTP {status}")]
struct RobotsAccessError {
    status: u16,
    permanent: bool,
    retry_at: DateTime<Utc>,
}

pub(crate) fn retry_at(headers: &reqwest::header::HeaderMap, now: DateTime<Utc>) -> DateTime<Utc> {
    let fallback = now + chrono::Duration::days(1);
    headers
        .get(RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            value
                .parse::<i64>()
                .ok()
                .and_then(chrono::Duration::try_seconds)
                .and_then(|delay| now.checked_add_signed(delay))
                .or_else(|| {
                    DateTime::parse_from_rfc2822(value)
                        .ok()
                        .map(|date| date.with_timezone(&Utc))
                })
        })
        .map(|date| date.max(now + chrono::Duration::minutes(1)))
        .unwrap_or(fallback)
}

#[derive(Debug)]
pub(crate) struct RobotsDocument {
    pub(crate) url: String,
    pub(crate) bytes: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct BenefitPageFetcher {
    timeout: Duration,
    max_body_bytes: usize,
    max_pages_per_run: i64,
    max_links_per_page: usize,
}

#[derive(Debug, Default)]
pub struct FetchSummary {
    pub claimed: usize,
    pub ready_for_review: usize,
    pub failed: usize,
    pub blocked: usize,
}

#[derive(Debug, FromRow)]
struct Candidate {
    id: Uuid,
    domain_id: Uuid,
    url: String,
    domain: String,
    allow_subdomains: bool,
    allowed_path_prefixes: Vec<String>,
    min_request_interval_ms: i32,
    crawler_contact_url: String,
    etag: Option<String>,
    last_modified: Option<String>,
    candidate_kind: String,
    discovery_depth: i32,
    content_hash: Option<String>,
    monitored: bool,
    recheck_interval_seconds: i32,
}

#[derive(Debug)]
enum FetchOutcome {
    Ready,
    Failed,
    Blocked,
}

/// Performs the same SSRF-safe, no-redirect, strict robots check
/// ([`CRAWLER_ROBOTS`]) used immediately before collection. The API stores this evidence separately from crawl
/// approval; a successful preflight never enables collection by itself.
pub async fn preflight_robots(
    domain: &str,
    paths: &[String],
    crawler_contact_url: &str,
    timeout: Duration,
) -> Result<RobotsPreflightResult> {
    if paths.is_empty() {
        bail!("robots preflight requires at least one path");
    }
    if paths.iter().any(|path| {
        !path.starts_with('/')
            || path.starts_with("//")
            || path.contains("..")
            || path.contains('?')
            || path.contains('#')
    }) {
        bail!("robots preflight received an invalid absolute path");
    }
    let page_url = Url::parse(&format!("https://{domain}/"))
        .context("registered source domain is not a valid HTTPS host")?;
    if page_url.host_str() != Some(domain) {
        bail!("registered source domain is not canonical");
    }
    let user_agent = format!("{BOT_PRODUCT_TOKEN}/1.0 (+{crawler_contact_url})");
    let client = build_pinned_client(&page_url, &user_agent, timeout).await?;
    let document =
        fetch_robots_document(&client, &page_url, &user_agent, timeout, CRAWLER_ROBOTS).await?;
    let body = String::from_utf8_lossy(&document.bytes);
    let allowed = paths
        .iter()
        .all(|path| robots_allows(&body, BOT_PRODUCT_TOKEN, path));
    Ok(RobotsPreflightResult {
        robots_url: document.url,
        allowed,
        checked_at: Utc::now(),
        content_hash: fnv_hash_hex(&document.bytes),
    })
}

impl BenefitPageFetcher {
    /// The crawler is disabled unless explicitly enabled. No default contact
    /// identity is accepted: every approved domain stores its reviewed public
    /// contact URL and that URL is placed in the bot user-agent.
    pub fn from_env() -> Result<Option<Self>> {
        let enabled = std::env::var("BENEFIT_CRAWLER_ENABLED")
            .unwrap_or_default()
            .to_ascii_lowercase();
        if !matches!(enabled.as_str(), "true" | "1" | "yes") {
            return Ok(None);
        }

        let timeout_secs = parse_bounded_env("BENEFIT_CRAWLER_TIMEOUT_SECS", 20, 5, 60)?;
        let max_body_bytes = parse_bounded_env(
            "BENEFIT_CRAWLER_MAX_BODY_BYTES",
            1_048_576,
            65_536,
            2_097_152,
        )?;
        let max_pages_per_run = parse_bounded_env("BENEFIT_CRAWLER_MAX_PAGES", 20, 1, 100)?;
        let max_links_per_page =
            parse_bounded_env("BENEFIT_CRAWLER_MAX_LINKS_PER_PAGE", 50, 1, 100)?;

        Ok(Some(Self {
            timeout: Duration::from_secs(timeout_secs),
            max_body_bytes: max_body_bytes as usize,
            max_pages_per_run: max_pages_per_run as i64,
            max_links_per_page: max_links_per_page as usize,
        }))
    }

    pub async fn run(&self, pool: &PgPool) -> Result<FetchSummary> {
        let candidates = self.claim_candidates(pool).await?;
        let mut summary = FetchSummary {
            claimed: candidates.len(),
            ..FetchSummary::default()
        };

        for candidate in candidates {
            // An earlier page in this batch may have imposed a domain-wide
            // Retry-After. Do not park a worker task for hours or fetch siblings.
            let cooling_down: bool = sqlx::query_scalar("SELECT COALESCE(next_request_at>now()+interval '10 minutes',false) FROM benefit_source_domains WHERE id=$1")
                .bind(candidate.domain_id).fetch_one(pool).await?;
            if cooling_down {
                summary.blocked += 1;
                continue;
            }
            match self.fetch_candidate(pool, &candidate).await {
                Ok(FetchOutcome::Ready) => summary.ready_for_review += 1,
                Ok(FetchOutcome::Failed) => summary.failed += 1,
                Ok(FetchOutcome::Blocked) => summary.blocked += 1,
                Err(error) => {
                    warn!(candidate_id = %candidate.id, error = %crate::error_safety::persisted_error(&error), "benefit page fetch failed");
                    if candidate.monitored {
                        if let Err(monitoring_error) = source_monitoring::record_fetch_failure(
                            pool,
                            candidate.id,
                            &error.to_string(),
                        )
                        .await
                        {
                            warn!(
                                candidate_id = %candidate.id,
                                error = %monitoring_error,
                                "failed to record monitored source error"
                            );
                        }
                    } else {
                        mark_candidate_error(pool, candidate.id, &error.to_string()).await;
                    }
                    summary.failed += 1;
                }
            }
        }

        Ok(summary)
    }

    /// Tests only: a fetcher with the production bounds.
    #[cfg(test)]
    pub(crate) fn for_tests() -> Self {
        Self {
            timeout: Duration::from_secs(5),
            max_body_bytes: 1_048_576,
            max_pages_per_run: 100,
            max_links_per_page: 50,
        }
    }

    /// Tests only: the candidate ids the next run would claim (and claims).
    #[cfg(test)]
    pub(crate) async fn claim_ids_for_tests(&self, pool: &PgPool) -> Result<Vec<Uuid>> {
        Ok(self
            .claim_candidates(pool)
            .await?
            .into_iter()
            .map(|c| c.id)
            .collect())
    }

    async fn claim_candidates(&self, pool: &PgPool) -> Result<Vec<Candidate>> {
        sqlx::query_as::<_, Candidate>(
            &"WITH selected AS (
                 SELECT c.id, c.content_hash
                 FROM benefit_source_candidates c
                 JOIN benefit_source_domains d ON d.id = c.domain_id
                 WHERE NOT {hub_owned}
                   AND (
                         c.status IN ('queued', 'error')
                         OR (c.status = 'fetched' AND c.last_checked_at < now() - interval '1 hour')
                         OR (
                             c.status IN ('approved', 'change_review', 'unreachable_review', 'verification_due')
                             AND c.next_check_at IS NOT NULL
                             AND c.next_check_at <= now()
                             AND EXISTS (
                                 SELECT 1
                                 FROM program_source_monitors monitor
                                 WHERE monitor.candidate_id = c.id
                                   AND monitor.verification_state <> 'retired'
                             )
                         )
                       )
                   AND (
                       c.attempt_count < 5
                       OR (c.status='error' AND c.last_checked_at<now()-interval '7 days')
                       OR EXISTS (
                           SELECT 1
                           FROM program_source_monitors monitor
                           WHERE monitor.candidate_id = c.id
                             AND monitor.verification_state <> 'retired'
                       )
                   )
                   AND (c.next_check_at IS NULL OR c.next_check_at <= now())
                   AND d.is_active = true
                   AND d.verification_status = 'verified'
                   AND d.crawl_allowed = true
                   AND d.compliance_status = 'approved'
                   AND d.robots_status = 'allowed'
                   AND d.approval_expires_at > now()
                   AND (d.next_request_at IS NULL OR d.next_request_at<=now()+interval '10 minutes')
                   -- A publisher collection opt-out (domain or URL prefix).
                   AND NOT EXISTS (
                       SELECT 1 FROM benefit_source_optouts o
                       WHERE o.lifted_at IS NULL AND o.effective_at IS NOT NULL
                         AND o.scope IN ('collection', 'both')
                         AND ((o.url_prefix IS NULL AND d.id = ANY(o.domain_ids))
                              OR (o.url_prefix IS NOT NULL AND starts_with(c.url, o.url_prefix)))
                   )
                   -- Evidence verification (hide_only or live) owns sources
                   -- whose every monitor is evidence_auto; the hash fetcher
                   -- neither holds nor releases them.
                   AND NOT (
                       $2
                       AND EXISTS (SELECT 1 FROM program_source_monitors m
                                   WHERE m.candidate_id = c.id AND m.verification_state <> 'retired')
                       AND NOT EXISTS (SELECT 1 FROM program_source_monitors m
                                       WHERE m.candidate_id = c.id AND m.verification_state <> 'retired'
                                         AND m.verification_mode <> 'evidence_auto')
                   )
                 ORDER BY c.next_check_at NULLS FIRST, c.discovered_at ASC
                 FOR UPDATE OF c SKIP LOCKED
                 LIMIT $1
             )
             UPDATE benefit_source_candidates c
             SET status = 'fetched',
                 attempt_count = c.attempt_count + 1,
                 last_checked_at = now(),
                 last_error = NULL,
                 updated_at = now()
             FROM selected s, benefit_source_domains d
             WHERE c.id = s.id AND d.id = c.domain_id
             RETURNING c.id, c.domain_id, c.url, d.domain,
                       d.allow_subdomains, d.allowed_path_prefixes,
                       d.min_request_interval_ms, d.crawler_contact_url,
                       CASE WHEN EXISTS(SELECT 1 FROM program_source_monitors m WHERE m.candidate_id=c.id AND m.verification_state NOT IN ('current','retired')) THEN NULL ELSE c.etag END AS etag,
                       CASE WHEN EXISTS(SELECT 1 FROM program_source_monitors m WHERE m.candidate_id=c.id AND m.verification_state NOT IN ('current','retired')) THEN NULL ELSE c.last_modified END AS last_modified, c.candidate_kind,
                       c.discovery_depth, s.content_hash,
                       EXISTS (
                           SELECT 1
                           FROM program_source_monitors monitor
                           WHERE monitor.candidate_id = c.id
                             AND monitor.verification_state <> 'retired'
                       ) AS monitored,
                       COALESCE(
                           (
                               SELECT min(monitor.recheck_interval_seconds)
                               FROM program_source_monitors monitor
                               WHERE monitor.candidate_id = c.id
                                 AND monitor.verification_state <> 'retired'
                           ),
                           c.recheck_interval_seconds
                       ) AS recheck_interval_seconds"
                // An issuer hub's list page is the door job's alone.
                .replace("{hub_owned}", &crate::corporate_auto::policy::hub_owned("c")),
        )
        .bind(self.max_pages_per_run)
        .bind(crate::corporate_auto::evidence::owns_sources())
        .fetch_all(pool)
        .await
        .context("failed to claim approved benefit candidates")
    }

    async fn fetch_candidate(&self, pool: &PgPool, candidate: &Candidate) -> Result<FetchOutcome> {
        let url = validate_candidate_scope(candidate)?;
        let user_agent = format!(
            "{BOT_PRODUCT_TOKEN}/1.0 (+{})",
            candidate.crawler_contact_url
        );
        let client = build_pinned_client(&url, &user_agent, self.timeout).await?;

        if reserve_request_slot(pool, candidate).await? == SlotOutcome::Deferred {
            return Ok(FetchOutcome::Blocked);
        }
        let robots_result = self
            .check_robots(pool, candidate, &client, &url, &user_agent)
            .await;
        match robots_result {
            Ok(true) => {}
            Ok(false) => {
                block_domain(
                    pool,
                    candidate,
                    "blocked",
                    "robots.txt disallows this URL for MazimiBenefitBot",
                    true,
                    None,
                )
                .await?;
                return Ok(FetchOutcome::Blocked);
            }
            Err(error) => {
                let access = error.downcast_ref::<RobotsAccessError>();
                block_domain(
                    pool,
                    candidate,
                    "unavailable",
                    &format!("robots.txt could not be verified: {error}"),
                    access.is_some_and(|value| value.permanent),
                    access.map(|value| value.retry_at),
                )
                .await?;
                return Ok(FetchOutcome::Failed);
            }
        }

        if reserve_request_slot(pool, candidate).await? == SlotOutcome::Deferred {
            return Ok(FetchOutcome::Blocked);
        }

        let mut request = client
            .get(url.clone())
            .header(ACCEPT, "text/html,application/xhtml+xml;q=0.9")
            .header(ACCEPT_LANGUAGE, "ko-KR,ko;q=0.9,en;q=0.5");
        if let Some(etag) = candidate.etag.as_deref() {
            request = request.header(IF_NONE_MATCH, etag);
        }
        if let Some(last_modified) = candidate.last_modified.as_deref() {
            request = request.header(IF_MODIFIED_SINCE, last_modified);
        }

        let response = request.send().await.context("page request failed")?;
        let status = response.status();
        if status == StatusCode::NOT_MODIFIED {
            if candidate.monitored {
                source_monitoring::record_not_modified(
                    pool,
                    candidate.id,
                    candidate.content_hash.as_deref(),
                    candidate.recheck_interval_seconds,
                )
                .await?;
                return Ok(FetchOutcome::Ready);
            }
            sqlx::query(
                "UPDATE benefit_source_candidates
                 SET status = 'review', http_status = 304, next_check_at = NULL,
                     last_checked_at = now(), updated_at = now()
                 WHERE id = $1",
            )
            .bind(candidate.id)
            .execute(pool)
            .await?;
            return Ok(FetchOutcome::Ready);
        }
        if matches!(
            status,
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN | StatusCode::TOO_MANY_REQUESTS
        ) {
            block_domain(
                pool,
                candidate,
                "blocked",
                &format!("source returned HTTP {status}; automated access stopped"),
                status != StatusCode::TOO_MANY_REQUESTS,
                Some(retry_at(response.headers(), Utc::now())),
            )
            .await?;
            return Ok(FetchOutcome::Blocked);
        }
        if status.is_redirection() {
            // A same-scope move is requeued, not blindly followed. The next
            // request repeats DNS pinning and robots checks at the new path.
            if let Some(location) = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|h| h.to_str().ok())
            {
                if let Ok(next) = url.join(location) {
                    if next.scheme() == "https"
                        && next.username().is_empty()
                        && next.password().is_none()
                        && next.port_or_known_default() == Some(443)
                        && next.host_str().is_some_and(|h| {
                            host_is_within(h, &candidate.domain, candidate.allow_subdomains)
                        })
                        && path_is_allowed(next.path(), &candidate.allowed_path_prefixes)
                        && !is_interactive_or_purchase_url(&next)
                        && next != url
                    {
                        let moved = sqlx::query("UPDATE benefit_source_candidates SET url=$2, relocation_attempts=relocation_attempts+1,
                            status='queued', etag=NULL, last_modified=NULL, next_check_at=now()+interval '10 minutes', updated_at=now()
                            WHERE id=$1 AND relocation_attempts<5")
                            .bind(candidate.id).bind(next.as_str()).execute(pool).await?.rows_affected();
                        if moved > 0 {
                            return Ok(FetchOutcome::Ready);
                        }
                    }
                }
            }
            if candidate.monitored {
                source_monitoring::hold_source_unavailable(
                    pool,
                    candidate.id,
                    UnavailableKind::Redirected,
                    Some(status.as_u16() as i32),
                    "source URL now redirects; final URL requires scope and policy review",
                )
                .await?;
                return Ok(FetchOutcome::Blocked);
            }
            bail!("redirects are not followed; queue the reviewed final HTTPS URL (HTTP {status})");
        }
        if matches!(status, StatusCode::NOT_FOUND | StatusCode::GONE) && candidate.monitored {
            source_monitoring::hold_source_unavailable(
                pool,
                candidate.id,
                UnavailableKind::NotFound,
                Some(status.as_u16() as i32),
                &format!("source returned HTTP {status}"),
            )
            .await?;
            return Ok(FetchOutcome::Blocked);
        }
        if !status.is_success() {
            bail!("source returned HTTP {status}");
        }

        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !content_type.starts_with("text/html")
            && !content_type.starts_with("application/xhtml+xml")
        {
            bail!("unsupported content type: {content_type}");
        }
        if response
            .content_length()
            .is_some_and(|size| size > self.max_body_bytes as u64)
        {
            bail!("page exceeds the configured body-size limit");
        }

        let etag = header_string(response.headers().get(ETAG));
        let last_modified = header_string(response.headers().get(LAST_MODIFIED));
        let bytes = read_limited_body(response, self.max_body_bytes).await?;
        let html = decode_page(&bytes, &content_type)?;
        let (title, review_text) = extract_review_text(&html);
        let mut link_keys = Vec::new();
        let discovered_links =
            if candidate.candidate_kind == "index" && candidate.discovery_depth < 3 {
                let links = discover_benefit_links(&html, &url, candidate, self.max_links_per_page);
                link_keys = links
                    .iter()
                    .map(|link| link.canonical_url.clone())
                    .collect();
                let count = queue_discovered_links(pool, candidate, links).await?;
                if count > 0 {
                    info!(
                        candidate_id = %candidate.id,
                        discovered_links = count,
                        "official benefit index yielded detail-page candidates"
                    );
                }
                count
            } else {
                0
            };
        if discovered_links == 0 && review_text.is_empty() {
            bail!("no reviewable benefit text or in-scope benefit links found in the page");
        }
        // HTML is never retained. The fingerprint covers the title, every
        // main/article block and the in-scope link set, with volatile tokens
        // (timestamps, view counts, D-day) normalized so they cannot flap.
        let content_hash = content_fingerprint(title.as_deref(), &review_text, &link_keys);
        let thin_content =
            discovered_links == 0 && review_text.chars().count() < MIN_REVIEW_TEXT_CHARS;

        if candidate.monitored {
            let classification = source_monitoring::record_page_check(
                pool,
                PageCheck {
                    candidate_id: candidate.id,
                    previous_content_hash: candidate.content_hash.as_deref(),
                    content_hash: &content_hash,
                    page_title: title.as_deref(),
                    review_text: (discovered_links == 0).then_some(review_text.as_str()),
                    etag: etag.as_deref(),
                    last_modified: last_modified.as_deref(),
                    http_status: status.as_u16() as i32,
                    fetched_bytes: bytes.len() as i32,
                    recheck_interval_seconds: candidate.recheck_interval_seconds,
                    thin_content,
                },
            )
            .await?;
            match classification {
                ContentChange::BaselineEstablished => info!(
                    candidate_id = %candidate.id,
                    "source monitoring baseline established"
                ),
                ContentChange::Unchanged => info!(
                    candidate_id = %candidate.id,
                    "monitored source is unchanged"
                ),
                ContentChange::Changed => warn!(
                    candidate_id = %candidate.id,
                    "monitored source changed; linked programs placed on review hold"
                ),
            }
            return Ok(FetchOutcome::Ready);
        }

        let status = if discovered_links > 0 {
            "extracted"
        } else {
            "review"
        };
        let stored_review_text = (discovered_links == 0).then_some(review_text);
        // A near-empty extract is flagged for the reviewer; an enrichment draft
        // may later replace this value with the model's own confidence.
        let extraction_confidence: Option<f32> = thin_content.then_some(0.1);

        sqlx::query(
            "UPDATE benefit_source_candidates
             SET status = $2, page_title = $3, raw_text = $4,
                 content_hash = $5, etag = $6, last_modified = $7,
                 http_status = 200, fetched_bytes = $8, next_check_at = NULL,
                 extraction_confidence = COALESCE($9, extraction_confidence),
                 last_error = NULL, last_checked_at = now(), updated_at = now()
             WHERE id = $1",
        )
        .bind(candidate.id)
        .bind(status)
        .bind(title)
        .bind(stored_review_text)
        .bind(content_hash)
        .bind(etag)
        .bind(last_modified)
        .bind(bytes.len() as i32)
        .bind(extraction_confidence)
        .execute(pool)
        .await?;

        info!(candidate_id = %candidate.id, "benefit page queued for human review");
        Ok(FetchOutcome::Ready)
    }

    async fn check_robots(
        &self,
        pool: &PgPool,
        candidate: &Candidate,
        client: &Client,
        page_url: &Url,
        user_agent: &str,
    ) -> Result<bool> {
        let document =
            fetch_robots_document(client, page_url, user_agent, self.timeout, CRAWLER_ROBOTS)
                .await?;
        let body = String::from_utf8_lossy(&document.bytes);
        let target = match page_url.query() {
            Some(query) => format!("{}?{query}", page_url.path()),
            None => page_url.path().to_string(),
        };
        let allowed = robots_allows(&body, BOT_PRODUCT_TOKEN, &target);
        let robots_hash = fnv_hash_hex(&document.bytes);
        update_robots_evidence(
            pool,
            candidate.domain_id,
            &document.url,
            allowed,
            &robots_hash,
        )
        .await?;
        Ok(allowed)
    }
}

async fn update_robots_evidence<'e, E>(
    executor: E,
    domain_id: Uuid,
    robots_url: &str,
    allowed: bool,
    robots_hash: &str,
) -> Result<()>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    // The table requires crawl_allowed => robots_status=allowed. Revoke access
    // atomically with the disallow evidence so that constraint cannot reject it.
    sqlx::query(
        "UPDATE benefit_source_domains SET robots_url=$2,robots_status=$3,robots_checked_at=now(),
        crawl_allowed=CASE WHEN $3='blocked' THEN false ELSE crawl_allowed END,
        compliance_status=CASE WHEN $3='blocked' THEN 'rejected' ELSE compliance_status END,
        robots_content_hash=$4,updated_at=now() WHERE id=$1",
    )
    .bind(domain_id)
    .bind(robots_url)
    .bind(if allowed { "allowed" } else { "blocked" })
    .bind(robots_hash)
    .execute(executor)
    .await?;
    Ok(())
}

fn parse_bounded_env(name: &str, default: u64, min: u64, max: u64) -> Result<u64> {
    let value = match std::env::var(name) {
        Ok(value) if !value.trim().is_empty() => value
            .parse::<u64>()
            .with_context(|| format!("{name} must be an integer"))?,
        _ => default,
    };
    if !(min..=max).contains(&value) {
        bail!("{name} must be between {min} and {max}");
    }
    Ok(value)
}

fn validate_candidate_scope(candidate: &Candidate) -> Result<Url> {
    source_access::validate_scope(
        &candidate.url,
        &candidate.domain,
        candidate.allow_subdomains,
        &candidate.allowed_path_prefixes,
    )
}

pub(crate) async fn build_pinned_client(
    url: &Url,
    user_agent: &str,
    timeout: Duration,
) -> Result<Client> {
    let host = url.host_str().ok_or_else(|| anyhow!("URL has no host"))?;
    let addresses: Vec<SocketAddr> = lookup_host((host, 443))
        .await
        .context("DNS lookup failed")?
        .collect();
    if addresses.is_empty() {
        bail!("DNS returned no addresses");
    }
    if addresses.iter().any(|address| !is_public_ip(address.ip())) {
        bail!("DNS returned a non-public address; request blocked");
    }
    let pinned = addresses[0];

    Client::builder()
        .timeout(timeout)
        .connect_timeout(Duration::from_secs(10))
        .redirect(Policy::none())
        .no_proxy()
        .user_agent(user_agent)
        .resolve(host, pinned)
        .build()
        .context("failed to build pinned HTTP client")
}

/// Redirects followed for `/robots.txt` under [`RobotsPolicy::Rfc9309`]
/// (RFC 9309 §2.3.1.2); a strict caller follows none.
pub(crate) const MAX_ROBOTS_REDIRECTS: usize = 5;
/// The approved-page ingestion crawler and the domain-approval preflight.
pub(crate) const CRAWLER_ROBOTS: RobotsPolicy = RobotsPolicy::Strict;

/// What one `/robots.txt` response is, before any policy, or where it
/// redirects (followed only under [`RobotsPolicy::Rfc9309`], public https).
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RobotsStep {
    Answer(RobotsAnswer),
    /// A 200 robots file: read its body.
    File,
    Redirect(Url),
}

pub(crate) fn robots_step(
    policy: RobotsPolicy,
    status: StatusCode,
    content_type: &str,
    location: Option<Url>,
) -> RobotsStep {
    let content_type = content_type.to_ascii_lowercase();
    let plain = content_type.is_empty()
        || content_type.starts_with("text/plain")
        || content_type.starts_with("application/octet-stream");
    if status.is_redirection() {
        return match (policy, location) {
            (RobotsPolicy::Rfc9309, Some(next))
                if next.scheme() == "https"
                    && mazimi_core::application_resources::public_url(next.as_str()) =>
            {
                RobotsStep::Redirect(next)
            }
            _ => RobotsStep::Answer(RobotsAnswer::Unavailable("robots_redirect".into())),
        };
    }
    if status == StatusCode::TOO_MANY_REQUESTS
        || status.is_server_error()
        || (!status.is_client_error() && status != StatusCode::OK)
    {
        return RobotsStep::Answer(RobotsAnswer::Unavailable(format!(
            "robots_http_{}",
            status.as_u16()
        )));
    }
    if status.is_client_error() {
        return RobotsStep::Answer(RobotsAnswer::ClientError(status.as_u16()));
    }
    if plain {
        RobotsStep::File
    } else {
        RobotsStep::Answer(RobotsAnswer::Html)
    }
}

/// Reads `/robots.txt` for the page's origin and applies the caller's
/// [`RobotsPolicy`] (`mazimi_core::source_policy`): rules, or an empty
/// document for no restrictions. A refusal is a [`RobotsAccessError`],
/// `permanent` when the policy disallows everything (strict 401/403); an
/// origin that does not answer (or a strict caller's HTML bot wall or
/// redirect) is a non-permanent one. A redirect to another host (RFC 9309
/// only) gets its own DNS-pinned client.
pub(crate) async fn fetch_robots_document(
    client: &Client,
    page_url: &Url,
    user_agent: &str,
    timeout: Duration,
    policy: RobotsPolicy,
) -> Result<RobotsDocument> {
    let mut url = page_url.clone();
    url.set_path("/robots.txt");
    url.set_query(None);
    url.set_fragment(None);
    let mut hop_client: Option<Client> = None;
    let mut last_status = StatusCode::OK;
    let mut retry = Utc::now() + chrono::Duration::days(1);
    let mut answer = RobotsAnswer::Unavailable("robots_redirect".into());
    for _ in 0..=MAX_ROBOTS_REDIRECTS {
        let response = hop_client
            .as_ref()
            .unwrap_or(client)
            .get(url.clone())
            .header(ACCEPT, "text/plain")
            // The deadline holds whatever client the caller passed: a robots.txt
            // that never answers fails closed after `timeout`.
            .timeout(timeout)
            .send()
            .await
            .context("robots request failed")?;
        last_status = response.status();
        retry = retry_at(response.headers(), Utc::now());
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_string();
        let location = response
            .headers()
            .get(LOCATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| url.join(value).ok());
        match robots_step(policy, response.status(), &content_type, location) {
            RobotsStep::Redirect(next) => {
                if next.host_str() != url.host_str()
                    || next.port_or_known_default() != url.port_or_known_default()
                {
                    hop_client = Some(build_pinned_client(&next, user_agent, timeout).await?);
                }
                url = next;
                continue;
            }
            RobotsStep::Answer(a) => answer = a,
            RobotsStep::File => {
                if response
                    .content_length()
                    .is_some_and(|size| size > MAX_ROBOTS_BYTES as u64)
                {
                    bail!("robots.txt is unexpectedly large");
                }
                let bytes = read_limited_body(response, MAX_ROBOTS_BYTES).await?;
                // An HTML page served as text is a page, not a robots file.
                answer = if robots_response_looks_like_html(&bytes) {
                    RobotsAnswer::Html
                } else {
                    RobotsAnswer::Rules(String::from_utf8_lossy(&bytes).into_owned())
                };
            }
        }
        break;
    }
    let document = |bytes: Vec<u8>| RobotsDocument {
        url: url.to_string(),
        bytes,
    };
    match policy.verdict(&answer) {
        RobotsVerdict::Rules(body) => Ok(document(body.into_bytes())),
        RobotsVerdict::Allow => Ok(document(Vec::new())),
        RobotsVerdict::DisallowAll => Err(RobotsAccessError {
            status: last_status.as_u16(),
            permanent: true,
            retry_at: retry,
        }
        .into()),
        RobotsVerdict::Unavailable(code) => Err(anyhow::Error::from(RobotsAccessError {
            status: last_status.as_u16(),
            permanent: false,
            retry_at: retry,
        })
        .context(format!("robots.txt could not be used: {code}"))),
    }
}

pub(crate) async fn read_limited_body(
    mut response: reqwest::Response,
    limit: usize,
) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .context("failed to read source body")?
    {
        if chunk.len() > limit.saturating_sub(bytes.len()) {
            bail!("source body exceeds size limit");
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

pub(crate) fn decode_page(bytes: &[u8], content_type: &str) -> Result<String> {
    let declared = content_type
        .split(';')
        .find_map(|part| part.trim().strip_prefix("charset="))
        .map(|label| label.trim_matches(['\"', '\'']).trim());
    let encoding = declared.and_then(|label| encoding_rs::Encoding::for_label(label.as_bytes()));
    if let Some(encoding) = encoding {
        let (text, _, invalid) = encoding.decode(bytes);
        if invalid {
            bail!("invalid declared page encoding");
        }
        return Ok(text.into_owned());
    }
    if let Ok(text) = std::str::from_utf8(bytes) {
        return Ok(text.to_owned());
    }
    let prefix = String::from_utf8_lossy(&bytes[..bytes.len().min(4096)]).to_ascii_lowercase();
    if prefix.contains("euc-kr") || prefix.contains("ks_c_5601") || prefix.contains("windows-949") {
        let (text, _, invalid) = encoding_rs::EUC_KR.decode(bytes);
        if !invalid {
            return Ok(text.into_owned());
        }
    }
    bail!("unknown or invalid page encoding; refusing to reinterpret eligibility text")
}

pub(crate) fn robots_response_looks_like_html(bytes: &[u8]) -> bool {
    let prefix = String::from_utf8_lossy(&bytes[..bytes.len().min(1_024)]).to_ascii_lowercase();
    prefix.contains("<!doctype html") || prefix.contains("<html")
}

fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_public_ipv4(ip),
        IpAddr::V6(ip) => is_public_ipv6(ip),
    }
}

fn is_public_ipv4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _d] = ip.octets();
    !matches!(
        (a, b, c),
        (0, _, _)
            | (10, _, _)
            | (100, 64..=127, _)
            | (127, _, _)
            | (169, 254, _)
            | (172, 16..=31, _)
            | (192, 0, 0)
            | (192, 0, 2)
            | (192, 168, _)
            | (198, 18..=19, _)
            | (198, 51, 100)
            | (203, 0, 113)
            | (224..=255, _, _)
    )
}

fn is_public_ipv6(ip: Ipv6Addr) -> bool {
    if ip.is_unspecified() || ip.is_loopback() || ip.is_multicast() {
        return false;
    }
    if let Some(mapped) = ip.to_ipv4_mapped() {
        return is_public_ipv4(mapped);
    }
    let segments = ip.segments();
    // Only global unicast; exclude IPv4-compatible/NAT64/transition addresses
    // and IETF special-purpose space that can otherwise reach local networks.
    if (segments[0] & 0xe000) != 0x2000
        || segments[0] == 0x2002
        || (segments[0] == 0x2001 && segments[1] < 0x0200)
    {
        return false;
    }
    let unique_local = (segments[0] & 0xfe00) == 0xfc00;
    let link_local = (segments[0] & 0xffc0) == 0xfe80;
    let documentation = segments[0] == 0x2001 && segments[1] == 0x0db8;
    !(unique_local || link_local || documentation)
}

fn header_string(value: Option<&reqwest::header::HeaderValue>) -> Option<String> {
    value
        .and_then(|value| value.to_str().ok())
        .map(|value| value.chars().take(1_000).collect())
}

pub(crate) fn extract_review_text(html: &str) -> (Option<String>, String) {
    let document = Html::parse_document(html);
    let title = Selector::parse("title")
        .ok()
        .and_then(|selector| document.select(&selector).next())
        .map(|element| clean_fragment(&element.text().collect::<Vec<_>>().join(" ")))
        .filter(|value| !value.is_empty())
        .map(|value| value.chars().take(500).collect());

    // Every main/article block counts: list pages keep benefits in sibling
    // articles, and an app shell with an empty <main> must not hide a change
    // elsewhere. Only a page without any such landmark falls back to <body>.
    let has_content_landmark = Selector::parse("main, article")
        .ok()
        .is_some_and(|selector| document.select(&selector).next().is_some());
    let scope_tags: &[&str] = if has_content_landmark {
        &["main", "article"]
    } else {
        &["body"]
    };
    // Read text nodes rather than site-specific CSS or a fixed set of tags.
    // A paragraph changed to div/span yields the same normalized content.
    // Navigation, executable text and hidden controls are excluded. Walking
    // the whole document once also prevents nested blocks from repeating text.
    let mut fragments = Vec::new();
    for node in document.root_element().descendants() {
        let Some(text) = node.value().as_text() else {
            continue;
        };
        let mut in_scope = false;
        let mut excluded = false;
        for ancestor in node.ancestors() {
            let Some(element) = ancestor.value().as_element() else {
                continue;
            };
            if scope_tags.contains(&element.name()) {
                in_scope = true;
            }
            if matches!(
                element.name(),
                "script"
                    | "style"
                    | "noscript"
                    | "nav"
                    | "footer"
                    | "header"
                    | "button"
                    | "input"
                    | "select"
            ) || element.attr("hidden").is_some()
                || element.attr("aria-hidden") == Some("true")
            {
                excluded = true;
                break;
            }
        }
        if in_scope && !excluded {
            fragments.push(text.to_string());
        }
    }
    let output = clean_fragment(&fragments.join(" "))
        .chars()
        .take(MAX_REVIEW_TEXT_CHARS)
        .collect::<String>();
    (title, output)
}

fn clean_fragment(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Tokens that change on every render without the offer changing. Only
/// *labelled* publish timestamps (등록일/작성일/수정일/updated …, with their
/// time part), labelled relative ages, board counters, D-day badges and
/// sub-day relative times are normalized. Anything else that looks like a
/// date or time is material and left alone: prices, percentages, plain
/// deadlines ("2026년 8월 31일까지"), deadlines with a time
/// ("접수 마감: 2026. 8. 31.(월) 18:00까지"), opening hours ("09:00~18:00")
/// and cancellation terms ("마감 7일 전").
static VOLATILE_TOKEN_PATTERNS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    [
        // Labelled date, optionally with weekday and time: 등록일 2026.10.01 (수) 14:33:05
        (
            r"(?i)(등록일|작성일|게시일|수정일|최종\s*수정|업데이트|updated|posted|published)\s*[:：]?\s*\d{4}[.\-/년]\s?\d{1,2}[.\-/월]\s?\d{1,2}일?\.?(\s*\([^)]{1,3}\))?(\s*\d{1,2}:\d{2}(:\d{2})?)?",
            "$1 <date>",
        ),
        // Labelled bare time: 등록 14:33, updated 09:10:00
        (
            r"(?i)(등록|작성|게시|수정|업데이트|updated|posted|published)\s*(시간|시각|time)?\s*[:：]?\s*\d{1,2}:\d{2}(:\d{2})?",
            "$1 <time>",
        ),
        // Board counters. The number must not be the start of an amount
        // ("친구추천 10,000원") and the label must stand alone ("추천 12",
        // not "친구추천").
        (
            r"(?i)(^|[^가-힣])(조회수?|views?|hits?|댓글|좋아요|공감|추천수?)\s*[:：]?\s*\d[\d,]*([^\d,원%만천]|$)",
            "${1}${2} <n>${3}",
        ),
        (r"\bD\s?[-+]\s?\d{1,3}", "D-<n>"),
        // Sub-day relative times are always render-time; day-or-longer ages
        // only when they label a post ("등록 3일 전"), never as terms
        // ("마감 7일 전 취소 가능").
        (r"\d+\s*(초|분|시간)\s*전", "<ago>"),
        (
            r"(?i)(등록|작성|게시|수정|업데이트|updated|posted|published)\s*[:：]?\s*\d+\s*(일|주|개월|달)\s*전",
            "$1 <ago>",
        ),
    ]
    .into_iter()
    .map(|(pattern, replacement)| {
        (
            Regex::new(pattern).expect("volatile token pattern is valid"),
            replacement,
        )
    })
    .collect()
});

pub(crate) fn normalize_volatile_tokens(value: &str) -> String {
    let mut output = value.to_string();
    for (pattern, replacement) in VOLATILE_TOKEN_PATTERNS.iter() {
        output = pattern.replace_all(&output, *replacement).into_owned();
    }
    output
}

/// Change-detection key for one fetched page. The review text itself is
/// stored verbatim for the reviewer; only the fingerprint is normalized.
fn content_fingerprint(title: Option<&str>, review_text: &str, link_keys: &[String]) -> String {
    let mut links: Vec<&str> = link_keys.iter().map(String::as_str).collect();
    links.sort_unstable();
    links.dedup();
    let mut input = normalize_volatile_tokens(title.unwrap_or(""));
    input.push('\n');
    input.push_str(&normalize_volatile_tokens(review_text));
    for link in links {
        input.push('\n');
        input.push_str(link);
    }
    fnv_hash_hex(input.as_bytes())
}

#[derive(Debug)]
struct DiscoveredLink {
    url: String,
    canonical_url: String,
    title: Option<String>,
    candidate_kind: &'static str,
}

/// Finds only benefit-like links within the already approved domain and path
/// scope. This is link discovery, not a new permission boundary.
fn discover_benefit_links(
    html: &str,
    page_url: &Url,
    candidate: &Candidate,
    limit: usize,
) -> Vec<DiscoveredLink> {
    let document = Html::parse_document(html);
    let Ok(anchor_selector) = Selector::parse("a[href]") else {
        return Vec::new();
    };
    let image_selector = Selector::parse("img[alt]").ok();
    let page_key = canonical_url_string(page_url);
    let mut seen = HashSet::new();
    let mut output = Vec::new();

    for anchor in document.select(&anchor_selector) {
        if output.len() >= limit {
            break;
        }
        let Some(href) = anchor.value().attr("href") else {
            continue;
        };
        let Ok(mut url) = page_url.join(href.trim()) else {
            continue;
        };
        if url.scheme() != "https"
            || url.username() != ""
            || url.password().is_some()
            || url.port_or_known_default() != Some(443)
        {
            continue;
        }
        let Some(host) = url
            .host_str()
            .map(|value| value.trim_end_matches('.').to_ascii_lowercase())
        else {
            continue;
        };
        let host_allowed = host_is_within(&host, &candidate.domain, candidate.allow_subdomains);
        if !host_allowed || !path_is_allowed(url.path(), &candidate.allowed_path_prefixes) {
            continue;
        }
        url.set_fragment(None);
        strip_tracking_query_parameters(&mut url);
        let canonical_url = canonical_url_string(&url);
        if canonical_url == page_key
            || canonical_url.chars().count() > 2_000
            || !seen.insert(canonical_url.clone())
        {
            continue;
        }

        let mut label_parts = Vec::new();
        if let Some(title) = anchor.value().attr("title") {
            label_parts.push(title.to_string());
        }
        label_parts.push(anchor.text().collect::<Vec<_>>().join(" "));
        if let Some(selector) = image_selector.as_ref() {
            label_parts.extend(
                anchor
                    .select(selector)
                    .filter_map(|image| image.value().attr("alt"))
                    .map(str::to_string),
            );
        }
        let label = clean_fragment(&label_parts.join(" "));
        if !looks_like_benefit_link(&label, &url) || looks_like_account_or_purchase_link(&url) {
            continue;
        }

        output.push(DiscoveredLink {
            url: url.to_string(),
            canonical_url,
            title: (!label.is_empty()).then(|| label.chars().take(500).collect::<String>()),
            candidate_kind: classify_discovered_link(&url),
        });
    }
    output
}

fn looks_like_benefit_link(label: &str, url: &Url) -> bool {
    contains_benefit_signal(&format!("{} {}", label, url.as_str()), &[])
}

fn looks_like_account_or_purchase_link(url: &Url) -> bool {
    is_interactive_or_purchase_url(url)
}

fn classify_discovered_link(url: &Url) -> &'static str {
    match classify_candidate_kind(url) {
        CandidateKind::Index => "index",
        CandidateKind::Detail | CandidateKind::Unknown => "detail",
    }
}

async fn queue_discovered_links(
    pool: &PgPool,
    parent: &Candidate,
    links: Vec<DiscoveredLink>,
) -> Result<usize> {
    if links.is_empty() {
        return Ok(0);
    }
    let mut tx = pool.begin().await?;
    let mut stored = 0usize;
    for link in links {
        let id = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO benefit_source_candidates (
                 id, domain_id, url, canonical_url, discovered_via,
                 discovery_query, page_title, status, candidate_kind,
                 discovery_depth, parent_candidate_id,
                 discovered_at, last_discovered_at, created_at, updated_at
             ) VALUES (
                 $1, $2, $3, $4, 'official_page', $5, $6, 'queued', $7,
                 $8, $9, now(), now(), now(), now()
             )
             ON CONFLICT (canonical_url) DO UPDATE SET
                 page_title = COALESCE(
                     benefit_source_candidates.page_title,
                     EXCLUDED.page_title
                 ),
                 parent_candidate_id = COALESCE(
                     benefit_source_candidates.parent_candidate_id,
                     EXCLUDED.parent_candidate_id
                 ),
                 last_discovered_at = now(), updated_at = now()
             RETURNING id",
        )
        .bind(Uuid::new_v4())
        .bind(parent.domain_id)
        .bind(&link.url)
        .bind(&link.canonical_url)
        .bind(parent.url.chars().take(500).collect::<String>())
        .bind(link.title)
        .bind(link.candidate_kind)
        .bind(parent.discovery_depth + 1)
        .bind(parent.id)
        .fetch_one(&mut *tx)
        .await?;

        sqlx::query(
            "INSERT INTO benefit_candidate_brands (candidate_id, brand_id)
             SELECT $1, source.brand_id
             FROM (
                 SELECT brand_id
                 FROM benefit_candidate_brands
                 WHERE candidate_id = $2
                 UNION
                 SELECT brand_id
                 FROM benefit_source_domains
                 WHERE id = $3 AND brand_id IS NOT NULL
             ) source
             ON CONFLICT (candidate_id, brand_id) DO NOTHING",
        )
        .bind(id)
        .bind(parent.id)
        .bind(parent.domain_id)
        .execute(&mut *tx)
        .await?;
        stored += 1;
    }
    tx.commit().await?;
    Ok(stored)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SlotOutcome {
    Reserved,
    /// The domain is cooling down for longer than a worker task may sleep.
    /// The candidate was rescheduled for the end of the cooldown instead.
    Deferred,
}

/// Shared slot reservation (`sources::source_access`) plus this collector's
/// bookkeeping: a deferred candidate is rescheduled for the end of the
/// domain cooldown instead of stalling the batch.
async fn reserve_request_slot(pool: &PgPool, candidate: &Candidate) -> Result<SlotOutcome> {
    match source_access::reserve_request_slot(
        pool,
        candidate.domain_id,
        candidate.min_request_interval_ms as i64,
        source_access::MAX_SLOT_WAIT,
    )
    .await?
    {
        source_access::SlotOutcome::Reserved => Ok(SlotOutcome::Reserved),
        source_access::SlotOutcome::Deferred(next_request_at) => {
            let reason = format!(
                "domain request slot is reserved until {}; fetch deferred",
                next_request_at.to_rfc3339()
            );
            sqlx::query(
                "UPDATE benefit_source_candidates
                 SET status = 'error', last_error = $2, next_check_at = $3,
                     last_checked_at = now(), updated_at = now()
                 WHERE id = $1",
            )
            .bind(candidate.id)
            .bind(truncate_error(&reason))
            .bind(next_request_at)
            .execute(pool)
            .await?;
            warn!(
                candidate_id = %candidate.id,
                next_request_at = %next_request_at,
                "domain cooldown exceeds the slot wait limit; candidate deferred"
            );
            Ok(SlotOutcome::Deferred)
        }
    }
}

#[derive(Debug, Default)]
struct RobotsGroup {
    agents: Vec<String>,
    rules: Vec<RobotsRule>,
    crawl_delay: Option<f64>,
    /// `Visit-time` windows (minutes after 00:00 UTC, start..end), the
    /// non-standard extension www.gsretail.com states (`0400-0845`).
    visit_times: Vec<(u16, u16)>,
    /// A `Visit-time` line of this group could not be read: the window is
    /// closed (fail closed), never "any time".
    visit_time_unreadable: bool,
    /// An `allow` or `disallow` line (an empty `Disallow:` included) was
    /// read: the next user-agent line starts a new group.
    seen_rule_line: bool,
}

#[derive(Debug)]
struct RobotsRule {
    allow: bool,
    pattern: String,
}

/// The groups of a robots.txt, as RFC 9309 §2.2 reads them:
/// - one leading byte-order mark is dropped, and CR, LF and CRLF all end a
///   line;
/// - consecutive user-agent lines share one group; a group ends only at the
///   first user-agent line after one of its rule lines (`allow`,
///   `disallow`, an empty `Disallow:` included). Blank lines, comments and
///   records outside the protocol (`Crawl-delay`, `Visit-time`, `Sitemap`)
///   never end a group (§2.2.4: they must not interfere with the defined
///   records).
fn parse_robots_groups(body: &str) -> Vec<RobotsGroup> {
    let body = body.strip_prefix('\u{feff}').unwrap_or(body);
    let mut groups = Vec::new();
    let mut current = RobotsGroup::default();
    for raw_line in body.split(['\n', '\r']) {
        let line = raw_line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let Some((field, value)) = line.split_once(':') else {
            continue;
        };
        let field = field.trim().to_ascii_lowercase();
        let value = value.trim();
        match field.as_str() {
            "user-agent" => {
                if current.seen_rule_line {
                    groups.push(std::mem::take(&mut current));
                }
                if !value.is_empty() {
                    current.agents.push(value.to_ascii_lowercase());
                }
            }
            "allow" | "disallow" if !current.agents.is_empty() => {
                current.seen_rule_line = true;
                if !value.is_empty() {
                    current.rules.push(RobotsRule {
                        allow: field == "allow",
                        pattern: value.to_string(),
                    });
                }
            }
            "crawl-delay" if !current.agents.is_empty() => {
                current.crawl_delay = value
                    .parse::<f64>()
                    .ok()
                    .filter(|delay| delay.is_finite() && *delay >= 0.0);
            }
            "visit-time" if !current.agents.is_empty() => match parse_visit_time(value) {
                Some(window) => current.visit_times.push(window),
                None => current.visit_time_unreadable = true,
            },
            _ => {}
        }
    }
    if !current.agents.is_empty() {
        groups.push(current);
    }
    groups
}

/// A file that names agents but neither `product_token` nor `*` states
/// who may crawl, and we are not among them: a refusal of every path,
/// whatever it allows the agents it names (the owner's rule; the structure
/// decides, not the spelling of one rule).
fn is_named_allowlist(groups: &[RobotsGroup], product_token: &str) -> bool {
    !groups.is_empty() && matching_robots_groups(groups, product_token).is_empty()
}

/// Whether a user-agent value names `product_token`: its leading product
/// token (`MazimiBenefitBot/1.0` names `MazimiBenefitBot`), compared
/// without case; a value ending in `*` names every token it is a prefix of
/// (`Mazimi*`). A bare `*` is the wildcard group, never a name.
fn agent_names(agent: &str, product_token: &str) -> bool {
    let token = product_token.to_ascii_lowercase();
    let agent = agent.trim().to_ascii_lowercase();
    if agent == "*" {
        return false;
    }
    if let Some(prefix) = agent.strip_suffix('*') {
        return !prefix.is_empty() && token.starts_with(prefix);
    }
    let lead: String = agent
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    lead == token
}

/// The groups that apply to `product_token`: its own groups when any exist,
/// otherwise the `*` groups (RFC 9309 §2.2.1).
fn matching_robots_groups<'a>(
    groups: &'a [RobotsGroup],
    product_token: &str,
) -> Vec<&'a RobotsGroup> {
    let exact: Vec<&RobotsGroup> = groups
        .iter()
        .filter(|group| {
            group
                .agents
                .iter()
                .any(|agent| agent_names(agent, product_token))
        })
        .collect();
    if exact.is_empty() {
        groups
            .iter()
            .filter(|group| group.agents.iter().any(|agent| agent == "*"))
            .collect()
    } else {
        exact
    }
}

/// Crawl-delay in seconds from the groups that apply to `product_token`.
/// The largest stated delay wins when several matching groups state one.
pub(crate) fn robots_crawl_delay(body: &str, product_token: &str) -> Option<f64> {
    let groups = parse_robots_groups(body);
    matching_robots_groups(&groups, product_token)
        .iter()
        .filter_map(|group| group.crawl_delay)
        .fold(None, |best: Option<f64>, delay| {
            Some(best.map_or(delay, |b| b.max(delay)))
        })
}

/// One `Visit-time` value, `HHMM-HHMM` or `HH:MM-HH:MM` in UTC, as minutes
/// after midnight. A malformed value states no window.
fn parse_visit_time(value: &str) -> Option<(u16, u16)> {
    let minutes = |part: &str| -> Option<u16> {
        let digits: String = part.trim().chars().filter(|c| *c != ':').collect();
        if digits.len() != 4 || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let (h, m) = (
            digits[..2].parse::<u16>().ok()?,
            digits[2..].parse::<u16>().ok()?,
        );
        (h <= 24 && m < 60 && h * 60 + m <= 1440).then_some(h * 60 + m)
    };
    let (start, end) = value.split_once('-')?;
    Some((minutes(start)?, minutes(end)?))
}

/// When robots `Visit-time` lets `product_token` request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum VisitTime {
    /// The file states no window for us: any time.
    Any,
    /// UTC minutes, start..end; a window may wrap past midnight.
    Windows(Vec<(u16, u16)>),
    /// A window line for us could not be read: closed (fail closed).
    Unreadable,
}

impl VisitTime {
    pub(crate) fn allows(&self, minute: u16) -> bool {
        match self {
            Self::Any => true,
            Self::Windows(windows) => inside_visit_window(windows, minute),
            Self::Unreadable => false,
        }
    }
}

/// The `Visit-time` of the groups that apply to `product_token`.
pub(crate) fn robots_visit_time(body: &str, product_token: &str) -> VisitTime {
    let groups = parse_robots_groups(body);
    let matching = matching_robots_groups(&groups, product_token);
    if matching.iter().any(|group| group.visit_time_unreadable) {
        return VisitTime::Unreadable;
    }
    let windows: Vec<(u16, u16)> = matching
        .iter()
        .flat_map(|group| group.visit_times.iter().copied())
        .collect();
    if windows.is_empty() {
        VisitTime::Any
    } else {
        VisitTime::Windows(windows)
    }
}

/// The `Visit-time` windows (UTC minutes) of the groups that apply to
/// `product_token`; empty when the file states none or none is readable.
#[cfg(test)]
pub(crate) fn robots_visit_windows(body: &str, product_token: &str) -> Vec<(u16, u16)> {
    match robots_visit_time(body, product_token) {
        VisitTime::Windows(windows) => windows,
        _ => Vec::new(),
    }
}

/// Whether `minute` (after 00:00 UTC) falls inside one of `windows`; true
/// when there are none.
pub(crate) fn inside_visit_window(windows: &[(u16, u16)], minute: u16) -> bool {
    windows.is_empty()
        || windows.iter().any(|(start, end)| {
            if start <= end {
                (*start..*end).contains(&minute)
            } else {
                minute >= *start || minute < *end
            }
        })
}

/// The one robots decision every fetch path makes: whether `product_token`
/// may fetch `target` (path plus query) under `body`. A named-agent
/// allowlist the token is not on refuses every path; otherwise the RFC 9309
/// rules of the groups that apply decide.
pub(crate) fn robots_allows(body: &str, product_token: &str, target: &str) -> bool {
    let groups = parse_robots_groups(body);
    !is_named_allowlist(&groups, product_token)
        && robots_rules_allow(&groups, product_token, target)
}

/// RFC 9309 §2.2.2: the most specific matching rule wins, `Allow` on a tie;
/// no matching rule allows.
fn robots_rules_allow(groups: &[RobotsGroup], product_token: &str, target: &str) -> bool {
    let matching = matching_robots_groups(groups, product_token);

    let mut best: Option<(usize, bool)> = None;
    for rule in matching.iter().flat_map(|group| &group.rules) {
        if !robots_pattern_matches(&rule.pattern, target) {
            continue;
        }
        let specificity = rule
            .pattern
            .trim_end_matches('$')
            .bytes()
            .filter(|byte| *byte != b'*')
            .count();
        match best {
            None => best = Some((specificity, rule.allow)),
            Some((best_specificity, best_allow))
                if specificity > best_specificity
                    || (specificity == best_specificity && rule.allow && !best_allow) =>
            {
                best = Some((specificity, rule.allow));
            }
            _ => {}
        }
    }
    best.map(|(_, allow)| allow).unwrap_or(true)
}

fn robots_pattern_matches(pattern: &str, target: &str) -> bool {
    let exact_end = pattern.ends_with('$');
    let mut pattern = pattern.trim_end_matches('$').as_bytes().to_vec();
    if !exact_end {
        pattern.push(b'*');
    }
    glob_matches(&pattern, target.as_bytes())
}

fn glob_matches(pattern: &[u8], value: &[u8]) -> bool {
    let (mut p, mut v) = (0usize, 0usize);
    let (mut star, mut retry_value) = (None, 0usize);
    while v < value.len() {
        if p < pattern.len() && pattern[p] == value[v] {
            p += 1;
            v += 1;
        } else if p < pattern.len() && pattern[p] == b'*' {
            star = Some(p);
            p += 1;
            retry_value = v;
        } else if let Some(star_index) = star {
            retry_value += 1;
            v = retry_value;
            p = star_index + 1;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }
    p == pattern.len()
}

async fn block_domain(
    pool: &PgPool,
    candidate: &Candidate,
    robots_status: &str,
    reason: &str,
    permanent: bool,
    retry: Option<DateTime<Utc>>,
) -> Result<()> {
    let reason = truncate_error(reason);
    if !permanent {
        if candidate.monitored {
            source_monitoring::record_fetch_failure(pool, candidate.id, &reason).await?;
        }
        let mut tx = pool.begin().await?;
        defer_domain(
            &mut tx,
            candidate.domain_id,
            candidate.id,
            &reason,
            retry.unwrap_or_else(|| Utc::now() + chrono::Duration::days(1)),
        )
        .await?;
        tx.commit().await?;
        return Ok(());
    }
    let mut tx = pool.begin().await?;
    sqlx::query("UPDATE benefit_source_domains SET crawl_allowed=false,compliance_status='rejected',robots_status=$2,robots_checked_at=now(),next_request_at=NULL,updated_at=now() WHERE id=$1")
        .bind(candidate.domain_id).bind(robots_status).execute(&mut *tx).await?;
    if !candidate.monitored {
        sqlx::query("UPDATE benefit_source_candidates SET status='rejected',last_error=$2,next_check_at=NULL,last_checked_at=now(),updated_at=now() WHERE id=$1")
            .bind(candidate.id).bind(&reason).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    if candidate.monitored {
        source_monitoring::hold_source_unavailable(
            pool,
            candidate.id,
            UnavailableKind::Blocked,
            None,
            &reason,
        )
        .await?;
    }
    Ok(())
}

async fn defer_domain(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    domain_id: Uuid,
    candidate_id: Uuid,
    reason: &str,
    retry: DateTime<Utc>,
) -> Result<()> {
    // Preserve the existing time-bounded permission and its last successful
    // robots evidence. Every subsequent page still repeats the robots preflight.
    sqlx::query("UPDATE benefit_source_domains SET next_request_at=GREATEST(next_request_at,$2),updated_at=now() WHERE id=$1")
        .bind(domain_id).bind(retry).execute(&mut **tx).await?;
    sqlx::query("UPDATE benefit_source_candidates SET status='error',last_error=$2,next_check_at=$3,last_checked_at=now(),updated_at=now() WHERE id=$1")
        .bind(candidate_id).bind(reason).bind(retry).execute(&mut **tx).await?;
    Ok(())
}

async fn mark_candidate_error(pool: &PgPool, id: Uuid, reason: &str) {
    let _ = sqlx::query(
        "UPDATE benefit_source_candidates
         SET status = 'error', last_error = $2,
             next_check_at = now() + interval '24 hours',
             last_checked_at = now(), updated_at = now()
         WHERE id = $1",
    )
    .bind(id)
    .bind(truncate_error(reason))
    .execute(pool)
    .await;
}

fn truncate_error(value: &str) -> String {
    crate::error_safety::persisted_error(value)
}

/// Measurement only (ignored): each saved robots.txt in `ROBOTS_DIR`
/// (`<host>.txt`, `_codes` lines `host status`) read three ways.
#[cfg(test)]
mod robots_census_probe {
    #[test]
    #[ignore = "measurement on saved robots files"]
    fn robots_files_read_by_the_shared_decision() {
        let dir = std::env::var("ROBOTS_DIR").expect("ROBOTS_DIR");
        let codes = std::fs::read_to_string(format!("{dir}/_codes")).unwrap();
        for line in codes.lines() {
            let mut parts = line.split_whitespace();
            let (Some(host), Some(code)) = (parts.next(), parts.next()) else {
                continue;
            };
            let body = std::fs::read_to_string(format!("{dir}/{host}.txt")).unwrap_or_default();
            if code != "200" || super::robots_response_looks_like_html(body.as_bytes()) {
                println!(
                    "ROBOTS | {host} | status={code} html={}",
                    super::robots_response_looks_like_html(body.as_bytes())
                );
                continue;
            }
            let groups = super::parse_robots_groups(&body);
            for token in [
                super::BOT_PRODUCT_TOKEN,
                crate::link_resolver::check::ROBOTS_TOKEN,
            ] {
                println!(
                    "ROBOTS | {host} | token={token} groups={} star={} rfc_root={} named_allowlist={} allows_root={}",
                    groups.len(),
                    groups.iter().any(|g| g.agents.iter().any(|a| a == "*")),
                    super::robots_rules_allow(&groups, token, "/"),
                    super::is_named_allowlist(&groups, token),
                    super::robots_allows(&body, token, "/"),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_after_honors_seconds_and_http_dates() {
        let now: DateTime<Utc> = "2026-10-02T00:00:00Z".parse().unwrap();
        let mut headers = reqwest::header::HeaderMap::new();
        assert_eq!(retry_at(&headers, now), now + chrono::Duration::days(1));
        headers.insert(RETRY_AFTER, "172800".parse().unwrap());
        assert_eq!(retry_at(&headers, now), now + chrono::Duration::days(2));
        headers.insert(
            RETRY_AFTER,
            "Sat, 03 Oct 2026 00:00:00 GMT".parse().unwrap(),
        );
        assert_eq!(retry_at(&headers, now), now + chrono::Duration::days(1));
        headers.insert(RETRY_AFTER, "0".parse().unwrap());
        assert_eq!(retry_at(&headers, now), now + chrono::Duration::minutes(1));
    }

    #[tokio::test]
    async fn transient_access_preserves_approval_but_robots_disallow_revokes_it() {
        let database_url = std::env::var("TEST_DATABASE_URL")
            .unwrap_or_else(|_| "postgres://wello:wello@localhost:5432/wello".into());
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .expect("isolated test database required");
        let mut tx = pool.begin().await.unwrap();
        let domain_id = Uuid::new_v4();
        let candidate_id = Uuid::new_v4();
        let domain = format!("{}.example", domain_id.simple());
        let url = format!("https://{domain}/benefits");
        sqlx::query("INSERT INTO benefit_source_domains(id,organization_name,domain,source_role,crawl_allowed,verification_status,verification_reference,
          compliance_status,approval_basis,approval_reference,terms_checked_at,terms_review_outcome,robots_status,robots_checked_at,robots_content_hash,
          allowed_path_prefixes,robots_reviewed_path_prefixes,crawler_contact_url,compliance_reviewed_by,compliance_reviewed_at,approval_expires_at,collection_mode)
          VALUES($1,'isolated fixture',$2,'brand_official',true,'verified','fixture','approved','written_permission','fixture',now(),'explicit_permission','allowed',now(),repeat('a',32),
          ARRAY['/benefits'],ARRAY['/benefits'],'https://mazimi.example/bot','fixture',now(),now()+interval '30 days','direct_public')")
            .bind(domain_id).bind(&domain).execute(&mut *tx).await.unwrap();
        sqlx::query("INSERT INTO benefit_source_candidates(id,domain_id,url,canonical_url,discovered_via,status) VALUES($1,$2,$3,$3,'manual','fetched')")
            .bind(candidate_id).bind(domain_id).bind(&url).execute(&mut *tx).await.unwrap();
        let retry = Utc::now() + chrono::Duration::days(2);
        defer_domain(
            &mut tx,
            domain_id,
            candidate_id,
            "temporary HTTP 429",
            retry,
        )
        .await
        .unwrap();
        let (allowed,compliance,robots,cooldown):(bool,String,String,DateTime<Utc>)=sqlx::query_as("SELECT crawl_allowed,compliance_status,robots_status,next_request_at FROM benefit_source_domains WHERE id=$1")
            .bind(domain_id).fetch_one(&mut *tx).await.unwrap();
        assert!(allowed);
        assert_eq!(compliance, "approved");
        assert_eq!(robots, "allowed");
        assert_eq!(cooldown.timestamp(), retry.timestamp());
        let (status, next): (String, DateTime<Utc>) = sqlx::query_as(
            "SELECT status,next_check_at FROM benefit_source_candidates WHERE id=$1",
        )
        .bind(candidate_id)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        assert_eq!(status, "error");
        assert_eq!(next.timestamp(), retry.timestamp());
        update_robots_evidence(
            &mut *tx,
            domain_id,
            &format!("https://{domain}/robots.txt"),
            false,
            &"b".repeat(32),
        )
        .await
        .unwrap();
        let (allowed,compliance,robots):(bool,String,String)=sqlx::query_as("SELECT crawl_allowed,compliance_status,robots_status FROM benefit_source_domains WHERE id=$1")
            .bind(domain_id).fetch_one(&mut *tx).await.unwrap();
        assert!(!allowed);
        assert_eq!(compliance, "rejected");
        assert_eq!(robots, "blocked");
        // A later allowed robots file cannot manufacture a new legal approval.
        update_robots_evidence(
            &mut *tx,
            domain_id,
            &format!("https://{domain}/robots.txt"),
            true,
            &"c".repeat(32),
        )
        .await
        .unwrap();
        let allowed: bool =
            sqlx::query_scalar("SELECT crawl_allowed FROM benefit_source_domains WHERE id=$1")
                .bind(domain_id)
                .fetch_one(&mut *tx)
                .await
                .unwrap();
        assert!(!allowed);
        tx.rollback().await.unwrap();
        pool.close().await;
    }

    #[tokio::test]
    async fn long_domain_cooldown_defers_candidate_instead_of_sleeping() {
        let database_url = std::env::var("TEST_DATABASE_URL")
            .unwrap_or_else(|_| "postgres://wello:wello@localhost:5432/wello".into());
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .expect("isolated test database required");
        let domain_id = Uuid::new_v4();
        let candidate_id = Uuid::new_v4();
        let domain = format!("{}.example", domain_id.simple());
        let url = format!("https://{domain}/benefits");
        let cooldown_until = Utc::now() + chrono::Duration::days(1);
        sqlx::query("INSERT INTO benefit_source_domains(id,organization_name,domain,source_role,crawl_allowed,verification_status,verification_reference,
          compliance_status,approval_basis,approval_reference,terms_checked_at,terms_review_outcome,robots_status,robots_checked_at,robots_content_hash,
          allowed_path_prefixes,robots_reviewed_path_prefixes,crawler_contact_url,compliance_reviewed_by,compliance_reviewed_at,approval_expires_at,collection_mode,next_request_at)
          VALUES($1,'slot fixture',$2,'brand_official',true,'verified','fixture','approved','written_permission','fixture',now(),'explicit_permission','allowed',now(),repeat('a',32),
          ARRAY['/benefits'],ARRAY['/benefits'],'https://mazimi.example/bot','fixture',now(),now()+interval '30 days','direct_public',$3)")
            .bind(domain_id).bind(&domain).bind(cooldown_until).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO benefit_source_candidates(id,domain_id,url,canonical_url,discovered_via,status) VALUES($1,$2,$3,$3,'manual','fetched')")
            .bind(candidate_id).bind(domain_id).bind(&url).execute(&pool).await.unwrap();
        let candidate = Candidate {
            id: candidate_id,
            domain_id,
            url: url.clone(),
            domain: domain.clone(),
            allow_subdomains: false,
            allowed_path_prefixes: vec!["/benefits".to_string()],
            min_request_interval_ms: 10_000,
            crawler_contact_url: "https://mazimi.example/bot".to_string(),
            etag: None,
            last_modified: None,
            candidate_kind: "detail".to_string(),
            discovery_depth: 0,
            content_hash: None,
            monitored: false,
            recheck_interval_seconds: 2_592_000,
        };

        let started = std::time::Instant::now();
        let outcome = reserve_request_slot(&pool, &candidate).await.unwrap();
        assert_eq!(outcome, SlotOutcome::Deferred);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "must not sleep out the cooldown"
        );
        let (status, next_check_at, last_error): (String, DateTime<Utc>, String) = sqlx::query_as(
            "SELECT status,next_check_at,last_error FROM benefit_source_candidates WHERE id=$1",
        )
        .bind(candidate_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(status, "error");
        assert_eq!(next_check_at.timestamp(), cooldown_until.timestamp());
        assert!(last_error.contains("deferred"));

        // A free slot is still reserved atomically.
        sqlx::query("UPDATE benefit_source_domains SET next_request_at=NULL WHERE id=$1")
            .bind(domain_id)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            reserve_request_slot(&pool, &candidate).await.unwrap(),
            SlotOutcome::Reserved
        );
        let reserved_until: Option<DateTime<Utc>> =
            sqlx::query_scalar("SELECT next_request_at FROM benefit_source_domains WHERE id=$1")
                .bind(domain_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(reserved_until.is_some_and(|until| until > Utc::now()));

        sqlx::query("DELETE FROM benefit_source_candidates WHERE id=$1")
            .bind(candidate_id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM benefit_source_domains WHERE id=$1")
            .bind(domain_id)
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
    }

    #[test]
    fn fingerprint_ignores_volatile_tokens_but_keeps_material_terms() {
        let title = Some("청년 할인");
        let first = content_fingerprint(
            title,
            "조회 123 등록일 2026.10.01 14:33 청년 20% 할인 2026년 8월 31일까지 D-3 3시간 전",
            &[],
        );
        let rerendered = content_fingerprint(
            title,
            "조회 1,456 등록일 2026.10.02 09:10 청년 20% 할인 2026년 8월 31일까지 D-2 5분 전",
            &[],
        );
        assert_eq!(first, rerendered, "timestamps and counters must not flap");

        let price_changed = content_fingerprint(
            title,
            "조회 123 등록일 2026.10.01 14:33 청년 10% 할인 2026년 8월 31일까지 D-3 3시간 전",
            &[],
        );
        assert_ne!(first, price_changed, "a changed percentage is material");
        let deadline_changed = content_fingerprint(
            title,
            "조회 123 등록일 2026.10.01 14:33 청년 20% 할인 2026년 9월 30일까지 D-3 3시간 전",
            &[],
        );
        assert_ne!(first, deadline_changed, "a changed deadline is material");
        let title_changed = content_fingerprint(
            Some("[종료] 청년 할인"),
            "조회 123 등록일 2026.10.01 14:33 청년 20% 할인 2026년 8월 31일까지 D-3 3시간 전",
            &[],
        );
        assert_ne!(
            first, title_changed,
            "a title-only closure notice is material"
        );
    }

    /// Korean notices attach a time to the deadline. Only a *labelled*
    /// timestamp is a render artefact; a deadline with a time is the offer.
    #[test]
    fn fingerprint_keeps_unlabelled_date_times_and_drops_labelled_ones() {
        let fp = |text: &str| content_fingerprint(Some("청년 할인"), text, &[]);

        assert_ne!(
            fp("접수 마감: 2026. 8. 31.(월) 18:00까지"),
            fp("접수 마감: 2026. 9. 30.(수) 18:00까지"),
            "a changed date+time deadline is material"
        );
        assert_ne!(
            fp("신청기간 2026년 8월 31일 23:59까지"),
            fp("신청기간 2026년 12월 31일 23:59까지")
        );
        assert_eq!(
            fp("작성일: 2026.10.01 14:33 청년 20% 할인"),
            fp("작성일: 2026.10.02 09:10 청년 20% 할인"),
            "a labelled timestamp, time part included, must not flap"
        );
        assert_eq!(
            fp("최종 수정 2026-10-01 (수) 14:33:05 조건 동일"),
            fp("최종 수정 2026-10-02 (목) 09:10:44 조건 동일")
        );
        assert_eq!(fp("등록 14:33 이벤트"), fp("등록 09:10 이벤트"));

        // Opening hours and cancellation terms are the offer, not a render.
        assert_ne!(fp("운영시간 09:00~18:00"), fp("운영시간 10:00~17:00"));
        assert_ne!(fp("마감 7일 전 취소 가능"), fp("마감 1일 전 취소 가능"));
        assert_eq!(fp("등록 3일 전 이벤트"), fp("등록 5일 전 이벤트"));
        assert_eq!(fp("3시간 전 이벤트"), fp("5분 전 이벤트"));

        // A board counter is volatile; a referral amount is not.
        assert_eq!(fp("추천 12 청년 할인"), fp("추천 40 청년 할인"));
        assert_eq!(fp("조회수: 1,456 청년 할인"), fp("조회수: 2,001 청년 할인"));
        assert_ne!(
            fp("친구추천 10,000원 적립"),
            fp("친구추천 5,000원 적립"),
            "an amount after 추천 is material"
        );
        assert_ne!(fp("추천 시 5,000원"), fp("추천 시 3,000원"));
    }

    #[test]
    fn fingerprint_covers_the_link_set_regardless_of_order() {
        let ordered = content_fingerprint(
            None,
            "이벤트 목록",
            &[
                "https://brand.example/events/1".to_string(),
                "https://brand.example/events/2".to_string(),
            ],
        );
        let shuffled = content_fingerprint(
            None,
            "이벤트 목록",
            &[
                "https://brand.example/events/2".to_string(),
                "https://brand.example/events/1".to_string(),
            ],
        );
        assert_eq!(ordered, shuffled);
        let one_removed = content_fingerprint(
            None,
            "이벤트 목록",
            &["https://brand.example/events/1".to_string()],
        );
        assert_ne!(ordered, one_removed);
    }

    #[test]
    fn review_text_combines_every_main_and_article_block_once() {
        let html = "<html><head><title>혜택</title></head><body><nav>메뉴</nav>\
            <main><article><p>학생 10% 할인</p></article><article><p>청년 멤버십 무료</p></article></main>\
            <footer>푸터</footer></body></html>";
        let (_, text) = extract_review_text(html);
        assert_eq!(text, "학생 10% 할인 청년 멤버십 무료");

        let (_, no_landmark) =
            extract_review_text("<body><div>본문 할인</div><script>x()</script></body>");
        assert_eq!(no_landmark, "본문 할인");
    }

    #[test]
    fn discovered_links_use_the_shared_canonical_key() {
        let candidate = Candidate {
            id: Uuid::nil(),
            domain_id: Uuid::nil(),
            url: "https://brand.example/events".to_string(),
            domain: "brand.example".to_string(),
            allow_subdomains: false,
            allowed_path_prefixes: vec!["/events".to_string()],
            min_request_interval_ms: 10_000,
            crawler_contact_url: "https://mazimi.example/crawler".to_string(),
            etag: None,
            last_modified: None,
            candidate_kind: "index".to_string(),
            discovery_depth: 0,
            content_hash: None,
            monitored: false,
            recheck_interval_seconds: 2_592_000,
        };
        let html = r#"<main>
            <a href="/events/detail/?b=2&a=1">청년 할인</a>
            <a href="/events/detail?a=1&b=2">청년 할인 (중복)</a>
            <a href="/events/">이벤트 목록 (자기 자신)</a>
        </main>"#;
        let links =
            discover_benefit_links(html, &Url::parse(&candidate.url).unwrap(), &candidate, 50);
        assert_eq!(links.len(), 1, "{links:?}");
        assert_eq!(
            links[0].canonical_url,
            "https://brand.example/events/detail?a=1&b=2"
        );
    }

    #[test]
    fn korean_encoding_and_linked_conditions_are_preserved() {
        let html = "<main><p>대학생 <a href='/conditions'>휴학생 제외</a></p></main>";
        let (bytes, _, _) = encoding_rs::EUC_KR.encode(html);
        let decoded = decode_page(&bytes, "text/html; charset=euc-kr").unwrap();
        assert!(extract_review_text(&decoded).1.contains("휴학생 제외"));
        assert!(decode_page(&[0xff, 0xfe, 0x80], "text/html").is_err());
    }

    #[test]
    fn approved_path_scope_does_not_widen_by_prefix() {
        let paths = vec!["/events".to_string()];
        assert!(path_is_allowed("/events", &paths));
        assert!(path_is_allowed("/events/youth", &paths));
        assert!(!path_is_allowed("/eventstore/admin", &paths));
    }

    /// Security review H1, H2, H3, M5, M6 and L5, and RFC 9309 §2.2:
    /// (robots.txt, path, allowed for MazimiBenefitBot).
    #[test]
    fn robots_groups_follow_rfc_9309() {
        let cases: &[(&str, &str, bool)] = &[
            // H1: records outside the protocol never end an agent list.
            (
                "User-agent: *\nCrawl-delay: 30\nUser-agent: Yeti\nDisallow: /\n",
                "/x",
                false,
            ),
            (
                "User-agent: *\nVisit-time: 0100-0200\nUser-agent: Yeti\nDisallow: /\n",
                "/x",
                false,
            ),
            (
                "User-agent: *\nCrawl-delay: 10\n\nUser-agent: Yeti\nDisallow: /\n",
                "/x",
                false,
            ),
            // Several user-agent lines, comments and blank lines in one group.
            (
                "User-agent: Yeti # naver\n\nUser-agent: *\n# none\nDisallow: /private\n",
                "/private/a",
                false,
            ),
            (
                "User-agent: Yeti\nUser-agent: *\nDisallow: /private\n",
                "/public",
                true,
            ),
            // A group ends at the first user-agent after a rule line,
            // an empty Disallow included.
            (
                "User-agent: *\nDisallow:\nUser-agent: Yeti\nDisallow: /\n",
                "/x",
                true,
            ),
            (
                "User-agent: *\nDisallow: /a\nUser-agent: Yeti\nDisallow: /\n",
                "/b",
                true,
            ),
            // `*` after named groups still applies to us.
            (
                "User-agent: Googlebot\nAllow: /\n\nUser-agent: *\nDisallow: /\n",
                "/x",
                false,
            ),
            (
                "User-agent: Googlebot\nDisallow: /\n\nUser-agent: *\nAllow: /\n",
                "/x",
                true,
            ),
            // H2: a byte-order mark never hides the first group.
            ("\u{feff}User-agent: *\nDisallow: /\n", "/x", false),
            // L5: CR and CRLF line ends.
            ("User-agent: *\rDisallow: /\r", "/x", false),
            ("User-agent: *\r\nDisallow: /secret\r\n", "/secret/1", false),
            // H3: a file that names only other bots is a refusal, whatever
            // it allows them.
            ("User-agent: Yeti\nDisallow:\n", "/x", false),
            ("User-agent: Yeti\nAllow: /*\n", "/x", false),
            ("User-agent: Googlebot\nDisallow: /private\n", "/x", false),
            ("User-agent: Yeti\nAllow: /\n", "/x", false),
            // M5: our token written with a version or a wildcard names us.
            (
                "User-agent: MazimiBenefitBot/1.0\nDisallow: /\n\nUser-agent: *\nAllow: /\n",
                "/x",
                false,
            ),
            (
                "User-agent: Mazimi*\nDisallow: /\n\nUser-agent: *\nAllow: /\n",
                "/x",
                false,
            ),
            ("User-agent: mazimibenefitbot\nAllow: /\n", "/x", true),
            (
                "User-agent: MazimiBenefitBotX\nAllow: /\n\nUser-agent: *\nDisallow: /\n",
                "/x",
                false,
            ),
            // No file content at all allows (RFC: no rule matches).
            ("", "/x", true),
        ];
        for (body, path, allowed) in cases {
            assert_eq!(
                robots_allows(body, BOT_PRODUCT_TOKEN, path),
                *allowed,
                "{body:?} {path}"
            );
        }
        // M6: an unreadable Visit-time for us closes the window.
        let unreadable = "User-agent: *\nDisallow: /api/\nVisit-time: 04:00 - 08:45 UTC\n";
        assert_eq!(
            robots_visit_time(unreadable, BOT_PRODUCT_TOKEN),
            VisitTime::Unreadable
        );
        assert!(!robots_visit_time(unreadable, BOT_PRODUCT_TOKEN).allows(300));
        let other =
            "User-agent: Yeti\nDisallow: /x\nVisit-time: bogus\n\nUser-agent: *\nDisallow: /api/\n";
        assert_eq!(robots_visit_time(other, BOT_PRODUCT_TOKEN), VisitTime::Any);
    }

    /// Every saved robots.txt of the 2026-11-10 census, read by today's
    /// decision and by the one production ran before this change
    /// (`robots_legacy`): the hosts whose verdict on `/` changes are
    /// listed here, so a change is a decision, never a surprise.
    #[test]
    fn robots_verdict_changes_on_the_saved_corpus_are_the_listed_ones() {
        let dir = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/robots_corpus_20261110"
        );
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        entries.sort_by_key(|e| e.file_name());
        let mut changed = Vec::new();
        let mut read = 0;
        for entry in entries {
            let body = std::fs::read_to_string(entry.path()).unwrap_or_default();
            if robots_response_looks_like_html(body.as_bytes()) {
                continue;
            }
            read += 1;
            for path in ["/", "/event/list"] {
                let old = crate::sources::robots_legacy::legacy_robots_allows(
                    &body,
                    BOT_PRODUCT_TOKEN,
                    path,
                );
                let new = robots_allows(&body, BOT_PRODUCT_TOKEN, path);
                if old != new {
                    changed.push(format!(
                        "{} {path}: {} -> {}",
                        entry.file_name().to_string_lossy(),
                        if old { "allowed" } else { "refused" },
                        if new { "allowed" } else { "refused" }
                    ));
                }
            }
        }
        assert!(read > 100, "{read}");
        // Every change is toward refusal:
        // - chatgpt.com: the rule for `/event/list` changed because
        //   group boundaries now follow RFC 9309 (records outside the protocol
        //   and blank lines no longer split or join groups);
        // - txbus.t-money.co.kr: `Disallow: /` behind bare CR line ends;
        // - www.data.go.kr, www.hanacard.co.kr: they name only other bots.
        let expected: &[&str] = &[
            "chatgpt.com.txt /event/list: allowed -> refused",
            "txbus.t-money.co.kr.txt /: allowed -> refused",
            "txbus.t-money.co.kr.txt /event/list: allowed -> refused",
            "www.data.go.kr.txt /: allowed -> refused",
            "www.data.go.kr.txt /event/list: allowed -> refused",
            "www.hanacard.co.kr.txt /: allowed -> refused",
            "www.hanacard.co.kr.txt /event/list: allowed -> refused",
        ];
        assert_eq!(changed, expected, "verdict changes on the saved corpus");
    }

    #[test]
    fn visit_time_is_read_for_our_group_and_wraps_midnight() {
        // www.gsretail.com/robots.txt as read 2026-11-10 (doors-B §9).
        let gsretail = "User-agent: *\nDisallow: /api/\nCrawl-delay: 10\nVisit-time: 0400-0845\n";
        let windows = robots_visit_windows(gsretail, BOT_PRODUCT_TOKEN);
        assert_eq!(windows, vec![(240, 525)]);
        assert!(inside_visit_window(&windows, 4 * 60));
        assert!(inside_visit_window(&windows, 8 * 60 + 44));
        assert!(!inside_visit_window(&windows, 8 * 60 + 45));
        assert!(!inside_visit_window(&windows, 14 * 60 + 14));
        assert!(
            inside_visit_window(&[], 14 * 60),
            "no window means any time"
        );
        assert!(inside_visit_window(&[(22 * 60, 2 * 60)], 23 * 60));
        assert!(inside_visit_window(&[(22 * 60, 2 * 60)], 60));
        assert!(!inside_visit_window(&[(22 * 60, 2 * 60)], 12 * 60));
        assert_eq!(parse_visit_time("04:00-08:45"), Some((240, 525)));
        for bad in ["4-8", "0400", "abcd-efgh", "2500-2600"] {
            assert_eq!(parse_visit_time(bad), None, "{bad}");
        }
        // Another agent's window is not ours.
        let other = "User-agent: Yeti\nDisallow: /x\nVisit-time: 0100-0200\n\nUser-agent: *\nDisallow: /api/\n";
        assert!(robots_visit_windows(other, BOT_PRODUCT_TOKEN).is_empty());
    }

    #[test]
    fn robots_prefers_longer_allow_rule() {
        let robots = r#"
            User-agent: *
            Disallow: /events/
            Allow: /events/youth/
        "#;
        assert!(robots_allows(
            robots,
            BOT_PRODUCT_TOKEN,
            "/events/youth/2026"
        ));
        assert!(!robots_allows(robots, BOT_PRODUCT_TOKEN, "/events/private"));
    }

    #[test]
    fn robots_specific_bot_group_overrides_wildcard_group() {
        let robots = r#"
            User-agent: *
            Allow: /

            User-agent: MazimiBenefitBot
            Disallow: /
        "#;
        assert!(!robots_allows(robots, BOT_PRODUCT_TOKEN, "/events"));
    }

    /// The one decision every fetch path makes: a file that lists the
    /// crawlers that may come, without the asking token, refuses every
    /// path for each of our tokens, and so does one that only restricts
    /// others; a file that names the token is read by its rules.
    #[test]
    fn a_named_agent_allowlist_refuses_every_path_for_every_token() {
        let allowlist = "User-agent: Yeti\nAllow: /\n\nUser-agent: Googlebot\nAllow: /\n";
        for token in [BOT_PRODUCT_TOKEN, crate::link_resolver::check::ROBOTS_TOKEN] {
            for path in ["/", "/benefit", "/a?b=1"] {
                assert!(!robots_allows(allowlist, token, path), "{token} {path}");
            }
            let restricts = "User-agent: Googlebot-Image\nDisallow: /images\n";
            assert!(!robots_allows(restricts, token, "/benefit"), "{token}");
            let names_us = format!("User-agent: {token}\nAllow: /\n\nUser-agent: Yeti\nAllow: /\n");
            assert!(robots_allows(&names_us, token, "/benefit"), "{token}");
            assert!(robots_allows("# nothing\n", token, "/benefit"), "{token}");
        }
    }

    /// One response, before the policy: only an RFC 9309 caller follows a
    /// redirect (public https only); an HTML answer and 4xx are facts the
    /// shared core policy then reads.
    #[test]
    fn robots_steps_by_policy() {
        use RobotsPolicy::{Rfc9309, Strict};
        let next = Url::parse("https://www.bokjiro.go.kr/robots.txt").unwrap();
        assert_eq!(
            robots_step(Rfc9309, StatusCode::FOUND, "", Some(next.clone())),
            RobotsStep::Redirect(next.clone())
        );
        assert_eq!(
            robots_step(Strict, StatusCode::FOUND, "", Some(next)),
            RobotsStep::Answer(RobotsAnswer::Unavailable("robots_redirect".into()))
        );
        for target in [
            "http://www.bokjiro.go.kr/robots.txt",
            "https://127.0.0.1/robots.txt",
        ] {
            assert_eq!(
                robots_step(
                    Rfc9309,
                    StatusCode::FOUND,
                    "",
                    Some(Url::parse(target).unwrap())
                ),
                RobotsStep::Answer(RobotsAnswer::Unavailable("robots_redirect".into())),
                "{target}"
            );
        }
        for policy in [Strict, Rfc9309] {
            assert_eq!(
                robots_step(policy, StatusCode::FORBIDDEN, "text/html", None),
                RobotsStep::Answer(RobotsAnswer::ClientError(403))
            );
            assert_eq!(
                robots_step(policy, StatusCode::OK, "text/html; charset=utf-8", None),
                RobotsStep::Answer(RobotsAnswer::Html)
            );
            assert_eq!(
                robots_step(policy, StatusCode::OK, "text/plain", None),
                RobotsStep::File
            );
            assert_eq!(
                robots_step(policy, StatusCode::TOO_MANY_REQUESTS, "", None),
                RobotsStep::Answer(RobotsAnswer::Unavailable("robots_http_429".into()))
            );
            assert_eq!(
                robots_step(policy, StatusCode::SERVICE_UNAVAILABLE, "", None),
                RobotsStep::Answer(RobotsAnswer::Unavailable("robots_http_503".into()))
            );
        }
        assert_eq!(MAX_ROBOTS_REDIRECTS, 5);
    }

    /// Which caller reads robots.txt how. The forms collector and the
    /// apply-link reader follow RFC 9309 on government hosts only, and are
    /// strict everywhere else; the ingestion crawler, the domain-approval
    /// preflight (and its daily renewal) and the assistant's live pages are
    /// strict on every host. The corporate crawler's own reading is pinned by
    /// `corporate_auto::census_tests` (HTML robots and 403 are a bot wall).
    #[test]
    fn each_caller_states_its_robots_policy() {
        assert_eq!(CRAWLER_ROBOTS, RobotsPolicy::Strict);
        assert_eq!(
            crate::sources::live_page::LIVE_PAGE_ROBOTS,
            RobotsPolicy::Strict
        );
        let collector = |u: &str| {
            crate::application_resources::fetch::robots_policy_for(&Url::parse(u).unwrap())
        };
        let apply_links = |u: &str| {
            crate::application_resources::apply_entry::fetch::robots_policy_for(
                &Url::parse(u).unwrap(),
            )
        };
        for caller in [collector, apply_links] {
            assert_eq!(
                caller("https://www.bokjiro.go.kr/ssis-tbu/a.hwp"),
                RobotsPolicy::Rfc9309
            );
            assert_eq!(caller("https://www.gijang.go.kr/a"), RobotsPolicy::Rfc9309);
            assert_eq!(caller("https://www.kosaf.or.kr/a"), RobotsPolicy::Strict);
            assert_eq!(caller("https://jobs.example.com/a"), RobotsPolicy::Strict);
            assert_eq!(
                caller("https://www.gijang.go.kr.evil.example/a"),
                RobotsPolicy::Strict
            );
        }
    }

    /// The fetch itself, against a local server: a 403, an HTML page and a
    /// same-host redirect to an HTML error page, read under each policy.
    #[tokio::test]
    async fn robots_fetch_by_policy_against_a_local_server() {
        use axum::{http::header, response::IntoResponse, routing::get, Router};
        async fn serve(router: Router) -> Url {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
            Url::parse(&format!("http://{addr}/page")).unwrap()
        }
        let client = Client::builder().redirect(Policy::none()).build().unwrap();
        let fetch = |url: Url, policy| {
            let client = client.clone();
            async move {
                fetch_robots_document(&client, &url, "test", Duration::from_secs(5), policy).await
            }
        };
        let forbidden = serve(Router::new().route(
            "/robots.txt",
            get(|| async { axum::http::StatusCode::FORBIDDEN }),
        ))
        .await;
        let err = fetch(forbidden.clone(), RobotsPolicy::Strict)
            .await
            .unwrap_err();
        assert!(
            err.downcast_ref::<RobotsAccessError>()
                .is_some_and(|e| e.permanent),
            "strict: 403 is a permanent refusal"
        );
        assert!(fetch(forbidden, RobotsPolicy::Rfc9309)
            .await
            .unwrap()
            .bytes
            .is_empty());
        let html = serve(Router::new().route(
            "/robots.txt",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/plain")],
                    "<!doctype html><html><title>Just a moment</title>",
                )
                    .into_response()
            }),
        ))
        .await;
        assert!(
            fetch(html.clone(), RobotsPolicy::Strict).await.is_err(),
            "strict: a challenge page is a refusal"
        );
        assert!(fetch(html, RobotsPolicy::Rfc9309)
            .await
            .unwrap()
            .bytes
            .is_empty());
        let rules = serve(Router::new().route(
            "/robots.txt",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/plain")],
                    "User-agent: *\nDisallow: /x\n",
                )
            }),
        ))
        .await;
        for policy in [RobotsPolicy::Strict, RobotsPolicy::Rfc9309] {
            assert_eq!(
                fetch(rules.clone(), policy).await.unwrap().bytes,
                b"User-agent: *\nDisallow: /x\n"
            );
        }
        // A plain-http redirect target is never followed (https and public only).
        let redirect = serve(Router::new().route(
            "/robots.txt",
            get(|| async {
                (
                    axum::http::StatusCode::FOUND,
                    [(header::LOCATION, "/error.html")],
                )
                    .into_response()
            }),
        ))
        .await;
        assert!(fetch(redirect.clone(), RobotsPolicy::Strict).await.is_err());
        assert!(fetch(redirect, RobotsPolicy::Rfc9309).await.is_err());
        // A server error and a robots.txt that never answers stay fail-closed
        // under both policies, and are retried later (not permanent).
        let unavailable = serve(Router::new().route(
            "/robots.txt",
            get(|| async { axum::http::StatusCode::SERVICE_UNAVAILABLE }),
        ))
        .await;
        let silent = {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tokio::spawn(async move {
                let mut held = Vec::new();
                while let Ok((socket, _)) = listener.accept().await {
                    held.push(socket);
                }
            });
            Url::parse(&format!("http://{addr}/page")).unwrap()
        };
        for policy in [RobotsPolicy::Strict, RobotsPolicy::Rfc9309] {
            for url in [unavailable.clone(), silent.clone()] {
                let err = fetch_robots_document(
                    &client,
                    &url,
                    "test",
                    Duration::from_millis(500),
                    policy,
                )
                .await
                .unwrap_err();
                assert!(
                    err.downcast_ref::<RobotsAccessError>()
                        .is_none_or(|e| !e.permanent),
                    "{policy:?} {url}: fail closed, retry later"
                );
            }
        }
    }

    #[test]
    fn html_bot_challenge_is_detected_as_html() {
        assert!(robots_response_looks_like_html(
            b"<!doctype html><html><title>Just a moment</title>"
        ));
        assert!(!robots_response_looks_like_html(
            b"User-agent: *\nAllow: /events"
        ));
    }

    #[test]
    fn private_and_documentation_addresses_are_rejected() {
        assert!(!is_public_ipv4(Ipv4Addr::new(127, 0, 0, 1)));
        assert!(!is_public_ipv4(Ipv4Addr::new(10, 1, 2, 3)));
        assert!(!is_public_ipv4(Ipv4Addr::new(203, 0, 113, 5)));
        assert!(is_public_ipv4(Ipv4Addr::new(8, 8, 8, 8)));
        assert!(!is_public_ipv6(Ipv6Addr::LOCALHOST));
    }

    #[test]
    fn extracts_only_plain_review_text() {
        let html = r#"<html><head><title>청년 할인</title><script>secret()</script></head>
            <body><main><h1>청년 20% 할인</h1><p>2026년 8월 31일까지</p></main></body></html>"#;
        let (title, text) = extract_review_text(html);
        assert_eq!(title.as_deref(), Some("청년 할인"));
        assert!(text.contains("청년 20% 할인"));
        assert!(!text.contains("secret"));
        assert!(!text.contains("<h1>"));
    }

    #[test]
    fn layout_and_css_changes_preserve_semantic_content_but_terms_do_not() {
        let (_, a) = extract_review_text(
            "<main><h1>학생 할인</h1><p>10% 할인</p><p>재학생만 가능</p></main>",
        );
        let (_, b) = extract_review_text("<main class='new'><div>학생 할인</div><section><span>10% 할인</span></section><div>재학생만 가능</div><script>ignore()</script></main>");
        let (_, c) = extract_review_text(
            "<main><div>학생 할인</div><div>5% 할인</div><div>재학생만 가능</div></main>",
        );
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn discovers_only_in_scope_benefit_links() {
        let candidate = Candidate {
            id: Uuid::nil(),
            domain_id: Uuid::nil(),
            url: "https://brand.example/events".to_string(),
            domain: "brand.example".to_string(),
            allow_subdomains: false,
            allowed_path_prefixes: vec!["/events".to_string()],
            min_request_interval_ms: 10_000,
            crawler_contact_url: "https://mazimi.example/crawler".to_string(),
            etag: None,
            last_modified: None,
            candidate_kind: "index".to_string(),
            discovery_depth: 0,
            content_hash: None,
            monitored: false,
            recheck_interval_seconds: 2_592_000,
        };
        let html = r#"
          <main>
            <a href="/events/detail?eventId=7&utm_source=list"><img alt="청년 20% 할인"></a>
            <a href="/events/login">멤버십 로그인</a>
            <a href="/orders/discount">할인 구매</a>
            <a href="https://attacker.example/events/benefit">이벤트</a>
            <a href="/about">회사소개</a>
          </main>
        "#;
        let (_, review_text) = extract_review_text(html);
        assert!(review_text.contains("멤버십 로그인"));
        let links =
            discover_benefit_links(html, &Url::parse(&candidate.url).unwrap(), &candidate, 50);
        assert_eq!(links.len(), 1);
        assert_eq!(
            links[0].canonical_url,
            "https://brand.example/events/detail?eventId=7"
        );
        assert_eq!(links[0].candidate_kind, "detail");
    }
}
