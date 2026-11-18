//! Compliance approval service for direct benefit-page collection.
//!
//! Domain registration proves identity only. This service owns the separate,
//! time-bounded approval workflow and its database invariant inputs.

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use sqlx::{FromRow, PgPool};
use thiserror::Error;
use uuid::Uuid;

const APPROVAL_BASES: &[&str] = &[
    "written_permission",
    "terms_explicit",
    "partner_contract",
    "open_license",
    "public_facts_review",
];
const TERMS_REVIEW_OUTCOMES: &[&str] = &[
    "explicit_permission",
    "no_automated_access_restriction",
    "restricted",
    "not_found",
];
const DIRECT_COLLECTION_MODES: &[&str] = &["direct_public", "official_feed"];
const MAX_ROBOTS_REVIEW_AGE: Duration = Duration::hours(24);
const MAX_CLOCK_SKEW: Duration = Duration::minutes(5);

pub const SOURCE_DOMAIN_RETURNING: &str =
    "id, brand_id, organization_name, domain, source_role, crawl_allowed, \
     verification_status, verification_reference, allow_subdomains, \
     compliance_status, approval_basis, approval_reference, terms_url, \
     terms_review_outcome, collection_mode, policy_notes, robots_url, \
     robots_status, robots_checked_at, robots_content_hash, \
     robots_reviewed_path_prefixes, \
     allowed_path_prefixes, min_request_interval_ms, crawler_contact_url, \
     compliance_reviewed_by, compliance_reviewed_at, approval_expires_at, \
     is_active, updated_at";

#[derive(Debug, Serialize, FromRow)]
pub struct SourceDomainResponse {
    pub id: Uuid,
    pub brand_id: Option<Uuid>,
    pub organization_name: String,
    pub domain: String,
    pub source_role: String,
    pub crawl_allowed: bool,
    pub verification_status: String,
    pub verification_reference: Option<String>,
    pub allow_subdomains: bool,
    pub compliance_status: String,
    pub approval_basis: Option<String>,
    pub approval_reference: Option<String>,
    pub terms_url: Option<String>,
    pub terms_review_outcome: Option<String>,
    pub collection_mode: String,
    pub policy_notes: Option<String>,
    pub robots_url: Option<String>,
    pub robots_status: String,
    pub robots_checked_at: Option<DateTime<Utc>>,
    pub robots_content_hash: Option<String>,
    pub robots_reviewed_path_prefixes: Vec<String>,
    pub allowed_path_prefixes: Vec<String>,
    pub min_request_interval_ms: i32,
    pub crawler_contact_url: Option<String>,
    pub compliance_reviewed_by: Option<String>,
    pub compliance_reviewed_at: Option<DateTime<Utc>>,
    pub approval_expires_at: Option<DateTime<Utc>>,
    pub is_active: bool,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug)]
pub struct RegisterSourceDomainCommand {
    pub organization_name: String,
    pub domain: String,
    pub source_role: String,
    pub brand_id: Option<Uuid>,
    pub verification_reference: String,
    pub is_active: bool,
    pub allow_shared_domain: bool,
}

#[derive(Debug)]
pub struct ApproveCrawlCommand {
    pub approval_basis: String,
    pub approval_reference: String,
    pub terms_url: Option<String>,
    pub terms_review_outcome: Option<String>,
    pub collection_mode: Option<String>,
    pub policy_notes: Option<String>,
    pub allowed_path_prefixes: Vec<String>,
    pub allow_subdomains: bool,
    pub min_request_interval_ms: i32,
    pub crawler_contact_url: String,
    pub reviewer_id: Uuid,
    pub approval_expires_at: DateTime<Utc>,
}

#[derive(Debug)]
pub struct RobotsPreflightEvidence {
    pub robots_url: String,
    pub allowed: bool,
    pub checked_at: DateTime<Utc>,
    pub content_hash: String,
    pub reviewed_path_prefixes: Vec<String>,
}

