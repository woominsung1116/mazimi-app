import React, { useCallback, useEffect, useRef, useState } from 'react';
import { Linking, Pressable, ScrollView, StyleSheet, TextInput, View } from 'react-native';
import { useFocusEffect, useLocalSearchParams, useRouter } from 'expo-router';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import Text from '@/components/Text';
import { LinkLabel, EmptyState, ScreenHeader, ui } from '@/components/FigmaUI';
import { api } from '@/lib/api';
import { invalidateAfterWrite } from '@/lib/writeEffects';
import { eligibilityLabels, profileFieldList, safeOfficialUrl, type AssistantTopic, type BenefitGuidance, type GroundedAnswer, type GroundedPassage } from '@/lib/assistantTypes';
import { finalSummary, followUpSuggestions, highlightSegments, verdictBadge } from '@/lib/grounding';
import { streamBenefitAnswer } from '@/lib/answerStream';
import { useAuthStore } from '@/store/auth';
import NonMemberState from '@/components/NonMemberState';
import { dictatesOutsideForm, openFormContext, staysLocal } from '@/lib/formFill/chatFill';

const FORM_VALUES_HERE_NOTE = '서식 칸에 적을 내용은 보내지 않아요. 작성 도우미에서 직접 입력해 주세요.';

export default function BenefitAssistantScreen() {
  const params = useLocalSearchParams<{ programId: string }>();
  const programId = typeof params.programId === 'string' ? params.programId : '';
  const user = useAuthStore(s => s.user);
  const router = useRouter();
  return <View style={ui.screen}><ScreenHeader title="혜택 도우미" back />
    {!user ? <NonMemberState title="로그인하고 남은 준비를 확인하세요" description="저장된 조건과 준비 기록을 바탕으로 안내해요." /> :
      !programId ? <EmptyState title="혜택을 먼저 선택해 주세요" action="혜택 찾기" onAction={() => router.push('/explore')} /> :
        <Guidance key={`${user.id}:${programId}`} userId={user.id} programId={programId} />}
  </View>;
}

type Summary =
  | { state: 'writing'; draft: string }
  | { state: 'final'; text: string; grounded: boolean }
  | { state: 'stopped'; message: string };
interface Turn {
  id: number; question: string;
  status: 'pending' | 'done' | 'error'; error?: string;
  answer?: GroundedAnswer; sections?: BenefitGuidance['sections'];
  summary?: Summary;
}

