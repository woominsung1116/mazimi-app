//! Application service for brand-source discovery administration.
//!
//! HTTP handlers own authentication and request validation. This service owns
//! workflow invariants and transaction boundaries so the same operations can
//! later be called by a CLI or internal job without duplicating policy.

use chrono::{DateTime, Utc};
use mazimi_core::source_policy::canonicalize_public_https_url;
use serde::Serialize;
use sqlx::{FromRow, PgPool};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum ServiceError {
    #[error("source suggestion not found")]
    NotFound,
    #[error("only pending suggestions can be reviewed")]
    AlreadyReviewed,
    #[error("only official_homepage suggestions can establish a source domain")]
    UnsupportedSuggestionKind,
    #[error("suggestion contains an invalid public HTTPS URL")]
    InvalidUrl,
    #[error("suggestion domain does not match its URL")]
    DomainMismatch,
    #[error("invalid pagination")]
    InvalidPagination,
    #[error("target already has a different verified official domain")]
    TargetDomainConflict,
    #[error("official domain is already assigned to another brand")]
    RegisteredDomainConflict,
    #[error("official domain was previously rejected")]
    DomainRejected,
    #[error("brand discovery storage operation failed")]
    Storage(#[from] sqlx::Error),
}

#[derive(Debug, Serialize, FromRow)]
pub struct DiscoveryTargetItem {
    pub id: Uuid,
    pub target_key: String,
    pub brand_id: Option<Uuid>,
    pub display_name: String,
    pub coverage_category: String,
    pub discovery_priority: i32,
    pub official_domain: Option<String>,
    pub official_homepage_url: Option<String>,
    pub domain_status: String,
    pub preferred_collection_modes: Vec<String>,
    pub seed_urls: Vec<String>,
    pub policy_status: String,
    pub policy_notes: Option<String>,
    pub last_search_at: Option<DateTime<Utc>>,
    pub next_search_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
}

#[derive(Debug, Serialize, FromRow)]
pub struct SourceSuggestionItem {
    pub id: Uuid,
    pub target_id: Uuid,
    pub display_name: String,
    pub coverage_category: String,
    pub url: String,
    pub domain: String,
    pub page_title: Option<String>,
    pub discovered_via: String,
    pub suggestion_kind: String,
    pub status: String,
    pub first_seen_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub total: i64,
    pub page: i64,
    pub per_page: i64,
    pub has_next: bool,
}

#[derive(Debug, Clone, Copy)]
pub enum ReviewDecision {
    Approve,
    Reject,
}

#[derive(Debug)]
pub struct ReviewSuggestionCommand {
    pub decision: ReviewDecision,
    pub reviewer_id: Uuid,
    pub note: Option<String>,
    pub allow_shared_domain: bool,
}

#[derive(Debug, Serialize)]
pub struct ReviewOutcome {
    pub id: Uuid,
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub official_domain: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub official_homepage_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub crawl_allowed: Option<bool>,
}

#[derive(Debug, FromRow)]
struct SuggestionForReview {
    id: Uuid,
    target_id: Uuid,
    brand_id: Option<Uuid>,
    display_name: String,
    canonical_url: String,
    domain: String,
    suggestion_kind: String,
    status: String,
    official_domain: Option<String>,
    domain_status: String,
}

pub async fn list_targets(
    pool: &PgPool,
    domain_status: Option<&str>,
    category: Option<&str>,
    page: i64,
    per_page: i64,
) -> Result<Page<DiscoveryTargetItem>, ServiceError> {
    let offset = pagination_offset(page, per_page)?;
    let total = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*)
         FROM brand_discovery_targets
         WHERE is_active = true
           AND ($1::text IS NULL OR domain_status = $1)
           AND ($2::text IS NULL OR coverage_category = $2)",
    )
    .bind(domain_status)
    .bind(category)
    .fetch_one(pool)
    .await?;
    let items = sqlx::query_as::<_, DiscoveryTargetItem>(
        "SELECT id, target_key, brand_id, display_name, coverage_category,
                discovery_priority, official_domain, official_homepage_url,
                domain_status, preferred_collection_modes, seed_urls,
                policy_status, policy_notes, last_search_at, next_search_at,
                last_error
         FROM brand_discovery_targets
         WHERE is_active = true
           AND ($1::text IS NULL OR domain_status = $1)
           AND ($2::text IS NULL OR coverage_category = $2)
         ORDER BY discovery_priority DESC, display_name
         LIMIT $3 OFFSET $4",
    )
    .bind(domain_status)
    .bind(category)
    .bind(per_page)
    .bind(offset)
    .fetch_all(pool)
    .await?;
    Ok(Page {
        items,
        total,
        page,
        per_page,
        has_next: offset + per_page < total,
    })
}

