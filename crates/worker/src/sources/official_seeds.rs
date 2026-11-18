//! Idempotent discovery of administrator-reviewed official seed URLs.
//!
//! Seed URLs are configuration, not permission to fetch a page. This service
//! only places canonical URLs in the private review/fetch queue. A candidate
//! is fetchable only when the independently registered source domain has an
//! active compliance approval and the URL is within its approved path scope.
//!
//! Re-discovery never rewinds a candidate that is already in a human or
//! monitoring state; only a plain `queued` row follows the current domain
//! approval. Otherwise a 6-hourly seed run would turn a `change_review` hold
//! back into a fresh-looking candidate.

use mazimi_core::source_policy::{
    canonicalize_public_https_url, classify_candidate_kind, host_is_within,
    is_interactive_or_purchase_url, path_is_allowed, CanonicalPublicUrl, PublicUrlError,
};
use sqlx::{FromRow, PgPool};
use thiserror::Error;
use tracing::{info, warn};
use uuid::Uuid;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct OfficialSeedSummary {
    pub targets: usize,
    pub configured_urls: usize,
    pub candidates_refreshed: usize,
    pub skipped: usize,
}

#[derive(Debug, Error)]
pub enum OfficialSeedDiscoveryError {
    #[error("official seed discovery storage operation failed")]
    Storage(#[from] sqlx::Error),
}

#[derive(Debug, FromRow)]
struct SeedTarget {
    id: Uuid,
    brand_id: Option<Uuid>,
    display_name: String,
    official_domain: Option<String>,
    seed_urls: Vec<String>,
}

#[derive(Debug, FromRow)]
struct RegisteredDomain {
    id: Uuid,
    crawl_allowed: bool,
    allow_subdomains: bool,
    allowed_path_prefixes: Vec<String>,
}

#[derive(Debug, Error, PartialEq, Eq)]
enum SeedValidationError {
    #[error("invalid public URL: {0}")]
    InvalidUrl(#[from] PublicUrlError),
    #[error("URL is outside the registered official domain")]
    OutsideOfficialDomain,
    #[error("interactive account, checkout, or purchase URLs cannot be discovery seeds")]
    InteractiveUrl,
}

/// Stateless application service for configured official URLs.
pub struct OfficialSeedDiscoverer;

impl OfficialSeedDiscoverer {
    pub async fn run(pool: &PgPool) -> Result<OfficialSeedSummary, OfficialSeedDiscoveryError> {
        let targets = sqlx::query_as::<_, SeedTarget>(
            "SELECT id, brand_id, display_name, official_domain, seed_urls
             FROM brand_discovery_targets
             WHERE is_active = true
               AND domain_status = 'verified'
               AND policy_status <> 'blocked'
               AND cardinality(seed_urls) > 0
             ORDER BY discovery_priority DESC, display_name",
        )
        .fetch_all(pool)
        .await?;

        let mut summary = OfficialSeedSummary {
            targets: targets.len(),
            configured_urls: targets.iter().map(|target| target.seed_urls.len()).sum(),
            ..OfficialSeedSummary::default()
        };

        for target in targets {
            let Some(official_domain) = target.official_domain.as_deref() else {
                summary.skipped += target.seed_urls.len();
                warn!(
                    target_id = %target.id,
                    brand = %target.display_name,
                    "verified seed target has no official domain"
                );
                continue;
            };

            let registered = sqlx::query_as::<_, RegisteredDomain>(
                "SELECT id, crawl_allowed, allow_subdomains, allowed_path_prefixes
                 FROM benefit_source_domains
                 WHERE domain = $1
                   AND is_active = true
                   AND verification_status = 'verified'",
            )
            .bind(official_domain)
            .fetch_optional(pool)
            .await?;

            let Some(registered) = registered else {
                summary.skipped += target.seed_urls.len();
                warn!(
                    target_id = %target.id,
                    brand = %target.display_name,
                    domain = official_domain,
                    "official seed target has no active verified source-domain record"
                );
                continue;
            };

            for raw_url in &target.seed_urls {
                let canonical = match validate_seed_url(
                    raw_url,
                    official_domain,
                    registered.allow_subdomains,
                ) {
                    Ok(canonical) => canonical,
                    Err(error) => {
                        summary.skipped += 1;
                        warn!(
                            target_id = %target.id,
                            brand = %target.display_name,
                            seed_url = raw_url,
                            error = %error,
                            "invalid official seed URL skipped"
                        );
                        continue;
                    }
                };

                store_seed_candidate(pool, &target, &registered, canonical).await?;
                summary.candidates_refreshed += 1;
            }
        }

        info!(
            targets = summary.targets,
            configured_urls = summary.configured_urls,
            candidates_refreshed = summary.candidates_refreshed,
            skipped = summary.skipped,
            "official seed discovery complete"
        );
        Ok(summary)
    }
}

fn validate_seed_url(
    raw_url: &str,
    official_domain: &str,
    allow_subdomains: bool,
) -> Result<CanonicalPublicUrl, SeedValidationError> {
    let canonical = canonicalize_public_https_url(raw_url)?;
    if !host_is_within(&canonical.domain, official_domain, allow_subdomains) {
        return Err(SeedValidationError::OutsideOfficialDomain);
    }
    if is_interactive_or_purchase_url(&canonical.url) {
        return Err(SeedValidationError::InteractiveUrl);
    }
    Ok(canonical)
}

async fn store_seed_candidate(
    pool: &PgPool,
    target: &SeedTarget,
    domain: &RegisteredDomain,
    canonical: CanonicalPublicUrl,
) -> Result<(), sqlx::Error> {
    let directly_fetchable = domain.crawl_allowed
        && path_is_allowed(canonical.url.path(), &domain.allowed_path_prefixes);
    let status = if directly_fetchable {
        "queued"
    } else {
        "review"
    };
    let candidate_kind = classify_candidate_kind(&canonical.url).as_str();

    let mut tx = pool.begin().await?;
    let candidate_id = sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO benefit_source_candidates (
             id, domain_id, url, canonical_url, discovered_via,
             status, candidate_kind, discovery_depth,
             discovered_at, last_discovered_at, created_at, updated_at
         ) VALUES (
             $1, $2, $3, $4, 'official_page',
             $5, $6, 0,
             now(), now(), now(), now()
         )
         ON CONFLICT (canonical_url) DO UPDATE SET
             domain_id = EXCLUDED.domain_id,
             url = EXCLUDED.url,
             last_discovered_at = now(),
             candidate_kind = CASE
                 WHEN benefit_source_candidates.candidate_kind = 'unknown'
                 THEN EXCLUDED.candidate_kind
                 ELSE benefit_source_candidates.candidate_kind
             END,
             status = CASE
                 WHEN benefit_source_candidates.status IN (
                     'fetched', 'extracted', 'review', 'approved', 'rejected',
                     'change_review', 'unreachable_review', 'verification_due', 'error'
                 ) OR EXISTS (
                     SELECT 1 FROM program_source_monitors monitor
                     WHERE monitor.candidate_id = benefit_source_candidates.id
                 ) THEN benefit_source_candidates.status
                 ELSE EXCLUDED.status
             END,
             updated_at = now()
         RETURNING id",
    )
    .bind(Uuid::new_v4())
    .bind(domain.id)
    .bind(canonical.url.as_str())
    .bind(canonical.canonical_url)
    .bind(status)
    .bind(candidate_kind)
    .fetch_one(&mut *tx)
    .await?;