function Guidance({ userId, programId }: { userId: string; programId: string }) {
  const router = useRouter(); const client = useQueryClient();
  const [question, setQuestion] = useState(''); const [consent, setConsent] = useState(false); const [message, setMessage] = useState('');
  const [turns, setTurns] = useState<Turn[]>([]);
  // Multi-turn-lite: only the last answer's topic, on this device.
  const [lastTopic, setLastTopic] = useState<AssistantTopic | undefined>();
  const nextId = useRef(0);
  const streams = useRef(new Map<number, AbortController>());
  useEffect(() => () => { streams.current.forEach(c => c.abort()); streams.current.clear(); }, []);
  const key = ['benefit-assistant', userId, programId];
  const query = useQuery({ queryKey: key, queryFn: ({ signal }) => api.getBenefitGuidance(programId, signal), staleTime: 0, retry: 1 });
  const update = (id: number, change: (turn: Turn) => Turn) => setTurns(list => list.map(t => t.id === id ? change(t) : t));
  const ask = useMutation({
    mutationFn: ({ text }: { id: number; text: string; allowAi: boolean }) => api.askBenefitAssistant(programId, text, false, lastTopic),
    retry: false,
    onSuccess: (data, { id, text, allowAi }) => {
      if (useAuthStore.getState().user?.id !== userId) return;
      // Refresh the eligibility card from the same snapshot; overview sections stay.
      client.setQueryData<BenefitGuidance>(key, old => old ? { ...data, sections: old.sections, answer: undefined } : data);
      const answer = data.answer;
      if (answer?.follow_up_topic) setLastTopic(answer.follow_up_topic);
      update(id, t => ({ ...t, status: 'done', answer, sections: data.sections.filter(s => s.topic === 'steps' || s.topic === 'alerts' || s.topic === 'unsupported') }));
      if (allowAi && answer && !answer.unsupported && answer.passages.length > 0) summarize(id, text, answer.follow_up_topic);
    },
    onError: (error: Error, { id }) => { update(id, t => ({ ...t, status: 'error', error: error.message })); void query.refetch(); },
  });
  function summarize(id: number, text: string, topic: AssistantTopic | undefined) {
    const controller = new AbortController();
    streams.current.set(id, controller);
    update(id, t => ({ ...t, summary: { state: 'writing', draft: '' } }));
    streamBenefitAnswer(programId, text, topic, event => {
      if (event.type === 'delta') update(id, t => t.summary?.state === 'writing' ? { ...t, summary: { state: 'writing', draft: t.summary.draft + event.text } } : t);
      if (event.type === 'final') update(id, t => ({ ...t, summary: { state: 'final', ...finalSummary(event) } }));
    }, controller.signal)
      .then(() => update(id, t => t.summary?.state === 'writing' ? { ...t, summary: { state: 'stopped', message: '요약이 끝나지 않았어요. 근거 문장을 확인해 주세요.' } } : t))
      .catch((error: Error) => update(id, t => ({ ...t, summary: { state: 'stopped', message: controller.signal.aborted ? '요약을 취소했어요.' : error.message } })))
      .finally(() => streams.current.delete(id));
  }
  function cancel(id: number) { streams.current.get(id)?.abort(); }
  // Returning from preparation/profile must re-read the authoritative revision.
  useFocusEffect(useCallback(() => { void client.invalidateQueries({ queryKey: ['benefit-assistant', userId, programId] }); }, [client, userId, programId]));
  const alerts = useMutation({
    mutationFn: (enabled: boolean) => api.upsertAlertPreference(programId, enabled),
    onSuccess: (_, enabled) => {
      if (useAuthStore.getState().user?.id !== userId) return;
      client.setQueryData(['alert-preference', userId, programId], { program_id: programId, enabled });
      void invalidateAfterWrite(client, 'set_alert', { userId, programId });
      setMessage(enabled ? '이 혜택의 알림 구독을 켰어요. 기기의 알림 권한도 확인해 주세요.' : '이 혜택의 알림 구독을 껐어요.');
    }, onError: (error: Error) => setMessage(error.message),
  });
  const data = query.data;
  if (query.isError || !data) return <EmptyState loading={query.isLoading} title={query.isError ? '현재 안내를 불러올 수 없어요' : '내 조건과 남은 준비를 확인하고 있어요'} description={query.error?.message} action="다시 확인" onAction={() => void query.refetch()} />;
  const busy = turns.some(t => t.status === 'pending');
  const submit = (text: string) => {
    const trimmed = text.trim();
    if (!trimmed || busy) return;
    // Form values are never sent from here: a dictation, or anything the 비서
    // would keep on the device while this programme's form is open.
    const form = openFormContext();
    if (dictatesOutsideForm(trimmed) || (form?.programId === programId && staysLocal(trimmed, form))) { setMessage(FORM_VALUES_HERE_NOTE); return; }
    setMessage(''); setQuestion('');
    const id = nextId.current++;
    setTurns(list => [...list, { id, question: trimmed, status: 'pending' }]);
    ask.mutate({ id, text: trimmed, allowAi: consent && data.ai_available });
  };
  const remaining = data.preparation.tasks.filter(task => data.remaining_task_ids.includes(task.id));
  const sourceUrl = safeOfficialUrl(data.source.official_url);
  async function openSource() {
    try { if (sourceUrl) await Linking.openURL(sourceUrl); }
    catch { setMessage('공식 안내를 열지 못했어요. 잠시 후 다시 시도해 주세요.'); }
  }
  return <ScrollView contentContainerStyle={s.content} keyboardShouldPersistTaps="handled">
    <Text style={s.title}>{data.title}</Text><Text style={s.note}>{data.notice}</Text>
    <View style={s.card}><Text style={s.heading}>자격 상태 · {eligibilityLabels[data.eligibility.status]}</Text>
      <Text style={s.body}>{data.eligibility.summary}</Text>
      {data.eligibility.checks.map(check => <View key={check.label} style={s.row}>
        <Text style={s.label}>{check.label} · {eligibilityLabels[check.status]}</Text><Text style={s.body}>{check.detail}</Text>
      </View>)}
      {!!data.eligibility.missing_fields.length && <Text style={s.note}>먼저 확인할 정보: {profileFieldList(data.eligibility.missing_fields)}</Text>}
      <Pressable accessibilityRole="button" onPress={() => router.push('/onboarding?mode=edit')} style={ui.outlineButton}><Text style={ui.outlineButtonText}>저장한 조건 확인·수정</Text></Pressable>
    </View>
    <View style={s.card}><Text style={s.heading}>아직 확인하지 않은 준비 {remaining.length}개</Text>
      {data.preparation.progress_reset && <Text style={s.notice}>공고 내용이 변경되어 준비 항목을 다시 확인해야 해요.</Text>}
      {remaining.slice(0, 3).map((task, i) => <Text key={task.id} style={s.body}>{i + 1}. {task.title}</Text>)}
      {!remaining.length && <Text style={s.body}>체크리스트를 모두 확인했어요.</Text>}
      <Pressable accessibilityRole="button" onPress={() => router.push(`/apply-assistant?programId=${programId}`)} style={ui.primaryButton}><Text style={ui.primaryButtonText}>준비 목록 이어서 확인</Text></Pressable>
    </View>
    <View style={s.card}>
      <Text style={s.heading}>이 혜택에 대해 물어보기</Text>
      {data.sections.map(section => <View key={section.topic} style={[s.bubble, s.assistantBubble]}><Text style={s.label}>{section.title}</Text><Text style={s.body}>{section.body}</Text>
      </View>)}
      {turns.map(turn => <View key={turn.id} style={s.turn}>
        <View style={[s.bubble, s.userBubble]}><Text style={s.userText}>{turn.question}</Text></View>
        <TurnAnswer turn={turn} sourceUrl={sourceUrl} onOpenSource={() => void openSource()} onCancel={() => cancel(turn.id)} onFollowUp={submit} disabled={busy} />
      </View>)}
      {!turns.length && <View style={s.chips}>{['필요한 서류는?', '언제까지 신청해?', '누가 받을 수 있어?'].map(text => <Pressable key={text} accessibilityRole="button" disabled={busy} onPress={() => submit(text)} style={s.chip}><Text style={s.chipText}>{text}</Text></Pressable>)}</View>}
      <TextInput accessibilityLabel="혜택 질문" placeholder="이 혜택에 대해 물어보세요" multiline maxLength={500} value={question} onChangeText={setQuestion} editable={!busy} style={s.input} />
      {data.ai_available && <Pressable accessibilityRole="checkbox" accessibilityLabel="AI 요약을 위해 질문을 자체 호스팅 AI로 처리하는 데 동의" accessibilityState={{ checked: consent }} onPress={() => setConsent(v => !v)} style={s.consent}>
        <View style={s.checkRow}><View importantForAccessibility="no" accessibilityElementsHidden style={[s.box, consent && s.boxOn]} /><Text style={s.body}>AI 요약도 받기 (자체 호스팅 AI 처리에 동의)</Text></View>
        <Text style={s.note}>질문과 찾은 공고 문장만 전송해요. 프로필·보관 서류는 전송하지 않아요. 동의하지 않아도 공고 문장 답변은 그대로 받아요.</Text>
      </Pressable>}
      <Pressable accessibilityRole="button" disabled={!question.trim() || busy} onPress={() => submit(question)} style={[ui.primaryButton, (!question.trim() || busy) && { opacity: 0.5 }]}><Text style={ui.primaryButtonText}>{busy ? '공고에서 찾는 중…' : '질문하기'}</Text></Pressable>
    </View>
    <View style={s.card}><Text style={s.heading}>혜택 알림 {data.alert_enabled ? '켜짐' : '꺼짐'}</Text>
      <Pressable accessibilityRole="button" disabled={alerts.isPending || query.isFetching} onPress={() => { setMessage(''); alerts.mutate(!data.alert_enabled); }} style={ui.outlineButton}><Text style={ui.outlineButtonText}>{alerts.isPending ? '저장 중…' : data.alert_enabled ? '이 혜택 알림 끄기' : '이 혜택 알림 켜기'}</Text></Pressable>
      <Pressable accessibilityRole="button" onPress={() => router.push('/profile')} style={s.consent}><LinkLabel style={s.link}>전체 알림 설정 확인</LinkLabel></Pressable>
    </View>
    {/* No source note or collection time: the official link only */}
    {sourceUrl ? <View style={s.card}><Pressable accessibilityRole="link" onPress={() => void openSource()} style={s.consent}><LinkLabel style={s.link}>공식 공고 확인</LinkLabel></Pressable></View> : null}
    {!!message && <Text accessibilityRole="alert" style={s.notice}>{message}</Text>}
  </ScrollView>;
}

