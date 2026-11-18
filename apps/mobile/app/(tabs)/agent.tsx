import React, { useCallback, useEffect, useRef, useState } from 'react';
import { KeyboardAvoidingView, Platform, Pressable, ScrollView, StyleSheet, TextInput, View } from 'react-native';
import { useFocusEffect, useLocalSearchParams, useRouter } from 'expo-router';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import Text from '@/components/Text';
import { EmptyState, ScreenHeader, ui } from '@/components/FigmaUI';
import { ConfirmationCard, MessageCard, ResultCard, RunTimeline } from '@/components/AgentCards';
import AgentJourney from '@/components/AgentJourney';
import { api } from '@/lib/api';
import { streamAgentRun } from '@/lib/agentStream';
import type { AgentConfirmation, AgentRun } from '@/lib/agentTypes';
import { approvalBody, draftRoute, goalChips, pendingCards, initialTimeline, isResting, journeyGoal, journeyOf, newIdempotencyKey, pollBudgetMs, pollDelaysMs, officialHandoffUrl, pollUntilResting, reduceTimeline, routeAfterApproval, STILL_RUNNING_MESSAGE, timelineFromRun, type Timeline } from '@/lib/agent';
import { useAuthStore } from '@/store/auth';
import { calendarEventOf, calendarNotice } from '@/lib/calendarEvent';
import { addEventOnThisDevice } from '@/lib/deviceCalendar';
import NonMemberState from '@/components/NonMemberState';
import AssistantBriefingCard from '@/components/AssistantBriefingCard';
import { openApplyUrl } from '@/lib/openExternalUrl';
import { invalidateAfterApproval } from '@/lib/writeEffects';
import { decideLocal, dispatchMessage, FORM_GOAL, handChatValues, heldContext, holdDictation, localRun, localValues, openFormContext, parseLocal, takeHeldDictation, LOCAL_PREFIX, type FormContext, type LocalFill } from '@/lib/formFill/chatFill';
import { parseTemplate } from '@/lib/formFill/fields';

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

/** `local`: a turn handled on this device (form values in form context); never sent. */
interface Entry { runId: string; goal: string; timeline: Timeline; run: AgentRun | null; error?: string; local?: LocalFill }

/** The form context for values said in chat (lib/formFill/chatFill.ts).
 * null only when the programme has no form. A form that fails to load, or
 * several forms, is still form context (held): it fails closed. */
async function loadFormContext(programId: string): Promise<FormContext | null> {
  const program = await api.getProgram(programId);
  const templates = program.application_policy?.resources?.templates ?? [];
  if (!templates.length) return null;
  if (templates.length > 1) return heldContext(programId, 'several');
  const resource = templates[0];
  const raw = await api.getFormTemplate(programId, resource.id).catch(() => null);
  if (!raw) return heldContext(programId, 'failed');
  try { return { programId, resourceId: resource.id, template: parseTemplate(raw, resource.content_hash ?? '', resource.file_format ?? null) }; } catch { return heldContext(programId, 'failed'); }
}

export default function AgentScreen() {
  const user = useAuthStore(s => s.user);
  const router = useRouter();
  const params = useLocalSearchParams<{ programId?: string; programIds?: string; goal?: string }>();
  const programId = typeof params.programId === 'string' && UUID.test(params.programId) ? params.programId : undefined;
  // A journey key from another screen (form helper → 상태 기록); never free text.
  const initialGoal = programId ? journeyGoal(params.goal) ?? undefined : undefined;
  const selected = typeof params.programIds === 'string' ? params.programIds.split(',').filter(id => UUID.test(id)).slice(0, 4) : [];
  const status = useQuery({ queryKey: ['agent-status', user?.id], queryFn: ({ signal }) => api.getAgentStatus(signal), enabled: !!user, staleTime: 60_000 });
  return <View style={ui.screen}>
    <ScreenHeader title="마지미 비서" action={user ? <Pressable accessibilityRole="button" accessibilityLabel="대화 기록" onPress={() => router.push('/agent-history')} style={ui.iconButton}><Text style={s.link}>기록</Text></Pressable> : undefined} />
    {!user ? <NonMemberState title="로그인하고 비서에게 맡겨 보세요" description="저장한 혜택과 준비 기록을 바탕으로 할 일을 정리해요." />
      : status.isLoading ? <EmptyState title="비서를 불러오는 중이에요" loading />
        : !status.data?.enabled ? <ScrollView contentContainerStyle={s.content}><AssistantBriefingCard /><EmptyState title="AI 비서는 준비 중이에요" description="혜택 상세의 ‘내 조건·남은 준비 확인’은 지금도 쓸 수 있어요." action="혜택 찾기" onAction={() => router.push('/explore')} /></ScrollView>
          : <Assistant key={`${user.id}:${programId ?? ''}:${selected.join(',')}`} userId={user.id} programId={programId} selected={selected} initialGoal={initialGoal}
            chips={status.data.goals} runTimeoutSecs={status.data.run_timeout_secs} />}
  </View>;
}

