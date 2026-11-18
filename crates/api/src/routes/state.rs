use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use mazimi_core::models::UserProgramState;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::PgPool;
use std::collections::HashMap;
use uuid::Uuid;

use crate::auth::AuthUser;

const VALID_STATES: &[&str] = &[
    "interested",
    "planning",
    "applying",
    "applied",
    "waiting",
    "received",
    "abandoned",
];

// ── Request / response types ──

#[derive(Debug, Deserialize)]
pub struct UpsertStateRequest {
    pub state: String,
    pub memo: Option<String>,
}

/// Body for PUT /api/v1/my/applications/{program_id}
#[derive(Debug, Deserialize)]
pub struct UpdateStatusRequest {
    pub status: String,
    pub memo: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct StateHistoryEntry {
    pub state: String,
    pub memo: Option<String>,
    pub changed_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct ApplicationDetail {
    pub program_id: Uuid,
    pub program_title: String,
    pub current_status: String,
    pub memo: Option<String>,
    pub applied_at: Option<DateTime<Utc>>,
    pub result_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
    pub history: Vec<StateHistoryEntry>,
    /// Journey outcome: `checking`, `unknown` or `reported_applied`.
    pub submission_check: Option<String>,
    /// One status for every client, derived from the user-recorded state and
    /// the journey outcome. Both are self-reported; nothing here is an
    /// institution-verified receipt.
    pub display_status: String,
    pub display_label: &'static str,
    /// The programme's provider (or company), for the 신청 이력 card subtitle
    /// (Figma 34068:3827). Only on the list.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_name: Option<String>,
    /// The programme's summary cleaned for display, the card body (34068:3831).
    /// Only on the list.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary_display: Option<String>,
}

// ── Helpers ──

/// Settled states are the user's own final word; an unconfirmed journey must
/// not override them.
fn settled(state: &str) -> bool {
    matches!(state, "applied" | "waiting" | "received")
}

pub fn display_status(state: &str, journey_outcome: Option<&str>) -> (String, &'static str) {
    let status = match journey_outcome {
        Some("unknown") if !settled(state) => "submission_unconfirmed",
        _ => state,
    };
    let label = match status {
        "interested" => "관심",
        "planning" => "신청 예정",
        "applying" => "신청 준비 중",
        "applied" => "신청 완료 · 직접 기록",
        "waiting" => "결과 대기",
        "received" => "수혜 완료",
        "abandoned" => "진행 중단 · 직접 기록",
        "submission_unconfirmed" => "접수 여부 확인 필요",
        _ => "상태 확인 필요",
    };
    (status.to_owned(), label)
}

fn invalid_state_error(state: &str) -> (StatusCode, Json<Value>) {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({
            "error": format!(
                "Invalid status '{}'. Valid values: {}",
                state,
                VALID_STATES.join(", ")
            )
        })),
    )
}

fn db_error(e: sqlx::Error) -> (StatusCode, Json<Value>) {
    crate::errors::internal_error(e)
}

/// Fetch history rows for a (user, program) pair, newest first.
async fn fetch_history(
    pool: &PgPool,
    user_id: Uuid,
    program_id: Uuid,
) -> Result<Vec<StateHistoryEntry>, (StatusCode, Json<Value>)> {
    let rows = sqlx::query_as::<_, (String, Option<String>, DateTime<Utc>)>(
        r#"
        SELECT state, memo, changed_at
        FROM   user_program_state_history
        WHERE  user_id = $1 AND program_id = $2
        ORDER  BY changed_at DESC
        "#,
    )
    .bind(user_id)
    .bind(program_id)
    .fetch_all(pool)
    .await
    .map_err(db_error)?;

    Ok(rows
        .into_iter()
        .map(|(state, memo, changed_at)| StateHistoryEntry {
            state,
            memo,
            changed_at,
        })
        .collect())
}

/// Append one row to the history table.
async fn record_history(
    connection: &mut sqlx::PgConnection,
    user_id: Uuid,
    program_id: Uuid,
    state: &str,
    memo: Option<&str>,
) -> Result<(), (StatusCode, Json<Value>)> {
    sqlx::query(
        r#"
        INSERT INTO user_program_state_history (user_id, program_id, state, memo)
        VALUES ($1, $2, $3, $4)
        "#,
    )
    .bind(user_id)
    .bind(program_id)
    .bind(state)
    .bind(memo)
    .execute(connection)
    .await
    .map_err(db_error)?;
    Ok(())
}

