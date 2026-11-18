// Pure helpers of the 비서 screen. No network, no React: `npm run test:session` loads this file.
import type {
  AgentConfirmation, AgentEvent, AgentRun, AgentToolResult, RunStatus, StepStatus, SuggestedAction, ToolName,
} from './agentTypes';
import type { ReadinessTask } from './documentReadiness';

const EVENT_TYPES = new Set(['status', 'planning', 'plan', 'tool', 'awaiting_confirmation', 'backgrounded', 'done']);

/** Split an NDJSON buffer into agent events; the unfinished tail is returned as `rest`.
 * Lines that are not JSON objects of a known type are dropped, never thrown. */
export function parseAgentEvents(buffer: string): { events: AgentEvent[]; rest: string } {
  const lines = buffer.split('\n');
  const rest = lines.pop() ?? '';
  const events: AgentEvent[] = [];
  for (const line of lines) {
    const text = line.trim();
    if (!text) continue;
    try {
      const value = JSON.parse(text) as { type?: unknown };
      if (value && typeof value === 'object' && typeof value.type === 'string' && EVENT_TYPES.has(value.type)) {
        events.push(value as AgentEvent);
      }
    } catch { /* partial or foreign line */ }
  }
  return { events, rest };
}

export interface TimelineStep { key: string; tool: ToolName; label: string; status: StepStatus; write: boolean; detail?: string }
export interface Timeline { status: RunStatus | 'connecting'; steps: TimelineStep[]; planner?: string; backgrounded: boolean; awaiting: number }

export const initialTimeline: Timeline = { status: 'connecting', steps: [], backgrounded: false, awaiting: 0 };

/** Fold one progress event into the timeline. Keys are stable per plan step
 * (index) and per executed call (seq), never array positions. */
export function reduceTimeline(state: Timeline, event: AgentEvent): Timeline {
  switch (event.type) {
    case 'status': return { ...state, status: event.status };
    case 'planning': return { ...state, status: 'planning', planner: event.planner };
    case 'plan': return {
      ...state, planner: event.planner,
      steps: event.steps.map(s => ({ key: `plan-${s.index}`, tool: s.tool, label: s.label, status: 'pending', write: s.permission === 'confirm_write' })),
    };
    case 'tool': {
      // An inserted step (a relaxed re-search) has no plan index: a new row.
      const key = event.index === null || event.index === undefined ? `extra-${event.seq}` : `plan-${event.index}`;
      const first = state.steps.findIndex(s => s.key === key);
      const callKey = `call-${event.seq}`;
      const existing = state.steps.findIndex(s => s.key === callKey);
      const step: TimelineStep = { key: callKey, tool: event.tool, label: event.label, status: event.status, write: state.steps[first]?.write ?? false };
      const steps = [...state.steps];
      if (existing >= 0) steps[existing] = { ...steps[existing], status: event.status };
      else if (first >= 0) steps[first] = step; // the first call of a planned step replaces its placeholder
      else steps.push(step);
      return { ...state, status: 'tool_running', steps };
    }
    case 'awaiting_confirmation': return { ...state, status: 'awaiting_confirmation', awaiting: event.count };
    case 'backgrounded': return { ...state, backgrounded: true };
    case 'done': return event.run ? timelineFromRun(event.run) : { ...state, status: event.status ?? state.status };
  }
}

export function timelineFromRun(run: AgentRun): Timeline {
  return {
    status: run.status, planner: run.planner ?? undefined, backgrounded: false,
    awaiting: run.confirmations.filter(c => c.status === 'pending').length,
    steps: run.steps.map(s => ({ key: `call-${s.seq}`, tool: s.tool, label: s.label, status: s.status, write: s.permission === 'confirm_write', detail: s.detail })),
  };
}

/** How long to keep polling after a stream ends: the server's run timeout
 * (from GET /assistant/agent, which bounds it) plus a 30 s margin, so the
 * client never gives up on a run the server is still working on. 300 s
 * when the server sent none. */
