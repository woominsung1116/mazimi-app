use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use mazimi_core::{canonical_region_code, models::Program};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::PgPool;
use std::collections::HashSet;
use uuid::Uuid;

use crate::auth::AdminUser;

type ApiError = (StatusCode, Json<Value>);

const PROGRAM_STATUSES: &[&str] = &["active", "upcoming", "closed", "open", "always_open"];

fn bad_request(message: impl Into<String>) -> ApiError {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "error": message.into() })),
    )
}

fn validate_plain_text(value: String, field: &str, max_chars: usize) -> Result<String, ApiError> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > max_chars {
        return Err(bad_request(format!(
            "{field} must be between 1 and {max_chars} characters"
        )));
    }
    if value.contains('<') || value.contains('>') {
        return Err(bad_request(format!("{field} must be plain text")));
    }
    Ok(value.to_string())
}

fn validate_optional_plain_text(
    value: Option<String>,
    field: &str,
    max_chars: usize,
) -> Result<Option<String>, ApiError> {
    value
        .map(|value| validate_plain_text(value, field, max_chars))
        .transpose()
}

/// `official_url` is opened by both clients and shown as the program's
/// official notice, so only a credential-free public HTTPS URL is accepted.
/// The trimmed original is stored: the canonicalizer validates but would drop
/// SPA fragments and re-encode EUC-KR query bytes (code review M5).
fn validate_official_url(value: Option<String>) -> Result<Option<String>, ApiError> {
    let Some(value) = validate_optional_plain_text(value, "official_url", 2_000)? else {
        return Ok(None);
    };
    // `validate_plain_text` already trimmed it.
    mazimi_core::source_policy::canonicalize_public_https_url(&value)
        .map(|_| Some(value))
        .map_err(|_| bad_request("official_url must be a public HTTPS URL"))
}

fn validate_program_status(value: String) -> Result<String, ApiError> {
    let value = validate_plain_text(value, "program_status", 50)?;
    if !PROGRAM_STATUSES.contains(&value.as_str()) {
        return Err(bad_request("Invalid program_status"));
    }
    Ok(value)
}

/// `program_type` is a category key compared exactly by the public list
/// filter and both clients' tabs, so only the canonical list is accepted.
fn validate_program_type(value: String) -> Result<String, ApiError> {
    let value = validate_plain_text(value, "program_type", 100)?;
    if !mazimi_core::is_program_type(&value) {
        let allowed: Vec<&str> = mazimi_core::PROGRAM_TYPES
            .iter()
            .map(|(key, _)| *key)
            .collect();
        return Err(bad_request(format!(
            "Unknown program_type; allowed: {}",
            allowed.join(", ")
        )));
    }
    Ok(value)
}

fn parse_optional_timestamp(
    value: Option<&str>,
    field: &str,
) -> Result<Option<chrono::DateTime<chrono::Utc>>, ApiError> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.trim().is_empty() {
        return Ok(None);
    }
    chrono::DateTime::parse_from_rfc3339(value.trim())
        .map(|date| Some(date.with_timezone(&chrono::Utc)))
        .map_err(|_| bad_request(format!("{field} must be an RFC 3339 timestamp")))
}

fn merge_timestamp(
    value: Option<&str>,
    current: Option<chrono::DateTime<chrono::Utc>>,
    field: &str,
) -> Result<Option<chrono::DateTime<chrono::Utc>>, ApiError> {
    match value {
        None => Ok(current),
        Some(value) if value.trim().is_empty() => Ok(current),
        Some(value) => parse_optional_timestamp(Some(value), field),
    }
}

fn validate_date_range(
    start: Option<&chrono::DateTime<chrono::Utc>>,
    end: Option<&chrono::DateTime<chrono::Utc>>,
) -> Result<(), ApiError> {
    if matches!((start, end), (Some(start), Some(end)) if end < start) {
        return Err(bad_request(
            "application_end_at cannot be before application_start_at",
        ));
    }
    Ok(())
}

fn validate_age_range(min_age: Option<i32>, max_age: Option<i32>) -> Result<(), ApiError> {
    if min_age.is_some_and(|age| !(0..=100).contains(&age))
        || max_age.is_some_and(|age| !(0..=100).contains(&age))
        || min_age
            .zip(max_age)
            .is_some_and(|(minimum, maximum)| minimum > maximum)
    {
        return Err(bad_request("Invalid min_age/max_age range"));
    }
    Ok(())
}

