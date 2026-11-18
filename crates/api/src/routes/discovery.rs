use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use mazimi_core::canonical_region_code;
use mazimi_core::source_policy::{canonicalize_public_https_url, path_is_allowed};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{FromRow, PgPool};
use std::{collections::HashSet, net::IpAddr};
use uuid::Uuid;

use crate::{
    auth::AdminUser,
    services::brand_discovery::{self, ReviewDecision, ReviewSuggestionCommand, ServiceError},
    services::source_domains::{
        self, ApproveCrawlCommand, RegisterSourceDomainCommand, RobotsPreflightEvidence,
        ServiceError as SourceDomainServiceError, SourceDomainResponse, SOURCE_DOMAIN_RETURNING,
    },
    services::source_reviews::{
        self, Resolution as SourceReviewResolution, ReviewCommand as SourceReviewCommand,
        ServiceError as SourceReviewServiceError,
    },
};

type ApiError = (StatusCode, Json<Value>);

const SOURCE_ROLES: &[&str] = &["brand_official", "benefit_provider", "partner"];
const DISCOVERY_METHODS: &[&str] = &[
    "search_api",
    "sitemap",
    "official_page",
    "official_feed",
    "manual",
    "user_submission",
    "partner_api",
];
const CANDIDATE_STATUSES: &[&str] = &[
    "queued",
    "fetched",
    "extracted",
    "review",
    "approved",
    "change_review",
    "unreachable_review",
    "verification_due",
    "rejected",
    "error",
];

fn bad_request(message: &str) -> ApiError {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": message })))
}

fn validate_text(value: &str, field: &str, max_chars: usize) -> Result<String, ApiError> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > max_chars {
        return Err(bad_request(&format!(
            "{field} must be between 1 and {max_chars} characters"
        )));
    }
    Ok(value.to_string())
}

fn validate_pagination(
    page: Option<i64>,
    per_page: Option<i64>,
    default_per_page: i64,
    max_per_page: i64,
) -> Result<(i64, i64), ApiError> {
    let page = page.unwrap_or(1);
    let per_page = per_page.unwrap_or(default_per_page);
    if !(1..=1_000_000).contains(&page) {
        return Err(bad_request("page must be between 1 and 1000000"));
    }
    if !(1..=max_per_page).contains(&per_page) {
        return Err(bad_request(&format!(
            "per_page must be between 1 and {max_per_page}"
        )));
    }
    Ok((page, per_page))
}

fn normalize_domain(value: &str) -> Result<String, ApiError> {
    let value = validate_text(value, "domain", 253)?;
    let candidate = if value.contains("://") {
        value
    } else {
        format!("https://{value}")
    };
    let parsed = reqwest::Url::parse(&candidate).map_err(|_| bad_request("Invalid domain"))?;
    if parsed.username() != ""
        || parsed.password().is_some()
        || parsed.port().is_some()
        || !matches!(parsed.scheme(), "http" | "https")
        || (parsed.path() != "" && parsed.path() != "/")
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(bad_request(
            "Domain must be a hostname without path or port",
        ));
    }

    let host = parsed
        .host_str()
        .ok_or_else(|| bad_request("Domain has no hostname"))?
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if host == "localhost" || host.ends_with(".localhost") || host.parse::<IpAddr>().is_ok() {
        return Err(bad_request("Public DNS domains only"));
    }
    if !host.contains('.') {
        return Err(bad_request("Domain must contain a public suffix"));
    }
    Ok(host)
}

fn canonicalize_candidate_url(value: &str) -> Result<(String, String), ApiError> {
    let value = validate_text(value, "url", 2_000)?;
    let parsed = canonicalize_public_https_url(&value)
        .map_err(|error| bad_request(&format!("Invalid public HTTPS URL: {error}")))?;
    Ok((parsed.canonical_url, parsed.domain))
}

fn validate_https_url(value: &str, field: &str) -> Result<String, ApiError> {
    let value = validate_text(value, field, 2_000)?;
    let parsed = reqwest::Url::parse(&value).map_err(|_| bad_request("Invalid HTTPS URL"))?;
    if parsed.scheme() != "https"
        || parsed.host_str().is_none()
        || parsed.username() != ""
        || parsed.password().is_some()
    {
        return Err(bad_request(&format!(
            "{field} must be a public HTTPS URL without credentials"
        )));
    }
    Ok(parsed.to_string())
}

fn validate_path_prefixes(values: Vec<String>) -> Result<Vec<String>, ApiError> {
    if values.is_empty() || values.len() > 50 {
        return Err(bad_request(
            "allowed_path_prefixes must contain 1 to 50 paths",
        ));
    }
    let mut seen = HashSet::new();
    let mut paths = Vec::with_capacity(values.len());
    for value in values {
        let path = validate_text(&value, "allowed_path_prefix", 500)?;
        if !path.starts_with('/')
            || path.starts_with("//")
            || path.contains("..")
            || path.contains('?')
            || path.contains('#')
            || path.chars().any(char::is_whitespace)
        {
            return Err(bad_request(
                "Each allowed path must be an absolute URL path without query, fragment, whitespace, or '..'",
            ));
        }
        if seen.insert(path.clone()) {
            paths.push(path);
        }
    }
    Ok(paths)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpsertSourceDomainInput {
    pub organization_name: String,
    pub domain: String,
    pub source_role: String,
    pub brand_id: Option<Uuid>,
    /// Evidence that this is the organisation's official/provider domain.
    /// Domain identity verification does not grant permission to crawl.
    pub verification_reference: String,
    pub is_active: Option<bool>,
    /// Required when intentionally associating a shared provider domain with
    /// more than one brand.
    pub allow_shared_domain: Option<bool>,
}

/// POST /api/v1/admin/benefit-source-domains
pub async fn upsert_source_domain(
    _admin: AdminUser,
    State(pool): State<PgPool>,
    Json(input): Json<UpsertSourceDomainInput>,
) -> Result<Json<SourceDomainResponse>, ApiError> {
    let organization_name = validate_text(&input.organization_name, "organization_name", 200)?;
    let domain = normalize_domain(&input.domain)?;
    let verification_reference = validate_text(
        &input.verification_reference,
        "verification_reference",
        2_000,
    )?;
    if !SOURCE_ROLES.contains(&input.source_role.as_str()) {
        return Err(bad_request("Invalid source_role"));
    }
    let row = source_domains::register_source_domain(
        &pool,
        RegisterSourceDomainCommand {
            organization_name,
            domain,
            source_role: input.source_role,
            brand_id: input.brand_id,
            verification_reference,
            is_active: input.is_active.unwrap_or(true),
            allow_shared_domain: input.allow_shared_domain.unwrap_or(false),
        },
    )
    .await
    .map_err(map_source_domain_error)?;

    Ok(Json(row))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApproveSourceDomainInput {
    pub approval_basis: String,
    pub approval_reference: String,
    pub terms_url: Option<String>,
    /// Required for public_facts_review. Explicit-evidence approvals default
    /// to explicit_permission for backwards compatibility.
    pub terms_review_outcome: Option<String>,
    /// direct_public or official_feed. Defaults to direct_public.
    pub collection_mode: Option<String>,
    pub policy_notes: Option<String>,
    pub allowed_path_prefixes: Vec<String>,
    pub allow_subdomains: Option<bool>,
    pub min_request_interval_ms: Option<i32>,
    pub crawler_contact_url: String,
    pub approval_expires_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RobotsPreflightInput {
    pub allowed_path_prefixes: Vec<String>,
    pub crawler_contact_url: String,
}

/// POST /api/v1/admin/benefit-source-domains/{id}/robots-preflight
///
/// A successful check records auditable evidence but always leaves direct
/// collection disabled. The separate approval endpoint consumes this evidence.
pub async fn preflight_source_domain_robots(
    _admin: AdminUser,
    State(pool): State<PgPool>,
    Path(id): Path<Uuid>,
    Json(input): Json<RobotsPreflightInput>,
) -> Result<Json<SourceDomainResponse>, ApiError> {
    let paths = validate_path_prefixes(input.allowed_path_prefixes)?;
    let contact_url = validate_https_url(&input.crawler_contact_url, "crawler_contact_url")?;
    let domain = sqlx::query_scalar::<_, String>(
        "SELECT domain
         FROM benefit_source_domains
         WHERE id = $1 AND is_active = true AND verification_status = 'verified'",
    )
    .bind(id)
    .fetch_optional(&pool)
    .await
    .map_err(crate::errors::internal_error)?
    .ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "Verified active source domain not found" })),
        )
    })?;

    let result = mazimi_worker::sources::benefit_pages::preflight_robots(
        &domain,
        &paths,
        &contact_url,
        std::time::Duration::from_secs(20),
    )
    .await;
    let result = match result {
        Ok(result) => result,
        Err(error) => {
            tracing::warn!(source_domain_id = %id, domain = %domain, error = %error, "robots preflight failed");
            source_domains::record_robots_unavailable(&pool, id)
                .await
                .map_err(map_source_domain_error)?;
            return Err((
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({ "error": "robots.txt could not be verified" })),
            ));
        }
    };
    let evidence = RobotsPreflightEvidence {
        robots_url: result.robots_url,
        allowed: result.allowed,
        checked_at: result.checked_at,
        content_hash: result.content_hash,
        reviewed_path_prefixes: paths,
    };
    let row = source_domains::record_robots_preflight(&pool, id, evidence)
        .await
        .map_err(map_source_domain_error)?;
    Ok(Json(row))
}