export function pollBudgetMs(runTimeoutSecs?: number): number {
  const secs = typeof runTimeoutSecs === 'number' && Number.isFinite(runTimeoutSecs) && runTimeoutSecs > 0
    ? runTimeoutSecs : 300;
  return (secs + 30) * 1000;
}

export type PollOutcome = 'rested' | 'aborted' | 'still_running';

/** Poll a run until it rests: check, fetch, wait, for each delay, then one
 * last fetch after the final wait so a run that finished during it is seen.
 * `resting()` reads what the screen already knows (e.g. from the stream). */
export async function pollUntilResting(options: {
  delays: number[];
  fetchStatus: () => Promise<RunStatus | null>;
  resting: () => boolean;
  aborted: () => boolean;
  sleep?: (ms: number) => Promise<void>;
}): Promise<PollOutcome> {
  const sleep = options.sleep ?? (ms => new Promise<void>(resolve => setTimeout(resolve, ms)));
  for (const delay of options.delays) {
    if (options.aborted()) return 'aborted';
    if (options.resting()) return 'rested';
    const status = await options.fetchStatus();
    if (status && isResting(status)) return 'rested';
    await sleep(delay);
  }
  if (options.aborted()) return 'aborted';
  if (options.resting()) return 'rested';
  const status = await options.fetchStatus();
  if (status && isResting(status)) return 'rested';
  return options.aborted() ? 'aborted' : 'still_running';
}

/** Poll delays after a stream ends: 2 s growing to 10 s, covering the
 * budget (about 330 s for the default 300 s run timeout). */
export function pollDelaysMs(totalMs = pollBudgetMs()): number[] {
  const delays: number[] = [];
  let spent = 0; let delay = 2000;
  while (spent < totalMs) { delays.push(delay); spent += delay; delay = Math.min(10_000, Math.round(delay * 1.5)); }
  return delays;
}

export const runStatusLabels: Record<RunStatus | 'connecting', string> = {
  connecting: '생각 중', received: '생각 중', planning: '생각 중', tool_running: '정보 확인 중',
  awaiting_confirmation: '승인 기다리는 중', executing: '실행 중', validating_response: '답변 정리 중',
  completed: '완료', failed: '처리하지 못함', timed_out: '시간 초과', cancelled: '취소됨',
};

export const stepStatusLabels: Record<StepStatus, string> = {
  pending: '대기', running: '진행 중', ok: '완료', error: '실패', skipped: '변경 없음',
  awaiting_confirmation: '승인 필요', executed: '실행함', rejected: '취소함', expired: '만료', stale: '상태 바뀜', failed: '실패',
};

export function isResting(status: RunStatus | 'connecting'): boolean {
  return status === 'completed' || status === 'failed' || status === 'timed_out' || status === 'cancelled' || status === 'awaiting_confirmation';
}

/** The line under the progress state, if any. The model plans in about
 * 40-60 s on the server; the user learns that while waiting (요청 취소 is its own button). */
export function progressNote(timeline: Timeline): string | null {
  if (timeline.status === 'timed_out') return '시간이 너무 오래 걸려 멈췄어요. 요청을 짧게 나눠 다시 보내 주세요.';
  if (isResting(timeline.status)) return null;
  if (timeline.backgrounded) return '시간이 걸리고 있어요. 다른 화면을 봐도 계속 진행하고, 끝나면 알림으로 알려드려요.';
  if (timeline.planner === 'model' && timeline.status === 'planning') return '보통 1분 안에 끝나요.';
  return null;
}

/** Shown when polling ends while the run is still going. */
export const STILL_RUNNING_MESSAGE = '아직 끝나지 않았어요. 끝나면 알림으로 알려드리고, 기록 화면에서도 볼 수 있어요.';

/** A confirmation card may be approved only while pending and unexpired. */
export function canDecide(confirmation: AgentConfirmation, now: number = Date.now()): boolean {
  const expires = Date.parse(confirmation.expires_at);
  return confirmation.status === 'pending' && Number.isFinite(expires) && expires > now;
}

/**
 * The card's state once it is not waiting for a decision. A decidable card
 * has none: the answer above it already says nothing changes before 승인.
 */
