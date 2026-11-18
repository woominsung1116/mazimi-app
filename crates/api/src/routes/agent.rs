//! HTTP surface of the AI benefit agent (docs/mazimi-ai-agent.md §API).
//! user_id always comes from the JWT; other users' ids read as 404.
//! Starting runs and deciding confirmations need `ASSISTANT_AGENT_ENABLED`;
//! reading and deleting history always work so users keep control of it.
use crate::{
    agent::{
        confirm,
        goals::GOAL_CHIPS,
        run::{self, StartInput},
        store::{self, error, Error},
    },
    auth::{AdminUser, AuthUser},
    AppState,
};
use axum::{
    body::{Body, Bytes},
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::{broadcast::error::RecvError, mpsc};
use uuid::Uuid;

fn require_enabled(app: &AppState) -> Result<(), Error> {
    if app.agent.enabled() {
        Ok(())
    } else {
        Err(error(
            StatusCode::NOT_FOUND,
            "agent_disabled",
            "AI 비서는 아직 열려 있지 않아요.",
        ))
    }
}

fn idempotency_key(headers: &HeaderMap) -> Option<String> {
    headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim().to_owned())
}

/// GET /api/v1/assistant/agent: what the client may show.
pub async fn status(_user: AuthUser, State(app): State<AppState>) -> Json<Value> {
    Json(json!({
        "enabled": app.agent.enabled(),
        "model_planner_available": app.agent.enabled() && app.agent.model_planner_available(),
        "retention_days": app.agent.config.retention_days,
        // The client keeps polling at least this long, so it never gives
        // up on a run the server is still working on.
        "run_timeout_secs": app.agent.config.run_timeout.as_secs(),
        "goals": GOAL_CHIPS.iter().map(|(kind, goal, needs_program)| json!({
            "kind": kind, "goal": goal, "needs_program": needs_program
        })).collect::<Vec<_>>(),
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewConversation {
    #[serde(default)]
    context_program_id: Option<Uuid>,
}

pub async fn create_conversation(
    user: AuthUser,
    State(app): State<AppState>,
    Json(input): Json<NewConversation>,
) -> Result<(StatusCode, Json<store::ConversationRow>), Error> {
    require_enabled(&app)?;
    store::create_conversation(&app.pool, user.id, input.context_program_id)
        .await
        .map(|c| (StatusCode::CREATED, Json(c)))
}

pub async fn list_conversations(
    user: AuthUser,
    State(app): State<AppState>,
) -> Result<Json<Value>, Error> {
    let items = store::list_conversations(&app.pool, user.id).await?;
    Ok(Json(
        json!({"items": items, "retention_days": app.agent.config.retention_days}),
    ))
}

pub async fn get_conversation(
    user: AuthUser,
    State(app): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<store::ConversationDetail>, Error> {
    store::conversation(&app.pool, user.id, id).await.map(Json)
}

pub async fn delete_conversation(
    user: AuthUser,
    State(app): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, Error> {
    for run in store::delete_conversation(&app.pool, user.id, id).await? {
        if let Some(channel) = app.agent.channel(run) {
            channel.cancel();
        }
    }
    Ok(Json(json!({"deleted": true})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewMessage {
    goal: String,
    #[serde(default)]
    program_id: Option<Uuid>,
    #[serde(default)]
    program_ids: Vec<Uuid>,
}

/// `X-Mazimi-Client-Caps`: comma-separated features the client handles.
pub(crate) fn client_caps(headers: &HeaderMap) -> impl Iterator<Item = &str> {
    headers
        .get("x-mazimi-client-caps")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|c| !c.is_empty() && c.len() <= 32)
        .take(8)
}

/// POST /api/v1/assistant/conversations/{id}/messages → 202 {run_id}.
pub async fn post_message(
    user: AuthUser,
    State(app): State<AppState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(input): Json<NewMessage>,
) -> Result<(StatusCode, Json<run::Started>), Error> {
    require_enabled(&app)?;
    let started = run::start(
        &app,
        user.id,
        id,
        StartInput {
            goal: input.goal,
            program_id: input.program_id,
            program_ids: input.program_ids,
            idempotency_key: idempotency_key(&headers),
            journey_client: client_caps(&headers).any(|c| c == "journey-v1"),
        },
    )
    .await?;
    Ok((StatusCode::ACCEPTED, Json(started)))
}

pub async fn get_run(
    user: AuthUser,
    State(app): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<crate::agent::RunView>, Error> {
    let mut connection = app.pool.acquire().await.map_err(store::db)?;
    store::expire_due(&mut connection, user.id).await?;
    store::run_view(&mut connection, user.id, id)
        .await
        .map(Json)
}

fn line(event: &Value) -> Bytes {
    let mut text = event.to_string();
    text.push('\n');
    Bytes::from(text)
}

/// GET /api/v1/assistant/runs/{id}/events: NDJSON progress. Replays what
/// happened so far, then live events until `done`. After the background
/// threshold it sends `backgrounded` and closes; the run continues and a
/// push is queued when it ends. Finished or unknown-to-this-process runs
/// get one `done` snapshot from the database.
pub async fn run_events(
    user: AuthUser,
    State(app): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Response, Error> {
    let mut connection = app.pool.acquire().await.map_err(store::db)?;
    let view = store::run_view(&mut connection, user.id, id).await?;
    drop(connection);
    let (out, mut lines) = mpsc::channel::<Bytes>(64);
    match app.agent.channel(id) {
        Some(channel) => {
            let (log, mut live) = channel.subscribe();
            let wait = app.agent.config.background_after;
            tokio::spawn(async move {
                for event in &log {
                    if out.send(line(event)).await.is_err() {
                        return;
                    }
                    if event["type"] == "done" {
                        return;
                    }
                }
                let deadline = tokio::time::sleep(wait);
                tokio::pin!(deadline);
                loop {
                    tokio::select! {
                        received = live.recv() => match received {
                            Ok(event) => {
                                let done = event["type"] == "done";
                                if out.send(line(&event)).await.is_err() || done {
                                    return;
                                }
                            }
                            Err(RecvError::Lagged(_)) => continue,
                            Err(RecvError::Closed) => return,
                        },
                        () = &mut deadline => {
                            let _ = out.send(line(&json!({"type":"backgrounded","run_id":id}))).await;
                            return;
                        }
                        () = out.closed() => return,
                    }
                }
            });
        }
        None => {
            let status = view.status.clone();
            let _ = out.try_send(line(&json!({"type":"done","status":status,"run":view})));
        }
    }
    let body = Body::from_stream(futures_util::stream::poll_fn(move |cx| {
        lines
            .poll_recv(cx)
            .map(|next| next.map(Ok::<_, std::convert::Infallible>))
    }));
    let mut response = body.into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/x-ndjson; charset=utf-8"),
    );
    response
        .headers_mut()
        .insert("x-accel-buffering", HeaderValue::from_static("no"));
    Ok(response)
}

pub async fn cancel_run(
    user: AuthUser,
    State(app): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, Error> {
    run::cancel(&app, user.id, id).await.map(Json)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Approval {
    payload_hash: String,
}

fn admit_decision(app: &AppState, user: Uuid) -> Result<(), Error> {
    if app.agent.admit_decision(user) {
        Ok(())
    } else {
        Err(error(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "요청이 너무 많아요. 잠시 후 다시 시도해 주세요.",
        ))
    }
}

pub async fn approve(
    user: AuthUser,
    State(app): State<AppState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(input): Json<Approval>,
) -> Result<Json<confirm::DecisionView>, Error> {
    require_enabled(&app)?;
    admit_decision(&app, user.id)?;
    let key = idempotency_key(&headers);
    let key = run::valid_idempotency_key(key.as_deref())?;
    if input.payload_hash.len() != 64 || !input.payload_hash.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "invalid_payload_hash",
            "확인 카드 정보가 올바르지 않아요.",
        ));
    }
    confirm::approve(
        &app.pool,
        user.id,
        id,
        &input.payload_hash,
        key,
        app.agent.config.tools,
    )
    .await
    .map(Json)
}

pub async fn reject(
    user: AuthUser,
    State(app): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<confirm::DecisionView>, Error> {
    require_enabled(&app)?;
    admit_decision(&app, user.id)?;
    confirm::reject(&app.pool, user.id, id).await.map(Json)
}

#[derive(Deserialize, serde::Serialize, sqlx::FromRow)]
#[serde(deny_unknown_fields)]
pub struct Proactive {
    weekly_briefing: bool,
    change_alerts: bool,
    resume_suggestions: bool,
}

pub async fn get_proactive(
    user: AuthUser,
    State(app): State<AppState>,
) -> Result<Json<Value>, Error> {
    let settings = sqlx::query_as::<_, Proactive>(
        "SELECT weekly_briefing, change_alerts, resume_suggestions FROM assistant_proactive_settings WHERE user_id=$1",
    )
    .bind(user.id)
    .fetch_optional(&app.pool)
    .await
    .map_err(store::db)?
    .unwrap_or(Proactive {
        weekly_briefing: false,
        change_alerts: false,
        resume_suggestions: false,
    });
    Ok(Json(json!({
        "settings": settings,
        "quiet_hours": "21:00-08:59 KST",
        "note": "켜 둔 항목만, 알림 설정에서 푸시를 허용한 경우에 하루 최대 1건 보내요.",
    })))
}

pub async fn put_proactive(
    user: AuthUser,
    State(app): State<AppState>,
    Json(input): Json<Proactive>,
) -> Result<Json<Value>, Error> {
    sqlx::query(
        "INSERT INTO assistant_proactive_settings(user_id, weekly_briefing, change_alerts, resume_suggestions)
         VALUES($1,$2,$3,$4)
         ON CONFLICT(user_id) DO UPDATE SET weekly_briefing=$2, change_alerts=$3, resume_suggestions=$4, updated_at=now()",
    )
    .bind(user.id)
    .bind(input.weekly_briefing)
    .bind(input.change_alerts)
    .bind(input.resume_suggestions)
    .execute(&app.pool)
    .await
    .map_err(store::db)?;
    if !input.change_alerts {
        // Stop tracking what the user saw once they opt out.
        sqlx::query("DELETE FROM assistant_program_fingerprints WHERE user_id=$1")
            .bind(user.id)
            .execute(&app.pool)
            .await
            .map_err(store::db)?;
    }
    Ok(Json(json!({"settings": input})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditQuery {
    #[serde(default)]
    limit: Option<i64>,
}

/// GET /api/v1/admin/assistant/runs: read-only operations view. Metadata
/// only: no goals, messages, results or user ids.
pub async fn admin_runs(
    _: AdminUser,
    State(app): State<AppState>,
    Query(query): Query<AuditQuery>,
) -> Result<Json<Value>, Error> {
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    let rows = sqlx::query_as::<_, (Uuid, String, Option<String>, Option<String>, Option<String>, Option<i32>, Option<String>, chrono::DateTime<chrono::Utc>, Value, i64, i64)>(
        "SELECT r.id, r.status, r.planner, r.goal_kind, r.model, r.latency_ms, r.error_code, r.created_at,
           COALESCE((SELECT jsonb_agg(jsonb_build_object('seq',t.seq,'tool',t.tool_name,'permission',t.permission,
               'status',t.status,'latency_ms',t.latency_ms,'error_code',t.error_code) ORDER BY t.seq)
             FROM assistant_tool_calls t WHERE t.run_id=r.id), '[]'::jsonb),
           (SELECT count(*) FROM assistant_confirmations c WHERE c.run_id=r.id),
           (SELECT count(*) FROM assistant_confirmations c WHERE c.run_id=r.id AND c.status='executed')
         FROM assistant_runs r ORDER BY r.created_at DESC LIMIT $1",
    )
    .bind(limit)
    .fetch_all(&app.pool)
    .await
    .map_err(store::db)?;
    let totals = sqlx::query_as::<_, (String, i64)>(
        "SELECT status, count(*) FROM assistant_runs WHERE created_at > now() - interval '7 days' GROUP BY status",
    )
    .fetch_all(&app.pool)
    .await
    .map_err(store::db)?;
    Ok(Json(json!({
        "items": rows.into_iter().map(|(id, status, planner, goal_kind, model, latency_ms, error_code, created_at, tools, confirmations, executed)| json!({
            "id": id, "status": status, "planner": planner, "goal_kind": goal_kind, "model": model,
            "latency_ms": latency_ms, "error_code": error_code, "created_at": created_at,
            "tools": tools, "confirmations": confirmations, "executed": executed,
        })).collect::<Vec<_>>(),
        "last_7_days": totals.into_iter().map(|(s, n)| json!({"status": s, "count": n})).collect::<Vec<_>>(),
    })))
}