/// PUT /api/v1/admin/benefit-source-domains/{id}/approval
///
/// This is deliberately separate from domain registration. It records the
/// evidence and narrow path scope required before an automated fetcher can see
/// the domain as eligible.
pub async fn approve_source_domain_crawl(
    admin: AdminUser,
    State(pool): State<PgPool>,
    Path(id): Path<Uuid>,
    Json(input): Json<ApproveSourceDomainInput>,
) -> Result<Json<SourceDomainResponse>, ApiError> {
    let approval_basis = validate_text(&input.approval_basis, "approval_basis", 100)?;
    let approval_reference = validate_text(&input.approval_reference, "approval_reference", 2_000)?;
    let paths = validate_path_prefixes(input.allowed_path_prefixes)?;
    let contact_url = validate_https_url(&input.crawler_contact_url, "crawler_contact_url")?;
    let terms_url = input
        .terms_url
        .as_deref()
        .map(|value| validate_https_url(value, "terms_url"))
        .transpose()?;
    let terms_review_outcome = input
        .terms_review_outcome
        .as_deref()
        .map(|value| validate_text(value, "terms_review_outcome", 100))
        .transpose()?;
    let collection_mode = input
        .collection_mode
        .as_deref()
        .map(|value| validate_text(value, "collection_mode", 100))
        .transpose()?;
    let policy_notes = input
        .policy_notes
        .as_deref()
        .map(|value| validate_text(value, "policy_notes", 2_000))
        .transpose()?;
    let interval = input.min_request_interval_ms.unwrap_or(10_000);
    let now = Utc::now();
    let command = ApproveCrawlCommand {
        approval_basis,
        approval_reference,
        terms_url,
        terms_review_outcome,
        collection_mode,
        policy_notes,
        allowed_path_prefixes: paths,
        allow_subdomains: input.allow_subdomains.unwrap_or(false),
        min_request_interval_ms: interval,
        crawler_contact_url: contact_url,
        reviewer_id: admin.0.id,
        approval_expires_at: input.approval_expires_at,
    };
    let row = source_domains::approve_crawl(&pool, id, command, now)
        .await
        .map_err(map_source_domain_error)?;
    Ok(Json(row))
}

fn map_source_domain_error(error: SourceDomainServiceError) -> ApiError {
    if error.is_validation() {
        return bad_request(&error.to_string());
    }
    match error {
        SourceDomainServiceError::BrandNotFound => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "Brand not found" })),
        ),
        SourceDomainServiceError::TargetDomainConflict => (
            StatusCode::CONFLICT,
            Json(json!({ "error": "Target already has a different verified official domain" })),
        ),
        SourceDomainServiceError::RegisteredDomainConflict => (
            StatusCode::CONFLICT,
            Json(json!({ "error": "Official domain is already assigned to another brand" })),
        ),
        SourceDomainServiceError::NotFound => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "Verified active source domain not found" })),
        ),
        SourceDomainServiceError::Storage(error) => crate::errors::internal_error(error),
        _ => unreachable!("validation errors returned above"),
    }
}

/// DELETE /api/v1/admin/benefit-source-domains/{id}/approval
pub async fn revoke_source_domain_crawl(
    _admin: AdminUser,
    State(pool): State<PgPool>,
    Path(id): Path<Uuid>,
) -> Result<Json<SourceDomainResponse>, ApiError> {
    let sql = format!(
        "UPDATE benefit_source_domains SET
             crawl_allowed = false,
             compliance_status = 'rejected',
             collection_mode = 'manual_review',
             next_request_at = NULL,
             robots_status = CASE WHEN robots_status = 'blocked' THEN robots_status ELSE 'pending' END,
             compliance_reviewed_at = now(),
             updated_at = now()
         WHERE id = $1
         RETURNING {SOURCE_DOMAIN_RETURNING}"
    );
    let row = sqlx::query_as::<_, SourceDomainResponse>(&sql)
        .bind(id)
        .fetch_optional(&pool)
        .await
        .map_err(crate::errors::internal_error)?
        .ok_or_else(|| bad_request("Source domain not found"))?;
    Ok(Json(row))
}

#[derive(Debug, Deserialize)]
pub struct QueueCandidateInput {
    pub url: String,
    pub discovered_via: String,
    pub discovery_query: Option<String>,
    pub brand_ids: Option<Vec<Uuid>>,
    pub candidate_kind: Option<String>,
}