#[derive(Debug, FromRow)]
struct RobotsApprovalState {
    robots_status: String,
    robots_checked_at: Option<DateTime<Utc>>,
    robots_content_hash: Option<String>,
    robots_reviewed_path_prefixes: Vec<String>,
}

#[derive(Debug, Error)]
pub enum ServiceError {
    #[error("brand not found")]
    BrandNotFound,
    #[error("target already has a different verified official domain")]
    TargetDomainConflict,
    #[error("official domain is already assigned to another brand")]
    RegisteredDomainConflict,
    #[error("invalid approval_basis")]
    InvalidApprovalBasis,
    #[error("invalid or missing terms_review_outcome")]
    InvalidTermsReviewOutcome,
    #[error("a domain with a hub door needs an explicit terms_review_outcome")]
    HubTermsOutcomeRequired,
    #[error("a source with restrictive terms cannot be approved for direct collection")]
    RestrictedTerms,
    #[error("terms_url is required for this approval")]
    TermsUrlRequired,
    #[error("collection_mode must be direct_public or official_feed")]
    InvalidCollectionMode,
    #[error("allowed_path_prefixes must contain at least one path")]
    EmptyPathScope,
    #[error("min_request_interval_ms must be between 5000 and 600000")]
    InvalidRequestInterval,
    #[error("robots_content_hash must be 32 to 128 lowercase or uppercase hexadecimal characters")]
    InvalidRobotsHash,
    #[error("a successful server-side robots preflight is required before crawl approval")]
    RobotsPreflightRequired,
    #[error("approved paths must exactly match paths covered by the latest robots preflight")]
    RobotsScopeMismatch,
    #[error("robots review must be no more than 24 hours old and cannot be in the future")]
    StaleRobotsReview,
    #[error("approval_expires_at must be in the future and no more than 366 days away")]
    InvalidApprovalWindow,
    #[error("verified active source domain not found")]
    NotFound,
    #[error("source-domain storage operation failed")]
    Storage(#[from] sqlx::Error),
}

impl ServiceError {
    pub fn is_validation(&self) -> bool {
        !matches!(
            self,
            Self::BrandNotFound
                | Self::TargetDomainConflict
                | Self::RegisteredDomainConflict
                | Self::NotFound
                | Self::Storage(_)
        )
    }
}

/// Registers domain identity without granting direct collection permission.
///
/// When the domain is an official brand domain, the linked discovery target
/// is updated in the same transaction. Re-registering a domain deliberately
/// clears any previous crawl approval so identity review can never act as an
/// implicit compliance approval.
pub async fn register_source_domain(
    pool: &PgPool,
    command: RegisterSourceDomainCommand,
) -> Result<SourceDomainResponse, ServiceError> {
    let mut tx = pool.begin().await?;

    if let Some(brand_id) = command.brand_id {
        let brand_exists = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(
                 SELECT 1 FROM brands
                 WHERE id = $1 AND is_active = true
             )",
        )
        .bind(brand_id)
        .fetch_one(&mut *tx)
        .await?;
        if !brand_exists {
            return Err(ServiceError::BrandNotFound);
        }

        if command.source_role == "brand_official" && command.is_active {
            let targets = sqlx::query_as::<_, (Option<String>, String)>(
                "SELECT official_domain, domain_status
                 FROM brand_discovery_targets
                 WHERE brand_id = $1 AND is_active = true
                 FOR UPDATE",
            )
            .bind(brand_id)
            .fetch_all(&mut *tx)
            .await?;
            if targets.iter().any(|(official_domain, domain_status)| {
                domain_status == "verified"
                    && official_domain
                        .as_deref()
                        .is_some_and(|domain| domain != command.domain)
            }) {
                return Err(ServiceError::TargetDomainConflict);
            }
        }
    }