fn validate_nonnegative_amount(value: Option<i32>, field: &str) -> Result<(), ApiError> {
    if value.is_some_and(|value| value < 0) {
        return Err(bad_request(format!("{field} cannot be negative")));
    }
    Ok(())
}

fn canonicalize_regions(values: Option<Vec<String>>) -> Result<Option<Vec<String>>, ApiError> {
    let Some(values) = values else {
        return Ok(None);
    };
    if values.len() > 17 {
        return Err(bad_request("regions may contain at most 17 values"));
    }

    let mut seen = HashSet::new();
    let mut regions = Vec::with_capacity(values.len());
    for value in values {
        let canonical = canonical_region_code(&value)
            .ok_or_else(|| bad_request(format!("Unknown region: {}", value.trim())))?;
        if seen.insert(canonical) {
            regions.push(canonical.to_string());
        }
    }
    regions.sort_unstable();
    Ok(Some(regions))
}

/// POST /api/v1/admin/sync
pub async fn trigger_sync(_admin: AdminUser, State(pool): State<PgPool>) -> Json<Value> {
    tokio::spawn(async move {
        tracing::info!("manual sync triggered via admin API");

        match mazimi_worker::sources::youth_center::YouthCenterSource::from_env() {
            Ok(src) => {
                if let Err(e) = mazimi_worker::pipeline::run_ingestion(&pool, &src).await {
                    tracing::error!(source = "youth_center", error = %e, "ingestion failed");
                }
            }
            Err(e) => {
                tracing::warn!(source = "youth_center", error = %e, "source skipped (env not set)");
            }
        }

        match mazimi_worker::sources::gov_benefits::GovBenefitsSource::from_env() {
            Ok(src) => {
                if let Err(e) = mazimi_worker::pipeline::run_ingestion(&pool, &src).await {
                    tracing::error!(source = "gov_benefits", error = %e, "ingestion failed");
                }
            }
            Err(e) => {
                tracing::warn!(source = "gov_benefits", error = %e, "source skipped (env not set)");
            }
        }

        match mazimi_worker::sources::scholarship::ScholarshipSource::from_env() {
            Ok(src) => {
                if let Err(e) = mazimi_worker::pipeline::run_ingestion(&pool, &src).await {
                    tracing::error!(source = "scholarship", error = %e, "ingestion failed");
                }
            }
            Err(e) => {
                tracing::warn!(source = "scholarship", error = %e, "source skipped (env not set)");
            }
        }

        match mazimi_worker::sources::brand_catalog::FtcBrandCatalogSource::from_env() {
            Ok(src) => {
                if let Err(e) =
                    mazimi_worker::sources::brand_catalog::run_brand_catalog_ingestion(&pool, &src)
                        .await
                {
                    tracing::error!(source = "ftc_brand_catalog", error = %e, "ingestion failed");
                }
            }
            Err(e) => {
                tracing::warn!(source = "ftc_brand_catalog", error = %e, "source skipped (env not set)");
            }
        }

        tracing::info!("manual sync complete");
    });

    Json(json!({
        "status": "sync_started",
        "message": "동기화 작업이 시작되었습니다"
    }))
}