/// POST /api/v1/admin/benefit-candidates
pub async fn queue_candidate(
    _admin: AdminUser,
    State(pool): State<PgPool>,
    Json(input): Json<QueueCandidateInput>,
) -> Result<Json<Value>, ApiError> {
    if !DISCOVERY_METHODS.contains(&input.discovered_via.as_str()) {
        return Err(bad_request("Invalid discovered_via"));
    }
    let candidate_kind = input
        .candidate_kind
        .unwrap_or_else(|| "unknown".to_string());
    if !matches!(candidate_kind.as_str(), "index" | "detail" | "unknown") {
        return Err(bad_request("Invalid candidate_kind"));
    }
    let discovery_query = input
        .discovery_query
        .as_deref()
        .map(|value| validate_text(value, "discovery_query", 500))
        .transpose()?;
    let (canonical_url, host) = canonicalize_candidate_url(&input.url)?;

    let domain: Option<(Uuid, String, bool, Vec<String>, bool)> = sqlx::query_as(
        "SELECT id, domain, crawl_allowed, allowed_path_prefixes, allow_subdomains
         FROM benefit_source_domains
         WHERE is_active = true
           AND verification_status = 'verified'
           AND (
               $1 = domain
               OR (
                   allow_subdomains = true
                   AND right($1, char_length(domain) + 1) = '.' || domain
               )
           )
         ORDER BY char_length(domain) DESC
         LIMIT 1",
    )
    .bind(&host)
    .fetch_optional(&pool)
    .await
    .map_err(crate::errors::internal_error)?;
    let (domain_id, trusted_domain, crawl_allowed, allowed_paths, _allow_subdomains) = domain
        .ok_or_else(|| bad_request("URL domain is not registered as a verified benefit source"))?;
    if crawl_allowed {
        let candidate_path = reqwest::Url::parse(&canonical_url)
            .map_err(|_| bad_request("Invalid candidate URL"))?
            .path()
            .to_string();
        if !path_is_allowed(&candidate_path, &allowed_paths) {
            return Err(bad_request(
                "URL path is outside the domain's approved crawl scope",
            ));
        }
    }
    let initial_status = if crawl_allowed { "queued" } else { "review" };

    let requested_brand_ids = input.brand_ids.unwrap_or_default();
    if requested_brand_ids.len() > 50 {
        return Err(bad_request("A candidate can link at most 50 brands"));
    }
    let mut seen = HashSet::new();
    let brand_ids: Vec<Uuid> = requested_brand_ids
        .into_iter()
        .filter(|brand_id| seen.insert(*brand_id))
        .collect();
    if !brand_ids.is_empty() {
        let count = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM brands WHERE id = ANY($1::uuid[]) AND is_active = true",
        )
        .bind(&brand_ids)
        .fetch_one(&pool)
        .await
        .map_err(crate::errors::internal_error)?;
        if count != brand_ids.len() as i64 {
            return Err(bad_request("One or more brands do not exist"));
        }
    }

    let mut tx = pool.begin().await.map_err(crate::errors::internal_error)?;
    // Re-queuing a known URL must never rewind a human or monitoring state:
    // a `change_review` hold that reappeared as a fresh `review` row would
    // split the state machine (see program_source_holds).
    let (candidate_id, status) = sqlx::query_as::<_, (Uuid, String)>(
        "INSERT INTO benefit_source_candidates (
             id, domain_id, url, canonical_url, discovered_via, discovery_query,
             status, candidate_kind, discovered_at, last_discovered_at, created_at, updated_at
         ) VALUES (
             $1, $2, $3, $4, $5, $6, $7, $8, now(), now(), now(), now()
         )
         ON CONFLICT (canonical_url) DO UPDATE SET
             domain_id = EXCLUDED.domain_id,
             url = EXCLUDED.url,
             discovered_via = EXCLUDED.discovered_via,
             discovery_query = COALESCE(EXCLUDED.discovery_query, benefit_source_candidates.discovery_query),
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
             last_discovered_at = now(),
             updated_at = now()
         RETURNING id, status",
    )
    .bind(Uuid::new_v4())
    .bind(domain_id)
    .bind(&input.url)
    .bind(&canonical_url)
    .bind(&input.discovered_via)
    .bind(discovery_query)
    .bind(initial_status)
    .bind(&candidate_kind)
    .fetch_one(&mut *tx)
    .await
    .map_err(crate::errors::internal_error)?;

    if !brand_ids.is_empty() {
        sqlx::query(
            "INSERT INTO benefit_candidate_brands (candidate_id, brand_id)
             SELECT $1, item.brand_id
             FROM unnest($2::uuid[]) AS item(brand_id)
             ON CONFLICT (candidate_id, brand_id) DO NOTHING",
        )
        .bind(candidate_id)
        .bind(&brand_ids)
        .execute(&mut *tx)
        .await
        .map_err(crate::errors::internal_error)?;
    }
    sqlx::query(
        "UPDATE benefit_source_domains SET last_discovered_at = now(), updated_at = now()
         WHERE id = $1",
    )
    .bind(domain_id)
    .execute(&mut *tx)
    .await
    .map_err(crate::errors::internal_error)?;
    tx.commit().await.map_err(crate::errors::internal_error)?;

    Ok(Json(json!({
        "id": candidate_id,
        "canonical_url": canonical_url,
        "trusted_domain": trusted_domain,
        "crawl_allowed": crawl_allowed,
        "brand_ids": brand_ids,
        "status": status
    })))
}

#[derive(Debug, Deserialize)]
pub struct DiscoveryTargetQuery {
    pub domain_status: Option<String>,
    pub category: Option<String>,
    pub page: Option<i64>,
    pub per_page: Option<i64>,
}

/// GET /api/v1/admin/brand-discovery-targets
pub async fn list_discovery_targets(
    _admin: AdminUser,
    State(pool): State<PgPool>,
    Query(params): Query<DiscoveryTargetQuery>,
) -> Result<Json<Value>, ApiError> {
    if params
        .domain_status
        .as_deref()
        .is_some_and(|value| !matches!(value, "unresolved" | "candidate" | "verified" | "rejected"))
    {
        return Err(bad_request("Invalid domain_status"));
    }
    let category = params
        .category
        .as_deref()
        .map(|value| validate_text(value, "category", 100))
        .transpose()?;
    let (page, per_page) = validate_pagination(params.page, params.per_page, 100, 200)?;
    let result = brand_discovery::list_targets(
        &pool,
        params.domain_status.as_deref(),
        category.as_deref(),
        page,
        per_page,
    )
    .await
    .map_err(map_brand_discovery_error)?;
    Ok(Json(json!(result)))
}

#[derive(Debug, Deserialize)]
pub struct SourceSuggestionQuery {
    pub status: Option<String>,
    pub page: Option<i64>,
    pub per_page: Option<i64>,
}