// ── Handlers ──

/// POST /api/v1/programs/{program_id}/state
///
/// Upsert user program state.
/// The authenticated user's ID is taken from the JWT — no client-supplied user_id accepted.
pub async fn upsert_state(
    auth_user: AuthUser,
    State(pool): State<PgPool>,
    Path(program_id): Path<Uuid>,
    Json(payload): Json<UpsertStateRequest>,
) -> Result<Json<UserProgramState>, (StatusCode, Json<Value>)> {
    if !VALID_STATES.contains(&payload.state.as_str()) {
        return Err(invalid_state_error(&payload.state));
    }

    if payload
        .memo
        .as_ref()
        .is_some_and(|memo| memo.chars().count() > 2000)
    {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"Memo must be at most 2000 characters"})),
        ));
    }
    let user_id = auth_user.id;
    let mut tx = pool.begin().await.map_err(db_error)?;

    let record = sqlx::query_as::<_, UserProgramState>(
        r#"
        INSERT INTO user_program_states (user_id, program_id, state, memo, applied_at, result_at, updated_at)
        VALUES ($1, $2, $3, $4, CASE WHEN $3 = 'applied' THEN now() END,
                CASE WHEN $3 IN ('received','abandoned') THEN now() END, now())
        ON CONFLICT (user_id, program_id) DO UPDATE SET
            state      = EXCLUDED.state,
            memo       = EXCLUDED.memo,
            applied_at = COALESCE(user_program_states.applied_at, EXCLUDED.applied_at),
            result_at  = COALESCE(user_program_states.result_at, EXCLUDED.result_at),
            updated_at = now()
        RETURNING *
        "#,
    )
    .bind(user_id)
    .bind(program_id)
    .bind(&payload.state)
    .bind(&payload.memo)
    .fetch_one(&mut *tx)
    .await
    .map_err(db_error)?;

    record_history(
        &mut tx,
        user_id,
        program_id,
        &payload.state,
        payload.memo.as_deref(),
    )
    .await?;

    tx.commit().await.map_err(db_error)?;
    Ok(Json(record))
}

/// GET /api/v1/my/applications
///
/// Returns all programs the user has a status for, with program title and
/// per-entry state-change history.
/// The authenticated user's ID is taken from the JWT.
pub async fn list_applications(
    auth_user: AuthUser,
    State(pool): State<PgPool>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let user_id = auth_user.id;

    // (program_id, program_title, state, memo, applied_at, result_at, updated_at, provider, summary)
    type AppRow = (
        Uuid,
        String,
        String,
        Option<String>,
        Option<DateTime<Utc>>,
        Option<DateTime<Utc>>,
        DateTime<Utc>,
        Option<String>,
        Option<String>,
    );

    let rows = sqlx::query_as::<_, AppRow>(
        r#"
        SELECT
            ups.program_id,
            COALESCE((SELECT a.program->>'title' FROM application_program_snapshots a WHERE a.user_id=ups.user_id AND a.program_id=ups.program_id),p.title) AS program_title,
            ups.state,
            ups.memo,
            ups.applied_at,
            ups.result_at,
            ups.updated_at,
            COALESCE(NULLIF(BTRIM(p.provider_name), ''), NULLIF(BTRIM(p.company_name), '')) AS provider_name,
            p.summary
        FROM   user_program_states ups
        JOIN   programs p ON p.id = ups.program_id
        WHERE  ups.user_id = $1
        ORDER  BY ups.updated_at DESC
        "#,
    )
    .bind(user_id)
    .fetch_all(&pool)
    .await
    .map_err(db_error)?;

    // Batch-load all history rows for this user in one query to avoid N+1.
    let all_history_rows = sqlx::query_as::<_, (Uuid, String, Option<String>, DateTime<Utc>)>(
        r#"
        SELECT program_id, state, memo, changed_at
        FROM   user_program_state_history
        WHERE  user_id = $1
        ORDER  BY changed_at DESC
        "#,
    )
    .bind(user_id)
    .fetch_all(&pool)
    .await
    .map_err(db_error)?;

    let mut history_map: HashMap<Uuid, Vec<StateHistoryEntry>> = HashMap::new();
    for (pid, state, memo, changed_at) in all_history_rows {
        history_map.entry(pid).or_default().push(StateHistoryEntry {
            state,
            memo,
            changed_at,
        });
    }

    let journey_rows: Vec<(Uuid, String)> =
        sqlx::query_as("SELECT program_id,outcome FROM application_journeys WHERE user_id=$1")
            .bind(user_id)
            .fetch_all(&pool)
            .await
            .map_err(db_error)?;
    let mut journey_map: HashMap<Uuid, String> = journey_rows.into_iter().collect();
    let mut items: Vec<ApplicationDetail> = Vec::with_capacity(rows.len());
    for (
        program_id,
        program_title,
        state,
        memo,
        applied_at,
        result_at,
        updated_at,
        provider_name,
        summary,
    ) in rows
    {
        let history = history_map.remove(&program_id).unwrap_or_default();
        let submission_check = journey_map.remove(&program_id);
        let (display_status, display_label) = display_status(&state, submission_check.as_deref());
        items.push(ApplicationDetail {
            program_id,
            program_title,
            current_status: state,
            memo,
            applied_at,
            result_at,
            updated_at,
            history,
            submission_check,
            display_status,
            display_label,
            provider_name,
            summary_display: mazimi_core::display_text::normalize(summary.as_deref()).display,
        });
    }

    Ok(Json(json!({
        "total": items.len(),
        "items": items,
    })))
}