    let existing_brand_id = sqlx::query_scalar::<_, Option<Uuid>>(
        "SELECT brand_id
         FROM benefit_source_domains
         WHERE domain = $1
         FOR UPDATE",
    )
    .bind(&command.domain)
    .fetch_optional(&mut *tx)
    .await?
    .flatten();
    if existing_brand_id
        .zip(command.brand_id)
        .is_some_and(|(existing, requested)| existing != requested)
        && !command.allow_shared_domain
    {
        return Err(ServiceError::RegisteredDomainConflict);
    }

    let sql = format!(
        "INSERT INTO benefit_source_domains (
             id, brand_id, organization_name, domain, source_role,
             crawl_allowed, verification_status, verification_reference,
             verified_at, is_active,
             created_at, updated_at
         ) VALUES (
             $1, $2, $3, $4, $5, false, 'verified', $6, now(), $7, now(), now()
         )
         ON CONFLICT (domain) DO UPDATE SET
             brand_id = COALESCE(benefit_source_domains.brand_id, EXCLUDED.brand_id),
             organization_name = EXCLUDED.organization_name,
             source_role = EXCLUDED.source_role,
             crawl_allowed = false,
             verification_status = 'verified',
             verification_reference = EXCLUDED.verification_reference,
             verified_at = now(),
             compliance_status = 'pending',
             approval_basis = NULL,
             approval_reference = NULL,
             terms_url = NULL,
             terms_checked_at = NULL,
             terms_review_outcome = NULL,
             robots_url = NULL,
             robots_status = 'pending',
             robots_checked_at = NULL,
             robots_content_hash = NULL,
             robots_reviewed_path_prefixes = ARRAY[]::text[],
             next_request_at = NULL,
             allowed_path_prefixes = ARRAY[]::text[],
             allow_subdomains = false,
             min_request_interval_ms = 10000,
             crawler_contact_url = NULL,
             compliance_reviewed_by = NULL,
             compliance_reviewed_at = NULL,
             approval_expires_at = NULL,
             collection_mode = 'manual_review',
             policy_notes = NULL,
             is_active = EXCLUDED.is_active,
             updated_at = now()
         RETURNING {SOURCE_DOMAIN_RETURNING}"
    );
    let row = sqlx::query_as::<_, SourceDomainResponse>(&sql)
        .bind(Uuid::new_v4())
        .bind(command.brand_id)
        .bind(&command.organization_name)
        .bind(&command.domain)
        .bind(&command.source_role)
        .bind(&command.verification_reference)
        .bind(command.is_active)
        .fetch_one(&mut *tx)
        .await?;

    if let Some(brand_id) = command.brand_id {
        sqlx::query(
            "INSERT INTO benefit_source_domain_brands (domain_id, brand_id)
             VALUES ($1, $2)
             ON CONFLICT (domain_id, brand_id) DO NOTHING",
        )
        .bind(row.id)
        .bind(brand_id)
        .execute(&mut *tx)
        .await?;

        if command.source_role == "brand_official" {
            if command.is_active {
                let homepage = format!("https://{}/", command.domain);
                sqlx::query(
                    "UPDATE brand_discovery_targets
                     SET official_domain = $2,
                         official_homepage_url = COALESCE(official_homepage_url, $3),
                         domain_status = 'verified',
                         policy_status = CASE
                             WHEN policy_status = 'blocked' THEN 'blocked'
                             ELSE 'search_only'
                         END,
                         policy_notes = COALESCE(
                             policy_notes,
                             'Official domain verified; direct collection remains disabled pending separate compliance approval.'
                         ),
                         next_search_at = now(), last_error = NULL, updated_at = now()
                     WHERE brand_id = $1 AND is_active = true",
                )
                .bind(brand_id)
                .bind(&command.domain)
                .bind(&homepage)
                .execute(&mut *tx)
                .await?;
                sqlx::query(
                    "UPDATE brands
                     SET homepage_url = COALESCE(homepage_url, $2), updated_at = now()
                     WHERE id = $1",
                )
                .bind(brand_id)
                .bind(homepage)
                .execute(&mut *tx)
                .await?;
            } else {
                sqlx::query(
                    "UPDATE brand_discovery_targets
                     SET domain_status = 'unresolved',
                         policy_status = CASE
                             WHEN policy_status = 'blocked' THEN 'blocked'
                             ELSE 'unreviewed'
                         END,
                         next_search_at = now(), updated_at = now()
                     WHERE brand_id = $1 AND official_domain = $2 AND is_active = true",
                )
                .bind(brand_id)
                .bind(&command.domain)
                .execute(&mut *tx)
                .await?;
            }
        }
    }

    tx.commit().await?;
    Ok(row)
}

