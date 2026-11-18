//! `/api/v1/my/document-items`: the user's own document checklist entries
//! ("직접 추가"). Names and status only; files never leave the device. Every
//! query is scoped by the token's user.
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::services::profile_validation::field_error;

type Error = (StatusCode, Json<Value>);

/// Items per (user, programme) and per user.
const MAX_PER_PROGRAM: i64 = 30;
const MAX_PER_USER: i64 = 200;
const MAX_NAME_CHARS: usize = 60;
const STATUSES: [&str; 2] = ["todo", "done"];

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct DocumentItem {
    pub id: Uuid,
    pub program_id: Option<Uuid>,
    pub name: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

const COLUMNS: &str = "id, program_id, name, status, created_at, updated_at";

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    pub program_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateItem {
    pub name: String,
    #[serde(default)]
    pub program_id: Option<Uuid>,
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateItem {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
}

fn valid_name(name: &str) -> Result<String, Error> {
    let name = name.trim();
    if name.is_empty()
        || name.chars().count() > MAX_NAME_CHARS
        || name.chars().any(char::is_control)
    {
        return Err(field_error(
            "서류 이름은 1~60자로 입력해 주세요.",
            "name",
            "invalid_name",
        ));
    }
    Ok(name.to_owned())
}

fn valid_status(status: Option<&str>) -> Result<Option<&'static str>, Error> {
    status
        .map(|s| {
            STATUSES
                .iter()
                .find(|known| **known == s.trim())
                .copied()
                .ok_or_else(|| {
                    field_error(
                        "상태는 todo 또는 done이어야 해요.",
                        "status",
                        "invalid_status",
                    )
                })
        })
        .transpose()
}

fn not_found() -> Error {
    (
        StatusCode::NOT_FOUND,
        Json(json!({"error": "서류 항목을 찾을 수 없어요."})),
    )
}

/// GET /api/v1/my/document-items?program_id=
pub async fn list(
    auth_user: AuthUser,
    State(pool): State<PgPool>,
    Query(query): Query<ListQuery>,
) -> Result<Json<Value>, Error> {
    let items = sqlx::query_as::<_, DocumentItem>(&format!(
        "SELECT {COLUMNS} FROM user_document_items
         WHERE user_id = $1 AND ($2::uuid IS NULL OR program_id = $2)
         ORDER BY created_at, id"
    ))
    .bind(auth_user.id)
    .bind(query.program_id)
    .fetch_all(&pool)
    .await
    .map_err(crate::errors::internal_error)?;
    Ok(Json(json!({ "items": items })))
}

/// POST /api/v1/my/document-items
pub async fn create(
    auth_user: AuthUser,
    State(pool): State<PgPool>,
    Json(body): Json<CreateItem>,
) -> Result<(StatusCode, Json<DocumentItem>), Error> {
    let name = valid_name(&body.name)?;
    let status = valid_status(body.status.as_deref())?.unwrap_or("todo");
    let mut tx = pool.begin().await.map_err(crate::errors::internal_error)?;
    // One writer per user at a time, so the limits hold under concurrency.
    sqlx::query("SELECT 1 FROM users WHERE id = $1 FOR UPDATE")
        .bind(auth_user.id)
        .execute(&mut *tx)
        .await
        .map_err(crate::errors::internal_error)?;
    if let Some(program) = body.program_id {
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM current_programs WHERE id = $1)")
                .bind(program)
                .fetch_one(&mut *tx)
                .await
                .map_err(crate::errors::internal_error)?;
        if !exists {
            return Err((
                StatusCode::NOT_FOUND,
                Json(json!({"error": "혜택을 찾을 수 없어요."})),
            ));
        }
    }
    let (for_user, for_program): (i64, i64) = sqlx::query_as(
        "SELECT count(*), count(*) FILTER (WHERE program_id IS NOT DISTINCT FROM $2)
         FROM user_document_items WHERE user_id = $1",
    )
    .bind(auth_user.id)
    .bind(body.program_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(crate::errors::internal_error)?;
    if for_user >= MAX_PER_USER || for_program >= MAX_PER_PROGRAM {
        return Err((
            StatusCode::CONFLICT,
            Json(json!({"error": "서류 항목은 더 추가할 수 없어요.", "code": "too_many_items"})),
        ));
    }
    let item = sqlx::query_as::<_, DocumentItem>(&format!(
        "INSERT INTO user_document_items (user_id, program_id, name, status)
         VALUES ($1, $2, $3, $4) RETURNING {COLUMNS}"
    ))
    .bind(auth_user.id)
    .bind(body.program_id)
    .bind(&name)
    .bind(status)
    .fetch_one(&mut *tx)
    .await
    .map_err(crate::errors::internal_error)?;
    tx.commit().await.map_err(crate::errors::internal_error)?;
    Ok((StatusCode::CREATED, Json(item)))
}

/// PUT /api/v1/my/document-items/{id}
pub async fn update(
    auth_user: AuthUser,
    State(pool): State<PgPool>,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateItem>,
) -> Result<Json<DocumentItem>, Error> {
    let name = body.name.as_deref().map(valid_name).transpose()?;
    let status = valid_status(body.status.as_deref())?;
    sqlx::query_as::<_, DocumentItem>(&format!(
        "UPDATE user_document_items
         SET name = COALESCE($3, name), status = COALESCE($4, status), updated_at = now()
         WHERE id = $1 AND user_id = $2 RETURNING {COLUMNS}"
    ))
    .bind(id)
    .bind(auth_user.id)
    .bind(name)
    .bind(status)
    .fetch_optional(&pool)
    .await
    .map_err(crate::errors::internal_error)?
    .map(Json)
    .ok_or_else(not_found)
}

/// DELETE /api/v1/my/document-items/{id}: the user removes their own entry.
pub async fn delete(
    auth_user: AuthUser,
    State(pool): State<PgPool>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, Error> {
    let removed = sqlx::query("DELETE FROM user_document_items WHERE id = $1 AND user_id = $2")
        .bind(id)
        .bind(auth_user.id)
        .execute(&pool)
        .await
        .map_err(crate::errors::internal_error)?
        .rows_affected();
    if removed == 0 {
        return Err(not_found());
    }
    Ok(StatusCode::NO_CONTENT)
}
