//! Admin: reviewed applicant classes (`program_applicant_reviews`). Every
//! write is checked against the programme's current text and logged with the
//! admin id from the verified token.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use mazimi_core::applicant_review::{self, Review};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use crate::{auth::AdminUser, errors::internal_error};

type ApiError = (StatusCode, Json<Value>);

#[derive(Deserialize)]
pub struct ListQuery {
    class: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

type ListRow = (
    String,
    String,
    String,
    String,
    String,
    String,
    DateTime<Utc>,
    Option<Uuid>,
    Option<String>,
    Option<Value>,
);

/// GET /api/v1/admin/applicant-reviews?class=&limit=&offset=
pub async fn list(
    _admin: AdminUser,
    State(pool): State<PgPool>,
    Query(query): Query<ListQuery>,
) -> Result<Json<Value>, ApiError> {
    let class = match query.class.as_deref() {
        None => None,
        Some(value) => Some(
            applicant_review::ApplicantClass::parse(value)
                .ok_or_else(|| rejected(applicant_review::Rejection::UnknownClass))?
                .as_str(),
        ),
    };
    let limit = query.limit.unwrap_or(100).clamp(1, 500);
    let offset = query.offset.unwrap_or(0).max(0);
    let total: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM program_applicant_reviews WHERE ($1::text IS NULL OR class = $1)",
    )
    .bind(class)
    .fetch_one(&pool)
    .await
    .map_err(internal_error)?;
    let rows = sqlx::query_as::<_, ListRow>(
        "SELECT r.source_type, r.source_id, r.class, r.evidence, r.reviewer, r.text_hash,
                r.updated_at, p.id, p.title, p.raw_payload
         FROM program_applicant_reviews r
         LEFT JOIN programs p ON p.source_type = r.source_type AND p.source_id = r.source_id
         WHERE ($1::text IS NULL OR r.class = $1)
         ORDER BY r.updated_at DESC, r.source_type, r.source_id
         LIMIT $2 OFFSET $3",
    )
    .bind(class)
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
                class,
                evidence,
                reviewer,
                hash,
                updated_at,
                id,
                title,
                raw,
            )| {
                let current = raw
                    .as_ref()
                    .and_then(|raw| applicant_review::text_hash(&source_type, raw));
                json!({
                    "source_type": source_type,
                    "source_id": source_id,
                    "program_id": id,
                    "title": title,
                    "class": class,
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
    class: String,
    evidence: String,
    reviewer: String,
    /// `current_text_hash` from the listing when the admin read the text;
    /// the write is rejected (`text_changed`) if the text changed since.
    expected_text_hash: Option<String>,
}

/// PUT /api/v1/admin/programs/{id}/applicant-review
pub async fn put(
    admin: AdminUser,
    State(pool): State<PgPool>,
    Path(id): Path<Uuid>,
    Json(input): Json<PutReview>,
) -> Result<Json<Value>, ApiError> {
    let (source_type, source_id) = source(&pool, id).await?;
    let review = Review {
        source_type,
        source_id,
        class: input.class,
        evidence: input.evidence,
        reviewer: input.reviewer,
        text_hash: input.expected_text_hash,
    };
    let actor = format!("admin:{}", admin.0.id);
    let mut connection = pool.acquire().await.map_err(internal_error)?;
    let written = applicant_review::upsert(&mut connection, &review, &actor, false)
        .await
        .map_err(internal_error)?
        .map_err(rejected)?;
    tracing::info!(program_id = %id, admin_id = %admin.0.id, class = %review.class.trim(), ?written, "admin wrote applicant review");
    Ok(Json(json!({
        "program_id": id,
        "source_type": review.source_type,
        "source_id": review.source_id,
        "class": review.class.trim(),
        "written": written,
        "stale": false,
    })))
}

/// DELETE /api/v1/admin/programs/{id}/applicant-review
pub async fn delete(
    admin: AdminUser,
    State(pool): State<PgPool>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let (source_type, source_id) = source(&pool, id).await?;
    let actor = format!("admin:{}", admin.0.id);
    let mut connection = pool.acquire().await.map_err(internal_error)?;
    if applicant_review::delete(&mut connection, &source_type, &source_id, &actor)
        .await
        .map_err(internal_error)?
    {
        tracing::info!(program_id = %id, admin_id = %admin.0.id, "admin deleted applicant review");
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
        Json(json!({"error": "검토 기록이 없습니다.", "code": "applicant_review_not_found"})),
    )
}

fn rejected(reason: applicant_review::Rejection) -> ApiError {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error": reason.message(), "code": reason})),
    )
}