function Assistant({ userId, programId, selected, chips, runTimeoutSecs, initialGoal }: {
  userId: string; programId?: string; selected: string[]; chips: { kind: string; goal: string; needs_program: boolean }[]; runTimeoutSecs?: number; initialGoal?: string;
}) {
  const router = useRouter();
  const client = useQueryClient();
  const [goal, setGoal] = useState('');
  const [conversationId, setConversationId] = useState<string | null>(null);
  const [entries, setEntries] = useState<Entry[]>([]);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState('');
  const [notice, setNotice] = useState('');
  const streams = useRef(new Map<string, AbortController>());
  useEffect(() => () => { streams.current.forEach(c => c.abort()); streams.current.clear(); }, []);
  const update = (runId: string, change: (e: Entry) => Entry) => setEntries(list => list.map(e => e.runId === runId ? change(e) : e));
  const sameUser = () => useAuthStore.getState().user?.id === userId;

  const refresh = useCallback(async (runId: string) => {
    try {
      const run = await api.getAgentRun(runId);
      // A resting run clears a stale "still running" or stream error.
      if (sameUser()) update(runId, e => ({ ...e, run, timeline: timelineFromRun(run), error: isResting(run.status) ? undefined : e.error }));
      return run;
    } catch { return null; }
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [userId]);

  // Runs with a live stream or poll; the focus refresh skips them.
  const following = useRef(new Set<string>());
  const entriesRef = useRef(entries);
  useEffect(() => { entriesRef.current = entries; }, [entries]);

  // On focus only (not on every entries change): re-read resting-pending
  // runs nobody is following, e.g. after the app was in the background.
  useFocusEffect(useCallback(() => {
    entriesRef.current
      .filter(e => !isResting(e.timeline.status) && !following.current.has(e.runId))
      .forEach(e => { void refresh(e.runId); });
  }, [refresh]));

  async function follow(runId: string) {
    const controller = new AbortController();
    streams.current.set(runId, controller);
    following.current.add(runId);
    try {
      try {
        await streamAgentRun(runId, event => {
          update(runId, e => ({ ...e, timeline: reduceTimeline(e.timeline, event), run: event.type === 'done' && event.run ? event.run : e.run }));
        }, controller.signal);
      } catch (e) {
        if (!controller.signal.aborted) update(runId, x => ({ ...x, error: e instanceof Error ? e.message : '진행 상황을 불러오지 못했어요.' }));
      }
      // Backgrounded or disconnected: poll with backoff until the run rests
      // or the server's run timeout has surely passed. Unmount aborts it.
      const outcome = await pollUntilResting({
        delays: pollDelaysMs(pollBudgetMs(runTimeoutSecs)),
        fetchStatus: async () => (await refresh(runId))?.status ?? null,
        resting: () => {
          const current = entriesRef.current.find(e => e.runId === runId);
          return !!current && isResting(current.timeline.status);
        },
        aborted: () => controller.signal.aborted,
      });
      // The budget outlasts the server's run timeout, so this is rare: say
      // the run is still going instead of leaving a spinner.
      if (outcome === 'still_running' && sameUser()) update(runId, e => ({ ...e, error: STILL_RUNNING_MESSAGE }));
    } finally {
      streams.current.delete(runId);
      following.current.delete(runId);
    }
  }

  // Form context, decided on this device: the open form helper, the
  // programme on screen, or the form the last card opened. While its
  // template is still loading, a dictating message is held, not sent.
  const formProgram = programId ?? [...entries].reverse().flatMap(e => e.run?.confirmations ?? [])
    .find(c => c.tool === 'open_form_fill' && (c.status === 'pending' || c.status === 'executed') && c.program_id)?.program_id ?? undefined;
  const programForm = useQuery({ queryKey: ['form-context', formProgram], queryFn: () => loadFormContext(formProgram as string), enabled: !!formProgram, staleTime: 300_000 });
  const formContext = (): FormContext | null => openFormContext() ?? programForm.data
    ?? (formProgram && !programForm.isSuccess ? heldContext(formProgram, 'loading') : null);

  function addLocal(text: string, fill: LocalFill) {
    const id = `${LOCAL_PREFIX}${Date.now()}`;
    const run = localRun(fill, id);
    setGoal('');
    setEntries(list => [...list, { runId: id, goal: text, timeline: timelineFromRun(run), run, local: fill }]);
  }

  // A dictation said before any form was in context was held on this device
  // (the server got FORM_GOAL only). Once the form's card brings a form
  // context, it is parsed here.
  useEffect(() => {
    const ctx = programForm.data;
    if (!ctx) return;
    const held = takeHeldDictation(userId);
    if (held) addLocal(held, parseLocal(held, ctx));
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [programForm.data]);

  /** `chip`: a goal the server wrote (a chip or a journey turn), never a dictation. */
  async function send(text: string, pickedProgram?: string, pickedIds?: string[], chip = false) {
    const trimmed = text.trim();
    if (!trimmed || busy) return;
    await dispatchMessage(trimmed, formContext(), chip, {
      local: fill => addLocal(trimmed, fill),
      server: () => sendToServer(trimmed, pickedProgram, pickedIds),
      hold: async () => { holdDictation(userId, trimmed); setGoal(''); await sendToServer(FORM_GOAL, pickedProgram, pickedIds); },
    });
  }

  async function sendToServer(trimmed: string, pickedProgram?: string, pickedIds?: string[]) {
    setBusy('send'); setError('');
    try {
      const conversation = conversationId ?? (await api.createAgentConversation(programId)).id;
      if (!sameUser()) return;
      setConversationId(conversation);
      const started = await api.sendAgentGoal(conversation, trimmed, newIdempotencyKey(),
        // A chip for found programmes sends them as the selection. The
        // conversation keeps its screen programme as context; the server's
        // `고른 혜택` route prefers the selection (code review M2).
        pickedIds?.length ? undefined : pickedProgram ?? programId, pickedIds ?? selected);
      setGoal('');
      setEntries(list => [...list, { runId: started.run_id, goal: trimmed, timeline: initialTimeline, run: null }]);
      void follow(started.run_id);
    } catch (e) {
      setError(e instanceof Error ? e.message : '요청을 보내지 못했어요.');
    } finally { setBusy(null); }
  }

  async function decide(entry: Entry, confirmationId: string, approve: boolean) {
    const confirmation = entry.run?.confirmations.find(c => c.id === confirmationId);
    if (!confirmation || busy) return;
    // A local card: 승인 puts the values into this form's draft on this
    // device, as suggestions. Nothing is sent.
    const local = entry.local;
    if (local) {
      const route = approve ? draftRoute(local.programId, local.resourceId) : null;
      if (route) { handChatValues(userId, local.programId, local.resourceId, localValues(local)); router.push(route as never); }
      update(entry.runId, e => { if (!e.run) return e; const run = decideLocal(e.run, approve); return { ...e, run, timeline: timelineFromRun(run) }; });
      return;
    }
    setBusy(confirmationId); setError(''); setNotice('');
    let decided: AgentConfirmation | null = null;
    try {
      if (approve) {
        const decision = await api.approveAgentConfirmation(confirmation.id, approvalBody(confirmation).payload_hash, newIdempotencyKey());
        decided = decision.confirmation;
        // An approved calendar card: this device writes the event (after its
        // own permission prompt); the server stored nothing.
        const event = calendarEventOf(decision.confirmation);
        if (event && sameUser()) setNotice(calendarNotice(await addEventOnThisDevice(event)));
        // One form approved: open it with the draft built on this device.
        const route = routeAfterApproval(decision.confirmation);
        if (route && sameUser()) router.push(route as never);
        // An approved handoff: the official page opens in the browser (the
        // opener shows its domain first). Nothing is submitted.
        const official = officialHandoffUrl(decision.confirmation);
        if (official && sameUser()) openApplyUrl(official);
      } else await api.rejectAgentConfirmation(confirmation.id);
    } catch (e) {
      setError(e instanceof Error ? e.message : '처리하지 못했어요.');
    } finally {
      const run = await refresh(entry.runId);
      // The server's record of the card decides what changed, even when the
      // approve answer was lost: every screen showing those rows refetches.
      if (approve) void invalidateAfterApproval(client, { run, decided, confirmationId }, userId);
      setBusy(null);
    }
  }

  // Retry an approved calendar event (permission denied the first time).
  async function writeCalendar(confirmationId: string, entry: Entry) {
    const confirmation = entry.run?.confirmations.find(c => c.id === confirmationId);
    const event = confirmation ? calendarEventOf(confirmation) : null;
    if (!event || busy) return;
    setBusy(confirmationId); setNotice('');
    try { setNotice(calendarNotice(await addEventOnThisDevice(event))); } finally { setBusy(null); }
  }

  async function cancel(entry: Entry) {
    streams.current.get(entry.runId)?.abort();
    try { await api.cancelAgentRun(entry.runId); } catch { /* already finished */ }
    await refresh(entry.runId);
  }

  // The journey's status turn from the form helper: prefilled, never sent
  // without the user's tap, so a link cannot start a run (security review L3).
  // The key leaves the goal out, so clearing the param below does not remount
  // this screen and wipe the prefill (re-review). The ref re-arms once the
  // param is gone, for a later visit with the same link.
  const filledInitial = useRef(false);
  useEffect(() => {
    if (!initialGoal) { filledInitial.current = false; return; }
    if (filledInitial.current) return;
    filledInitial.current = true;
    setGoal(initialGoal);
    // A later visit with the same link fills it again.
    router.setParams({ goal: undefined });
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [initialGoal]);

  const starters = chips.filter(c => !c.needs_program || programId).map(c => ({ key: c.kind, label: c.goal, goal: c.goal }));
  return <KeyboardAvoidingView style={{ flex: 1 }} behavior={Platform.OS === 'ios' ? 'padding' : undefined}>
    <ScrollView contentContainerStyle={s.content} keyboardShouldPersistTaps="handled">
      {programId ? <Text style={s.context}>보고 있던 혜택을 기준으로 도와드려요.</Text> : null}
      {selected.length ? <Text style={s.context}>고른 혜택 {selected.length}개를 기준으로 도와드려요.</Text> : null}
      {/* 저장한 혜택, 다음 할 일: moved here from 홈, which keeps the Figma order */}
      {entries.length === 0 ? <AssistantBriefingCard /> : null}
      {entries.length === 0 ? <Text style={s.title}>무엇을 맡길까요?</Text> : null}
      {entries.map(entry => <View key={entry.runId} style={s.turn}>
        <View style={s.bubble}><Text style={s.bubbleText}>{entry.goal}</Text></View>
        <RunTimeline timeline={entry.timeline} />
        {!isResting(entry.timeline.status)
          ? <Pressable accessibilityRole="button" accessibilityLabel="이 요청 취소" onPress={() => void cancel(entry)} style={s.cancel}><Text style={s.link}>요청 취소</Text></Pressable> : null}
        {entry.error ? <Text accessibilityRole="alert" style={s.error}>{entry.error}</Text> : null}
        {entry.run?.message ? <MessageCard message={entry.run.message} /> : null}
        {entry.run?.results.map(result => <ResultCard key={`${entry.runId}:${result.seq}`} result={result} />)}
        {entry.run?.confirmations.map(c => <ConfirmationCard key={c.id} confirmation={c} busy={busy === c.id} localFill={entry.local}
          onApprove={() => void decide(entry, c.id, true)} onReject={() => void decide(entry, c.id, false)}
          onCalendar={() => void writeCalendar(c.id, entry)} />)}
        {(() => {
          // 신청 준비: the prepare turn about the programme on screen.
          const journey = entry.run?.goal_kind === 'prepare_program' ? journeyOf(entry.run) : null;
          // Any pending card in this conversation holds the next turn.
          const pending = pendingCards(entries.map(e => e.run));
          return journey && journey.programId === programId && isResting(entry.timeline.status)
            ? <AgentJourney journey={{ ...journey, pendingCards: pending }} busy={!!busy} onGoal={goal => void send(goal, undefined, undefined, true)} /> : null;
        })()}
        {entry.run && isResting(entry.run.status) ? <View style={s.chips}>
          {goalChips(entry.run.message?.suggested_actions, !!programId).map((chip, i) => <Pressable key={`${entry.runId}:${chip.key}:${i}`} accessibilityRole="button"
            onPress={() => chip.route ? router.push(chip.route as never) : void send(chip.goal, chip.programId, chip.programIds, true)} style={s.chip}><Text style={s.chipText}>{chip.label}</Text></Pressable>)}
        </View> : null}
      </View>)}
      {entries.length === 0 ? <View style={s.chips}>
        {starters.map(chip => <Pressable key={chip.key} accessibilityRole="button" onPress={() => void send(chip.goal, undefined, undefined, true)} style={s.chip}>
          <Text style={s.chipText}>{chip.label}</Text>
        </Pressable>)}
      </View> : null}
      {notice ? <Text accessibilityLiveRegion="polite" style={s.notice}>{notice}</Text> : null}
      {error ? <Text accessibilityRole="alert" style={s.error}>{error}</Text> : null}
    </ScrollView>
    <View style={s.composer}>
      <View style={s.inputRow}>
        <TextInput value={goal} onChangeText={setGoal} maxLength={300} placeholder="궁금한 혜택을 물어보세요" accessibilityLabel="비서에게 맡길 일"
          style={s.input} multiline returnKeyType="send" onSubmitEditing={() => void send(goal)} />
        <Pressable accessibilityRole="button" accessibilityLabel="보내기" disabled={!goal.trim() || !!busy} onPress={() => void send(goal)}
          style={[s.send, !goal.trim() || busy ? s.disabled : null]}><Text style={s.sendText}>{busy === 'send' ? '…' : '보내기'}</Text></Pressable>
      </View>
    </View>
  </KeyboardAvoidingView>;
}

const s = StyleSheet.create({
  content: { padding: 16, paddingBottom: 32, gap: 14 },
  title: { fontSize: 20, lineHeight: 32, fontWeight: '700', color: '#070428' },
  context: { fontSize: 12, lineHeight: 18, color: '#239969', fontWeight: '600' },
  turn: { gap: 10 },
  bubble: { alignSelf: 'flex-end', maxWidth: '85%', backgroundColor: '#191A54', borderRadius: 14, paddingHorizontal: 12, paddingVertical: 8 },
  bubbleText: { color: '#fff', fontSize: 13, lineHeight: 20 },
  chips: { flexDirection: 'row', flexWrap: 'wrap', gap: 8 },
  chip: { borderWidth: 1, borderColor: '#F1F1F1', borderRadius: 20, paddingHorizontal: 14, paddingVertical: 10, minHeight: 44, justifyContent: 'center' },
  chipText: { fontSize: 13, lineHeight: 20, color: '#070428' },
  cancel: { alignSelf: 'flex-start', minHeight: 44, justifyContent: 'center' },
  link: { fontSize: 13, lineHeight: 20, color: '#239969', fontWeight: '600' },
  error: { fontSize: 13, lineHeight: 20, color: '#070428' },
  notice: { fontSize: 13, lineHeight: 20, color: '#239969' },
  composer: { borderTopWidth: 1, borderTopColor: '#F1F1F1', padding: 12, gap: 6, backgroundColor: '#fff' },
  inputRow: { flexDirection: 'row', gap: 8, alignItems: 'flex-end' },
  input: { flex: 1, minHeight: 44, maxHeight: 120, borderWidth: 1, borderColor: '#F1F1F1', borderRadius: 12, paddingHorizontal: 12, paddingVertical: 10, fontSize: 13, lineHeight: 20, color: '#070428' },
  send: { minHeight: 44, minWidth: 64, borderRadius: 12, backgroundColor: '#191A54', alignItems: 'center', justifyContent: 'center', paddingHorizontal: 12 },
  sendText: { color: '#fff', fontSize: 14, lineHeight: 20, fontWeight: '700' },
  disabled: { opacity: 0.5 },
});
