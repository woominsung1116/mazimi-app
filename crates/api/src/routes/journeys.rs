//! Persist the handoff and a user's report. No remote submissions are performed.
use crate::auth::AuthUser;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;
type Error = (StatusCode, Json<Value>);
fn error(code: StatusCode, message: &str) -> Error {
    (code, Json(json!({"error":message})))
}
#[derive(Debug, Serialize, FromRow)]
pub struct Journey {
    pub id: Uuid,
    pub program_id: Uuid,
    pub plan_revision: String,
    pub channel: String,
    pub outcome: String,
    pub snapshot: Value,
    pub handoff_at: DateTime<Utc>,
    pub reported_at: Option<DateTime<Utc>>,
    pub receipt_reference: Option<String>,
}
const COLUMNS: &str =
    "id,program_id,plan_revision,channel,outcome,snapshot,handoff_at,reported_at,receipt_reference";
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Start {
    revision: String,
    channel: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Report {
    journey_id: Uuid,
    outcome: String,
    receipt_reference: Option<String>,
    #[serde(default)]
    confirmed: bool,
}
pub async fn find(pool: &PgPool, user: Uuid, program: Uuid) -> Result<Option<Journey>, Error> {
    sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM application_journeys WHERE user_id=$1 AND program_id=$2"
    ))
    .bind(user)
    .bind(program)
    .fetch_optional(pool)
    .await
    .map_err(crate::errors::internal_error)
}
pub async fn get(
    user: AuthUser,
    State(pool): State<PgPool>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, Error> {
    Ok(Json(json!({"journey":find(&pool,user.id,id).await?})))
}
pub async fn start(
    user: AuthUser,
    State(pool): State<PgPool>,
    Path(id): Path<Uuid>,
    Json(input): Json<Start>,
) -> Result<Json<Journey>, Error> {
    let plan = super::preparation::build(&pool, id).await?;
    if input.revision != plan.revision {
        return Err(error(
            StatusCode::CONFLICT,
            "공고가 변경됐어요. 새 안내를 확인해 주세요.",
        ));
    }
    if !plan.policy.can_prepare {
        return Err(error(
            StatusCode::CONFLICT,
            "현재 신청 가능한 공고인지 확인해 주세요.",
        ));
    }
    let channel = plan
        .policy
        .workflow
        .channels
        .iter()
        .find(|c| c.id == input.channel)
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "안내된 접수 방식을 선택해 주세요."))?;
    if channel.id == "not_required" {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "별도 신청이 없는 혜택이에요. 이용 조건을 확인해 주세요.",
        ));
    }
    let destination = crate::services::application_policy::for_channel(&plan.policy, channel.id);
    let mut snapshot = json!({"title":plan.title,"method":plan.policy.workflow.method_text,"channel":channel,"destination":destination,"evidence_kind":"user_reported"});
    let mut tx = pool.begin().await.map_err(crate::errors::internal_error)?;
    snapshot["program"] = sqlx::query_scalar::<_, Value>("SELECT application_snapshot_payload($1)")
        .bind(id)
        .fetch_one(&mut *tx)
        .await
        .map_err(crate::errors::internal_error)?;

    // Serialize operations for this user's program, including simultaneous starts.
    sqlx::query("INSERT INTO user_program_states(user_id,program_id,state) VALUES($1,$2,'applying') ON CONFLICT(user_id,program_id) DO NOTHING")
        .bind(user.id).bind(id).execute(&mut *tx).await.map_err(crate::errors::internal_error)?;
    let status: String = sqlx::query_scalar(
        "SELECT state FROM user_program_states WHERE user_id=$1 AND program_id=$2 FOR UPDATE",
    )
    .bind(user.id)
    .bind(id)
    .fetch_one(&mut *tx)
    .await
    .map_err(crate::errors::internal_error)?;
    let previous: Option<Journey> = sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM application_journeys WHERE user_id=$1 AND program_id=$2 FOR UPDATE"
    ))
    .bind(user.id)
    .bind(id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(crate::errors::internal_error)?;
    if let Some(previous) = previous {
        if previous.plan_revision == plan.revision && previous.channel == channel.id {
            tx.commit().await.map_err(crate::errors::internal_error)?;
            return Ok(Json(previous));
        }
        if previous.outcome == "reported_applied" {
            return Err(error(
                StatusCode::CONFLICT,
                "이미 접수 완료로 기록한 신청이에요. 신청 이력을 확인해 주세요.",
            ));
        }
    }
    if matches!(status.as_str(), "applied" | "waiting" | "received") {
        return Err(error(
            StatusCode::CONFLICT,
            "이미 신청 이력이 있어요. 접수 내역부터 확인해 주세요.",
        ));
    }
    let journey:Journey=sqlx::query_as(&format!("INSERT INTO application_journeys(user_id,program_id,plan_revision,channel,snapshot) VALUES($1,$2,$3,$4,$5) ON CONFLICT(user_id,program_id) DO UPDATE SET id=gen_random_uuid(),plan_revision=EXCLUDED.plan_revision,channel=EXCLUDED.channel,snapshot=EXCLUDED.snapshot,outcome='checking',handoff_at=now(),reported_at=NULL,receipt_reference=NULL,updated_at=now() RETURNING {COLUMNS}"))
        .bind(user.id).bind(id).bind(&plan.revision).bind(channel.id).bind(snapshot).fetch_one(&mut *tx).await.map_err(crate::errors::internal_error)?;
    sqlx::query("UPDATE user_program_states SET state='applying',updated_at=now() WHERE user_id=$1 AND program_id=$2")
        .bind(user.id).bind(id).execute(&mut *tx).await.map_err(crate::errors::internal_error)?;
    sqlx::query("INSERT INTO user_program_state_history(user_id,program_id,state,memo) VALUES($1,$2,'applying','신청 준비를 저장함. 기관 접수 완료 여부는 아직 확인되지 않음')")
        .bind(user.id).bind(id).execute(&mut *tx).await.map_err(crate::errors::internal_error)?;
    tx.commit().await.map_err(crate::errors::internal_error)?;
    Ok(Json(journey))
}
pub async fn report(
    user: AuthUser,
    State(pool): State<PgPool>,
    Path(id): Path<Uuid>,
    Json(input): Json<Report>,
) -> Result<Json<Journey>, Error> {
    if !matches!(input.outcome.as_str(), "unknown" | "reported_applied") {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "유효하지 않은 접수 상태예요.",
        ));
    }
    let receipt = input
        .receipt_reference
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if receipt.is_some_and(|s| s.chars().count() > 120 || s.chars().any(char::is_control)) {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "접수번호는 줄바꿈 없이 120자 이내로 입력해 주세요.",
        ));
    }
    if input.outcome == "reported_applied" && !input.confirmed {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "기관의 접수 완료 안내를 직접 확인했는지 체크해 주세요.",
        ));
    }
    if input.outcome == "unknown" && receipt.is_some() {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "접수번호가 확인됐다면 접수 완료 확인으로 기록해 주세요.",
        ));
    }
    let mut tx = pool.begin().await.map_err(crate::errors::internal_error)?;
    // Same lock order as start and the legacy state handlers.
    let status: Option<String> = sqlx::query_scalar(
        "SELECT state FROM user_program_states WHERE user_id=$1 AND program_id=$2 FOR UPDATE",
    )
    .bind(user.id)
    .bind(id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(crate::errors::internal_error)?;
    let previous: Journey = sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM application_journeys WHERE user_id=$1 AND program_id=$2 FOR UPDATE"
    ))
    .bind(user.id)
    .bind(id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(crate::errors::internal_error)?
    .ok_or_else(|| error(StatusCode::NOT_FOUND, "저장된 신청 준비가 없어요."))?;
    if previous.id != input.journey_id {
        return Err(error(
            StatusCode::CONFLICT,
            "다른 접수 방식으로 준비가 변경됐어요. 새 기록을 확인해 주세요.",
        ));
    }
    if previous.outcome == input.outcome && previous.receipt_reference.as_deref() == receipt {
        tx.commit().await.map_err(crate::errors::internal_error)?;
        return Ok(Json(previous));
    }
    if previous.outcome == "reported_applied" && input.outcome == "unknown" {
        return Err(error(
            StatusCode::CONFLICT,
            "접수 완료로 기록되어 있어요. 신청 이력에서 상태를 확인해 주세요.",
        ));
    }
    if input.outcome == "unknown"
        && matches!(status.as_deref(), Some("applied" | "waiting" | "received"))
    {
        return Err(error(
            StatusCode::CONFLICT,
            "이미 완료 또는 결과 상태로 기록되어 있어요. 신청 이력을 확인해 주세요.",
        ));
    }
    let result:Journey=sqlx::query_as(&format!("UPDATE application_journeys SET outcome=$3,receipt_reference=$4,reported_at=CASE WHEN $3='reported_applied' THEN COALESCE(reported_at,now()) ELSE reported_at END,updated_at=now() WHERE user_id=$1 AND program_id=$2 RETURNING {COLUMNS}"))
        .bind(user.id).bind(id).bind(&input.outcome).bind(receipt).fetch_one(&mut *tx).await.map_err(crate::errors::internal_error)?;
    if input.outcome == "reported_applied"
        && !matches!(status.as_deref(), Some("applied" | "waiting" | "received"))
    {
        // Preserve the public terms captured when the user left Mazimi, even if
        // the live notice has changed or expired before they return.
        sqlx::query("UPDATE application_program_snapshots SET program=$3,capture_reason='handoff_report',submitted=true WHERE user_id=$1 AND program_id=$2 AND NOT submitted")
            .bind(user.id).bind(id).bind(&previous.snapshot["program"])
            .execute(&mut *tx).await.map_err(crate::errors::internal_error)?;
        sqlx::query("UPDATE user_program_states SET state='applied',applied_at=COALESCE(applied_at,now()),updated_at=now() WHERE user_id=$1 AND program_id=$2")
            .bind(user.id).bind(id).execute(&mut *tx).await.map_err(crate::errors::internal_error)?;
    }
    let history_status = if input.outcome == "reported_applied" {
        if matches!(status.as_deref(), Some("waiting" | "received")) {
            status.as_deref().unwrap()
        } else {
            "applied"
        }
    } else {
        status.as_deref().unwrap_or("applying")
    };
    let memo = if input.outcome == "reported_applied" {
        "사용자가 기관 접수 완료를 직접 확인해 기록함. 기관 연동 검증은 아님"
    } else {
        "접수 여부 확인 필요. 기관의 접수 내역을 확인하기 전에는 중복 신청하지 않기"
    };
    sqlx::query(
        "INSERT INTO user_program_state_history(user_id,program_id,state,memo) VALUES($1,$2,$3,$4)",
    )
    .bind(user.id)
    .bind(id)
    .bind(history_status)
    .bind(memo)
    .execute(&mut *tx)
    .await
    .map_err(crate::errors::internal_error)?;
    tx.commit().await.map_err(crate::errors::internal_error)?;
    Ok(Json(result))
}
/// 신청 진행 단계 of the signed-in user (`stages` of their recorded
/// checklist, handoff, report, status and connector submission). The user
/// comes from the token only; the response is never shared-cached.
pub async fn progress(
    user: AuthUser,
    State(pool): State<PgPool>,
    Path(id): Path<Uuid>,
) -> Result<(axum::http::HeaderMap, Json<Value>), Error> {
    let mut connection = pool
        .acquire()
        .await
        .map_err(crate::errors::internal_error)?;
    let recorded = crate::services::application_procedure::progress(&mut connection, user.id, id)
        .await?
        .ok_or_else(|| error(StatusCode::NOT_FOUND, "혜택을 찾을 수 없어요."))?;
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("private, no-store"),
    );
    headers.insert(
        axum::http::header::VARY,
        axum::http::HeaderValue::from_static("Authorization"),
    );
    Ok((
        headers,
        Json(json!({"stages": mazimi_core::application_procedure::stages(&recorded)})),
    ))
}
