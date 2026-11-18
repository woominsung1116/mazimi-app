//! Persistence of conversations, runs, tool calls and confirmations. Every
//! query is scoped by the authenticated user id; another user's id reads as
//! not found (404), never as forbidden, so ids cannot be probed.
use super::{
    permission_str, ConfirmationPreview, ConfirmationView, RunView, StepView, StoredResult,
};
use axum::{http::StatusCode, Json};
use chrono::{DateTime, Utc};
use mazimi_core::agent::{RunStatus, ToolName, TOOLSET_VERSION};
use serde::Serialize;
use serde_json::{json, Value};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

pub type Error = (StatusCode, Json<Value>);

pub fn error(status: StatusCode, code: &str, message: &str) -> Error {
    (status, Json(json!({"error": message, "code": code})))
}

pub fn not_found() -> Error {
    error(
        StatusCode::NOT_FOUND,
        "not_found",
        "대화나 실행 기록을 찾을 수 없어요.",
    )
}

pub fn db(error: sqlx::Error) -> Error {
    crate::errors::internal_error(error)
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct ConversationRow {
    pub id: Uuid,
    pub title: String,
    pub context_program_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct RunRow {
    pub id: Uuid,
    pub conversation_id: Uuid,
    pub user_id: Uuid,
    pub status: String,
    pub planner: Option<String>,
    pub goal_kind: Option<String>,
    pub plan: Option<Value>,
    pub result: Option<Value>,
    pub context_program_id: Option<Uuid>,
    pub selected_program_ids: Vec<Uuid>,
    pub error_code: Option<String>,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ToolCallRow {
    pub id: Uuid,
    pub seq: i32,
    pub tool_name: String,
    pub permission: String,
    pub program_id: Option<Uuid>,
    pub status: String,
    pub error_code: Option<String>,
    pub latency_ms: Option<i32>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ConfirmationRow {
    pub id: Uuid,
    pub run_id: Uuid,
    pub user_id: Uuid,
    pub tool_call_id: Uuid,
    pub tool_name: String,
    pub program_id: Uuid,
    pub arguments: Value,
    pub precondition: Value,
    pub preview: Value,
    pub payload_hash: String,
    pub status: String,
    pub expires_at: DateTime<Utc>,
    pub result: Option<Value>,
}

impl ConfirmationRow {
    pub fn view(&self) -> ConfirmationView {
        let preview: ConfirmationPreview =
            serde_json::from_value(self.preview.clone()).unwrap_or(ConfirmationPreview {
                title: String::new(),
                action: String::new(),
                before: String::new(),
                after: String::new(),
                reversible: true,
                note: None,
            });
        ConfirmationView {
            id: self.id,
            tool: self.tool_name.clone(),
            // The nil id binds a profile write in the hash; clients get null.
            program_id: (!self.program_id.is_nil()).then_some(self.program_id),
            preview,
            payload_hash: self.payload_hash.clone(),
            expires_at: self.expires_at,
            status: self.status.clone(),
            result: self.result.clone(),
        }
    }
}

const RUN_COLUMNS: &str =
    "id, conversation_id, user_id, status, planner, goal_kind, plan, result, \
    context_program_id, selected_program_ids, error_code, created_at, completed_at";
const CONFIRMATION_COLUMNS: &str = "id, run_id, user_id, tool_call_id, tool_name, program_id, \
    arguments, precondition, preview, payload_hash, status, expires_at, result";

/// Expire overdue confirmations of one user (database function shared with
/// the worker's purge).
pub async fn expire_due(connection: &mut PgConnection, user: Uuid) -> Result<(), Error> {
    sqlx::query("SELECT assistant_expire_confirmations($1)")
        .bind(user)
        .execute(connection)
        .await
        .map_err(db)?;
    Ok(())
}

/// Close every pending confirmation of a run that is being cancelled or
/// timed out, with the same audit trail as an expiry.
pub async fn expire_run_confirmations(
    connection: &mut PgConnection,
    run: Uuid,
) -> Result<(), Error> {
    sqlx::query(
        "WITH closed AS (
            UPDATE assistant_confirmations SET status='expired', decided_at=now()
            WHERE run_id=$1 AND status='pending'
            RETURNING id, user_id, tool_call_id, tool_name
         ), calls AS (
            UPDATE assistant_tool_calls t SET status='expired' FROM closed c WHERE t.id=c.tool_call_id
            RETURNING t.id
         )
         INSERT INTO assistant_action_audit(user_id, confirmation_id, tool_name, outcome)
         SELECT user_id, id, tool_name, 'expired' FROM closed",
    )
    .bind(run)
    .execute(connection)
    .await
    .map_err(db)?;
    Ok(())
}

/// States a run may move to `to` from, per the core state machine.
fn sources(to: RunStatus) -> Vec<&'static str> {
    RunStatus::ALL
        .into_iter()
        .filter(|from| from.can_transition(to))
        .map(RunStatus::as_str)
        .collect()
}

/// Guarded transition: one UPDATE that only matches a legal source state.
/// `false` means the run was not in a state that may move to `to` (for
/// example it was cancelled meanwhile).
pub async fn transition(
    connection: &mut PgConnection,
    run: Uuid,
    to: RunStatus,
) -> Result<bool, Error> {
    let updated = sqlx::query(
        "UPDATE assistant_runs SET status=$2, updated_at=now(),
           completed_at = CASE WHEN $3 THEN now() ELSE completed_at END
         WHERE id=$1 AND status = ANY($4)",
    )
    .bind(run)
    .bind(to.as_str())
    .bind(to.is_terminal())
    .bind(sources(to))
    .execute(connection)
    .await
    .map_err(db)?;
    Ok(updated.rows_affected() == 1)
}

pub async fn create_conversation(
    pool: &PgPool,
    user: Uuid,
    context_program_id: Option<Uuid>,
) -> Result<ConversationRow, Error> {
    if let Some(program) = context_program_id {
        let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM programs WHERE id=$1)")
            .bind(program)
            .fetch_one(pool)
            .await
            .map_err(db)?;
        if !exists {
            return Err(error(
                StatusCode::NOT_FOUND,
                "program_not_found",
                "혜택을 찾을 수 없어요.",
            ));
        }
    }
    sqlx::query_as::<_, ConversationRow>(
        "INSERT INTO assistant_conversations(user_id, context_program_id) VALUES($1,$2)
         RETURNING id, title, context_program_id, created_at, updated_at",
    )
    .bind(user)
    .bind(context_program_id)
    .fetch_one(pool)
    .await
    .map_err(db)
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct ConversationSummary {
    pub id: Uuid,
    pub title: String,
    pub context_program_id: Option<Uuid>,
    pub updated_at: DateTime<Utc>,
    pub last_status: Option<String>,
    pub pending_confirmations: i64,
}

pub async fn list_conversations(
    pool: &PgPool,
    user: Uuid,
) -> Result<Vec<ConversationSummary>, Error> {
    let mut connection = pool.acquire().await.map_err(db)?;
    expire_due(&mut connection, user).await?;
    sqlx::query_as::<_, ConversationSummary>(
        "SELECT c.id, c.title, c.context_program_id, c.updated_at,
           (SELECT r.status FROM assistant_runs r WHERE r.conversation_id=c.id ORDER BY r.created_at DESC LIMIT 1) AS last_status,
           (SELECT count(*) FROM assistant_confirmations k JOIN assistant_runs r ON r.id=k.run_id
              WHERE r.conversation_id=c.id AND k.status='pending') AS pending_confirmations
         FROM assistant_conversations c WHERE c.user_id=$1
         ORDER BY c.updated_at DESC LIMIT 50",
    )
    .bind(user)
    .fetch_all(&mut *connection)
    .await
    .map_err(db)
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct MessageRow {
    pub id: Uuid,
    pub run_id: Option<Uuid>,
    pub role: String,
    pub content: String,
    pub citations: Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct ConversationDetail {
    #[serde(flatten)]
    pub conversation: ConversationRow,
    pub messages: Vec<MessageRow>,
    pub runs: Vec<RunView>,
}

pub async fn conversation(
    pool: &PgPool,
    user: Uuid,
    id: Uuid,
) -> Result<ConversationDetail, Error> {
    let mut connection = pool.acquire().await.map_err(db)?;
    expire_due(&mut connection, user).await?;
    let conversation = sqlx::query_as::<_, ConversationRow>(
        "SELECT id, title, context_program_id, created_at, updated_at
         FROM assistant_conversations WHERE id=$1 AND user_id=$2",
    )
    .bind(id)
    .bind(user)
    .fetch_optional(&mut *connection)
    .await
    .map_err(db)?
    .ok_or_else(not_found)?;
    let messages = sqlx::query_as::<_, MessageRow>(
        "SELECT id, run_id, role, content, citations, created_at FROM assistant_messages
         WHERE conversation_id=$1 ORDER BY created_at, id LIMIT 200",
    )
    .bind(id)
    .fetch_all(&mut *connection)
    .await
    .map_err(db)?;
    let run_ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM assistant_runs WHERE conversation_id=$1 AND user_id=$2
         ORDER BY created_at DESC LIMIT 20",
    )
    .bind(id)
    .bind(user)
    .fetch_all(&mut *connection)
    .await
    .map_err(db)?;
    let mut runs = Vec::with_capacity(run_ids.len());
    for run in run_ids.into_iter().rev() {
        runs.push(run_view(&mut connection, user, run).await?);
    }
    Ok(ConversationDetail {
        conversation,
        messages,
        runs,
    })
}

/// Hard delete; runs, messages, tool calls and confirmations cascade.
/// Returns the run ids that were still active so their tasks can stop.
pub async fn delete_conversation(pool: &PgPool, user: Uuid, id: Uuid) -> Result<Vec<Uuid>, Error> {
    let mut tx = pool.begin().await.map_err(db)?;
    let active: Vec<Uuid> = sqlx::query_scalar(
        "SELECT r.id FROM assistant_runs r JOIN assistant_conversations c ON c.id=r.conversation_id
         WHERE c.id=$1 AND c.user_id=$2 AND r.status <> ALL($3)",
    )
    .bind(id)
    .bind(user)
    .bind(
        RunStatus::ALL
            .into_iter()
            .filter(|s| s.is_terminal())
            .map(RunStatus::as_str)
            .collect::<Vec<_>>(),
    )
    .fetch_all(&mut *tx)
    .await
    .map_err(db)?;
    // Same lock order as approve/reject (confirmation, then run) so a
    // concurrent decision waits instead of deadlocking.
    sqlx::query(
        "SELECT k.id FROM assistant_confirmations k JOIN assistant_runs r ON r.id=k.run_id
         JOIN assistant_conversations c ON c.id=r.conversation_id
         WHERE c.id=$1 AND c.user_id=$2 ORDER BY k.id FOR UPDATE OF k",
    )
    .bind(id)
    .bind(user)
    .fetch_all(&mut *tx)
    .await
    .map_err(db)?;
    sqlx::query("SELECT r.id FROM assistant_runs r WHERE r.conversation_id=$1 AND r.user_id=$2 ORDER BY r.id FOR UPDATE")
        .bind(id)
        .bind(user)
        .fetch_all(&mut *tx)
        .await
        .map_err(db)?;
    let deleted = sqlx::query("DELETE FROM assistant_conversations WHERE id=$1 AND user_id=$2")
        .bind(id)
        .bind(user)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
    if deleted.rows_affected() == 0 {
        return Err(not_found());
    }
    tx.commit().await.map_err(db)?;
    Ok(active)
}

pub async fn run_row(
    connection: &mut PgConnection,
    user: Uuid,
    run: Uuid,
) -> Result<RunRow, Error> {
    sqlx::query_as::<_, RunRow>(&format!(
        "SELECT {RUN_COLUMNS} FROM assistant_runs WHERE id=$1 AND user_id=$2"
    ))
    .bind(run)
    .bind(user)
    .fetch_optional(connection)
    .await
    .map_err(db)?
    .ok_or_else(not_found)
}

pub async fn confirmations_of(
    connection: &mut PgConnection,
    run: Uuid,
) -> Result<Vec<ConfirmationRow>, Error> {
    sqlx::query_as::<_, ConfirmationRow>(&format!(
        "SELECT {CONFIRMATION_COLUMNS} FROM assistant_confirmations WHERE run_id=$1 ORDER BY created_at, id"
    ))
    .bind(run)
    .fetch_all(connection)
    .await
    .map_err(db)
}

/// Lock one confirmation of `user` for a decision.
pub async fn lock_confirmation(
    connection: &mut PgConnection,
    user: Uuid,
    id: Uuid,
) -> Result<ConfirmationRow, Error> {
    sqlx::query_as::<_, ConfirmationRow>(&format!(
        "SELECT {CONFIRMATION_COLUMNS} FROM assistant_confirmations WHERE id=$1 AND user_id=$2 FOR UPDATE"
    ))
    .bind(id)
    .bind(user)
    .fetch_optional(connection)
    .await
    .map_err(db)?
    .ok_or_else(not_found)
}

/// The timeline: consecutive calls of one tool are one step, labelled once
/// ([`super::steps`]).
fn step_views(calls: &[ToolCallRow], stored: &StoredResult) -> Vec<StepView> {
    let tools: Vec<Option<ToolName>> = calls
        .iter()
        .map(|c| ToolName::parse(&c.tool_name))
        .collect();
    super::steps::group(&tools)
        .into_iter()
        .map(|g| {
            let group = &calls[g.clone()];
            let first = &group[0];
            let tool = tools[g.start];
            let mut programmes: Vec<Uuid> = Vec::new();
            for id in group.iter().filter_map(|c| c.program_id) {
                if !programmes.contains(&id) {
                    programmes.push(id);
                }
            }
            let mut details: Vec<String> = Vec::new();
            for c in group {
                let detail = stored
                    .results
                    .iter()
                    .find(|r| r.seq == c.seq)
                    .and_then(|r| r.data["topic_line"].as_str());
                if let Some(d) = detail.filter(|d| !details.iter().any(|x| x == d)) {
                    details.push(d.to_owned());
                }
            }
            let status = super::steps::status(group.iter().map(|c| c.status.as_str())).to_owned();
            StepView {
                seq: first.seq,
                label: tool.map_or_else(
                    || "도구".to_owned(),
                    |t| super::steps::label(t, programmes.len(), group.len()),
                ),
                permission: tool
                    .map(|t| permission_str(t.permission()))
                    .unwrap_or("forbidden"),
                tool: first.tool_name.clone(),
                program_id: (programmes.len() == 1).then(|| programmes[0]),
                latency_ms: group
                    .iter()
                    .filter_map(|c| c.latency_ms)
                    .reduce(|a, b| a + b),
                detail: (!details.is_empty()).then(|| details.join(" / ")),
                error_code: group
                    .iter()
                    .find(|c| c.status == status)
                    .and_then(|c| c.error_code.clone()),
                status,
            }
        })
        .collect()
}

pub async fn run_view(
    connection: &mut PgConnection,
    user: Uuid,
    run: Uuid,
) -> Result<RunView, Error> {
    let row = run_row(connection, user, run).await?;
    let calls = sqlx::query_as::<_, ToolCallRow>(
        "SELECT id, seq, tool_name, permission, program_id, status, error_code, latency_ms
         FROM assistant_tool_calls WHERE run_id=$1 ORDER BY seq",
    )
    .bind(run)
    .fetch_all(&mut *connection)
    .await
    .map_err(db)?;
    let confirmations = confirmations_of(connection, run).await?;
    let stored: StoredResult = row
        .result
        .clone()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();
    Ok(RunView {
        id: row.id,
        conversation_id: row.conversation_id,
        status: row.status,
        planner: row.planner,
        goal_kind: row.goal_kind,
        plan: row.plan,
        steps: step_views(&calls, &stored),
        results: stored.results,
        message: stored.message,
        confirmations: confirmations.iter().map(ConfirmationRow::view).collect(),
        error_code: row.error_code,
        created_at: row.created_at,
        completed_at: row.completed_at,
    })
}

pub struct NewRun<'a> {
    pub user: Uuid,
    pub conversation: Uuid,
    pub goal_masked: &'a str,
    pub context_program_id: Option<Uuid>,
    pub selected: &'a [Uuid],
    pub idempotency_key: Option<&'a str>,
}

pub enum Begun {
    Started {
        run: Uuid,
        context: Option<Uuid>,
    },
    /// Same idempotency key as an earlier request: that run.
    Existing {
        run: Uuid,
    },
}

pub async fn begin_run(pool: &PgPool, input: NewRun<'_>) -> Result<Begun, Error> {
    let mut tx = pool.begin().await.map_err(db)?;
    let conversation = sqlx::query_as::<_, (Option<Uuid>, String)>(
        "SELECT context_program_id, title FROM assistant_conversations WHERE id=$1 AND user_id=$2 FOR UPDATE",
    )
    .bind(input.conversation)
    .bind(input.user)
    .fetch_optional(&mut *tx)
    .await
    .map_err(db)?;
    let Some((conversation_program, title)) = conversation else {
        return Err(not_found());
    };
    if let Some(key) = input.idempotency_key {
        if let Some(run) = sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM assistant_runs WHERE user_id=$1 AND idempotency_key=$2",
        )
        .bind(input.user)
        .bind(key)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?
        {
            return Ok(Begun::Existing { run });
        }
    }
    expire_due(&mut tx, input.user).await?;
    let active: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM assistant_runs WHERE conversation_id=$1 AND status = ANY($2)",
    )
    .bind(input.conversation)
    .bind(
        RunStatus::ALL
            .into_iter()
            .filter(|s| !s.is_terminal())
            .map(RunStatus::as_str)
            .collect::<Vec<_>>(),
    )
    .fetch_optional(&mut *tx)
    .await
    .map_err(db)?;
    if let Some(active) = active {
        return Err((
            StatusCode::CONFLICT,
            Json(
                json!({"error":"이 대화에서 진행 중인 요청이 있어요. 끝나거나 취소한 뒤 다시 보내 주세요.",
                "code":"run_in_progress","run_id":active}),
            ),
        ));
    }
    let context = input.context_program_id.or(conversation_program);
    let run: Uuid = sqlx::query_scalar(
        "INSERT INTO assistant_runs(conversation_id, user_id, context_program_id, selected_program_ids,
            idempotency_key, toolset_version)
         VALUES($1,$2,$3,$4,$5,$6) RETURNING id",
    )
    .bind(input.conversation)
    .bind(input.user)
    .bind(context)
    .bind(input.selected)
    .bind(input.idempotency_key)
    .bind(TOOLSET_VERSION)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(d) if d.is_unique_violation() => error(
            StatusCode::CONFLICT,
            "run_in_progress",
            "이 대화에서 진행 중인 요청이 있어요.",
        ),
        _ => db(e),
    })?;
    sqlx::query(
        "INSERT INTO assistant_messages(conversation_id, run_id, role, content) VALUES($1,$2,'user',$3)",
    )
    .bind(input.conversation)
    .bind(run)
    .bind(input.goal_masked)
    .execute(&mut *tx)
    .await
    .map_err(db)?;
    let new_title: String = if title.is_empty() {
        input.goal_masked.chars().take(40).collect()
    } else {
        title
    };
    sqlx::query(
        "UPDATE assistant_conversations SET updated_at=now(), title=$3 WHERE id=$1 AND user_id=$2",
    )
    .bind(input.conversation)
    .bind(input.user)
    .bind(new_title)
    .execute(&mut *tx)
    .await
    .map_err(db)?;
    tx.commit().await.map_err(db)?;
    Ok(Begun::Started { run, context })
}

