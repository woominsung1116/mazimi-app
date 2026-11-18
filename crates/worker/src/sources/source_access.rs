//! Shared access rules for every collector that touches an approved source
//! domain: scope validation, the distributed request slot and Retry-After
//! handling. Two collectors with two copies drifted once (one slept a fixed
//! 5 s against a 10 s interval and always timed out); there is one copy now.

use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Utc};
use mazimi_core::source_policy::{host_is_within, path_is_allowed};
use reqwest::Url;
use sqlx::PgPool;
use std::time::Duration;
use uuid::Uuid;

/// A domain-wide Retry-After from a sibling fetch must not park this task.
pub(crate) const MAX_SLOT_WAIT: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SlotOutcome {
    Reserved,
    /// The domain is reserved beyond [`MAX_SLOT_WAIT`]; retry at this time.
    Deferred(DateTime<Utc>),
}

/// Validates that a URL is a credential-free HTTPS URL on port 443 whose host
/// and path fall inside one approved domain scope.
pub(crate) fn validate_scope(
    value: &str,
    domain: &str,
    allow_subdomains: bool,
    allowed_path_prefixes: &[String],
) -> Result<Url> {
    let url = Url::parse(value).context("invalid candidate URL")?;
    if url.scheme() != "https"
        || url.username() != ""
        || url.password().is_some()
        || url.port_or_known_default() != Some(443)
    {
        bail!("candidate must be a credential-free HTTPS URL on port 443");
    }
    let host = url
        .host_str()
        .ok_or_else(|| anyhow!("candidate URL has no host"))?
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if !host_is_within(&host, domain, allow_subdomains) {
        bail!("candidate host is outside the approved domain scope");
    }
    if !path_is_allowed(url.path(), allowed_path_prefixes) {
        bail!("candidate path is outside the approved crawl scope");
    }
    Ok(url)
}

/// Reserves a distributed request slot for the source domain. This prevents
/// two worker replicas from satisfying their local sleeps at the same time and
/// sending concurrent requests to one site. The caller sleeps for the actual
/// remaining time read back from the database, so a 10 s interval domain
/// waits 10 s instead of giving up. A wait longer than [`MAX_SLOT_WAIT`] (for
/// example a sibling fetch that received a one-day Retry-After) is reported
/// as [`SlotOutcome::Deferred`] rather than stalling the whole batch. A
/// caller with a tighter run budget passes a smaller `max_wait`.
pub(crate) async fn reserve_request_slot(
    pool: &PgPool,
    domain_id: Uuid,
    min_request_interval_ms: i64,
    max_wait: Duration,
) -> Result<SlotOutcome> {
    loop {
        let reserved = sqlx::query_scalar::<_, bool>(
            "UPDATE benefit_source_domains
             SET next_request_at = now() + make_interval(secs => $2::double precision / 1000.0),
                 updated_at = now()
             WHERE id = $1
               AND is_active = true
               AND verification_status = 'verified'
               AND crawl_allowed = true
               AND compliance_status = 'approved'
               AND robots_status = 'allowed'
               AND approval_expires_at > now()
               AND (next_request_at IS NULL OR next_request_at <= now())
             RETURNING true",
        )
        .bind(domain_id)
        .bind(min_request_interval_ms)
        .fetch_optional(pool)
        .await?
        .unwrap_or(false);
        if reserved {
            return Ok(SlotOutcome::Reserved);
        }

        let next_request_at = sqlx::query_scalar::<_, Option<DateTime<Utc>>>(
            "SELECT next_request_at
             FROM benefit_source_domains
             WHERE id = $1
               AND is_active = true
               AND verification_status = 'verified'
               AND crawl_allowed = true
               AND compliance_status = 'approved'
               AND robots_status = 'allowed'
               AND approval_expires_at > now()",
        )
        .bind(domain_id)
        .fetch_optional(pool)
        .await?
        .flatten()
        .ok_or_else(|| anyhow!("source approval changed while waiting for a request slot"))?;
        let wait = (next_request_at - Utc::now())
            .to_std()
            .unwrap_or_else(|_| Duration::from_millis(50));
        if wait > max_wait.min(MAX_SLOT_WAIT) {
            return Ok(SlotOutcome::Deferred(next_request_at));
        }
        tokio::time::sleep(wait.max(Duration::from_millis(50))).await;
    }
}