#[derive(Debug, Deserialize)]
pub struct Pagination {
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreateProgramInput {
    pub program_type: String,
    pub source_type: Option<String>,
    pub title: String,
    pub summary: Option<String>,
    pub provider_name: Option<String>,
    pub official_url: Option<String>,
    pub program_status: Option<String>,
    pub application_start_at: Option<String>,
    pub application_end_at: Option<String>,
    pub benefit_amount_monthly: Option<i32>,
    pub benefit_amount_semester: Option<i32>,
    pub benefit_amount_once: Option<i32>,
    pub min_age: Option<i32>,
    pub max_age: Option<i32>,
    pub regions: Option<Vec<String>>,
    pub deadline_at: Option<String>,
    /// Validity end of a corporate/campus offer; part of the effective deadline.
    pub benefit_valid_until: Option<String>,
    pub is_active: Option<bool>,
    pub company_name: Option<String>,
    pub benefit_category: Option<String>,
    pub provider_kind: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateProgramInput {
    pub program_type: Option<String>,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub provider_name: Option<String>,
    pub official_url: Option<String>,
    pub program_status: Option<String>,
    pub application_start_at: Option<String>,
    pub application_end_at: Option<String>,
    pub benefit_amount_monthly: Option<i32>,
    pub benefit_amount_semester: Option<i32>,
    pub benefit_amount_once: Option<i32>,
    pub min_age: Option<i32>,
    pub max_age: Option<i32>,
    pub regions: Option<Vec<String>>,
    pub deadline_at: Option<String>,
    pub benefit_valid_until: Option<String>,
    pub is_active: Option<bool>,
    pub company_name: Option<String>,
    pub benefit_category: Option<String>,
    pub provider_kind: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SetProgramBrandsInput {
    pub brand_ids: Vec<Uuid>,
}

const PROVIDER_KINDS: &[&str] = &[
    "government",
    "municipality",
    "university",
    "corporate",
    "private_foundation",
    "nonprofit",
    "public_agency",
];

fn validate_provider_kind(kind: Option<&str>) -> Result<(), ApiError> {
    if kind.is_some_and(|value| !PROVIDER_KINDS.contains(&value)) {
        return Err(bad_request("Invalid provider_kind"));
    }
    Ok(())
}

/// Published corporate benefits must come from a source that keeps them under
/// re-verification: the monitored official-page workflow or a licensed feed.
fn corporate_source_allows_publication(program_type: &str, source_type: &str) -> bool {
    program_type != "corporate_benefit"
        || matches!(source_type, "corporate_official" | "corporate_feed")
}

/// GET /api/v1/admin/programs
pub async fn list_admin_programs(
    _admin: AdminUser,
    State(pool): State<PgPool>,
    Query(pagination): Query<Pagination>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let limit = pagination.limit.unwrap_or(50).clamp(1, 100);
    let offset = pagination.offset.unwrap_or(0).max(0);

    let total: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM programs")
        .fetch_one(&pool)
        .await
        .map_err(crate::errors::internal_error)?;

    let programs = sqlx::query_as::<_, Program>(
        "SELECT * FROM programs ORDER BY created_at DESC LIMIT $1 OFFSET $2",
    )
    .bind(limit)
    .bind(offset)
    .fetch_all(&pool)
    .await
    .map_err(crate::errors::internal_error)?;

    Ok(Json(json!({
        "total": total.0,
        "limit": limit,
        "offset": offset,
        "items": programs
    })))
}

/// GET /api/v1/admin/programs/{id}
///
/// Same response shape as the public `GET /api/v1/programs/{id}`
/// (`routes::programs::ProgramDetail`), but read from `programs` without the
/// `is_active`/expiry filter so operators can open and republish a hidden or
/// expired listing. The sub-queries are shared with the public handler
/// through `programs::load_program_detail`.
pub async fn get_admin_program(
    _admin: AdminUser,
    State(pool): State<PgPool>,
    Path(id): Path<Uuid>,
) -> Result<Json<crate::routes::programs::ProgramDetail>, ApiError> {
    let program = sqlx::query_as::<_, Program>("SELECT * FROM programs WHERE id = $1")
        .bind(id)
        .fetch_optional(&pool)
        .await
        .map_err(crate::errors::internal_error)?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": "Program not found" })),
            )
        })?;

    crate::routes::programs::load_program_detail(&pool, program)
        .await
        .map(Json)
        .map_err(crate::errors::internal_error)
}