    if let Some(brand_id) = target.brand_id {
        sqlx::query(
            "INSERT INTO benefit_candidate_brands (candidate_id, brand_id)
             VALUES ($1, $2)
             ON CONFLICT (candidate_id, brand_id) DO NOTHING",
        )
        .bind(candidate_id)
        .bind(brand_id)
        .execute(&mut *tx)
        .await?;
    }

    sqlx::query(
        "UPDATE benefit_source_domains
         SET last_discovered_at = now(), updated_at = now()
         WHERE id = $1",
    )
    .bind(domain.id)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_and_canonicalizes_an_official_seed() {
        let seed = validate_seed_url(
            "https://brand.example/events?eventId=7&utm_source=seed#top",
            "brand.example",
            false,
        )
        .unwrap();
        assert_eq!(seed.canonical_url, "https://brand.example/events?eventId=7");
    }

    #[test]
    fn rejects_external_and_interactive_seeds() {
        assert_eq!(
            validate_seed_url("https://attacker.example/events", "brand.example", true)
                .unwrap_err(),
            SeedValidationError::OutsideOfficialDomain
        );
        assert_eq!(
            validate_seed_url("https://brand.example/checkout", "brand.example", false)
                .unwrap_err(),
            SeedValidationError::InteractiveUrl
        );
    }

    #[test]
    fn subdomains_require_explicit_scope() {
        assert!(validate_seed_url(
            "https://events.brand.example/events",
            "brand.example",
            false
        )
        .is_err());
        assert!(
            validate_seed_url("https://events.brand.example/events", "brand.example", true).is_ok()
        );
    }
}