/// GET /api/v1/admin/brand-source-suggestions
pub async fn list_source_suggestions(
    _admin: AdminUser,
    State(pool): State<PgPool>,
    Query(params): Query<SourceSuggestionQuery>,
) -> Result<Json<Value>, ApiError> {
    if params
        .status
        .as_deref()
        .is_some_and(|value| !matches!(value, "pending" | "approved" | "rejected"))
    {
        return Err(bad_request("Invalid suggestion status"));
    }
    let (page, per_page) = validate_pagination(params.page, params.per_page, 50, 100)?;
    let result = brand_discovery::list_suggestions(&pool, params.status.as_deref(), page, per_page)
        .await
        .map_err(map_brand_discovery_error)?;
    Ok(Json(json!(result)))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewSourceSuggestionInput {
    pub decision: String,
    pub note: Option<String>,
    /// Requires an explicit reviewer decision when a parent-company domain is
    /// already linked to another brand.
    pub allow_shared_domain: Option<bool>,
}

/// PUT /api/v1/admin/brand-source-suggestions/{id}/review
pub async fn review_source_suggestion(
    admin: AdminUser,
    State(pool): State<PgPool>,
    Path(id): Path<Uuid>,
    Json(input): Json<ReviewSourceSuggestionInput>,
) -> Result<Json<Value>, ApiError> {
    let decision = match input.decision.as_str() {
        "approve" => ReviewDecision::Approve,
        "reject" => ReviewDecision::Reject,
        _ => return Err(bad_request("decision must be approve or reject")),
    };
    let note = input
        .note
        .as_deref()
        .map(|value| validate_text(value, "note", 1_000))
        .transpose()?;
    let command = ReviewSuggestionCommand {
        decision,
        reviewer_id: admin.0.id,
        note,
        allow_shared_domain: input.allow_shared_domain.unwrap_or(false),
    };
    let result = brand_discovery::review_suggestion(&pool, id, command)
        .await
        .map_err(map_brand_discovery_error)?;
    Ok(Json(json!(result)))
}

fn map_brand_discovery_error(error: ServiceError) -> ApiError {
    match error {
        ServiceError::NotFound => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "Source suggestion not found" })),
        ),
        ServiceError::AlreadyReviewed => (
            StatusCode::CONFLICT,
            Json(json!({ "error": "Source suggestion was already reviewed" })),
        ),
        ServiceError::UnsupportedSuggestionKind => {
            bad_request("Only official_homepage suggestions can establish a source domain")
        }
        ServiceError::InvalidUrl => bad_request("Suggestion contains an invalid public HTTPS URL"),
        ServiceError::DomainMismatch => bad_request("Suggestion domain does not match its URL"),
        ServiceError::InvalidPagination => bad_request("Invalid pagination"),
        ServiceError::TargetDomainConflict => (
            StatusCode::CONFLICT,
            Json(json!({ "error": "Target already has a different verified official domain" })),
        ),
        ServiceError::RegisteredDomainConflict => (
            StatusCode::CONFLICT,
            Json(json!({ "error": "Official domain is already assigned to another brand" })),
        ),
        ServiceError::DomainRejected => (
            StatusCode::CONFLICT,
            Json(json!({
                "error": "Official domain was previously rejected; re-register it explicitly through POST /api/v1/admin/benefit-source-domains"
            })),
        ),
        ServiceError::Storage(error) => crate::errors::internal_error(error),
    }
}

#[derive(Debug, Deserialize)]
pub struct CandidateQuery {
    pub status: Option<String>,
    pub page: Option<i64>,
    pub per_page: Option<i64>,
}

#[derive(Debug, Serialize, FromRow)]
pub struct CandidateListItem {
    pub id: Uuid,
    pub url: String,
    pub domain: String,
    pub organization_name: String,
    pub discovered_via: String,
    pub candidate_kind: String,
    pub discovery_depth: i32,
    pub discovery_query: Option<String>,
    pub page_title: Option<String>,
    pub status: String,
    pub extraction_confidence: Option<f32>,
    pub program_id: Option<Uuid>,
    pub previous_content_hash: Option<String>,
    pub attempt_count: i32,
    pub last_error: Option<String>,
    pub next_check_at: Option<DateTime<Utc>>,
    pub last_successful_check_at: Option<DateTime<Utc>>,
    pub consecutive_failures: i32,
    pub brand_ids: Vec<Uuid>,
    pub monitored_program_ids: Vec<Uuid>,
    pub monitor_states: Vec<String>,
    pub monitored_programs: Value,
    pub discovered_at: DateTime<Utc>,
    pub last_checked_at: Option<DateTime<Utc>>,
}

/// GET /api/v1/admin/benefit-candidates
pub async fn list_candidates(
    _admin: AdminUser,
    State(pool): State<PgPool>,
    Query(params): Query<CandidateQuery>,
) -> Result<Json<Value>, ApiError> {
    if params
        .status
        .as_deref()
        .is_some_and(|status| !CANDIDATE_STATUSES.contains(&status))
    {
        return Err(bad_request("Invalid candidate status"));
    }
    let (page, per_page) = validate_pagination(params.page, params.per_page, 50, 100)?;
    let offset = (page - 1) * per_page;

    let total = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM benefit_source_candidates
         WHERE $1::text IS NULL OR status = $1",
    )
    .bind(&params.status)
    .fetch_one(&pool)
    .await
    .map_err(crate::errors::internal_error)?;

    let items = sqlx::query_as::<_, CandidateListItem>(
        "SELECT c.id, c.url, d.domain, d.organization_name, c.discovered_via,
                c.candidate_kind, c.discovery_depth,
                c.discovery_query, c.page_title, c.status, c.extraction_confidence,
                c.program_id, c.previous_content_hash, c.attempt_count, c.last_error,
                c.next_check_at, c.last_successful_check_at, c.consecutive_failures,
                ARRAY(
                    SELECT cb.brand_id
                    FROM benefit_candidate_brands cb
                    WHERE cb.candidate_id = c.id
                    ORDER BY cb.created_at, cb.brand_id
                ) AS brand_ids,
                ARRAY(
                    SELECT monitor.program_id
                    FROM program_source_monitors monitor
                    WHERE monitor.candidate_id = c.id
                    ORDER BY monitor.created_at, monitor.program_id
                ) AS monitored_program_ids,
                ARRAY(
                    SELECT DISTINCT monitor.verification_state
                    FROM program_source_monitors monitor
                    WHERE monitor.candidate_id = c.id
                    ORDER BY monitor.verification_state
                ) AS monitor_states,
                COALESCE((
                    SELECT jsonb_agg(
                        jsonb_build_object(
                            'program_id', program.id,
                            'title', program.title,
                            'summary', program.summary,
                            'provider_name', program.provider_name,
                            'benefit_category', program.benefit_category,
                            'is_active', program.is_active,
                            'program_status', program.program_status,
                            'source_health', program.source_health,
                            'verification_state', monitor.verification_state,
                            'verified_at', monitor.verified_at,
                            'review_due_at', monitor.review_due_at,
                            'change_detected_at', monitor.change_detected_at,
                            'last_error', monitor.last_error
                        )
                        ORDER BY monitor.created_at, program.id
                    )
                    FROM program_source_monitors monitor
                    JOIN programs program ON program.id = monitor.program_id
                    WHERE monitor.candidate_id = c.id
                ), '[]'::jsonb) AS monitored_programs,
                c.discovered_at, c.last_checked_at
         FROM benefit_source_candidates c
         JOIN benefit_source_domains d ON d.id = c.domain_id
         WHERE $1::text IS NULL OR c.status = $1
         ORDER BY c.discovered_at DESC
         LIMIT $2 OFFSET $3",
    )
    .bind(&params.status)
    .bind(per_page)
    .bind(offset)
    .fetch_all(&pool)
    .await
    .map_err(crate::errors::internal_error)?;

    Ok(Json(json!({
        "items": items,
        "total": total,
        "page": page,
        "per_page": per_page,
        "has_next": offset + per_page < total
    })))
}

