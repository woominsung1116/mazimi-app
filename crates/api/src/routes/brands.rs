use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use mazimi_core::{models::Brand, normalize_brand_name};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::auth::AdminUser;

#[derive(Debug, Deserialize)]
pub struct BrandQuery {
    pub q: Option<String>,
    pub industry: Option<String>,
    pub has_benefits: Option<bool>,
    pub page: Option<i64>,
    pub per_page: Option<i64>,
}

#[derive(Debug, Serialize, FromRow)]
pub struct BrandListItem {
    pub id: Uuid,
    pub name: String,
    pub company_name: Option<String>,
    pub industry_major: Option<String>,
    pub industry_minor: Option<String>,
    pub primary_product: Option<String>,
    pub homepage_url: Option<String>,
    pub logo_url: Option<String>,
    pub discovery_priority: i32,
    pub benefit_count: i64,
}

#[derive(Debug, Serialize)]
pub struct BrandListResponse {
    pub items: Vec<BrandListItem>,
    pub total: i64,
    pub page: i64,
    pub per_page: i64,
    pub has_next: bool,
}

#[derive(Debug, Serialize)]
pub struct BrandDetailResponse {
    #[serde(flatten)]
    pub brand: Brand,
    pub benefit_count: i64,
}

#[derive(Debug, Deserialize)]
pub struct UpsertBrandInput {
    pub name: String,
    pub company_name: Option<String>,
    pub industry_major: Option<String>,
    pub industry_minor: Option<String>,
    pub primary_product: Option<String>,
    pub homepage_url: Option<String>,
    pub logo_url: Option<String>,
    /// Optional stable identifier for repeated admin imports.
    pub source_id: Option<String>,
    pub discovery_priority: Option<i32>,
    pub is_active: Option<bool>,
}

type ApiError = (StatusCode, Json<Value>);

fn bad_request(message: &str) -> ApiError {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": message })))
}

fn clean_optional(value: Option<String>, max_chars: usize) -> Result<Option<String>, ApiError> {
    match value {
        Some(value) => {
            let value = value.trim();
            if value.is_empty() {
                Ok(None)
            } else if value.chars().count() > max_chars {
                Err(bad_request("입력값이 너무 깁니다"))
            } else {
                Ok(Some(value.to_string()))
            }
        }
        None => Ok(None),
    }
}

fn validate_public_url(value: Option<String>) -> Result<Option<String>, ApiError> {
    let Some(value) = clean_optional(value, 2_000)? else {
        return Ok(None);
    };
    let parsed =
        reqwest::Url::parse(&value).map_err(|_| bad_request("URL 형식이 올바르지 않습니다"))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(bad_request("http 또는 https URL만 사용할 수 있습니다"));
    }
    Ok(Some(parsed.to_string()))
}

/// GET /api/v1/brands
pub async fn list_brands(
    State(pool): State<PgPool>,
    Query(params): Query<BrandQuery>,
) -> Result<Json<BrandListResponse>, ApiError> {
    let page = params.page.unwrap_or(1).clamp(1, 1_000_000);
    let per_page = params.per_page.unwrap_or(30).clamp(1, 100);
    let offset = (page - 1) * per_page;

    let raw_query = clean_optional(params.q, 80)?;
    let normalized_query = raw_query
        .as_deref()
        .map(normalize_brand_name)
        .filter(|value| !value.is_empty());
    let industry = clean_optional(params.industry, 100)?;
    let has_benefits = params.has_benefits.unwrap_or(false);

    const FILTER: &str = "
        WHERE b.is_active = true
          AND (
              $1::text IS NULL
              OR b.normalized_name LIKE '%' || $1 || '%'
              OR b.name ILIKE '%' || $2 || '%'
              OR COALESCE(b.company_name, '') ILIKE '%' || $2 || '%'
          )
          AND (
              $3::text IS NULL
              OR b.industry_major ILIKE '%' || $3 || '%'
              OR b.industry_minor ILIKE '%' || $3 || '%'
          )
          AND (
              $4::boolean = false
              OR EXISTS (
                  SELECT 1
                  FROM program_brands pb
                  JOIN current_programs p ON p.id = pb.program_id
                  WHERE pb.brand_id = b.id AND p.is_active = true AND (LEAST(p.benefit_valid_until, p.deadline_at, p.application_end_at) IS NULL OR LEAST(p.benefit_valid_until, p.deadline_at, p.application_end_at) > now())
              )
          )";

    let count_sql = format!("SELECT COUNT(*) FROM brands b {FILTER}");
    let total = sqlx::query_scalar::<_, i64>(&count_sql)
        .bind(&normalized_query)
        .bind(&raw_query)
        .bind(&industry)
        .bind(has_benefits)
        .fetch_one(&pool)
        .await
        .map_err(crate::errors::internal_error)?;

    let items_sql = format!(
        "SELECT b.id, b.name, b.company_name, b.industry_major, b.industry_minor,
                b.primary_product, b.homepage_url, b.logo_url, b.discovery_priority,
                (
                    SELECT COUNT(*)
                    FROM program_brands pb
                    JOIN current_programs p ON p.id = pb.program_id
                    WHERE pb.brand_id = b.id AND p.is_active = true AND (LEAST(p.benefit_valid_until, p.deadline_at, p.application_end_at) IS NULL OR LEAST(p.benefit_valid_until, p.deadline_at, p.application_end_at) > now())
                ) AS benefit_count
         FROM brands b
         {FILTER}
         ORDER BY benefit_count DESC, b.discovery_priority DESC, b.name ASC
         LIMIT $5 OFFSET $6"
    );

    let items = sqlx::query_as::<_, BrandListItem>(&items_sql)
        .bind(&normalized_query)
        .bind(&raw_query)
        .bind(&industry)
        .bind(has_benefits)
        .bind(per_page)
        .bind(offset)
        .fetch_all(&pool)
        .await
        .map_err(crate::errors::internal_error)?;

    Ok(Json(BrandListResponse {
        items,
        total,
        page,
        per_page,
        has_next: offset + per_page < total,
    }))
}