/// POST /api/v1/admin/programs
pub async fn create_program(
    _admin: AdminUser,
    State(pool): State<PgPool>,
    Json(input): Json<CreateProgramInput>,
) -> Result<Json<Program>, ApiError> {
    let program_type = validate_program_type(input.program_type)?;
    let source_type = validate_optional_plain_text(input.source_type, "source_type", 100)?
        .unwrap_or_else(|| "manual".to_string());
    let title = validate_plain_text(input.title, "title", 300)?;
    let summary = validate_optional_plain_text(input.summary, "summary", 5_000)?;
    let provider_name = validate_optional_plain_text(input.provider_name, "provider_name", 200)?;
    let official_url = validate_official_url(input.official_url)?;
    let program_status =
        validate_program_status(input.program_status.unwrap_or_else(|| "open".to_string()))?;
    let company_name = validate_optional_plain_text(input.company_name, "company_name", 200)?;
    let benefit_category =
        validate_optional_plain_text(input.benefit_category, "benefit_category", 100)?;
    let provider_kind = validate_optional_plain_text(input.provider_kind, "provider_kind", 50)?;
    validate_provider_kind(provider_kind.as_deref())?;

    validate_nonnegative_amount(input.benefit_amount_monthly, "benefit_amount_monthly")?;
    validate_nonnegative_amount(input.benefit_amount_semester, "benefit_amount_semester")?;
    validate_nonnegative_amount(input.benefit_amount_once, "benefit_amount_once")?;
    validate_age_range(input.min_age, input.max_age)?;
    let regions = canonicalize_regions(input.regions)?;

    let application_start_at = parse_optional_timestamp(
        input.application_start_at.as_deref(),
        "application_start_at",
    )?;
    let application_end_at =
        parse_optional_timestamp(input.application_end_at.as_deref(), "application_end_at")?;
    let deadline_at = parse_optional_timestamp(input.deadline_at.as_deref(), "deadline_at")?;
    let benefit_valid_until =
        parse_optional_timestamp(input.benefit_valid_until.as_deref(), "benefit_valid_until")?;
    validate_date_range(application_start_at.as_ref(), application_end_at.as_ref())?;

    let is_active = input.is_active.unwrap_or(false);
    if source_type == "corporate_official" && is_active {
        return Err((
            StatusCode::CONFLICT,
            Json(json!({
                "error": "Corporate official benefits must be published through the verified source review workflow"
            })),
        ));
    }
    // A corporate benefit entered as `manual` would publish as `unmonitored`
    // and never enter re-verification. Only a monitored official source or a
    // licensed feed may carry a published corporate benefit.
    if is_active && !corporate_source_allows_publication(&program_type, &source_type) {
        return Err(bad_request(
            "Corporate benefits can only be published from a monitored official source (corporate_official) or a licensed feed (corporate_feed)",
        ));
    }

    let program = sqlx::query_as::<_, Program>(
        r#"
        INSERT INTO programs (
            id, program_type, source_type, title, summary, provider_name,
            official_url, program_status, application_start_at, application_end_at,
            benefit_amount_monthly, benefit_amount_semester, benefit_amount_once,
            min_age, max_age, regions, deadline_at, is_active,
            company_name, benefit_category, provider_kind, benefit_valid_until,
            admin_official_url, created_at, updated_at
        ) VALUES (
            gen_random_uuid(), $1, $2, $3, $4, $5, $6, $7, $8, $9,
            $10, $11, $12, $13, $14, $15, $16, $17,
            $18, $19, $20, $21,
            $6, NOW(), NOW()
        )
        RETURNING *
        "#,
    )
    .bind(&program_type)
    .bind(&source_type)
    .bind(&title)
    .bind(&summary)
    .bind(&provider_name)
    .bind(&official_url)
    .bind(program_status)
    .bind(application_start_at)
    .bind(application_end_at)
    .bind(input.benefit_amount_monthly)
    .bind(input.benefit_amount_semester)
    .bind(input.benefit_amount_once)
    .bind(input.min_age)
    .bind(input.max_age)
    .bind(regions.as_deref())
    .bind(deadline_at)
    .bind(is_active)
    .bind(&company_name)
    .bind(&benefit_category)
    .bind(&provider_kind)
    .bind(benefit_valid_until)
    .fetch_one(&pool)
    .await
    .map_err(crate::errors::internal_error)?;
    // Its 기업혜택 chip (none off the tab).
    let mut connection = pool
        .acquire()
        .await
        .map_err(crate::errors::internal_error)?;
    mazimi_core::corporate_categories::refresh(&mut connection, Some(&[program.id]), 1)
        .await
        .map_err(crate::errors::internal_error)?;

    Ok(Json(program))
}