pub async fn list_suggestions(
    pool: &PgPool,
    status: Option<&str>,
    page: i64,
    per_page: i64,
) -> Result<Page<SourceSuggestionItem>, ServiceError> {
    let offset = pagination_offset(page, per_page)?;
    let total = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM brand_source_suggestions
         WHERE $1::text IS NULL OR status = $1",
    )
    .bind(status)
    .fetch_one(pool)
    .await?;
    let items = sqlx::query_as::<_, SourceSuggestionItem>(
        "SELECT s.id, s.target_id, t.display_name, t.coverage_category,
                s.url, s.domain, s.page_title, s.discovered_via,
                s.suggestion_kind, s.status, s.first_seen_at, s.last_seen_at
         FROM brand_source_suggestions s
         JOIN brand_discovery_targets t ON t.id = s.target_id
         WHERE $1::text IS NULL OR s.status = $1
         ORDER BY s.last_seen_at DESC
         LIMIT $2 OFFSET $3",
    )
    .bind(status)
    .bind(per_page)
    .bind(offset)
    .fetch_all(pool)
    .await?;
    Ok(Page {
        items,
        total,
        page,
        per_page,
        has_next: offset + per_page < total,
    })
}

fn pagination_offset(page: i64, per_page: i64) -> Result<i64, ServiceError> {
    if page < 1 || per_page < 1 {
        return Err(ServiceError::InvalidPagination);
    }
    (page - 1)
        .checked_mul(per_page)
        .ok_or(ServiceError::InvalidPagination)
}