/// The model that made a plan: Ollama with its prompt version, or the
/// intent classifier with its artefact version.
pub struct PlanSource<'a> {
    pub provider: &'a str,
    pub model: &'a str,
    pub prompt_version: Option<&'a str>,
}

/// Record the plan of a run still planning; a run cancelled meanwhile
/// keeps its row as it is.
pub async fn set_plan(
    pool: &PgPool,
    run: Uuid,
    planner: &str,
    goal_kind: Option<&str>,
    plan: Option<&Value>,
    source: Option<PlanSource<'_>>,
) -> Result<(), Error> {
    sqlx::query(
        "UPDATE assistant_runs SET planner=$2, goal_kind=$3, plan=$4, provider=$5, model=$6,
            prompt_version=$7, updated_at=now() WHERE id=$1 AND status='planning'",
    )
    .bind(run)
    .bind(planner)
    .bind(goal_kind)
    .bind(plan)
    .bind(source.as_ref().map(|s| s.provider))
    .bind(source.as_ref().map(|s| s.model))
    .bind(source.as_ref().and_then(|s| s.prompt_version))
    .execute(pool)
    .await
    .map_err(db)?;
    Ok(())
}

pub async fn insert_tool_call(
    pool: &PgPool,
    run: Uuid,
    seq: i32,
    tool: ToolName,
    program_id: Option<Uuid>,
    arguments_redacted: &Value,
) -> Result<Uuid, Error> {
    sqlx::query_scalar(
        "INSERT INTO assistant_tool_calls(run_id, seq, tool_name, tool_version, permission, program_id, arguments_redacted)
         VALUES($1,$2,$3,$4,$5,$6,$7) RETURNING id",
    )
    .bind(run)
    .bind(seq)
    .bind(tool.as_str())
    .bind(tool.version())
    .bind(permission_str(tool.permission()))
    .bind(program_id)
    .bind(arguments_redacted)
    .fetch_one(pool)
    .await
    .map_err(db)
}