/// PUT /api/v1/admin/programs/{id}
pub async fn update_program(
    admin: AdminUser,
    State(pool): State<PgPool>,
    Path(id): Path<Uuid>,
    Json(input): Json<UpdateProgramInput>,
) -> Result<Json<Program>, ApiError> {
    let existing = sqlx::query_as::<_, Program>("SELECT * FROM programs WHERE id = $1")
        .bind(id)
        .fetch_optional(&pool)
        .await
        .map_err(crate::errors::internal_error)?;

    let existing = match existing {
        Some(p) => p,
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(json!({ "error": "Program not found" })),
            ))
        }
    };

    let source_health =
        sqlx::query_scalar::<_, String>("SELECT source_health FROM programs WHERE id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .map_err(crate::errors::internal_error)?;

    let source_type = existing.source_type.clone();
    let program_type = match input.program_type {
        Some(value) => validate_program_type(value)?,
        None => existing.program_type,
    };
    let title = match input.title {
        Some(value) => validate_plain_text(value, "title", 300)?,
        None => existing.title,
    };
    let summary = match input.summary {
        Some(value) => validate_optional_plain_text(Some(value), "summary", 5_000)?,
        None => existing.summary,
    };
    let provider_name = match input.provider_name {
        Some(value) => validate_optional_plain_text(Some(value), "provider_name", 200)?,
        None => existing.provider_name,
    };
    let official_url = match input.official_url {
        Some(value) => validate_official_url(Some(value))?,
        None => existing.official_url.clone(),
    };
    if official_url != existing.official_url {
        // Audit trail: an admin-written official_url stops binding verified
        // application destinations (see `admin_official_url`).
        tracing::warn!(
            program_id = %id,
            admin_id = %admin.0.id,
            previous = existing.official_url.as_deref().unwrap_or(""),
            next = official_url.as_deref().unwrap_or(""),
            "admin changed program official_url"
        );
    }
    let program_status = match input.program_status {
        Some(value) => validate_program_status(value)?,
        None => existing.program_status,
    };
    let benefit_amount_monthly = input
        .benefit_amount_monthly
        .or(existing.benefit_amount_monthly);
    let benefit_amount_semester = input
        .benefit_amount_semester
        .or(existing.benefit_amount_semester);
    let benefit_amount_once = input.benefit_amount_once.or(existing.benefit_amount_once);
    let min_age = input.min_age.or(existing.min_age);
    let max_age = input.max_age.or(existing.max_age);
    validate_nonnegative_amount(benefit_amount_monthly, "benefit_amount_monthly")?;
    validate_nonnegative_amount(benefit_amount_semester, "benefit_amount_semester")?;
    validate_nonnegative_amount(benefit_amount_once, "benefit_amount_once")?;
    validate_age_range(min_age, max_age)?;
    let regions = match input.regions {
        Some(values) => canonicalize_regions(Some(values))?,
        None => existing.regions,
    };
    let is_active = input.is_active.unwrap_or(existing.is_active);
    if is_active && source_type == "corporate_official" && source_health != "current" {
        return Err((
            StatusCode::CONFLICT,
            Json(json!({
                "error": "Resolve the source verification hold before publishing this benefit"
            })),
        ));
    }
    if is_active && !corporate_source_allows_publication(&program_type, &source_type) {
        let monitored = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (
                 SELECT 1 FROM program_source_monitors
                 WHERE program_id = $1 AND verification_state = 'current'
             )",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .map_err(crate::errors::internal_error)?;
        if !monitored {
            return Err(bad_request(
                "Corporate benefits can only be published from a monitored official source (corporate_official) or a licensed feed (corporate_feed); attach a current source monitor first",
            ));
        }
    }
    let company_name = match input.company_name {
        Some(value) => validate_optional_plain_text(Some(value), "company_name", 200)?,
        None => existing.company_name,
    };
    let benefit_category = match input.benefit_category {
        Some(value) => validate_optional_plain_text(Some(value), "benefit_category", 100)?,
        None => existing.benefit_category,
    };
    let provider_kind = match input.provider_kind {
        Some(value) => validate_optional_plain_text(Some(value), "provider_kind", 50)?,
        None => existing.provider_kind,
    };
    validate_provider_kind(provider_kind.as_deref())?;

    let application_start_at = merge_timestamp(
        input.application_start_at.as_deref(),
        existing.application_start_at,
        "application_start_at",
    )?;
    let application_end_at = merge_timestamp(
        input.application_end_at.as_deref(),
        existing.application_end_at,
        "application_end_at",
    )?;
    let deadline_at = merge_timestamp(
        input.deadline_at.as_deref(),
        existing.deadline_at,
        "deadline_at",
    )?;
    let benefit_valid_until = merge_timestamp(
        input.benefit_valid_until.as_deref(),
        existing.benefit_valid_until,
        "benefit_valid_until",
    )?;
    validate_date_range(application_start_at.as_ref(), application_end_at.as_ref())?;

    let mut tx = pool.begin().await.map_err(crate::errors::internal_error)?;
    // Reactivating clears the retirement record so a later fetch does not
    // read the row as still expired; deactivating here is an operator hold
    // (`manual`), which automatic lifecycle never reverses. An unchanged
    // `is_active` leaves both fields alone.
    let program = sqlx::query_as::<_, Program>(
        r#"
        UPDATE programs SET
            program_type = $1, title = $2, summary = $3, provider_name = $4,
            official_url = $5, program_status = $6, application_start_at = $7,
            application_end_at = $8, benefit_amount_monthly = $9,
            benefit_amount_semester = $10, benefit_amount_once = $11,
            min_age = $12, max_age = $13, regions = $14, deadline_at = $15,
            is_active = $16, company_name = $17, benefit_category = $18,
            provider_kind = $19, benefit_valid_until = $20,
            admin_official_url = CASE
                WHEN $5 IS DISTINCT FROM programs.official_url THEN $5
                ELSE admin_official_url END,
            retired_at = CASE
                WHEN $16 THEN NULL
                WHEN programs.is_active THEN NOW()
                ELSE retired_at END,
            retirement_reason = CASE
                WHEN $16 THEN NULL
                WHEN programs.is_active THEN 'manual'
                ELSE retirement_reason END,
            updated_at = NOW()
        WHERE id = $21
        RETURNING *
        "#,
    )
    .bind(&program_type)
    .bind(&title)
    .bind(&summary)
    .bind(&provider_name)
    .bind(&official_url)
    .bind(program_status)
    .bind(application_start_at)
    .bind(application_end_at)
    .bind(benefit_amount_monthly)
    .bind(benefit_amount_semester)
    .bind(benefit_amount_once)
    .bind(min_age)
    .bind(max_age)
    .bind(regions.as_deref())
    .bind(deadline_at)
    .bind(is_active)
    .bind(&company_name)
    .bind(&benefit_category)
    .bind(&provider_kind)
    .bind(benefit_valid_until)
    .bind(id)
    .fetch_one(&mut *tx)
    .await
    .map_err(crate::errors::internal_error)?;

    // Same transaction: a manual edit supersedes an approved AI value for that column; the
    // ingestion upsert must not restore the AI value over the edit.
    sqlx::query(
        "DELETE FROM program_ai_field_overrides o USING programs p
         WHERE o.program_id = p.id AND p.id = $1 AND (
             (o.field = 'min_age' AND o.int_value IS DISTINCT FROM p.min_age)
             OR (o.field = 'max_age' AND o.int_value IS DISTINCT FROM p.max_age)
             OR (o.field = 'application_start_at' AND o.timestamp_value IS DISTINCT FROM p.application_start_at)
             OR (o.field = 'application_end_at' AND o.timestamp_value IS DISTINCT FROM p.application_end_at))",
    )
    .bind(id)
    .execute(&mut *tx)
    .await
    .map_err(crate::errors::internal_error)?;
    // Title, provider or category may have changed: its 기업혜택 chip too.
    mazimi_core::corporate_categories::refresh(&mut tx, Some(&[id]), 1)
        .await
        .map_err(crate::errors::internal_error)?;
    tx.commit().await.map_err(crate::errors::internal_error)?;

    Ok(Json(program))
}

