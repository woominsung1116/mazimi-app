use crate::auth::AuthUser;
use axum::{extract::State, http::StatusCode, Json};
use chrono::{DateTime, Utc};
use mazimi_core::models::Program;
use serde::Serialize;
use serde_json::{json, Value};
use sqlx::PgPool;

/// A bookmarked programme as the saved list shows it. Saved rows are never
/// dropped; one that expired or was retired stays listed with
/// `available = false`.
#[derive(Debug, Serialize, sqlx::FromRow)]
struct SavedProgram {
    #[serde(flatten)]
    #[sqlx(flatten)]
    program: Program,
    /// Still in the public catalogue (`current_programs`): exactly the rows
    /// the detail page serves, so an unavailable row never links to a 404.
    available: bool,
    /// Earliest of `deadline_at`, `application_end_at` and
    /// `benefit_valid_until`, as on the detail page.
    #[sqlx(skip)]
    effective_deadline: Option<DateTime<Utc>>,
}

/// GET /api/v1/my/saved
/// Returns bookmarked programs for the authenticated user (JWT-based).
pub async fn get_saved(
    auth_user: AuthUser,
    State(pool): State<PgPool>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mut programs = sqlx::query_as::<_, SavedProgram>(
        r#"
        SELECT p.*, EXISTS (SELECT 1 FROM current_programs c WHERE c.id = p.id) AS available
        FROM programs p
        INNER JOIN user_bookmarks ub ON ub.program_id = p.id
        WHERE ub.user_id = $1
        ORDER BY ub.created_at DESC
        "#,
    )
    .bind(auth_user.id)
    .fetch_all(&pool)
    .await
    .map_err(crate::errors::internal_error)?;
    for saved in &mut programs {
        saved.effective_deadline = saved.program.effective_deadline();
    }

    Ok(Json(json!({
        "total": programs.len(),
        "items": programs
    })))
}