export function confirmationStatusLabel(confirmation: AgentConfirmation, now: number = Date.now()): string | null {
  if (confirmation.status === 'pending') return canDecide(confirmation, now) ? null : '확인 시간이 지났어요';
  return ({ approved: '승인함', executed: '변경했어요', rejected: '변경하지 않았어요', expired: '확인 시간이 지났어요',
    failed: '변경하지 못했어요', stale: '그 사이 상태가 바뀌어 실행하지 않았어요' } as const)[confirmation.status];
}

/**
 * The notes under a card's 지금/승인하면 cells, mapped from the server's card
 * data only, the same for every tool: its `reversible` flag and its `note`
 * (while it can be decided). The facts are the server's; per-tool text here
 * repeated them (capture 2026-11-04 #08 said the form fill three times).
 */
export function confirmationNotes(confirmation: AgentConfirmation, open: boolean): string[] {
  const notes: string[] = [];
  if (!confirmation.reversible) notes.push('이 기록은 나중에 되돌리기 어려워요.');
  if (open && confirmation.note) notes.push(confirmation.note);
  return notes;
}

/** The body of an approval: the hash shown on this card, nothing else. */
export function approvalBody(confirmation: AgentConfirmation): { payload_hash: string } {
  return { payload_hash: confirmation.payload_hash };
}

/** Idempotency key per user action (8-128 chars of [A-Za-z0-9_-]). */
export function newIdempotencyKey(random: () => number = Math.random): string {
  let key = 'agent-';
  for (let i = 0; i < 24; i++) key += 'abcdefghijklmnopqrstuvwxyz0123456789'[Math.floor(random() * 36)];
  return key;
}

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

/** In-app destination of a suggested action. Only fixed app routes with
 * validated ids; never an external URL from a payload. */
export function actionRoute(action: SuggestedAction): string | null {
  const program = action.payload.program_id;
  const validProgram = typeof program === 'string' && UUID.test(program);
  switch (action.type) {
    case 'open_program': return validProgram ? `/programs/${program}` : null;
    case 'prepare_application':
      if (!validProgram) return null;
      // The checklist screen: a task is checked only by the user's own tap.
      return action.payload.focus === 'checklist' ? `/apply-assistant?programId=${program}` : `/application/${program}`;
    case 'open_form_fill': {
      const resource = action.payload.resource_id;
      return validProgram && typeof resource === 'string' && UUID.test(resource)
        ? `/form-fill?programId=${program}&resourceId=${resource}` : null;
    }
    case 'edit_profile': return '/profile-edit';
    default: return null;
  }
}

/** Chips the user may tap: goals (with the screen's programme when one is
 * needed), programme picks, and the status picker and profile routes
 * ("신청 상태 기록", "프로필 수정": each writes only on the user's own tap
 * there). Confirm actions render as
 * cards instead. */
export interface GoalChip { key: string; label: string; goal: string; programId?: string; programIds?: string[]; route?: string }
export function goalChips(actions: SuggestedAction[] | undefined, hasProgram: boolean): GoalChip[] {
  return (actions ?? []).flatMap((a, index): GoalChip[] => {
    if (a.type === 'goal' && typeof a.payload.goal === 'string') {
      // "3개 마감 알림 켜기": the found programmes go as the selection (1-4).
      const ids = Array.isArray(a.payload.program_ids)
        ? a.payload.program_ids.filter((id): id is string => typeof id === 'string' && UUID.test(id)).slice(0, 4) : [];
      return [{ key: `goal:${a.payload.goal_kind ?? index}`, label: a.label, goal: a.payload.goal, ...(ids.length ? { programIds: ids } : {}) }];
    }
    if (a.type === 'pick_program' && typeof a.payload.goal === 'string' && typeof a.payload.program_id === 'string' && UUID.test(a.payload.program_id)) {
      return [{ key: `pick:${a.payload.program_id}`, label: a.label, goal: a.payload.goal, programId: a.payload.program_id }];
    }
    if (a.type === 'prepare_application' && (a.payload.focus === 'status' || a.payload.focus === 'checklist')) {
      const route = actionRoute(a);
      return route ? [{ key: `route:${a.payload.focus}:${a.payload.program_id}`, label: a.label, goal: '', route }] : [];
    }
    if (a.type === 'edit_profile') return [{ key: 'route:profile', label: a.label, goal: '', route: '/profile-edit' }];
    return [];
  }).filter(chip => hasProgram || !chip.goal.startsWith('이 혜택') || chip.programId);
}

