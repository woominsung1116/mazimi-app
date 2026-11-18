//! AI 혜택 비서 에이전트 (docs/mazimi-ai-agent.md).
//!
//! `goal → PLAN (fast path | one model call) → EXECUTE (read tools; write
//! tools become confirmations) → CONFIRM (user approves the exact hash) →
//! RESULT (server-composed message from tool results)`.
//!
//! - Verdicts, amounts, dates and documents come from the existing services
//!   (`benefit_assistant`, `grounded_qa`, `application_preparation`,
//!   `benefit_comparison`, recommendations); the model never writes them.
//! - The model planner sees only the masked goal and two booleans about the
//!   screen; its output must parse into [`mazimi_core::agent::Plan`].
//! - No write runs from a plan. `confirm` issues a confirmation bound to the
//!   user, run, tool call, normalized args, precondition and expiry, and
//!   executes it once after approval, re-validating the current state.
//! - Everything is behind `ASSISTANT_AGENT_ENABLED` (default off); the
//!   agent-tools-v3 tools each have their own flag ([`ToolFlags`]).
#[cfg(test)]
mod acceptance_probe;
pub mod answer;
pub mod args;
pub mod calendar;
pub(crate) mod cards;
pub mod classifier;
pub mod compose;
pub mod confirm;
pub mod documents;
pub mod eval;
pub mod goals;
pub mod guard;
pub mod intent;
pub mod observe;
pub mod official_page;
pub mod planner;
pub mod run;
pub mod runtime;
pub mod steps;
pub mod store;
pub mod tools;

pub use runtime::{AgentConfig, AgentRuntime, ToolFlags};

use chrono::{DateTime, Utc};
use mazimi_core::agent::{Permission, ToolName};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// A citation a tool result carries. `quote` is stored programme text
/// (never model output); `source_url` passed the public-https policy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceRef {
    /// `<seq>.<n>`, unique within a run.
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub program_id: Option<Uuid>,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quote: Option<String>,
    pub source_label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub captured_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Ok,
    Error,
    /// A write that is already in the requested state; nothing to confirm.
    Skipped,
    AwaitingConfirmation,
}

impl StepStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Error => "error",
            Self::Skipped => "skipped",
            Self::AwaitingConfirmation => "awaiting_confirmation",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolError {
    pub code: String,
    pub message: String,
}

/// Spec §7.3 result envelope, one per executed (expanded) tool call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    pub seq: i32,
    pub tool: ToolName,
    pub permission: Permission,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub program_id: Option<Uuid>,
    pub ok: bool,
    pub status: StepStatus,
    pub data: Value,
    pub evidence: Vec<EvidenceRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ToolError>,
    pub executed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Citation {
    pub evidence_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub program_id: Option<Uuid>,
    pub source_label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub captured_at: Option<DateTime<Utc>>,
    pub claim_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SuggestedAction {
    /// `open_program`, `prepare_application`, `confirm`, `goal`,
    /// `open_form_fill`, `edit_profile`.
    #[serde(rename = "type")]
    pub kind: String,
    pub label: String,
    pub payload: Value,
}

/// Spec §10 response: assembled by the server from tool results.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentMessage {
    pub answer: String,
    /// `pass|fail|unknown|not_evaluated`, always from the rules.
    pub verdict: String,
    pub citations: Vec<Citation>,
    pub suggested_actions: Vec<SuggestedAction>,
    pub missing_profile_fields: Vec<String>,
    pub warnings: Vec<String>,
    /// The conditions a search ran with ("주거 · 부산 · 대학생 조건으로 찾았어요.").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topic_line: Option<String>,
    /// One question for a profile field that blocked a check (`ask_user`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfirmationPreview {
    pub title: String,
    pub action: String,
    pub before: String,
    pub after: String,
    pub reversible: bool,
    /// A fact the card states that action/before/after do not (a device
    /// permission, where a value is kept). Shown while the card is pending.
    /// Optional on the wire and in stored rows: older rows and app builds
    /// without it keep working.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ConfirmationView {
    pub id: Uuid,
    pub tool: String,
    /// `None` for a write that names no programme (`profile_patch`).
    pub program_id: Option<Uuid>,
    #[serde(flatten)]
    pub preview: ConfirmationPreview,
    pub payload_hash: String,
    pub expires_at: DateTime<Utc>,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StepView {
    /// The first call's seq: consecutive calls of one tool are one step
    /// ([`steps`]).
    pub seq: i32,
    pub tool: String,
    pub label: String,
    pub permission: &'static str,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub program_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    /// What the step found or used ("주거 · 부산 · 대학생 조건으로 찾았어요.").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RunView {
    pub id: Uuid,
    pub conversation_id: Uuid,
    pub status: String,
    pub planner: Option<String>,
    pub goal_kind: Option<String>,
    pub plan: Option<Value>,
    pub steps: Vec<StepView>,
    pub results: Vec<ToolResult>,
    pub message: Option<AgentMessage>,
    pub confirmations: Vec<ConfirmationView>,
    pub error_code: Option<String>,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

/// What a run stores in `assistant_runs.result`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StoredResult {
    pub results: Vec<ToolResult>,
    pub message: Option<AgentMessage>,
}

pub fn permission_str(p: Permission) -> &'static str {
    match p {
        Permission::Read => "read",
        Permission::ConfirmWrite => "confirm_write",
    }
}