function TurnAnswer({ turn, sourceUrl, onOpenSource, onCancel, onFollowUp, disabled }: {
  turn: Turn; sourceUrl: string | null; onOpenSource: () => void; onCancel: () => void; onFollowUp: (text: string) => void; disabled: boolean;
}) {
  if (turn.status === 'pending') return <View style={[s.bubble, s.assistantBubble]}><Text style={s.note} accessibilityLiveRegion="polite">공고에서 찾는 중…</Text></View>;
  if (turn.status === 'error' || !turn.answer) return <View style={[s.bubble, s.assistantBubble]}><Text accessibilityRole="alert" style={s.notice}>{turn.error ?? '답변을 불러오지 못했어요.'}</Text></View>;
  const answer = turn.answer;
  const badge = answer.verdict ? verdictBadge(answer.verdict) : null;
  return <View style={[s.bubble, s.assistantBubble]}>
    {badge && answer.verdict && <View style={s.verdict}>
      <View style={[s.badge, badge.tone === 'fail' ? s.badgeFail : s.badgeUnknown]}><Text style={s.badgeText}>{badge.label}</Text></View>
      <Text style={s.body}>{answer.verdict.summary}</Text>
    </View>}
    {answer.unsupported && <Text style={s.body}>{answer.notice}</Text>}
    {answer.passages.map(passage => <Quote key={passage.id} passage={passage} />)}
    {answer.not_found && <View style={s.notFound}>
      <Text style={s.body}>{answer.notice}</Text>
      {sourceUrl ? <Pressable accessibilityRole="link" onPress={onOpenSource} style={s.consent}><LinkLabel style={s.link}>공식 공고에서 확인하기</LinkLabel></Pressable> : <Text style={s.note}>확인된 공식 링크가 없어요.</Text>}
    </View>}
    {turn.sections?.map(section => <View key={section.topic} style={s.row}><Text style={s.label}>{section.title}</Text><Text style={s.body}>{section.body}</Text></View>)}
    {turn.summary && <SummaryBlock summary={turn.summary} onCancel={onCancel} />}
    {!answer.unsupported && !answer.not_found && <Text style={s.note}>{answer.notice}</Text>}
    <View style={s.chips}>{followUpSuggestions(answer.follow_up_topic).map(text => <Pressable key={text} accessibilityRole="button" disabled={disabled} onPress={() => onFollowUp(text)} style={s.chip}><Text style={s.chipText}>{text}</Text></Pressable>)}</View>
  </View>;
}