// ── Narrow readers of tool results (data is untyped JSON) ──

const str = (v: unknown): string | null => typeof v === 'string' ? v : null;
const num = (v: unknown): number | null => typeof v === 'number' && Number.isFinite(v) ? v : null;
const list = (v: unknown): Record<string, unknown>[] => Array.isArray(v) ? v.filter((x): x is Record<string, unknown> => !!x && typeof x === 'object') : [];

export interface BenefitRow {
  programId: string; title: string; status: string | null; deadline: string | null; note: string | null;
  /** Field tab chip ("주거"), D-day and why it fits (search rows). */
  field: string | null; dDay: string | null; reason: string | null;
}

/** Programmes listed by a search, deadlines or comparison result. */
export function benefitRows(result: AgentToolResult): BenefitRow[] {
  const rows = result.tool === 'search_benefits' ? list(result.data.results)
    : result.tool === 'deadlines' ? list(result.data.items)
      : result.tool === 'compare' ? list(result.data.programs) : [];
  return rows.flatMap(row => {
    const programId = str(row.program_id); const title = str(row.title_display) || str(row.title);
    if (!programId || !UUID.test(programId) || !title) return [];
    const days = num(row.deadline_days);
    const fits = Array.isArray(row.fit_reasons) ? row.fit_reasons.filter((r): r is string => typeof r === 'string') : [];
    return [{
      programId, title,
      field: str(row.field_label),
      dDay: days === null ? null : days === 0 ? '오늘 마감' : days > 0 ? `D-${days}` : null,
      reason: fits.length ? `맞는 조건: ${fits.join(', ')}` : null,
      status: str(row.eligibility_status),
      deadline: str(row.deadline) ?? str(row.deadline_at),
      note: str(row.next_task) ? `다음: ${str(row.next_task)}` : str(row.eligibility_summary)
        ?? (num(row.remaining_tasks) !== null ? `남은 준비 ${num(row.remaining_tasks)}개` : null),
    }];
  });
}

export interface ChecklistRow { id: string; title: string; done: boolean }
export function checklistRows(result: AgentToolResult): ChecklistRow[] {
  if (result.tool !== 'get_preparation') return [];
  return list(result.data.tasks).flatMap(t => {
    const id = str(t.id); const title = str(t.title);
    return id && title ? [{ id, title, done: t.done === true }] : [];
  });
}

export function verdictOf(result: AgentToolResult): 'pass' | 'fail' | 'unknown' | null {
  if (result.tool !== 'check_eligibility') return null;
  const status = result.data.status;
  return status === 'pass' || status === 'fail' || status === 'unknown' ? status : null;
}

export function noticePassages(result: AgentToolResult): { id: string; text: string; source: string }[] {
  if (result.tool !== 'answer_from_notice') return [];
  const answer = result.data.answer as Record<string, unknown> | undefined;
  return list(answer?.passages).flatMap(p => {
    const id = str(p.id); const text = str(p.text); const source = str(p.source_label);
    return id && text ? [{ id, text, source: source ?? '공고 원문' }] : [];
  });
}

export function formLinks(result: AgentToolResult): { resourceId: string; label: string }[] {
  if (result.tool !== 'open_form_fill') return [];
  return list(result.data.forms).flatMap(f => {
    const id = str(f.resource_id); const label = str(f.label);
    return id && UUID.test(id) ? [{ resourceId: id, label: label ?? '양식' }] : [];
  });
}

// ── 신청 준비 journey: 조건 → 준비 서류 → 서식 → 공식 신청 → 상태 기록 ──

/** The journey's later turns. The server's fast path routes these exact
 * goals (crates/api/src/agent/compose.rs FORM_GOAL, APPLIED_GOAL); the
 * whole journey runs with the model off. */