/// PUT /api/v1/admin/programs/{id}/brands
///
/// Replaces the redeemable brand set atomically. The first brand is marked as
/// primary so clients have a deterministic logo/name to display.
pub async fn set_program_brands(
    _admin: AdminUser,
    State(pool): State<PgPool>,
    Path(id): Path<Uuid>,
    Json(input): Json<SetProgramBrandsInput>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if input.brand_ids.len() > 50 {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "A program can link at most 50 brands" })),
        ));
    }

    let mut seen = HashSet::new();
    let brand_ids: Vec<Uuid> = input
        .brand_ids
        .into_iter()
        .filter(|brand_id| seen.insert(*brand_id))
        .collect();

    let program_exists =
        sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM programs WHERE id = $1)")
            .bind(id)
            .fetch_one(&pool)
            .await
            .map_err(crate::errors::internal_error)?;
    if !program_exists {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "Program not found" })),
        ));
    }

    if !brand_ids.is_empty() {
        let existing_count = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM brands WHERE id = ANY($1::uuid[]) AND is_active = true",
        )
        .bind(&brand_ids)
        .fetch_one(&pool)
        .await
        .map_err(crate::errors::internal_error)?;
        if existing_count != brand_ids.len() as i64 {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "One or more brands do not exist" })),
            ));
        }
    }

    let mut tx = pool.begin().await.map_err(crate::errors::internal_error)?;
    sqlx::query("DELETE FROM program_brands WHERE program_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(crate::errors::internal_error)?;

    if !brand_ids.is_empty() {
        sqlx::query(
            "INSERT INTO program_brands (program_id, brand_id, relationship_type, is_primary)
             SELECT $1, item.brand_id, 'redeemable_at', item.ordinality = 1
             FROM unnest($2::uuid[]) WITH ORDINALITY AS item(brand_id, ordinality)",
        )
        .bind(id)
        .bind(&brand_ids)
        .execute(&mut *tx)
        .await
        .map_err(crate::errors::internal_error)?;
    }

    tx.commit().await.map_err(crate::errors::internal_error)?;

    Ok(Json(json!({
        "program_id": id,
        "brand_ids": brand_ids
    })))
}

