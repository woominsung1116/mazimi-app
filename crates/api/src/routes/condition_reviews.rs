//! Admin: reviewed eligibility conditions (`program_condition_reviews`).
//! Every write must name the text the admin read (`expected_text_hash`), is
//! checked against the programme's current text and is logged with the
//! admin id from the verified token.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use mazimi_core::condition_review::{
    self,
    store::{self, Rejection, ReviewInput},
    ReviewStatus,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use crate::{auth::AdminUser, errors::internal_error};

type ApiError = (StatusCode, Json<Value>);

#[derive(Deserialize)]
pub struct ListQuery {
    status: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

type ListRow = (
    String,
    String,
    String,
    Option<Value>,
    Value,
    Vec<String>,
    Value,
    String,
    String,
    Option<DateTime<Utc>>,
    DateTime<Utc>,
    Option<Uuid>,
    Option<String>,
    Option<Value>,
    Option<Value>,
);

/// GET /api/v1/admin/condition-reviews?status=&limit=&offset=
pub async fn list(
    _admin: AdminUser,
    State(pool): State<PgPool>,
    Query(query): Query<ListQuery>,
) -> Result<Json<Value>, ApiError> {
    let status = match query.status.as_deref() {
        None => None,
        Some(value) => Some(
            ReviewStatus::parse(value)
                .ok_or_else(|| rejected(Rejection::UnknownStatus))?
                .as_str(),
        ),
    };
    let limit = query.limit.unwrap_or(100).clamp(1, 500);
    let offset = query.offset.unwrap_or(0).max(0);
    let total: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM program_condition_reviews WHERE ($1::text IS NULL OR status = $1)",
    )
    .bind(status)
    .fetch_one(&pool)
    .await
    .map_err(internal_error)?;
    let rows = sqlx::query_as::<_, ListRow>(&format!(
        "SELECT r.source_type, r.source_id, r.status, r.rule_json, r.window_json,
                r.remaining_checks, r.evidence, r.reviewer, r.text_hash,
                r.window_confirmed_at, r.updated_at, p.id, p.title, {hashed}, r.code_overrides
         FROM program_condition_reviews r
         LEFT JOIN programs p ON p.source_type = r.source_type AND p.source_id = r.source_id
         WHERE ($1::text IS NULL OR r.status = $1)
         ORDER BY r.updated_at DESC, r.source_type, r.source_id
         LIMIT $2 OFFSET $3",
        hashed = condition_review::hashed_projection_sql("p."),
    ))
    .bind(status)
    .bind(limit)
    .bind(offset)
    .fetch_all(&pool)
    .await
    .map_err(internal_error)?;
    let items: Vec<Value> = rows
        .into_iter()
        .map(
            |(
                source_type,
                source_id,
                status,
                rule_json,
                window_json,
                remaining_checks,
                evidence,
                reviewer,
                hash,
                confirmed_at,
                updated_at,
                id,
                title,
                raw,
                code_overrides,
            )| {
                let current = raw
                    .as_ref()
                    .and_then(|raw| condition_review::text_hash(&source_type, raw));
                json!({
                    "source_type": source_type,
                    "source_id": source_id,
                    "program_id": id,
                    "title": title,
                    "status": status,
                    "rule_json": rule_json,
                    "window": window_json,
                    "window_confirmed_at": confirmed_at,
                    "remaining_checks": remaining_checks,
                    "code_overrides": code_overrides,
                    "evidence": evidence,
                    "reviewer": reviewer,
                    "text_hash": hash,
                    // The text changed (or the programme is gone): no effect.
                    "stale": current.as_deref() != Some(hash.as_str()),
                    // Pass back as `expected_text_hash` with a PUT.
                    "current_text_hash": current,
                    "updated_at": updated_at,
                })
            },
        )
        .collect();
    Ok(Json(json!({ "total": total, "items": items })))
}

#[derive(Deserialize)]
pub struct PutReview {
    status: String,
    #[serde(default)]
    rules: Vec<Value>,
    #[serde(default)]
    code_rules: Vec<Value>,
    window: Value,
    #[serde(default)]
    remaining_checks: Vec<String>,
    #[serde(default)]
    coverage: Option<Value>,
    reviewer: String,
    /// Widen/drop overrides of the programme's code rules
    /// ([`mazimi_core::condition_review::overrides`]).
    #[serde(default)]
    code_overrides: Vec<Value>,
    /// `current_text_hash` from the listing when the admin read the text;
    /// required, and the write is rejected (`text_changed`) if the text
    /// changed since.
    expected_text_hash: Option<String>,
}

/// PUT /api/v1/admin/programs/{id}/condition-review
pub async fn put(
    admin: AdminUser,
    State(pool): State<PgPool>,
    Path(id): Path<Uuid>,
    Json(input): Json<PutReview>,
) -> Result<Json<Value>, ApiError> {
    let (source_type, source_id) = source(&pool, id).await?;
    let review = ReviewInput {
        source_type,
        source_id,
        text_hash: input.expected_text_hash,
        status: input.status,
        rules: input.rules,
        code_rules: input.code_rules,
        window: input.window,
        remaining_checks: input.remaining_checks,
        coverage: input.coverage,
        reviewer: input.reviewer,
        code_overrides: input.code_overrides,
    };
    let actor = format!("admin:{}", admin.0.id);
    let mut connection = pool.acquire().await.map_err(internal_error)?;
    let written = store::upsert(&mut connection, &review, &actor, true)
        .await
        .map_err(internal_error)?
        .map_err(rejected)?;
    tracing::info!(program_id = %id, admin_id = %admin.0.id, status = %review.status.trim(), ?written, "admin wrote condition review");
    Ok(Json(json!({
        "program_id": id,
        "source_type": review.source_type,
        "source_id": review.source_id,
        "status": review.status.trim(),
        "written": written,
        "stale": false,
    })))
}

/// DELETE /api/v1/admin/programs/{id}/condition-review
pub async fn delete(
    admin: AdminUser,
    State(pool): State<PgPool>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let (source_type, source_id) = source(&pool, id).await?;
    let actor = format!("admin:{}", admin.0.id);
    let mut connection = pool.acquire().await.map_err(internal_error)?;
    if store::delete(&mut connection, &source_type, &source_id, &actor)
        .await
        .map_err(internal_error)?
    {
        tracing::info!(program_id = %id, admin_id = %admin.0.id, "admin deleted condition review");
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(not_found())
    }
}

async fn source(pool: &PgPool, id: Uuid) -> Result<(String, String), ApiError> {
    sqlx::query_as::<_, (String, Option<String>)>(
        "SELECT source_type, source_id FROM programs WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(internal_error)?
    .and_then(|(source_type, source_id)| Some((source_type, source_id?)))
    .ok_or_else(not_found)
}

fn not_found() -> ApiError {
    (
        StatusCode::NOT_FOUND,
        Json(json!({"error": "검토 기록이 없습니다.", "code": "condition_review_not_found"})),
    )
}

fn rejected(reason: Rejection) -> ApiError {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error": reason.message(), "code": reason})),
    )
}