/// Records an upstream rate limit (HTTP 429/503) as a domain-wide pause so
/// every collector, not only the one that saw the header, backs off.
pub(crate) async fn record_retry_after(
    pool: &PgPool,
    domain_id: Uuid,
    headers: &reqwest::header::HeaderMap,
) -> Result<DateTime<Utc>> {
    let until = super::benefit_pages::retry_at(headers, Utc::now());
    defer_domain(pool, domain_id, until).await?;
    Ok(until)
}

/// Domain-wide pause until `until` (never shortens an existing one).
pub(crate) async fn defer_domain(
    pool: &PgPool,
    domain_id: Uuid,
    until: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE benefit_source_domains SET next_request_at=GREATEST(COALESCE(next_request_at,now()),$2),updated_at=now() WHERE id=$1")
        .bind(domain_id).bind(until).execute(pool).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_validation_rejects_credentials_foreign_hosts_and_paths() {
        let prefixes = vec!["/apply".to_string()];
        assert!(validate_scope(
            "https://www.city.go.kr/apply/1",
            "city.go.kr",
            true,
            &prefixes
        )
        .is_ok());
        assert!(validate_scope(
            "https://www.city.go.kr/apply/1",
            "city.go.kr",
            false,
            &prefixes
        )
        .is_err());
        assert!(validate_scope(
            "https://u:p@city.go.kr/apply",
            "city.go.kr",
            true,
            &prefixes
        )
        .is_err());
        assert!(validate_scope(
            "https://city.go.kr/applystore",
            "city.go.kr",
            true,
            &prefixes
        )
        .is_err());
        assert!(validate_scope("http://city.go.kr/apply", "city.go.kr", true, &prefixes).is_err());
    }

    /// A domain with a 10 s interval: the second reservation must wait for
    /// the slot and succeed, not give up after a fixed short poll.
    #[tokio::test]
    async fn second_reservation_waits_for_the_interval_instead_of_failing() {
        let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
            eprintln!("TEST_DATABASE_URL unset; skipping database-backed slot test");
            return;
        };
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect(&url)
            .await
            .unwrap();
        let domain_id = Uuid::new_v4();
        let domain = format!("{}.example", domain_id.simple());
        sqlx::query("INSERT INTO benefit_source_domains(id,organization_name,domain,source_role,crawl_allowed,verification_status,verification_reference,
          compliance_status,approval_basis,approval_reference,terms_checked_at,terms_review_outcome,robots_status,robots_checked_at,robots_content_hash,
          allowed_path_prefixes,robots_reviewed_path_prefixes,crawler_contact_url,compliance_reviewed_by,compliance_reviewed_at,approval_expires_at,collection_mode,min_request_interval_ms)
          VALUES($1,'slot fixture',$2,'brand_official',true,'verified','fixture','approved','written_permission','fixture',now(),'explicit_permission','allowed',now(),repeat('a',32),
          ARRAY['/'],ARRAY['/'],'https://mazimi.example/bot','fixture',now(),now()+interval '30 days','direct_public',10000)")
            .bind(domain_id).bind(&domain).execute(&pool).await.unwrap();
        let started = std::time::Instant::now();
        assert_eq!(
            reserve_request_slot(&pool, domain_id, 10_000, MAX_SLOT_WAIT)
                .await
                .unwrap(),
            SlotOutcome::Reserved
        );
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(
            reserve_request_slot(&pool, domain_id, 10_000, MAX_SLOT_WAIT)
                .await
                .unwrap(),
            SlotOutcome::Reserved
        );
        let elapsed = started.elapsed();
        assert!(elapsed >= Duration::from_secs(9), "waited only {elapsed:?}");
        assert!(elapsed < Duration::from_secs(20), "waited {elapsed:?}");
        // A far-future pause (sibling Retry-After) defers instead of sleeping.
        sqlx::query(
            "UPDATE benefit_source_domains SET next_request_at=now()+interval '1 day' WHERE id=$1",
        )
        .bind(domain_id)
        .execute(&pool)
        .await
        .unwrap();
        assert!(matches!(
            reserve_request_slot(&pool, domain_id, 10_000, MAX_SLOT_WAIT)
                .await
                .unwrap(),
            SlotOutcome::Deferred(_)
        ));
        sqlx::query("DELETE FROM benefit_source_domains WHERE id=$1")
            .bind(domain_id)
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
    }
}