function Quote({ passage }: { passage: GroundedPassage }) {
  // The notice's own sentence; no source label, collection date or review date.
  return <View style={s.quote} accessibilityLabel={`공고 인용: ${passage.text}`}>
    <Text style={s.quoteText}>“{highlightSegments(passage.text, passage.highlights).map((segment, i) => <Text key={i} style={segment.marked ? s.mark : undefined}>{segment.text}</Text>)}”</Text>
  </View>;
}

function SummaryBlock({ summary, onCancel }: { summary: Summary; onCancel: () => void }) {
  if (summary.state === 'writing') return <View style={[s.summary, s.summaryDraft]} accessibilityLiveRegion="polite">
    <Text style={s.summaryLabel}>AI 요약 작성 중 · 검증 전</Text>
    {!!summary.draft && <Text style={s.draftText}>{summary.draft}</Text>}
    <Pressable accessibilityRole="button" onPress={onCancel} style={s.cancel}><Text style={s.link}>요약 취소</Text></Pressable>
  </View>;
  if (summary.state === 'final') return <View style={s.summary}>
    {summary.grounded && <View style={[s.badge, s.badgePass]}><Text style={s.badgeText}>근거 기반 요약</Text></View>}
    <Text style={s.body}>{summary.text}</Text>
    <Text style={s.note}>{summary.grounded ? 'AI가 위 공고 문장만으로 쓴 요약이에요. 최종 조건은 원문으로 확인해 주세요.' : 'AI 요약이 공고 문장과 맞지 않아 표시하지 않았어요.'}</Text>
  </View>;
  return <View style={s.summary}><Text style={s.note}>{summary.message}</Text></View>;
}