pub async fn approve_crawl(
    pool: &PgPool,
    id: Uuid,
    command: ApproveCrawlCommand,
    now: DateTime<Utc>,
) -> Result<SourceDomainResponse, ServiceError> {
    validate_approval(&command, now)?;

    let mut tx = pool.begin().await?;
    let robots = sqlx::query_as::<_, RobotsApprovalState>(
        "SELECT robots_status, robots_checked_at, robots_content_hash,
                robots_reviewed_path_prefixes
         FROM benefit_source_domains
         WHERE id = $1 AND is_active = true AND verification_status = 'verified'
         FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(ServiceError::NotFound)?;
    validate_robots_evidence(&robots, &command.allowed_path_prefixes, now)?;
    let owns_hub: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM corporate_doors WHERE domain_id = $1 AND kind = 'hub_rows')",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    validate_hub_terms(owns_hub, &command)?;

    let terms_review_outcome = command.terms_review_outcome.unwrap_or_else(|| {
        if command.approval_basis == "public_facts_review" {
            // Validation rejects this case; the empty value is never written.
            String::new()
        } else {
            "explicit_permission".to_string()
        }
    });
    let collection_mode = command
        .collection_mode
        .unwrap_or_else(|| "direct_public".to_string());
    let reviewer_id = command.reviewer_id.to_string();
    let sql = format!(
        "UPDATE benefit_source_domains SET
             crawl_allowed = true,
             compliance_status = 'approved',
             approval_basis = $2,
             approval_reference = $3,
             terms_url = $4,
             terms_checked_at = $5,
             terms_review_outcome = $6,
             allowed_path_prefixes = $7,
             allow_subdomains = $8,
             min_request_interval_ms = $9,
             crawler_contact_url = $10,
             compliance_reviewed_by = $11,
             compliance_reviewed_at = $5,
             approval_expires_at = $12,
             collection_mode = $13,
             policy_notes = $14,
             updated_at = $5
         WHERE id = $1
           AND is_active = true
           AND verification_status = 'verified'
         RETURNING {SOURCE_DOMAIN_RETURNING}"
    );
    let row = sqlx::query_as::<_, SourceDomainResponse>(&sql)
        .bind(id)
        .bind(command.approval_basis)
        .bind(command.approval_reference)
        .bind(command.terms_url)
        .bind(now)
        .bind(terms_review_outcome)
        .bind(command.allowed_path_prefixes)
        .bind(command.allow_subdomains)
        .bind(command.min_request_interval_ms)
        .bind(command.crawler_contact_url)
        .bind(reviewer_id)
        .bind(command.approval_expires_at)
        .bind(collection_mode)
        .bind(command.policy_notes)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(ServiceError::NotFound)?;
    tx.commit().await?;
    Ok(row)
}