/// GET /api/v1/my/applications/{program_id}
///
/// Returns status and full history for a single (user, program) pair.
/// The authenticated user's ID is taken from the JWT.
pub async fn get_application(
    auth_user: AuthUser,
    State(pool): State<PgPool>,
    Path(program_id): Path<Uuid>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let user_id = auth_user.id;

    // (program_title, state, memo, applied_at, result_at, updated_at)
    type AppRow = (
        String,
        String,
        Option<String>,
        Option<DateTime<Utc>>,
        Option<DateTime<Utc>>,
        DateTime<Utc>,
    );

    let row = sqlx::query_as::<_, AppRow>(
        r#"
        SELECT
            COALESCE((SELECT a.program->>'title' FROM application_program_snapshots a WHERE a.user_id=ups.user_id AND a.program_id=ups.program_id),p.title) AS program_title,
            ups.state,
            ups.memo,
            ups.applied_at,
            ups.result_at,
            ups.updated_at
        FROM   user_program_states ups
        JOIN   programs p ON p.id = ups.program_id
        WHERE  ups.user_id = $1 AND ups.program_id = $2
        "#,
    )
    .bind(user_id)
    .bind(program_id)
    .fetch_optional(&pool)
    .await
    .map_err(db_error)?;

    let (program_title, state, memo, applied_at, result_at, updated_at) = match row {
        Some(r) => r,
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(json!({ "error": "Application status not found" })),
            ))
        }
    };

    let history = fetch_history(&pool, user_id, program_id).await?;

    let journey = super::journeys::find(&pool, user_id, program_id).await?;
    let submission_check = journey.as_ref().map(|j| j.outcome.clone());
    let (display_status, display_label) = display_status(&state, submission_check.as_deref());
    let detail = ApplicationDetail {
        program_id,
        program_title,
        current_status: state,
        memo,
        applied_at,
        result_at,
        updated_at,
        history,
        submission_check,
        display_status,
        display_label,
        provider_name: None,
        summary_display: None,
    };

    let snapshot = sqlx::query_scalar::<_, Value>(
        "SELECT jsonb_build_object('program',program,'captured_at',captured_at,'capture_reason',capture_reason) FROM application_program_snapshots WHERE user_id=$1 AND program_id=$2")
        .bind(user_id).bind(program_id).fetch_optional(&pool).await.map_err(db_error)?;
    let mut response = json!(detail);
    response["snapshot"] = json!(snapshot);
    response["journey"] = json!(journey);
    Ok(Json(response))
}

/// PUT /api/v1/my/applications/{program_id}
///
/// Update the application status for one program.  Automatically sets
/// `applied_at` on first transition to "applied" and `result_at` on first
/// transition to "received" or "abandoned".  Every call appends a history row.
/// The authenticated user's ID is taken from the JWT.
///
/// Body: `{ "status": "applying", "memo": "optional note" }`
pub async fn update_application(
    auth_user: AuthUser,
    State(pool): State<PgPool>,
    Path(program_id): Path<Uuid>,
    Json(payload): Json<UpdateStatusRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if !VALID_STATES.contains(&payload.status.as_str()) {
        return Err(invalid_state_error(&payload.status));
    }

    if payload
        .memo
        .as_ref()
        .is_some_and(|memo| memo.chars().count() > 2000)
    {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"Memo must be at most 2000 characters"})),
        ));
    }
    let user_id = auth_user.id;
    let mut tx = pool.begin().await.map_err(db_error)?;
    let (state, memo, applied_at, result_at, updated_at) = record_status_on(
        &mut tx,
        user_id,
        program_id,
        &payload.status,
        payload.memo.as_deref(),
    )
    .await?;
    tx.commit().await.map_err(db_error)?;
    Ok(Json(json!({
        "program_id": program_id,
        "status":     state,
        "memo":       memo,
        "applied_at": applied_at,
        "result_at":  result_at,
        "updated_at": updated_at,
    })))
}