const s = StyleSheet.create({
  content: { padding: 16, paddingBottom: 40 }, title: { fontSize: 20, lineHeight: 32, fontWeight: '700', color: '#070428', marginBottom: 12 },
  card: { borderRadius: 16, borderWidth: 1, borderColor: '#F1F1F1', padding: 16, marginTop: 16 },
  heading: { fontSize: 16, lineHeight: 24, fontWeight: '700', color: '#070428' }, body: { fontSize: 13, lineHeight: 20, color: '#5D5D62', marginVertical: 4 },
  note: { fontSize: 12, lineHeight: 18, color: '#5D5D62', marginBottom: 8 }, notice: { fontSize: 13, lineHeight: 20, color: '#070428', marginVertical: 12 },
  row: { borderTopWidth: 1, borderColor: '#F1F1F1', paddingTop: 12, marginTop: 8 }, label: { fontSize: 13, lineHeight: 20, fontWeight: '600', color: '#070428' },
  input: { minHeight: 96, borderWidth: 1, borderColor: '#F1F1F1', borderRadius: 12, padding: 12, fontSize: 13, lineHeight: 20, textAlignVertical: 'top', marginBottom: 12 },
  chips: { flexDirection: 'row', flexWrap: 'wrap', gap: 8, marginVertical: 12 }, chip: { minHeight: 44, justifyContent: 'center', borderRadius: 20, paddingHorizontal: 12, backgroundColor: '#2A8F660A' },
  chipText: { fontSize: 12, lineHeight: 18, color: '#239969' }, consent: { paddingVertical: 12 },
  checkRow: { flexDirection: 'row', alignItems: 'center', gap: 8 }, box: { width: 20, height: 20, borderRadius: 4, borderWidth: 2, borderColor: '#C9CCCE' }, boxOn: { backgroundColor: '#239969', borderColor: '#34BB84' }, link: { fontSize: 13, lineHeight: 20, color: '#239969' },
  turn: { marginTop: 12, gap: 8 },
  bubble: { borderRadius: 16, padding: 12, marginTop: 8 },
  userBubble: { alignSelf: 'flex-end', maxWidth: '85%', backgroundColor: '#239969', borderBottomRightRadius: 4 },
  userText: { fontSize: 13, lineHeight: 20, color: '#FFFFFF' },
  assistantBubble: { alignSelf: 'stretch', backgroundColor: '#F7F9FA', borderWidth: 1, borderColor: '#F1F1F1', borderBottomLeftRadius: 4 },
  verdict: { gap: 4, marginBottom: 8 },
  badge: { alignSelf: 'flex-start', borderRadius: 8, paddingHorizontal: 8, paddingVertical: 4 },
  badgePass: { backgroundColor: '#2A8F660A' }, badgeFail: { backgroundColor: '#F72B2B0A' }, badgeUnknown: { backgroundColor: '#F72B2B0A' },
  badgeText: { fontSize: 12, lineHeight: 18, fontWeight: '700', color: '#070428' },
  quote: { borderLeftWidth: 4, borderLeftColor: '#2A8F6614', paddingLeft: 10, paddingVertical: 4, marginVertical: 6, gap: 6 },
  quoteText: { fontSize: 13, lineHeight: 20, color: '#070428' },
  mark: { fontWeight: '700', backgroundColor: '#F72B2B0A' },
  notFound: { borderRadius: 12, backgroundColor: '#F72B2B0A', padding: 10, marginVertical: 6 },
  summary: { borderTopWidth: 1, borderColor: '#F1F1F1', paddingTop: 10, marginTop: 8, gap: 4 },
  summaryDraft: { borderTopColor: '#C9CCCE' },
  summaryLabel: { fontSize: 12, lineHeight: 18, fontWeight: '700', color: '#8894A1' },
  draftText: { fontSize: 13, lineHeight: 20, color: '#5D5D62', fontStyle: 'italic' },
  cancel: { minHeight: 44, justifyContent: 'center', alignSelf: 'flex-start' },
});