/// GET /api/v1/brands/{id}
pub async fn get_brand(
    State(pool): State<PgPool>,
    Path(id): Path<Uuid>,
) -> Result<Json<BrandDetailResponse>, ApiError> {
    let brand = sqlx::query_as::<_, Brand>(
        "SELECT id, name, normalized_name, company_name, industry_major,
                industry_minor, primary_product, homepage_url, logo_url,
                source_type, source_id, discovery_priority, is_active,
                last_synced_at, created_at, updated_at
         FROM brands
         WHERE id = $1 AND is_active = true",
    )
    .bind(id)
    .fetch_optional(&pool)
    .await
    .map_err(crate::errors::internal_error)?
    .ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "Brand not found" })),
        )
    })?;

    let benefit_count = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*)
         FROM program_brands pb
         JOIN current_programs p ON p.id = pb.program_id
         WHERE pb.brand_id = $1 AND p.is_active = true AND (LEAST(p.benefit_valid_until, p.deadline_at, p.application_end_at) IS NULL OR LEAST(p.benefit_valid_until, p.deadline_at, p.application_end_at) > now())",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .map_err(crate::errors::internal_error)?;

    Ok(Json(BrandDetailResponse {
        brand,
        benefit_count,
    }))
}

/// POST /api/v1/admin/brands
///
/// Upserts direct-store and online brands that are not represented in the
/// franchise catalogue. Admin auth is enforced by the extractor and route
/// middleware; no caller-provided user or role is trusted.
pub async fn upsert_brand(
    _admin: AdminUser,
    State(pool): State<PgPool>,
    Json(input): Json<UpsertBrandInput>,
) -> Result<Json<Brand>, ApiError> {
    let name = input.name.trim();
    if name.is_empty() || name.chars().count() > 200 {
        return Err(bad_request("브랜드명은 1~200자여야 합니다"));
    }
    let normalized_name = normalize_brand_name(name);
    if normalized_name.is_empty() {
        return Err(bad_request("검색 가능한 브랜드명이 필요합니다"));
    }

    let company_name = clean_optional(input.company_name, 200)?;
    let industry_major = clean_optional(input.industry_major, 100)?;
    let industry_minor = clean_optional(input.industry_minor, 100)?;
    let primary_product = clean_optional(input.primary_product, 300)?;
    let homepage_url = validate_public_url(input.homepage_url)?;
    let logo_url = validate_public_url(input.logo_url)?;
    let source_id =
        clean_optional(input.source_id, 200)?.unwrap_or_else(|| normalized_name.clone());
    let discovery_priority = input.discovery_priority.unwrap_or(50);
    if !(0..=100).contains(&discovery_priority) {
        return Err(bad_request("discovery_priority는 0~100이어야 합니다"));
    }

    let brand = sqlx::query_as::<_, Brand>(
        "INSERT INTO brands (
             id, name, normalized_name, company_name, industry_major,
             industry_minor, primary_product, homepage_url, logo_url,
             source_type, source_id, discovery_priority, is_active,
             last_synced_at, created_at, updated_at
         ) VALUES (
             $1, $2, $3, $4, $5, $6, $7, $8, $9,
             'manual', $10, $11, $12, now(), now(), now()
         )
         ON CONFLICT (source_type, source_id) DO UPDATE SET
             name = EXCLUDED.name,
             normalized_name = EXCLUDED.normalized_name,
             company_name = EXCLUDED.company_name,
             industry_major = EXCLUDED.industry_major,
             industry_minor = EXCLUDED.industry_minor,
             primary_product = EXCLUDED.primary_product,
             homepage_url = EXCLUDED.homepage_url,
             logo_url = EXCLUDED.logo_url,
             discovery_priority = EXCLUDED.discovery_priority,
             is_active = EXCLUDED.is_active,
             last_synced_at = now(),
             updated_at = now()
         RETURNING id, name, normalized_name, company_name, industry_major,
                   industry_minor, primary_product, homepage_url, logo_url,
                   source_type, source_id, discovery_priority, is_active,
                   last_synced_at, created_at, updated_at",
    )
    .bind(Uuid::new_v4())
    .bind(name)
    .bind(&normalized_name)
    .bind(company_name)
    .bind(industry_major)
    .bind(industry_minor)
    .bind(primary_product)
    .bind(homepage_url)
    .bind(logo_url)
    .bind(source_id)
    .bind(discovery_priority)
    .bind(input.is_active.unwrap_or(true))
    .fetch_one(&pool)
    .await
    .map_err(crate::errors::internal_error)?;

    Ok(Json(brand))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_http_urls() {
        assert!(validate_public_url(Some("javascript:alert(1)".into())).is_err());
        assert!(validate_public_url(Some("file:///etc/passwd".into())).is_err());
    }

    #[test]
    fn accepts_https_url() {
        assert_eq!(
            validate_public_url(Some("https://example.com/events".into())).unwrap(),
            Some("https://example.com/events".into())
        );
    }
}
