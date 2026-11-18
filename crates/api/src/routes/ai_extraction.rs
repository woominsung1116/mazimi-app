//! Admin-only review of AI extraction candidates (A-09). Every handler
//! requires `AdminUser` (401 without a token, 403 for other roles); the
//! reviewer id comes from the verified JWT, never from the request body.
use crate::{
    auth::AdminUser,
    services::ai_extraction::{self, AuditInput, QueueQuery, RevertInput, ReviewInput},
};
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

type ResponseError = (StatusCode, Json<Value>);

/// GET /api/v1/admin/ai-extraction/candidates?status=pending|auto_applied|audit|open|…
pub async fn queue(
    _: AdminUser,
    State(pool): State<PgPool>,
    Query(query): Query<QueueQuery>,
) -> Result<Json<Value>, ResponseError> {
    ai_extraction::queue(&pool, query)
        .await
        .map(Json)
        .map_err(ai_extraction::Error::response)
}

/// GET /api/v1/admin/programs/{id}/ai-extraction
pub async fn program(
    _: AdminUser,
    State(pool): State<PgPool>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ResponseError> {
    ai_extraction::program_review(&pool, id)
        .await
        .map(Json)
        .map_err(ai_extraction::Error::response)
}

/// POST /api/v1/admin/programs/{id}/ai-extraction/review
pub async fn review(
    admin: AdminUser,
    State(pool): State<PgPool>,
    Path(id): Path<Uuid>,
    Json(input): Json<ReviewInput>,
) -> Result<Json<Value>, ResponseError> {
    ai_extraction::review(&pool, admin.0.id, id, input)
        .await
        .map(Json)
        .map_err(ai_extraction::Error::response)
}

/// GET /api/v1/admin/ai-extraction/auto-runs
pub async fn auto_runs(
    _: AdminUser,
    State(pool): State<PgPool>,
) -> Result<Json<Value>, ResponseError> {
    ai_extraction::auto_runs(&pool)
        .await
        .map(Json)
        .map_err(ai_extraction::Error::response)
}

/// POST /api/v1/admin/ai-extraction/candidates/{id}/audit
pub async fn audit(
    admin: AdminUser,
    State(pool): State<PgPool>,
    Path(id): Path<Uuid>,
    Json(input): Json<AuditInput>,
) -> Result<Json<Value>, ResponseError> {
    ai_extraction::confirm_audit(&pool, admin.0.id, id, input)
        .await
        .map(Json)
        .map_err(ai_extraction::Error::response)
}

/// POST /api/v1/admin/ai-extraction/revert
pub async fn revert(
    admin: AdminUser,
    State(pool): State<PgPool>,
    Json(input): Json<RevertInput>,
) -> Result<Json<Value>, ResponseError> {
    ai_extraction::revert(&pool, admin.0.id, input)
        .await
        .map(Json)
        .map_err(ai_extraction::Error::response)
}