pub async fn record_robots_preflight(
    pool: &PgPool,
    id: Uuid,
    evidence: RobotsPreflightEvidence,
) -> Result<SourceDomainResponse, ServiceError> {
    validate_hash(&evidence.content_hash)?;
    if evidence.reviewed_path_prefixes.is_empty() {
        return Err(ServiceError::EmptyPathScope);
    }
    let robots_status = if evidence.allowed {
        "allowed"
    } else {
        "blocked"
    };
    let compliance_status = if evidence.allowed {
        "pending"
    } else {
        "rejected"
    };
    let sql = format!(
        "UPDATE benefit_source_domains SET
             crawl_allowed = false,
             compliance_status = $2,
             collection_mode = 'manual_review',
             robots_url = $3,
             robots_status = $4,
             robots_checked_at = $5,
             robots_content_hash = $6,
             robots_reviewed_path_prefixes = $7,
             next_request_at = NULL,
             updated_at = now()
         WHERE id = $1 AND is_active = true AND verification_status = 'verified'
         RETURNING {SOURCE_DOMAIN_RETURNING}"
    );
    sqlx::query_as::<_, SourceDomainResponse>(&sql)
        .bind(id)
        .bind(compliance_status)
        .bind(evidence.robots_url)
        .bind(robots_status)
        .bind(evidence.checked_at)
        .bind(evidence.content_hash.to_ascii_lowercase())
        .bind(evidence.reviewed_path_prefixes)
        .fetch_optional(pool)
        .await?
        .ok_or(ServiceError::NotFound)
}

pub async fn record_robots_unavailable(pool: &PgPool, id: Uuid) -> Result<(), ServiceError> {
    let result = sqlx::query(
        "UPDATE benefit_source_domains SET
             crawl_allowed = false,
             compliance_status = 'pending',
             collection_mode = 'manual_review',
             robots_status = 'unavailable',
             robots_checked_at = now(),
             robots_content_hash = NULL,
             robots_reviewed_path_prefixes = ARRAY[]::TEXT[],
             next_request_at = NULL,
             updated_at = now()
         WHERE id = $1 AND is_active = true AND verification_status = 'verified'",
    )
    .bind(id)
    .execute(pool)
    .await?;
    if result.rows_affected() == 0 {
        return Err(ServiceError::NotFound);
    }
    Ok(())
}

/// The terms outcome is what lets the door job read a hub list page
/// (security review M3): for a domain that owns a `hub_rows` door it is
/// never defaulted from the approval basis; the reviewer states it.
fn validate_hub_terms(owns_hub: bool, command: &ApproveCrawlCommand) -> Result<(), ServiceError> {
    if owns_hub && command.terms_review_outcome.is_none() {
        return Err(ServiceError::HubTermsOutcomeRequired);
    }
    Ok(())
}

fn validate_approval(
    command: &ApproveCrawlCommand,
    now: DateTime<Utc>,
) -> Result<(), ServiceError> {
    if !APPROVAL_BASES.contains(&command.approval_basis.as_str()) {
        return Err(ServiceError::InvalidApprovalBasis);
    }
    let terms_outcome = command.terms_review_outcome.as_deref().unwrap_or_else(|| {
        if command.approval_basis == "public_facts_review" {
            ""
        } else {
            "explicit_permission"
        }
    });
    if !TERMS_REVIEW_OUTCOMES.contains(&terms_outcome) {
        return Err(ServiceError::InvalidTermsReviewOutcome);
    }
    if terms_outcome == "restricted" {
        return Err(ServiceError::RestrictedTerms);
    }
    let terms_required = matches!(
        command.approval_basis.as_str(),
        "terms_explicit" | "open_license"
    ) || (command.approval_basis == "public_facts_review"
        && terms_outcome != "not_found");
    if terms_required && command.terms_url.is_none() {
        return Err(ServiceError::TermsUrlRequired);
    }
    if command
        .collection_mode
        .as_deref()
        .is_some_and(|mode| !DIRECT_COLLECTION_MODES.contains(&mode))
    {
        return Err(ServiceError::InvalidCollectionMode);
    }
    if command.allowed_path_prefixes.is_empty() {
        return Err(ServiceError::EmptyPathScope);
    }
    if !(5_000..=600_000).contains(&command.min_request_interval_ms) {
        return Err(ServiceError::InvalidRequestInterval);
    }

    if command.approval_expires_at <= now || command.approval_expires_at > now + Duration::days(366)
    {
        return Err(ServiceError::InvalidApprovalWindow);
    }
    Ok(())
}

