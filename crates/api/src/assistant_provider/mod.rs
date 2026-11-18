//! Model seams of the benefit assistant.
//!
//! - Topics: the deterministic [`rules`] classifier picks topics for every
//!   question. [`IntentProvider`] (model topic classification) is kept for
//!   the `assistant-eval` benchmark only; no request path calls it.
//! - Generation: [`AnswerProvider`] streams a short summary of passages the
//!   server already retrieved. It is optional, behind `ASSISTANT_AI_ENABLED`
//!   (default off), runs only on the user's opt-in and receives only the
//!   masked question, the server verdict sentence and the quoted public
//!   programme passages; profile, documents and credentials are never
//!   prompt inputs. Its output is validated before it is marked final.
//!
//! The private Ollama provider ([`ollama`]) implements both. A hosted
//! provider would implement [`AnswerProvider`]; none is wired.
mod ollama;
mod rules;
mod runtime;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

pub use ollama::{
    plan_examples, plan_prompt_version, OllamaOptions, OllamaProvider, PlanPrompt,
    DEFAULT_KEEP_ALIVE, DEFAULT_MODEL, GENERATION_MODEL, PLAN_EXAMPLES, PLAN_EXAMPLES_V3,
    PLAN_PROMPT_VERSION, TUNED_PROMPT_VERSION, TUNED_SYSTEM,
};
pub(crate) use rules::{
    any_cue, any_cue_at, credential_masked, marker_words, normalize, normalize_marker,
    words as cue_words, MASK,
};
pub use rules::{is_out_of_scope, redact_question, rule_topics};
pub use runtime::{
    env_flag, models_allowed, models_allowed_from, AnswerMode, AssistantRuntime, Generation,
    Refusal,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Topic {
    Summary,
    Eligibility,
    Documents,
    Deadline,
    Steps,
    Alerts,
    Unsupported,
}

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum ProviderError {
    /// Transport failure, non-success status or timeout. Counted by the
    /// availability breaker, separately from malformed output.
    #[error("provider unavailable")]
    Unavailable,
    /// The model answered but violated the output contract.
    #[error("invalid provider output")]
    InvalidOutput,
    /// The client stopped reading. Not a provider failure; never counted.
    #[error("generation cancelled")]
    Cancelled,
}

/// What a generated answer may draw on. Built by the server from its own
/// retrieval; nothing here comes from the client except the masked question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnswerRequest {
    pub question: String,
    pub verdict: Option<String>,
    pub passages: Vec<String>,
}

#[async_trait]
pub trait AnswerProvider: Send + Sync {
    /// Generate a 1-3 sentence Korean answer. Every text delta is sent to
    /// `deltas` as it arrives; the full text is returned at the end. A
    /// closed `deltas` means the client left: stop and return
    /// [`ProviderError::Cancelled`].
    async fn generate(
        &self,
        request: &AnswerRequest,
        deltas: mpsc::Sender<String>,
    ) -> Result<String, ProviderError>;
    async fn warm_up(&self) -> Result<(), ProviderError> {
        Ok(())
    }
}

#[async_trait]
pub trait IntentProvider: Send + Sync {
    async fn classify(&self, question: &str) -> Result<Vec<Topic>, ProviderError>;
    /// Optional model preload so the first user question does not pay the
    /// cold start. Providers without a warm-up succeed immediately.
    async fn warm_up(&self) -> Result<(), ProviderError> {
        Ok(())
    }
}

/// What the agent's model planner sees: the masked goal and two booleans
/// about the screen. No profile, programme text, ids or history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanRequest {
    pub goal: String,
    pub has_context_program: bool,
    pub selected_programs: usize,
    /// Set on the single repair attempt: the rejected reply and the
    /// validator's reason (spec §5.3).
    pub repair: Option<PlanRepair>,
    /// The agent-tools-v3 tools this deployment offers: only these appear
    /// in the prompt and the output schema (none = prompt `agent-plan-v4`).
    pub v3_tools: Vec<mazimi_core::agent::ToolName>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanRepair {
    /// The previous model output, cut to a bounded length.
    pub previous: String,
    /// Short validator message; never user text.
    pub error: String,
}

/// The agent's model planner: one call that returns the raw reply JSON.
/// `crate::agent::planner::interpret` is the authority on what it may say.
#[async_trait]
pub trait PlanProvider: Send + Sync {
    async fn plan(&self, request: &PlanRequest) -> Result<String, ProviderError>;
    /// Model name for run records and eval reports.
    fn model(&self) -> &str;
    /// The prompt version a plan with these v3 tools sends (run records).
    fn prompt_version(&self, v3_tools: &[mazimi_core::agent::ToolName]) -> String {
        plan_prompt_version(v3_tools)
    }
    /// Load the model and its prompt prefix before a user waits on them
    /// (`AgentRuntime::spawn_warm_keeper`). Providers without one succeed.
    async fn warm_up(&self) -> Result<(), ProviderError> {
        Ok(())
    }
    /// The digest of the model this provider serves (`sha256:…`), `None`
    /// when the server does not list it. Providers without one return `None`.
    async fn model_digest(&self) -> Result<Option<String>, ProviderError> {
        Ok(None)
    }
}

/// 1 to 3 distinct topics; `unsupported` only on its own.
pub(crate) fn validate_topics(topics: &[Topic]) -> bool {
    (1..=3).contains(&topics.len())
        && topics
            .iter()
            .enumerate()
            .all(|(i, t)| !topics[..i].contains(t))
        && (!topics.contains(&Topic::Unsupported) || topics.len() == 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topic_sets_are_bounded_and_unsupported_is_exclusive() {
        assert!(!validate_topics(&[]));
        assert!(!validate_topics(&[Topic::Documents, Topic::Documents]));
        assert!(!validate_topics(&[Topic::Unsupported, Topic::Documents]));
        assert!(!validate_topics(&[
            Topic::Summary,
            Topic::Documents,
            Topic::Deadline,
            Topic::Steps
        ]));
        assert!(validate_topics(&[Topic::Documents, Topic::Deadline]));
        assert!(validate_topics(&[Topic::Unsupported]));
    }
}
