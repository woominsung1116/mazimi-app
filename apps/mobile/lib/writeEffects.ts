// One map from a write to the cached reads it makes stale. A write the 비서
// executed after the user's approval and the same write from a button or
// switch both end here, so every screen that shows the row refetches it
// (an open detail page included) and none keeps its own key list.
// Pure: `scripts/check-write-effects.cjs` runs it against a real QueryClient.
import type { AgentConfirmation, AgentRun, ToolName } from './agentTypes';

type Scope = { userId: string | null | undefined; programId: string | null | undefined };
type KeyOf = (scope: Scope) => readonly unknown[] | null;

/** A key needing the programme is skipped when there is none (prefix keys stay scoped to the user). */
const perProgram = (name: string): KeyOf => ({ userId, programId }) => programId ? [name, userId, programId] : null;
const perUser = (name: string): KeyOf => ({ userId }) => [name, userId];

// Readers shared by several writes. Server sources: services/benefit_assistant
// reads bookmarks, alerts, preparations, journeys and the profile.
const briefing = perUser('assistant-briefing');
const guidance = perProgram('benefit-assistant');
// 신청 진행 단계: the checklist, handoff and report all move it.
const progress = perProgram('program-progress');

/**
 * Each confirm-write tool → the query keys (prefixes) whose data it changes.
 * An empty list: the server changes nothing (the device writes, or a screen opens).
 */
export const WRITE_EFFECTS: Record<Exclude<ToolName, (typeof READ_TOOLS)[number]>, readonly KeyOf[]> = {
  bookmark: [perUser('saved'), briefing, guidance],
  set_alert: [perProgram('alert-preference'), briefing, guidance],
  toggle_checklist_item: [perProgram('preparation'), progress, briefing, guidance],
  record_application_status: [perProgram('application'), perUser('applications'), perProgram('application-journey'), progress, briefing, guidance],
  // A changed profile changes recommendations and every condition check:
  // guidance for every programme (a 비서 card names one, the profile is not
  // per programme), and the public lists the server gates on the viewer's
  // profile (routes/programs.rs viewer_gate: household, employment).
  profile_patch: [
    ({ userId }) => ['profile', 'me', userId], () => ['recommend'], briefing, perUser('benefit-assistant'),
    perUser('assistant-bundle'), perUser('assistant-compare'),
    () => ['programs'], perUser('corporate-categories'), () => ['home-imminent'], () => ['home-browse'],
  ],
  add_to_calendar: [],
  open_form_fill: [],
  request_signature: [],
  open_official_application: [],
};

/** Tools that only read; listed so a new ToolName must be placed on one side. */
export const READ_TOOLS = [
  'search_benefits', 'check_eligibility', 'answer_from_notice', 'get_preparation', 'compare',
  'deadlines', 'check_official_page', 'document_issue_link', 'match_vault_documents',
] as const;

export type WriteKind = keyof typeof WRITE_EFFECTS;

/** The subset of QueryClient used here, so node checks can pass a plain client. */
export interface Invalidator { invalidateQueries(filters: { queryKey: readonly unknown[] }): Promise<unknown> }

export function writeQueryKeys(write: WriteKind, scope: Scope): (readonly unknown[])[] {
  return (WRITE_EFFECTS[write] ?? []).flatMap(keyOf => { const key = keyOf(scope); return key ? [key] : []; });
}

/**
 * After a write the server confirmed: active screens refetch, the rest refetch when shown.
 * `waitFor` picks the refetches the caller needs before it moves on (default: all);
 * the others start at once and finish in the background.
 */
export async function invalidateAfterWrite(client: Invalidator, write: WriteKind, scope: Scope, waitFor: (key: readonly unknown[]) => boolean = () => true): Promise<void> {
  const started = writeQueryKeys(write, scope).map(queryKey => ({ queryKey, done: client.invalidateQueries({ queryKey }) }));
  await Promise.all(started.filter(s => waitFor(s.queryKey)).map(s => s.done));
}

const isWrite = (tool: ToolName): tool is WriteKind => Object.prototype.hasOwnProperty.call(WRITE_EFFECTS, tool);

/**
 * After an approve tap, whatever the request returned or threw: the server's
 * own record of the card decides. `run` is the run re-read after the tap
 * (null if that read failed); `decided` is the approve answer (null if it
 * never arrived, e.g. the network dropped after the server committed).
 * - executed: the rows changed;
 * - stale: nothing was written because the server's state no longer matched
 *   the card, so this device's copy of those rows is the likely stale one.
 * Any other status changed nothing.
 */
export async function invalidateAfterApproval(
  client: Invalidator,
  { run, decided, confirmationId }: { run: AgentRun | null; decided: AgentConfirmation | null; confirmationId: string },
  userId: string | null | undefined,
): Promise<void> {
  const card = run?.confirmations.find(c => c.id === confirmationId) ?? decided;
  if (!card || (card.status !== 'executed' && card.status !== 'stale') || !isWrite(card.tool)) return;
  await invalidateAfterWrite(client, card.tool, { userId, programId: card.program_id });
}
