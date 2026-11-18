use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use crate::auth::AuthUser;

/// POST /api/v1/programs/{program_id}/bookmark
/// Toggle bookmark: delete if it exists, else save it through the same write
/// as PUT ([`write`]): a double tap or an unknown id is never a 500 (audit
/// L5). The authenticated user's ID is taken from the JWT — no
/// client-supplied user_id accepted.
pub async fn toggle_bookmark(
    auth_user: AuthUser,
    State(pool): State<PgPool>,
    Path(program_id): Path<Uuid>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let user_id = auth_user.id;
    let deleted = sqlx::query_scalar::<_, Uuid>(
        "DELETE FROM user_bookmarks WHERE user_id = $1 AND program_id = $2 RETURNING id",
    )
    .bind(user_id)
    .bind(program_id)
    .fetch_optional(&pool)
    .await
    .map_err(crate::errors::internal_error)?;
    if deleted.is_some() {
        return Ok(Json(json!({ "bookmarked": false })));
    }
    let bookmarked = write(&pool, user_id, program_id, true).await?;
    Ok(Json(json!({ "bookmarked": bookmarked })))
}

/// Set one user's bookmark of a programme and return the state the row is
/// in afterwards. A new bookmark needs a published programme
/// (`current_programs`), else 404; removing one is allowed for any id, so a
/// saved row that was unpublished can still be unsaved.
async fn write(
    pool: &PgPool,
    user_id: Uuid,
    program_id: Uuid,
    bookmarked: bool,
) -> Result<bool, (StatusCode, Json<Value>)> {
    if bookmarked {
        // Same statement as an approved agent bookmark (agent/confirm.rs
        // apply), limited to published programmes.
        sqlx::query(
            "INSERT INTO user_bookmarks (user_id, program_id) \
             SELECT $1, id FROM current_programs WHERE id = $2 \
             ON CONFLICT (user_id, program_id) DO NOTHING",
        )
        .bind(user_id)
        .bind(program_id)
        .execute(pool)
        .await
        .map_err(crate::errors::internal_error)?;
    } else {
        sqlx::query("DELETE FROM user_bookmarks WHERE user_id = $1 AND program_id = $2")
            .bind(user_id)
            .bind(program_id)
            .execute(pool)
            .await
            .map_err(crate::errors::internal_error)?;
    }
    // The final state, read back rather than assumed.
    let state: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM user_bookmarks WHERE user_id = $1 AND program_id = $2)",
    )
    .bind(user_id)
    .bind(program_id)
    .fetch_one(pool)
    .await
    .map_err(crate::errors::internal_error)?;
    if bookmarked && !state {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "혜택을 찾을 수 없어요." })),
        ));
    }
    Ok(state)
}

#[derive(Debug, serde::Deserialize)]
pub struct SetBookmark {
    pub bookmarked: bool,
}

/// PUT /api/v1/programs/{program_id}/bookmark  `{"bookmarked": bool}`
///
/// Idempotent: sets the bookmark to the requested state and returns the state
/// the row is actually in afterwards. A repeated request (a retry after a lost
/// response) changes nothing, so a client can always send its intent again.
/// The user comes from the JWT only. POST (the toggle) stays for older app
/// builds.
///
/// 404 when saving a programme that is not published (an unknown id
/// included). Any programme can be unsaved, and a saved row stays saved
/// while its programme is unpublished, like the saved list itself.
pub async fn set_bookmark(
    auth_user: AuthUser,
    State(pool): State<PgPool>,
    Path(program_id): Path<Uuid>,
    Json(body): Json<SetBookmark>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let bookmarked = write(&pool, auth_user.id, program_id, body.bookmarked).await?;
    Ok(Json(json!({ "bookmarked": bookmarked })))
}
