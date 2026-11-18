use crate::{
    auth::AdminUser,
    services::application_resources::{self, Error, Queue, Review},
};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;
type ResponseError = (StatusCode, Json<Value>);

pub async fn list(
    _: AdminUser,
    State(pool): State<PgPool>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ResponseError> {
    application_resources::list(&pool, id)
        .await
        .map(Json)
        .map_err(Error::response)
}
pub async fn queue(
    _: AdminUser,
    State(pool): State<PgPool>,
    Path(id): Path<Uuid>,
    Json(input): Json<Queue>,
) -> Result<Json<mazimi_core::application_resources::Resource>, ResponseError> {
    application_resources::queue(&pool, id, input)
        .await
        .map(Json)
        .map_err(Error::response)
}
pub async fn review(
    admin: AdminUser,
    State(pool): State<PgPool>,
    Path(id): Path<Uuid>,
    Json(input): Json<Review>,
) -> Result<Json<mazimi_core::application_resources::Resource>, ResponseError> {
    application_resources::review(&pool, admin.0.id, id, input)
        .await
        .map(Json)
        .map_err(Error::response)
}