/// POST /api/v1/admin/programs/{id}/publish
pub async fn toggle_publish(
    _admin: AdminUser,
    State(pool): State<PgPool>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let program = sqlx::query_as::<_, (bool, String, String)>(
        "SELECT is_active, source_type, source_health FROM programs WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&pool)
    .await
    .map_err(crate::errors::internal_error)?;

    let program = match program {
        Some(p) => p,
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(json!({ "error": "Program not found" })),
            ))
        }
    };

    let new_active = !program.0;
    if new_active && program.1 == "corporate_official" && program.2 != "current" {
        return Err((
            StatusCode::CONFLICT,
            Json(json!({
                "error": "Resolve the source verification hold before publishing this benefit"
            })),
        ));
    }

    // Publishing clears any earlier retirement; unpublishing is an operator
    // hold (`manual`) that automatic lifecycle never reverses.
    sqlx::query(
        "UPDATE programs SET is_active = $1, \
             retired_at = CASE WHEN $1 THEN NULL ELSE NOW() END, \
             retirement_reason = CASE WHEN $1 THEN NULL ELSE 'manual' END, \
             updated_at = NOW() \
         WHERE id = $2",
    )
    .bind(new_active)
    .bind(id)
    .execute(&pool)
    .await
    .map_err(crate::errors::internal_error)?;

    Ok(Json(json!({
        "id": id,
        "is_active": new_active
    })))
}

/// GET /api/v1/admin/stats
pub async fn get_stats(
    _admin: AdminUser,
    State(pool): State<PgPool>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let total_programs: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM programs")
        .fetch_one(&pool)
        .await
        .map_err(crate::errors::internal_error)?;

    let active_programs: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM programs WHERE is_active = true")
            .fetch_one(&pool)
            .await
            .map_err(crate::errors::internal_error)?;

    let total_users: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM users")
        .fetch_one(&pool)
        .await
        .map_err(crate::errors::internal_error)?;

    let monitored_programs = sqlx::query_scalar::<_, i64>(
        "SELECT count(DISTINCT program_id) FROM program_source_monitors",
    )
    .fetch_one(&pool)
    .await
    .map_err(crate::errors::internal_error)?;
    let source_reviews_pending = sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM program_source_holds WHERE resolved_at IS NULL",
    )
    .fetch_one(&pool)
    .await
    .map_err(crate::errors::internal_error)?;
    let overdue_source_monitors = sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM program_source_monitors
         WHERE verification_state = 'current' AND review_due_at <= now()",
    )
    .fetch_one(&pool)
    .await
    .map_err(crate::errors::internal_error)?;
    let (open_operational_events, open_data_quality_issues) = sqlx::query_as::<_, (i64, i64)>(
        "SELECT
             (SELECT count(*) FROM operational_events WHERE status <> 'resolved'),
             (SELECT count(*) FROM program_data_quality_issues WHERE status = 'open')",
    )
    .fetch_one(&pool)
    .await
    .map_err(crate::errors::internal_error)?;

    Ok(Json(json!({
        "total_programs": total_programs.0,
        "active_programs": active_programs.0,
        "total_users": total_users.0,
        "monitored_programs": monitored_programs,
        "source_reviews_pending": source_reviews_pending,
        "overdue_source_monitors": overdue_source_monitors,
        "open_operational_events": open_operational_events,
        "open_data_quality_issues": open_data_quality_issues
    })))
}
