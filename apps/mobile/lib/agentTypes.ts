// Types of the AI benefit agent API (crates/api/src/agent, docs/mazimi-ai-agent.md).

export type RunStatus =
  | 'received' | 'planning' | 'tool_running' | 'awaiting_confirmation' | 'executing'
  | 'validating_response' | 'completed' | 'failed' | 'timed_out' | 'cancelled';
export type StepStatus = 'pending' | 'running' | 'ok' | 'error' | 'skipped' | 'awaiting_confirmation'
  | 'executed' | 'rejected' | 'expired' | 'stale' | 'failed';
export type ToolName =
  | 'search_benefits' | 'check_eligibility' | 'answer_from_notice' | 'get_preparation' | 'compare'
  | 'deadlines' | 'open_form_fill' | 'bookmark' | 'set_alert' | 'toggle_checklist_item' | 'record_application_status'
  | 'profile_patch' | 'check_official_page' | 'document_issue_link' | 'add_to_calendar'
  | 'match_vault_documents' | 'request_signature' | 'open_official_application';

export interface AgentStatus {
  enabled: boolean;
  model_planner_available: boolean;
  retention_days: number;
  /** Server run timeout; the client polls at least this long. */
  run_timeout_secs?: number;
  goals: { kind: string; goal: string; needs_program: boolean }[];
}

export interface AgentEvidence {
  id: string; program_id?: string; kind: string; quote?: string;
  source_label: string; source_url?: string; captured_at?: string;
}

export interface AgentToolResult {
  seq: number; tool: ToolName; permission: 'read' | 'confirm_write'; program_id?: string;
  ok: boolean; status: 'ok' | 'error' | 'skipped' | 'awaiting_confirmation';
  // Tool-specific; read through the narrow helpers in lib/agent.ts.
  data: Record<string, unknown>;
  evidence: AgentEvidence[];
  error?: { code: string; message: string };
  executed_at: string;
}

export interface SuggestedAction {
  type: 'open_program' | 'prepare_application' | 'confirm' | 'goal' | 'open_form_fill' | 'edit_profile' | 'pick_program' | 'ask_user';
  label: string;
  payload: {
    program_id?: string; resource_id?: string; confirmation_id?: string; goal?: string; goal_kind?: string; fields?: string[];
    /** A goal about these programmes (sent as the selection). */
    program_ids?: string[];
    /** ask_user: the profile field and the one question. */
    field?: string; question?: string; focus?: string;
  };
}

export interface AgentMessage {
  answer: string;
  verdict: 'pass' | 'fail' | 'unknown' | 'not_evaluated';
  citations: { evidence_id: string; program_id?: string; source_label: string; source_url?: string; captured_at?: string; claim_ids: string[] }[];
  suggested_actions: SuggestedAction[];
  missing_profile_fields: string[];
  warnings: string[];
  /** What the agent did, in order ("조건 정리 → 주거 혜택 검색 → 자격 점검"). */
  plan_steps?: string[];
  /** The conditions a search ran with. */
  topic_line?: string;
  /** One question for a profile field that blocked a check. */
  question?: string;
}

export interface AgentConfirmation {
  /** null for a write that names no programme (profile_patch). */
  id: string; tool: ToolName; program_id: string | null;
  title: string; action: string; before: string; after: string; reversible: boolean;
  /** A fact the server's card states beyond action/before/after; absent on older cards. */
  note?: string | null;
  payload_hash: string; expires_at: string;
  status: 'pending' | 'approved' | 'rejected' | 'expired' | 'executed' | 'failed' | 'stale';
  result?: Record<string, unknown>;
}

export interface AgentStep {
  seq: number; tool: ToolName; label: string; permission: 'read' | 'confirm_write' | 'forbidden';
  status: StepStatus; program_id?: string; latency_ms?: number; error_code?: string;
  /** What the step searched with ("주거 · 부산 · 대학생 조건으로 찾았어요."). */
  detail?: string;
}

export interface AgentRun {
  id: string; conversation_id: string; status: RunStatus;
  planner: 'fast_path' | 'fast_path_fallback' | 'model' | 'refused' | 'fallback' | null;
  goal_kind: string | null;
  steps: AgentStep[];
  results: AgentToolResult[];
  message: AgentMessage | null;
  confirmations: AgentConfirmation[];
  error_code: string | null;
  created_at: string; completed_at: string | null;
}

export interface AgentConversationSummary {
  id: string; title: string; context_program_id: string | null; updated_at: string;
  last_status: RunStatus | null; pending_confirmations: number;
}

export interface AgentConversation {
  id: string; title: string; context_program_id: string | null; created_at: string; updated_at: string;
  messages: { id: string; run_id: string | null; role: 'user' | 'assistant' | 'tool_summary'; content: string; created_at: string }[];
  runs: AgentRun[];
}

export interface StartedRun { run_id: string; status: RunStatus; existing: boolean }

export interface Decision {
  confirmation: AgentConfirmation; run_status: RunStatus; replayed: boolean; message: string;
}

export interface ProactiveSettings { weekly_briefing: boolean; change_alerts: boolean; resume_suggestions: boolean }

export type AgentEvent =
  | { type: 'status'; status: RunStatus }
  | { type: 'planning'; planner: 'model' }
  | { type: 'plan'; planner: string; goal_kind: string; steps: { index: number; tool: ToolName; label: string; permission: string }[] }
  | { type: 'tool'; seq: number; index: number | null; total: number; tool: ToolName; label: string; status: StepStatus; program_id?: string | null }
  | { type: 'awaiting_confirmation'; count: number }
  | { type: 'backgrounded'; run_id: string }
  | { type: 'done'; status: RunStatus | null; run: AgentRun | null };