#[derive(Debug, Serialize, FromRow)]
pub struct CandidateDetail {
    pub id: Uuid,
    pub url: String,
    pub domain: String,
    pub organization_name: String,
    pub discovered_via: String,
    pub candidate_kind: String,
    pub discovery_depth: i32,
    pub parent_candidate_id: Option<Uuid>,
    pub discovery_query: Option<String>,
    pub page_title: Option<String>,
    /// Admin-only, size-limited plain text prepared for factual review.
    pub review_text: Option<String>,
    /// Optional validated machine-generated draft. Never publication authority.
    pub extracted_payload: Option<Value>,
    pub extraction_confidence: Option<f32>,
    pub status: String,
    pub program_id: Option<Uuid>,
    pub previous_content_hash: Option<String>,
    pub attempt_count: i32,
    pub last_error: Option<String>,
    pub next_check_at: Option<DateTime<Utc>>,
    pub last_successful_check_at: Option<DateTime<Utc>>,
    pub consecutive_failures: i32,
    pub brand_ids: Vec<Uuid>,
    pub monitored_program_ids: Vec<Uuid>,
    pub monitor_states: Vec<String>,
    pub monitored_programs: Value,
    pub http_status: Option<i32>,
    pub fetched_bytes: Option<i32>,
    pub content_hash: Option<String>,
    pub discovered_at: DateTime<Utc>,
    pub last_checked_at: Option<DateTime<Utc>>,
}

/// GET /api/v1/admin/benefit-candidates/{id}
pub async fn get_candidate(
    _admin: AdminUser,
    State(pool): State<PgPool>,
    Path(id): Path<Uuid>,
) -> Result<Json<CandidateDetail>, ApiError> {
    let item = sqlx::query_as::<_, CandidateDetail>(
        "SELECT c.id, c.url, d.domain, d.organization_name, c.discovered_via,
                c.candidate_kind, c.discovery_depth, c.parent_candidate_id,
                c.discovery_query, c.page_title, c.raw_text AS review_text,
                c.extracted_payload, c.extraction_confidence,
                c.status, c.program_id, c.previous_content_hash,
                c.attempt_count, c.last_error,
                c.next_check_at, c.last_successful_check_at, c.consecutive_failures,
                ARRAY(
                    SELECT cb.brand_id
                    FROM benefit_candidate_brands cb
                    WHERE cb.candidate_id = c.id
                    ORDER BY cb.created_at, cb.brand_id
                ) AS brand_ids,
                ARRAY(
                    SELECT monitor.program_id
                    FROM program_source_monitors monitor
                    WHERE monitor.candidate_id = c.id
                    ORDER BY monitor.created_at, monitor.program_id
                ) AS monitored_program_ids,
                ARRAY(
                    SELECT DISTINCT monitor.verification_state
                    FROM program_source_monitors monitor
                    WHERE monitor.candidate_id = c.id
                    ORDER BY monitor.verification_state
                ) AS monitor_states,
                COALESCE((
                    SELECT jsonb_agg(
                        jsonb_build_object(
                            'program_id', program.id,
                            'title', program.title,
                            'summary', program.summary,
                            'provider_name', program.provider_name,
                            'benefit_category', program.benefit_category,
                            'is_active', program.is_active,
                            'program_status', program.program_status,
                            'source_health', program.source_health,
                            'verification_state', monitor.verification_state,
                            'verified_at', monitor.verified_at,
                            'review_due_at', monitor.review_due_at,
                            'change_detected_at', monitor.change_detected_at,
                            'last_error', monitor.last_error
                        )
                        ORDER BY monitor.created_at, program.id
                    )
                    FROM program_source_monitors monitor
                    JOIN programs program ON program.id = monitor.program_id
                    WHERE monitor.candidate_id = c.id
                ), '[]'::jsonb) AS monitored_programs,
                c.http_status, c.fetched_bytes, c.content_hash,
                c.discovered_at, c.last_checked_at
         FROM benefit_source_candidates c
         JOIN benefit_source_domains d ON d.id = c.domain_id
         WHERE c.id = $1",
    )
    .bind(id)
    .fetch_optional(&pool)
    .await
    .map_err(crate::errors::internal_error)?
    .ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "Benefit candidate not found" })),
        )
    })?;
    Ok(Json(item))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewCandidateInput {
    /// approve | reject
    pub decision: String,
    /// Required for rejection.
    pub reason: Option<String>,
    /// Required for approval. The summary must be a short factual paraphrase,
    /// not copied source prose.
    pub title: Option<String>,
    pub summary: Option<String>,
    pub provider_name: Option<String>,
    pub benefit_category: Option<String>,
    pub program_status: Option<String>,
    pub application_start_at: Option<DateTime<Utc>>,
    pub application_end_at: Option<DateTime<Utc>>,
    pub deadline_at: Option<DateTime<Utc>>,
    pub benefit_amount_monthly: Option<i32>,
    pub benefit_amount_semester: Option<i32>,
    pub benefit_amount_once: Option<i32>,
    pub min_age: Option<i32>,
    pub max_age: Option<i32>,
    pub regions: Option<Vec<String>>,
    pub tags: Option<Vec<String>>,
    /// Required for approval so publication is always a conscious choice.
    pub publish: Option<bool>,
}

#[derive(Debug, FromRow)]
struct CandidateForReview {
    id: Uuid,
    canonical_url: String,
    status: String,
    organization_name: String,
    domain_brand_id: Option<Uuid>,
    brand_ids: Vec<Uuid>,
    content_hash: Option<String>,
    /// The domain is still a verified, active official source. A domain later
    /// found to be fake must not publish benefits through a stale candidate.
    domain_verified: bool,
    /// Programs with an unresolved publication hold on this source.
    open_hold_program_ids: Vec<Uuid>,
}

fn validate_plain_optional(
    value: Option<String>,
    field: &str,
    max_chars: usize,
) -> Result<Option<String>, ApiError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = validate_text(&value, field, max_chars)?;
    if value.contains('<') || value.contains('>') {
        return Err(bad_request(&format!("{field} must be plain text")));
    }
    Ok(Some(value))
}

fn validate_short_list(
    values: Option<Vec<String>>,
    field: &str,
    max_items: usize,
    max_chars: usize,
) -> Result<Vec<String>, ApiError> {
    let values = values.unwrap_or_default();
    if values.len() > max_items {
        return Err(bad_request(&format!(
            "{field} may contain at most {max_items} values"
        )));
    }
    let mut seen = HashSet::new();
    values
        .into_iter()
        .map(|value| validate_text(&value, field, max_chars))
        .filter(|result| match result {
            Ok(value) => seen.insert(value.clone()),
            Err(_) => true,
        })
        .collect()
}

fn validate_nonnegative(value: Option<i32>, field: &str) -> Result<Option<i32>, ApiError> {
    if value.is_some_and(|value| value < 0) {
        return Err(bad_request(&format!("{field} cannot be negative")));
    }
    Ok(value)
}

fn validate_regions(values: Option<Vec<String>>) -> Result<Vec<String>, ApiError> {
    let values = validate_short_list(values, "region", 17, 100)?;
    let mut seen = HashSet::new();
    let mut regions = Vec::with_capacity(values.len());
    for value in values {
        let canonical = canonical_region_code(&value)
            .ok_or_else(|| bad_request(&format!("Unknown region: {}", value.trim())))?;
        if seen.insert(canonical) {
            regions.push(canonical.to_string());
        }
    }
    regions.sort_unstable();
    Ok(regions)
}