/// (state, memo, applied_at, result_at, updated_at)
pub(crate) type StatusRow = (
    String,
    Option<String>,
    Option<DateTime<Utc>>,
    Option<DateTime<Utc>>,
    DateTime<Utc>,
);

/// Upsert one validated status and append its history row on the caller's
/// connection (a transaction). Shared by the status route and the agent's
/// confirmed `record_application_status`.
pub(crate) async fn record_status_on(
    connection: &mut sqlx::PgConnection,
    user_id: Uuid,
    program_id: Uuid,
    status: &str,
    memo: Option<&str>,
) -> Result<StatusRow, (StatusCode, Json<Value>)> {
    if !VALID_STATES.contains(&status) {
        return Err(invalid_state_error(status));
    }
    // Choose the right upsert variant based on which timestamp column to set.
    // `COALESCE` ensures we never overwrite a timestamp that was already set.
    let upsert_sql = match status {
        "applied" => {
            r#"
            INSERT INTO user_program_states (user_id, program_id, state, memo, applied_at, updated_at)
            VALUES ($1, $2, $3, $4, now(), now())
            ON CONFLICT (user_id, program_id) DO UPDATE SET
                state      = EXCLUDED.state,
                memo       = EXCLUDED.memo,
                applied_at = COALESCE(user_program_states.applied_at, now()),
                updated_at = now()
            RETURNING state, memo, applied_at, result_at, updated_at
            "#
        }
        "received" | "abandoned" => {
            r#"
            INSERT INTO user_program_states (user_id, program_id, state, memo, result_at, updated_at)
            VALUES ($1, $2, $3, $4, now(), now())
            ON CONFLICT (user_id, program_id) DO UPDATE SET
                state     = EXCLUDED.state,
                memo      = EXCLUDED.memo,
                result_at = COALESCE(user_program_states.result_at, now()),
                updated_at = now()
            RETURNING state, memo, applied_at, result_at, updated_at
            "#
        }
        _ => {
            r#"
            INSERT INTO user_program_states (user_id, program_id, state, memo, updated_at)
            VALUES ($1, $2, $3, $4, now())
            ON CONFLICT (user_id, program_id) DO UPDATE SET
                state      = EXCLUDED.state,
                memo       = EXCLUDED.memo,
                updated_at = now()
            RETURNING state, memo, applied_at, result_at, updated_at
            "#
        }
    };

    let row = sqlx::query_as::<_, StatusRow>(upsert_sql)
        .bind(user_id)
        .bind(program_id)
        .bind(status)
        .bind(memo)
        .fetch_one(&mut *connection)
        .await
        .map_err(db_error)?;
    record_history(connection, user_id, program_id, status, memo).await?;
    Ok(row)
}

#[cfg(test)]
mod tests {
    use super::display_status;

    #[test]
    fn unconfirmed_journey_overrides_only_unsettled_states() {
        assert_eq!(
            display_status("applying", Some("unknown")),
            ("submission_unconfirmed".into(), "접수 여부 확인 필요")
        );
        assert_eq!(
            display_status("planning", Some("unknown")).0,
            "submission_unconfirmed"
        );
        for settled in ["applied", "waiting", "received"] {
            assert_eq!(display_status(settled, Some("unknown")).0, settled);
        }
        assert_eq!(
            display_status("applied", Some("reported_applied")),
            ("applied".into(), "신청 완료 · 직접 기록")
        );
        assert_eq!(
            display_status("applying", Some("checking")),
            ("applying".into(), "신청 준비 중")
        );
        assert_eq!(
            display_status("abandoned", None),
            ("abandoned".into(), "진행 중단 · 직접 기록")
        );
        assert_eq!(display_status("garbage", None).1, "상태 확인 필요");
    }
}