pub async fn finish_tool_call(
    pool: &PgPool,
    id: Uuid,
    status: &str,
    summary: &Value,
    error_code: Option<&str>,
    latency_ms: i32,
) -> Result<(), Error> {
    sqlx::query(
        "UPDATE assistant_tool_calls SET status=$2, result_summary=$3, error_code=$4, latency_ms=$5 WHERE id=$1",
    )
    .bind(id)
    .bind(status)
    .bind(summary)
    .bind(error_code)
    .bind(latency_ms)
    .execute(pool)
    .await
    .map_err(db)?;
    Ok(())
}

pub async fn save_result(
    pool: &PgPool,
    run: Uuid,
    result: &StoredResult,
    latency_ms: i32,
    error_code: Option<&str>,
) -> Result<(), Error> {
    sqlx::query(
        "UPDATE assistant_runs SET result=$2, latency_ms=$3, error_code=COALESCE($4, error_code), updated_at=now() WHERE id=$1",
    )
    .bind(run)
    .bind(serde_json::to_value(result).map_err(crate::errors::internal_error)?)
    .bind(latency_ms)
    .bind(error_code)
    .execute(pool)
    .await
    .map_err(db)?;
    Ok(())
}

pub async fn add_message(
    connection: &mut PgConnection,
    conversation: Uuid,
    run: Uuid,
    role: &str,
    content: &str,
    citations: &Value,
) -> Result<(), Error> {
    let content: String = content.chars().take(2000).collect();
    sqlx::query(
        "INSERT INTO assistant_messages(conversation_id, run_id, role, content, citations) VALUES($1,$2,$3,$4,$5)",
    )
    .bind(conversation)
    .bind(run)
    .bind(role)
    .bind(content)
    .bind(citations)
    .execute(connection)
    .await
    .map_err(db)?;
    Ok(())
}