/// PUT /api/v1/admin/benefit-candidates/{id}/review
///
/// Approval creates/updates a normalized corporate benefit from factual fields
/// supplied by a human reviewer. Source excerpts are deleted in the same
/// transaction. Rejection also deletes the excerpt.
pub async fn review_candidate(
    _admin: AdminUser,
    State(pool): State<PgPool>,
    Path(id): Path<Uuid>,
    Json(input): Json<ReviewCandidateInput>,
) -> Result<Json<Value>, ApiError> {
    if !matches!(input.decision.as_str(), "approve" | "reject") {
        return Err(bad_request("decision must be approve or reject"));
    }

    let mut tx = pool.begin().await.map_err(crate::errors::internal_error)?;
    let candidate = sqlx::query_as::<_, CandidateForReview>(
        "SELECT c.id, c.canonical_url, c.status, d.organization_name,
                d.brand_id AS domain_brand_id,
                ARRAY(
                    SELECT cb.brand_id
                    FROM benefit_candidate_brands cb
                    WHERE cb.candidate_id = c.id
                    ORDER BY cb.created_at, cb.brand_id
                ) AS brand_ids,
                c.content_hash,
                (d.verification_status = 'verified' AND d.is_active) AS domain_verified,
                ARRAY(
                    SELECT hold.program_id
                    FROM program_source_holds hold
                    WHERE hold.candidate_id = c.id AND hold.resolved_at IS NULL
                    ORDER BY hold.created_at, hold.program_id
                ) AS open_hold_program_ids
         FROM benefit_source_candidates c
         JOIN benefit_source_domains d ON d.id = c.domain_id
         WHERE c.id = $1
         FOR UPDATE OF c",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(crate::errors::internal_error)?
    .ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "Benefit candidate not found" })),
        )
    })?;

    // A held source belongs to the program-level verification workflow. Approving
    // it here would republish over the hold; rejecting it would leave monitors in
    // change_detected and the hold unresolved forever.
    if !candidate.open_hold_program_ids.is_empty() {
        let paths: Vec<String> = candidate
            .open_hold_program_ids
            .iter()
            .map(|program_id| format!("/api/v1/admin/programs/{program_id}/source-verification"))
            .collect();
        return Err((
            StatusCode::CONFLICT,
            Json(json!({
                "error": "This source has unresolved publication holds; resolve them through PUT /api/v1/admin/programs/{program_id}/source-verification before reviewing the candidate",
                "held_program_ids": candidate.open_hold_program_ids,
                "source_verification_paths": paths
            })),
        ));
    }

    if input.decision == "reject" {
        if candidate.status == "approved" {
            return Err(bad_request(
                "An approved candidate must be unpublished through the program admin workflow",
            ));
        }
        let reason = validate_plain_optional(input.reason, "reason", 1_000)?
            .ok_or_else(|| bad_request("reason is required when rejecting"))?;
        sqlx::query(
            "UPDATE benefit_source_candidates
             SET status = 'rejected', raw_text = NULL, extracted_payload = NULL,
                 last_error = $2, updated_at = now()
             WHERE id = $1",
        )
        .bind(candidate.id)
        .bind(reason)
        .execute(&mut *tx)
        .await
        .map_err(crate::errors::internal_error)?;
        tx.commit().await.map_err(crate::errors::internal_error)?;
        return Ok(Json(json!({ "id": candidate.id, "status": "rejected" })));
    }

    if candidate.status != "review" {
        return Err(bad_request(
            "Only a fetched candidate in review status can be approved",
        ));
    }
    if !candidate.domain_verified {
        return Err((
            StatusCode::CONFLICT,
            Json(json!({
                "error": "The source domain is no longer a verified, active official source; re-verify the domain before approving its candidates"
            })),
        ));
    }
    let title = validate_plain_optional(input.title, "title", 300)?
        .ok_or_else(|| bad_request("title is required when approving"))?;
    let summary = validate_plain_optional(input.summary, "summary", 1_000)?
        .ok_or_else(|| bad_request("summary is required when approving"))?;
    let provider_name = validate_plain_optional(input.provider_name, "provider_name", 200)?
        .unwrap_or_else(|| candidate.organization_name.clone());
    let benefit_category =
        validate_plain_optional(input.benefit_category, "benefit_category", 100)?
            .ok_or_else(|| bad_request("benefit_category is required when approving"))?;
    let recheck_interval_seconds = source_recheck_interval_seconds(&benefit_category);
    let program_status = input.program_status.unwrap_or_else(|| "active".to_string());
    if !matches!(
        program_status.as_str(),
        "active" | "upcoming" | "closed" | "open" | "always_open"
    ) {
        return Err(bad_request("Invalid program_status"));
    }
    let publish = input
        .publish
        .ok_or_else(|| bad_request("publish must be explicitly true or false"))?;
    if matches!(
        (&input.application_start_at, &input.application_end_at),
        (Some(start), Some(end)) if end < start
    ) {
        return Err(bad_request(
            "application_end_at cannot be before application_start_at",
        ));
    }
    if input.min_age.is_some_and(|age| !(0..=100).contains(&age))
        || input.max_age.is_some_and(|age| !(0..=100).contains(&age))
        || input
            .min_age
            .zip(input.max_age)
            .is_some_and(|(min, max)| min > max)
    {
        return Err(bad_request("Invalid min_age/max_age range"));
    }
    // A corporate offer's amounts are no support money (`pays_support`): none
    // is stored, as the worker's publish clears them (review 2026-10-21 H2).
    // The input is still validated.
    let paid = mazimi_core::pays_support("corporate_benefit");
    let monthly = validate_nonnegative(input.benefit_amount_monthly, "benefit_amount_monthly")?
        .filter(|_| paid);
    let semester = validate_nonnegative(input.benefit_amount_semester, "benefit_amount_semester")?
        .filter(|_| paid);
    let once =
        validate_nonnegative(input.benefit_amount_once, "benefit_amount_once")?.filter(|_| paid);
    let regions = validate_regions(input.regions)?;
    let tags = validate_short_list(input.tags, "tag", 30, 100)?;
    let deadline_at = input.deadline_at.or(input.application_end_at);
    let program_id = Uuid::new_v4();

    let program_id = sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO programs (
             id, program_type, source_type, source_id, title, summary,
             provider_name, official_url, program_status,
             application_start_at, application_end_at,
             benefit_amount_monthly, benefit_amount_semester, benefit_amount_once,
             tags, raw_payload, normalized_payload, search_tsv,
             min_age, max_age, regions, deadline_at, is_active, last_synced_at,
             company_name, benefit_category, provider_kind,
             source_health, source_verified_at, source_review_due_at,
             created_at, updated_at
         ) VALUES (
             $1, 'corporate_benefit', 'corporate_official', $2, $3, $4,
             $5, $2, $6, $7, $8, $9, $10, $11,
             $12, NULL, NULL,
             to_tsvector('simple', coalesce($3, '') || ' ' || coalesce($4, '') || ' ' || coalesce($5, '')),
             $13, $14, $15, $16, $17, now(), $5, $18, 'corporate',
             'current', now(), now() + make_interval(secs => $19), now(), now()
         )
         ON CONFLICT (source_type, source_id) DO UPDATE SET
             title = EXCLUDED.title,
             summary = EXCLUDED.summary,
             provider_name = EXCLUDED.provider_name,
             official_url = EXCLUDED.official_url,
             program_status = EXCLUDED.program_status,
             application_start_at = EXCLUDED.application_start_at,
             application_end_at = EXCLUDED.application_end_at,
             benefit_amount_monthly = EXCLUDED.benefit_amount_monthly,
             benefit_amount_semester = EXCLUDED.benefit_amount_semester,
             benefit_amount_once = EXCLUDED.benefit_amount_once,
             tags = EXCLUDED.tags,
             raw_payload = NULL,
             normalized_payload = NULL,
             search_tsv = EXCLUDED.search_tsv,
             min_age = EXCLUDED.min_age,
             max_age = EXCLUDED.max_age,
             regions = EXCLUDED.regions,
             deadline_at = EXCLUDED.deadline_at,
             is_active = EXCLUDED.is_active,
             last_synced_at = now(),
             company_name = EXCLUDED.company_name,
             benefit_category = EXCLUDED.benefit_category,
             provider_kind = 'corporate',
             source_health = 'current',
             source_verified_at = now(),
             source_review_due_at = EXCLUDED.source_review_due_at,
             source_change_detected_at = NULL,
             updated_at = now()
         RETURNING id",
    )
    .bind(program_id)
    .bind(&candidate.canonical_url)
    .bind(&title)
    .bind(&summary)
    .bind(&provider_name)
    .bind(&program_status)
    .bind(input.application_start_at)
    .bind(input.application_end_at)
    .bind(monthly)
    .bind(semester)
    .bind(once)
    .bind(json!(tags))
    .bind(input.min_age)
    .bind(input.max_age)
    .bind(&regions)
    .bind(deadline_at)
    .bind(publish)
    .bind(&benefit_category)
    .bind(recheck_interval_seconds as f64)
    .fetch_one(&mut *tx)
    .await
    .map_err(crate::errors::internal_error)?;
    // Its 기업혜택 chip, stored with the write.
    mazimi_core::corporate_categories::refresh(&mut tx, Some(&[program_id]), 1)
        .await
        .map_err(crate::errors::internal_error)?;

    let mut brand_ids = candidate.brand_ids;
    if let Some(brand_id) = candidate.domain_brand_id {
        brand_ids.insert(0, brand_id);
    }
    let mut seen = HashSet::new();
    brand_ids.retain(|brand_id| seen.insert(*brand_id));
    sqlx::query("DELETE FROM program_brands WHERE program_id = $1")
        .bind(program_id)
        .execute(&mut *tx)
        .await
        .map_err(crate::errors::internal_error)?;
    if !brand_ids.is_empty() {
        sqlx::query(
            "INSERT INTO program_brands (
                 program_id, brand_id, relationship_type, is_primary, created_at
             )
             SELECT $1, item.brand_id, 'redeemable_at', item.ordinality = 1, now()
             FROM unnest($2::uuid[]) WITH ORDINALITY AS item(brand_id, ordinality)",
        )
        .bind(program_id)
        .bind(&brand_ids)
        .execute(&mut *tx)
        .await
        .map_err(crate::errors::internal_error)?;
    }

    sqlx::query(
        "INSERT INTO program_source_monitors (
             program_id, candidate_id, is_primary, verification_state,
             verified_content_hash, verified_at, review_due_at,
             recheck_interval_seconds
         ) VALUES (
             $1, $2, true, 'current', $3, now(),
             now() + make_interval(secs => $4), $4
         )
         ON CONFLICT (program_id, candidate_id) DO UPDATE SET
             verification_state = 'current',
             verified_content_hash = COALESCE(EXCLUDED.verified_content_hash, program_source_monitors.verified_content_hash),
             verified_at = now(), review_due_at = EXCLUDED.review_due_at,
             recheck_interval_seconds = EXCLUDED.recheck_interval_seconds,
             change_detected_at = NULL, last_error = NULL, updated_at = now()",
    )
    .bind(program_id)
    .bind(candidate.id)
    .bind(&candidate.content_hash)
    .bind(recheck_interval_seconds as f64)
    .execute(&mut *tx)
    .await
    .map_err(crate::errors::internal_error)?;

    sqlx::query(
        "UPDATE benefit_source_candidates
         SET status = 'approved', program_id = $2, raw_text = NULL,
             extracted_payload = NULL, previous_content_hash = NULL,
             consecutive_failures = 0, last_error = NULL,
             last_successful_check_at = COALESCE(last_checked_at, now()),
             recheck_interval_seconds = $3,
             next_check_at = now() + make_interval(secs => $3),
             updated_at = now()
         WHERE id = $1",
    )
    .bind(candidate.id)
    .bind(program_id)
    .bind(recheck_interval_seconds as f64)
    .execute(&mut *tx)
    .await
    .map_err(crate::errors::internal_error)?;
    tx.commit().await.map_err(crate::errors::internal_error)?;

    Ok(Json(json!({
        "id": candidate.id,
        "status": "approved",
        "program_id": program_id,
        "published": publish,
        "source_url": candidate.canonical_url,
        "brand_ids": brand_ids
    })))
}