fn validate_robots_evidence(
    robots: &RobotsApprovalState,
    requested_paths: &[String],
    now: DateTime<Utc>,
) -> Result<(), ServiceError> {
    if robots.robots_status != "allowed" {
        return Err(ServiceError::RobotsPreflightRequired);
    }
    let checked_at = robots
        .robots_checked_at
        .ok_or(ServiceError::RobotsPreflightRequired)?;
    let hash = robots
        .robots_content_hash
        .as_deref()
        .ok_or(ServiceError::RobotsPreflightRequired)?;
    validate_hash(hash)?;
    if checked_at < now - MAX_ROBOTS_REVIEW_AGE || checked_at > now + MAX_CLOCK_SKEW {
        return Err(ServiceError::StaleRobotsReview);
    }
    if requested_paths.iter().any(|requested| {
        !robots
            .robots_reviewed_path_prefixes
            .iter()
            .any(|reviewed| reviewed == requested)
    }) {
        return Err(ServiceError::RobotsScopeMismatch);
    }
    Ok(())
}

fn validate_hash(hash: &str) -> Result<(), ServiceError> {
    let hash = hash.trim();
    if !(32..=128).contains(&hash.len()) || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(ServiceError::InvalidRobotsHash);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_command(now: DateTime<Utc>) -> ApproveCrawlCommand {
        ApproveCrawlCommand {
            approval_basis: "terms_explicit".to_string(),
            approval_reference: "ticket-1".to_string(),
            terms_url: Some("https://brand.example/terms".to_string()),
            terms_review_outcome: Some("explicit_permission".to_string()),
            collection_mode: Some("direct_public".to_string()),
            policy_notes: None,
            allowed_path_prefixes: vec!["/events".to_string()],
            allow_subdomains: false,
            min_request_interval_ms: 10_000,
            crawler_contact_url: "https://mazimi.example/bot".to_string(),
            reviewer_id: Uuid::nil(),
            approval_expires_at: now + Duration::days(30),
        }
    }

    #[test]
    fn a_hub_domain_needs_a_stated_terms_outcome() {
        let now = Utc::now();
        let mut command = valid_command(now);
        command.terms_review_outcome = None;
        assert!(matches!(
            validate_hub_terms(true, &command),
            Err(ServiceError::HubTermsOutcomeRequired)
        ));
        assert!(
            validate_hub_terms(false, &command).is_ok(),
            "other domains keep the default"
        );
        command.terms_review_outcome = Some("explicit_permission".to_string());
        assert!(validate_hub_terms(true, &command).is_ok());
    }

    #[test]
    fn rejects_restrictive_terms() {
        let now = Utc::now();
        let mut command = valid_command(now);
        command.terms_review_outcome = Some("restricted".to_string());
        assert!(matches!(
            validate_approval(&command, now),
            Err(ServiceError::RestrictedTerms)
        ));
    }

    #[test]
    fn rejects_stale_or_unverifiable_robots_evidence() {
        let now = Utc::now();
        let mut robots = RobotsApprovalState {
            robots_status: "allowed".to_string(),
            robots_checked_at: Some(now - Duration::hours(25)),
            robots_content_hash: Some("a".repeat(64)),
            robots_reviewed_path_prefixes: vec!["/events".to_string()],
        };
        assert!(matches!(
            validate_robots_evidence(&robots, &["/events".to_string()], now),
            Err(ServiceError::StaleRobotsReview)
        ));

        robots.robots_checked_at = Some(now);
        robots.robots_content_hash = Some("not-a-hash".to_string());
        assert!(matches!(
            validate_robots_evidence(&robots, &["/events".to_string()], now),
            Err(ServiceError::InvalidRobotsHash)
        ));
    }
}