pub async fn audit(
    connection: &mut PgConnection,
    user: Uuid,
    confirmation: Uuid,
    tool: &str,
    outcome: &str,
) -> Result<(), Error> {
    sqlx::query(
        "INSERT INTO assistant_action_audit(user_id, confirmation_id, tool_name, outcome) VALUES($1,$2,$3,$4)",
    )
    .bind(user)
    .bind(confirmation)
    .bind(tool)
    .bind(outcome)
    .execute(connection)
    .await
    .map_err(db)?;
    Ok(())
}

pub async fn queue_push(pool: &PgPool, user: Uuid, run: Uuid, body: &str) -> Result<(), Error> {
    sqlx::query(
        "INSERT INTO assistant_notification_outbox(user_id, run_id, title, body) VALUES($1,$2,'마지미 비서',$3)",
    )
    .bind(user)
    .bind(run)
    .bind(body)
    .execute(pool)
    .await
    .map_err(db)?;
    Ok(())
}

#[cfg(test)]
mod step_view_tests {
    use super::*;

    fn call(seq: i32, tool: &str, program: Option<u128>, status: &str) -> ToolCallRow {
        ToolCallRow {
            id: Uuid::new_v4(),
            seq,
            tool_name: tool.into(),
            permission: "read".into(),
            program_id: program.map(Uuid::from_u128),
            status: status.into(),
            error_code: None,
            latency_ms: Some(10),
        }
    }

    /// User report (2026-10-31): "부산 사는 대학생인데 월세 지원 받을 수 있는
    /// 거 있을까?" showed 1 조건에 맞는 혜택 찾기, 2-4 내 조건 확인 ×3. The
    /// checks of the found programmes are one step.
    #[test]
    fn checks_of_found_programmes_are_one_timeline_step() {
        let calls = [
            call(0, "search_benefits", None, "ok"),
            call(1, "check_eligibility", Some(1), "ok"),
            call(2, "check_eligibility", Some(2), "ok"),
            call(3, "check_eligibility", Some(3), "error"),
        ];
        let steps = step_views(&calls, &StoredResult::default());
        let labels: Vec<&str> = steps.iter().map(|s| s.label.as_str()).collect();
        assert_eq!(labels, ["조건에 맞는 혜택 찾기", "내 조건 확인 · 혜택 3개"]);
        assert_eq!((steps[1].seq, steps[1].status.as_str()), (1, "ok"));
        assert_eq!(steps[1].program_id, None);
        assert_eq!(steps[1].latency_ms, Some(30));
    }
}