pub async fn review_suggestion(
    pool: &PgPool,
    id: Uuid,
    command: ReviewSuggestionCommand,
) -> Result<ReviewOutcome, ServiceError> {
    let mut tx = pool.begin().await?;
    let suggestion = sqlx::query_as::<_, SuggestionForReview>(
        "SELECT s.id, s.target_id, t.brand_id, t.display_name,
                s.canonical_url, s.domain, s.suggestion_kind, s.status,
                t.official_domain, t.domain_status
         FROM brand_source_suggestions s
         JOIN brand_discovery_targets t ON t.id = s.target_id
         WHERE s.id = $1
         FOR UPDATE OF s, t",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(ServiceError::NotFound)?;
    if suggestion.status != "pending" {
        return Err(ServiceError::AlreadyReviewed);
    }

    let reviewed_by = command.reviewer_id.to_string();
    if matches!(command.decision, ReviewDecision::Reject) {
        sqlx::query(
            "UPDATE brand_source_suggestions
             SET status = 'rejected', reviewed_at = now(), reviewed_by = $2,
                 review_note = $3, updated_at = now()
             WHERE id = $1",
        )
        .bind(suggestion.id)
        .bind(&reviewed_by)
        .bind(command.note.as_deref())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        return Ok(ReviewOutcome {
            id: suggestion.id,
            status: "rejected",
            target_id: None,
            official_domain: None,
            official_homepage_url: None,
            crawl_allowed: None,
        });
    }

    if suggestion.suggestion_kind != "official_homepage" {
        return Err(ServiceError::UnsupportedSuggestionKind);
    }
    let canonical = canonicalize_public_https_url(&suggestion.canonical_url)
        .map_err(|_| ServiceError::InvalidUrl)?;
    if canonical.domain != suggestion.domain {
        return Err(ServiceError::DomainMismatch);
    }
    if suggestion.domain_status == "verified"
        && suggestion.official_domain.as_deref() != Some(canonical.domain.as_str())
    {
        return Err(ServiceError::TargetDomainConflict);
    }
    let existing_domain = sqlx::query_as::<_, (Uuid, Option<Uuid>, String)>(
        "SELECT id, brand_id, verification_status
         FROM benefit_source_domains
         WHERE domain = $1
         FOR UPDATE",
    )
    .bind(&canonical.domain)
    .fetch_optional(&mut *tx)
    .await?;
    // A domain an administrator already judged fake or unofficial must not
    // come back as verified through an unrelated search suggestion.
    if existing_domain
        .as_ref()
        .is_some_and(|(_, _, status)| status == "rejected")
    {
        return Err(ServiceError::DomainRejected);
    }
    if existing_domain
        .as_ref()
        .and_then(|(_, brand_id, _)| *brand_id)
        .zip(suggestion.brand_id)
        .is_some_and(|(existing, suggested)| existing != suggested)
        && !command.allow_shared_domain
    {
        return Err(ServiceError::RegisteredDomainConflict);
    }

    sqlx::query(
        "UPDATE brand_discovery_targets
         SET official_domain = $2, official_homepage_url = $3,
             domain_status = 'verified',
             policy_status = CASE
                 WHEN policy_status = 'blocked' THEN 'blocked'
                 ELSE 'search_only'
             END,
             policy_notes = COALESCE(
                 policy_notes,
                 'Official domain verified for licensed-search link discovery only; direct collection remains disabled.'
             ),
             next_search_at = now(), last_error = NULL, updated_at = now()
         WHERE id = $1",
    )
    .bind(suggestion.target_id)
    .bind(&canonical.domain)
    .bind(&canonical.canonical_url)
    .execute(&mut *tx)
    .await?;

    if let Some(brand_id) = suggestion.brand_id {
        sqlx::query("UPDATE brands SET homepage_url = $2, updated_at = now() WHERE id = $1")
            .bind(brand_id)
            .bind(&canonical.canonical_url)
            .execute(&mut *tx)
            .await?;
    }

    // Domain verification intentionally does not enable direct crawling.
    let source_domain_id = sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO benefit_source_domains (
             id, brand_id, organization_name, domain, source_role,
             crawl_allowed, verification_status, verification_reference,
             verified_at, compliance_status, collection_mode, is_active,
             created_at, updated_at
         ) VALUES (
             $1, $2, $3, $4, 'brand_official', false, 'verified', $5,
             now(), 'pending', 'licensed_search', true, now(), now()
         )
         ON CONFLICT (domain) DO UPDATE SET
             brand_id = COALESCE(benefit_source_domains.brand_id, EXCLUDED.brand_id),
             organization_name = benefit_source_domains.organization_name,
             verification_status = 'verified',
             verification_reference = COALESCE(
                 benefit_source_domains.verification_reference,
                 EXCLUDED.verification_reference
             ),
             verified_at = now(), updated_at = now()
         RETURNING id",
    )
    .bind(Uuid::new_v4())
    .bind(suggestion.brand_id)
    .bind(&suggestion.display_name)
    .bind(&canonical.domain)
    .bind(&canonical.canonical_url)
    .fetch_one(&mut *tx)
    .await?;

    if let Some(brand_id) = suggestion.brand_id {
        sqlx::query(
            "INSERT INTO benefit_source_domain_brands (domain_id, brand_id)
             VALUES ($1, $2)
             ON CONFLICT (domain_id, brand_id) DO NOTHING",
        )
        .bind(source_domain_id)
        .bind(brand_id)
        .execute(&mut *tx)
        .await?;
    }

    sqlx::query(
        "UPDATE brand_source_suggestions
         SET status = 'approved', reviewed_at = now(), reviewed_by = $2,
             review_note = $3, updated_at = now()
         WHERE id = $1",
    )
    .bind(suggestion.id)
    .bind(reviewed_by)
    .bind(command.note)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(ReviewOutcome {
        id: suggestion.id,
        status: "approved",
        target_id: Some(suggestion.target_id),
        official_domain: Some(canonical.domain),
        official_homepage_url: Some(canonical.canonical_url),
        crawl_allowed: Some(false),
    })
}