/** The journey's buttons: each tap asks for that handoff, which the server
 * then proposes as a confirmation card (the journey itself proposes none). */
export const JOURNEY_GOALS = { form: '이 혜택 서식 작성 도와줘', official: '이 혜택 공식 신청 페이지 열어줘', applied: '이 혜택 신청 완료로 기록해줘' } as const;

/** A `goal=` key another screen may pass to the agent tab. Keys only, never
 * free text, so a link cannot type a goal for the user. */
export function journeyGoal(key: unknown): string | null {
  return key === 'form' ? JOURNEY_GOALS.form : key === 'applied' ? JOURNEY_GOALS.applied : null;
}

/** Checklist tasks with what document matching needs (kind, detail). */
export function preparationTasks(result: AgentToolResult): ReadinessTask[] {
  if (result.tool !== 'get_preparation' || !result.ok) return [];
  return list(result.data.tasks).flatMap(t => {
    const id = str(t.id); const title = str(t.title);
    return id && title ? [{ id, title, detail: str(t.detail), kind: str(t.kind), done: t.done === true }] : [];
  });
}

export interface FormLink { resourceId: string; label: string }

/** Forms an executed `open_form_fill` card returned: in-app ids only. */
export function approvedForms(confirmation: AgentConfirmation): FormLink[] {
  if (confirmation.tool !== 'open_form_fill' || confirmation.status !== 'executed') return [];
  return list(confirmation.result?.forms).flatMap(f => {
    const id = str(f.resource_id); const label = str(f.label);
    return id && UUID.test(id) ? [{ resourceId: id, label: label ?? '양식' }] : [];
  });
}

/** The form helper with the device-built draft. Only after an approved card. */
export function draftRoute(programId: string | null | undefined, resourceId: string): string | null {
  return programId && UUID.test(programId) && UUID.test(resourceId)
    ? `/form-fill?programId=${programId}&resourceId=${resourceId}&draft=1` : null;
}

/** Where an approval takes the user at once: the form helper when the
 * approved card opened exactly one form; otherwise the card lists them. */
export function routeAfterApproval(confirmation: AgentConfirmation): string | null {
  const forms = approvedForms(confirmation);
  return forms.length === 1 ? draftRoute(confirmation.program_id, forms[0].resourceId) : null;
}

/** The official application page an approved handoff card opens (https only;
 * the opener still shows the domain before leaving). */
export function officialHandoffUrl(confirmation: AgentConfirmation): string | null {
  if (confirmation.tool !== 'open_official_application' || confirmation.status !== 'executed') return null;
  const url = str(confirmation.result?.url);
  return url && url.startsWith('https://') ? url : null;
}


/** The guided copy sheet + official page, when no template can be filled. */
export function guidedRoute(programId: string | undefined): string | null {
  return programId && UUID.test(programId) ? `/form-fill?programId=${programId}` : null;
}

/** Cards still open across every run of a conversation: one active run
 * per conversation, so any of them holds the next journey turn. */
export function pendingCards(runs: (AgentRun | null)[]): number {
  return runs.reduce((n, run) => n + (run?.confirmations.filter(c => canDecide(c)).length ?? 0), 0);
}

export interface Journey {
  programId: string;
  verdict: 'pass' | 'fail' | 'unknown' | null;
  tasks: ReadinessTask[];
  /** Cards of this run still open; the screen widens it to the whole conversation ([`pendingCards`]). */
  pendingCards: number;
}

/** The journey of a run that prepared one programme, or null. */
export function journeyOf(run: AgentRun | null): Journey | null {
  if (!run) return null;
  const prep = run.results.find(r => r.tool === 'get_preparation' && r.ok && r.program_id && UUID.test(r.program_id));
  const anchor = prep;
  if (!anchor?.program_id || !UUID.test(anchor.program_id)) return null;
  const check = run.results.find(r => r.tool === 'check_eligibility' && r.program_id === anchor.program_id);
  return {
    programId: anchor.program_id,
    verdict: check ? verdictOf(check) : null,
    tasks: preparationTasks(anchor),
    pendingCards: pendingCards([run]),
  };
}