fn source_recheck_interval_seconds(benefit_category: &str) -> i32 {
    match benefit_category {
        "software" | "student_discount" => 7_776_000,
        "membership" | "subscription" | "telecom" | "transport" | "discount" => 2_592_000,
        _ => 5_184_000,
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewProgramSourceInput {
    pub candidate_id: Uuid,
    /// confirmed_unchanged | program_updated | retired
    pub resolution: String,
    pub note: Option<String>,
}

/// PUT /api/v1/admin/programs/{id}/source-verification
pub async fn review_program_source(
    admin: AdminUser,
    State(pool): State<PgPool>,
    Path(program_id): Path<Uuid>,
    Json(input): Json<ReviewProgramSourceInput>,
) -> Result<Json<Value>, ApiError> {
    let resolution = match input.resolution.as_str() {
        "confirmed_unchanged" => SourceReviewResolution::ConfirmedUnchanged,
        "program_updated" => SourceReviewResolution::ProgramUpdated,
        "retired" => SourceReviewResolution::Retired,
        _ => {
            return Err(bad_request(
                "resolution must be confirmed_unchanged, program_updated, or retired",
            ))
        }
    };
    let note = validate_plain_optional(input.note, "note", 1_000)?;
    let outcome = source_reviews::review_program_source(
        &pool,
        SourceReviewCommand {
            program_id,
            candidate_id: input.candidate_id,
            reviewer_id: admin.0.id,
            resolution,
            note,
        },
    )
    .await
    .map_err(map_source_review_error)?;
    Ok(Json(json!(outcome)))
}

const MAX_SOURCE_VERIFICATION_BATCH: usize = 100;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BatchSourceVerificationItem {
    pub program_id: Uuid,
    /// Optional when the program has exactly one active source monitor.
    pub candidate_id: Option<Uuid>,
    /// confirmed_unchanged | program_updated. Retirement stays single-item.
    pub resolution: String,
    /// Required for every item: what was checked and where.
    pub note: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BatchSourceVerificationInput {
    pub items: Vec<BatchSourceVerificationItem>,
}

struct ValidatedBatchItem {
    program_id: Uuid,
    candidate_id: Option<Uuid>,
    resolution: SourceReviewResolution,
    note: String,
}

/// PUT /api/v1/admin/programs/source-verification/batch
///
/// Seeded benefits on non-crawlable domains all come due in the same window.
/// This applies the single-item review service to up to 100 programs, one
/// transaction per program, and reports each outcome. Input is validated in
/// full before the first write so a malformed later item cannot leave the
/// batch half applied for an avoidable reason.
pub async fn review_program_sources_batch(
    admin: AdminUser,
    State(pool): State<PgPool>,
    Json(input): Json<BatchSourceVerificationInput>,
) -> Result<Json<Value>, ApiError> {
    if input.items.is_empty() || input.items.len() > MAX_SOURCE_VERIFICATION_BATCH {
        return Err(bad_request(&format!(
            "items must contain 1 to {MAX_SOURCE_VERIFICATION_BATCH} entries"
        )));
    }
    let mut seen = HashSet::new();
    let mut validated = Vec::with_capacity(input.items.len());
    for (index, item) in input.items.into_iter().enumerate() {
        if !seen.insert(item.program_id) {
            return Err(bad_request(&format!(
                "items[{index}]: program_id appears more than once"
            )));
        }
        let resolution = match item.resolution.as_str() {
            "confirmed_unchanged" => SourceReviewResolution::ConfirmedUnchanged,
            "program_updated" => SourceReviewResolution::ProgramUpdated,
            "retired" => {
                return Err(bad_request(&format!(
                    "items[{index}]: retire sources one at a time through PUT /api/v1/admin/programs/{{id}}/source-verification"
                )))
            }
            _ => {
                return Err(bad_request(&format!(
                    "items[{index}]: resolution must be confirmed_unchanged or program_updated"
                )))
            }
        };
        let note = validate_plain_optional(item.note, &format!("items[{index}].note"), 1_000)?
            .ok_or_else(|| bad_request(&format!("items[{index}]: note is required")))?;
        validated.push(ValidatedBatchItem {
            program_id: item.program_id,
            candidate_id: item.candidate_id,
            resolution,
            note,
        });
    }

    let mut results = Vec::with_capacity(validated.len());
    let mut succeeded = 0usize;
    let mut failed = 0usize;
    for item in validated {
        let candidate_id = match item.candidate_id {
            Some(candidate_id) => candidate_id,
            None => {
                let monitors = match sqlx::query_scalar::<_, Uuid>(
                    "SELECT candidate_id FROM program_source_monitors
                     WHERE program_id = $1 AND verification_state <> 'retired'
                     ORDER BY is_primary DESC, created_at",
                )
                .bind(item.program_id)
                .fetch_all(&pool)
                .await
                {
                    Ok(monitors) => monitors,
                    Err(error) => {
                        // Same per-item policy as a failed review below: earlier
                        // items are committed, so this one is reported, not the
                        // whole batch.
                        failed += 1;
                        tracing::error!(program_id = %item.program_id, error = %error, "batch source verification monitor lookup failed");
                        results.push(json!({
                            "program_id": item.program_id,
                            "status": "error",
                            "http_status": 500,
                            "error": "Internal server error"
                        }));
                        continue;
                    }
                };
                match monitors.as_slice() {
                    [only] => *only,
                    [] => {
                        failed += 1;
                        results.push(json!({
                            "program_id": item.program_id,
                            "status": "error",
                            "http_status": 404,
                            "error": "Program has no active source monitor"
                        }));
                        continue;
                    }
                    _ => {
                        failed += 1;
                        results.push(json!({
                            "program_id": item.program_id,
                            "status": "error",
                            "http_status": 400,
                            "error": "Program has several source monitors; candidate_id is required",
                            "candidate_ids": monitors
                        }));
                        continue;
                    }
                }
            }
        };
        match source_reviews::review_program_source(
            &pool,
            SourceReviewCommand {
                program_id: item.program_id,
                candidate_id,
                reviewer_id: admin.0.id,
                resolution: item.resolution,
                note: Some(item.note),
            },
        )
        .await
        {
            Ok(outcome) => {
                succeeded += 1;
                results.push(json!({
                    "program_id": item.program_id,
                    "candidate_id": candidate_id,
                    "status": "ok",
                    "outcome": outcome
                }));
            }
            Err(error) => {
                failed += 1;
                // Earlier items are already committed, so an internal failure is
                // reported per item (generic message, details in the log) instead
                // of discarding the whole batch response.
                if matches!(
                    error,
                    SourceReviewServiceError::Storage(_)
                        | SourceReviewServiceError::OperationalEvent(_)
                ) {
                    tracing::error!(program_id = %item.program_id, candidate_id = %candidate_id, error = %error, "batch source verification item failed");
                    results.push(json!({
                        "program_id": item.program_id,
                        "candidate_id": candidate_id,
                        "status": "error",
                        "http_status": 500,
                        "error": "Internal server error"
                    }));
                } else {
                    let (status, body) = map_source_review_error(error);
                    results.push(json!({
                        "program_id": item.program_id,
                        "candidate_id": candidate_id,
                        "status": "error",
                        "http_status": status.as_u16(),
                        "error": body.0["error"]
                    }));
                }
            }
        }
    }

    Ok(Json(json!({
        "succeeded": succeeded,
        "failed": failed,
        "results": results
    })))
}

fn map_source_review_error(error: SourceReviewServiceError) -> ApiError {
    match error {
        SourceReviewServiceError::NotFound => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "Program source monitor not found" })),
        ),
        SourceReviewServiceError::InvalidState => (
            StatusCode::CONFLICT,
            Json(json!({ "error": "Source monitor is not in a reviewable state" })),
        ),
        SourceReviewServiceError::RetirementNoteRequired => {
            bad_request("note is required when retiring a source")
        }
        SourceReviewServiceError::PublisherOptOut => (
            StatusCode::CONFLICT,
            Json(
                json!({ "error": "This offer is held by a publisher opt-out; lift the opt-out instead" }),
            ),
        ),
        SourceReviewServiceError::Storage(error) => crate::errors::internal_error(error),
        SourceReviewServiceError::OperationalEvent(error) => crate::errors::internal_error(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_domain_without_widening_host_scope() {
        assert_eq!(
            normalize_domain("https://www.Example.COM/").unwrap(),
            "www.example.com"
        );
    }

    #[test]
    fn candidate_url_drops_fragment() {
        let (url, host) =
            canonicalize_candidate_url("https://events.example.com/youth#detail").unwrap();
        assert_eq!(url, "https://events.example.com/youth");
        assert_eq!(host, "events.example.com");
    }

    #[test]
    fn rejects_local_and_ip_targets() {
        assert!(normalize_domain("localhost").is_err());
        assert!(canonicalize_candidate_url("http://127.0.0.1/admin").is_err());
        assert!(canonicalize_candidate_url("file:///etc/passwd").is_err());
        assert!(canonicalize_candidate_url("http://example.com/benefit").is_err());
    }

    #[test]
    fn approved_paths_do_not_match_sibling_prefixes() {
        let paths = vec!["/events".to_string(), "/benefits/".to_string()];
        assert!(path_is_allowed("/events", &paths));
        assert!(path_is_allowed("/events/youth", &paths));
        assert!(path_is_allowed("/benefits/2026", &paths));
        assert!(!path_is_allowed("/eventstore/admin", &paths));
        assert!(!path_is_allowed("/about", &paths));
    }
}
