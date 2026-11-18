import React from 'react';
import { ActivityIndicator, Pressable, StyleSheet, View } from 'react-native';
import { useRouter } from 'expo-router';
import Text from './Text';
import { eligibilityLabels } from '@/lib/assistantTypes';
import type { AgentConfirmation, AgentMessage, AgentToolResult } from '@/lib/agentTypes';
import {
  actionRoute, approvedForms, benefitRows, officialHandoffUrl, canDecide, checklistRows, confirmationNotes, confirmationStatusLabel, draftRoute, formLinks, guidedRoute,
  noticePassages, progressNote, runStatusLabels, stepStatusLabels, verdictOf, type Timeline,
} from '@/lib/agent';
import { issueLinkRows, noticeLink, officialPageView } from '@/lib/agentTools';
import { canRetryCalendar } from '@/lib/calendarEvent';
import { localRows, type LocalFill } from '@/lib/formFill/chatFill';
import { openApplyUrl } from '@/lib/openExternalUrl';
import { useVaultDocuments } from '@/lib/vault';
import { vaultRows, vaultStatusLabels } from '@/lib/agentVault';

/** Steps with a text status (no glyph-only icons), keyed by plan index or call seq. */
export function RunTimeline({ timeline }: { timeline: Timeline }) {
  const running = !['completed', 'failed', 'timed_out', 'cancelled', 'awaiting_confirmation'].includes(timeline.status);
  const note = progressNote(timeline);
  return <View style={s.timeline} accessibilityLabel={`진행 상황: ${runStatusLabels[timeline.status]}`}>
    <View style={s.row}>
      {running ? <ActivityIndicator size="small" color="#239969" accessibilityElementsHidden /> : null}
      <Text style={s.status}>{runStatusLabels[timeline.status]}{timeline.planner === 'model' ? ' · AI 계획' : ''}</Text>
    </View>
    {timeline.steps.map((step, index) => <View key={step.key} style={s.step}
      accessibilityLabel={`${index + 1}단계 ${step.label}: ${stepStatusLabels[step.status]}`}>
      <Text style={s.stepIndex}>{index + 1}</Text>
      <View style={s.flex}>
        <Text style={s.stepLabel}>{step.label}{step.write && step.status !== 'awaiting_confirmation' ? ' (승인 필요)' : ''}</Text>
        {step.detail ? <Text style={s.note}>{step.detail}</Text> : null}
      </View>
      <Text style={[s.stepStatus, step.status === 'error' || step.status === 'failed' ? s.bad : null]}>{stepStatusLabels[step.status]}</Text>
    </View>)}
    {note ? <Text style={s.note} accessibilityLiveRegion="polite">{note}</Text> : null}
  </View>;
}

export function MessageCard({ message }: { message: AgentMessage }) {
  const verdict = message.verdict === 'not_evaluated' ? null : message.verdict;
  return <View style={s.card}>
    {verdict ? <View style={[s.badge, verdict === 'fail' ? s.badgeFail : null]}>
      <Text style={s.badgeText}>{eligibilityLabels[verdict]}</Text>
    </View> : null}
    {message.plan_steps?.length ? <Text style={s.note} accessibilityLabel={`비서가 한 일: ${message.plan_steps.join(', ')}`}>{message.plan_steps.join(' → ')}</Text> : null}
    <Text style={s.body}>{message.answer}</Text>
    {message.question ? <Text style={s.rowTitle}>{message.question}</Text> : null}
    {message.warnings.map(w => <Text key={w} style={s.note}>{w}</Text>)}
  </View>;
}

export function ResultCard({ result }: { result: AgentToolResult }) {
  const router = useRouter();
  // Hooks before any return (re-review L2). The vault is matched on this
  // device only; nothing of it is sent.
  const vaultDocuments = useVaultDocuments();
  if (!result.ok) return <View style={s.card}><Text style={s.bad}>{result.error?.message ?? '처리하지 못했어요.'}</Text></View>;
  const rows = benefitRows(result);
  const vault = vaultRows(result, vaultDocuments);
  const checklist = checklistRows(result);
  const verdict = verdictOf(result);
  const passages = noticePassages(result);
  const forms = formLinks(result);
  const live = officialPageView(result);
  const issueLinks = issueLinkRows(result);
  const notice = noticeLink(result);
  const title = typeof result.data.title === 'string' ? result.data.title : null;
  if (result.status === 'skipped' || result.status === 'awaiting_confirmation') return null;
  return <View style={s.card}>
    {title ? <Text style={s.cardTitle}>{title}</Text> : null}
    {verdict ? <Text style={s.body}>{eligibilityLabels[verdict]}: {String(result.data.summary ?? '')}</Text> : null}
    {rows.map(row => <Pressable key={row.programId} accessibilityRole="button" accessibilityLabel={`${row.title} 자세히 보기`}
      onPress={() => router.push(`/programs/${row.programId}`)} style={s.listRow}>
      {row.field || row.dDay ? <View style={s.row}>
        {row.field ? <View style={s.badge}><Text style={s.badgeText}>{row.field}</Text></View> : null}
        {row.dDay ? <View style={s.badge}><Text style={s.badgeText}>{row.dDay}</Text></View> : null}
      </View> : null}
      <Text style={s.rowTitle}>{row.title}</Text>
      <Text style={s.note}>{[row.status ? eligibilityLabels[row.status as 'pass' | 'fail' | 'unknown'] ?? null : null, row.deadline ? `마감 ${row.deadline.slice(0, 10)}` : null, row.note].filter(Boolean).join(' · ')}</Text>
      {row.reason ? <Text style={s.note}>{row.reason}</Text> : null}
    </Pressable>)}
    {vault.map(d => <View key={d.name} style={s.listRow} accessibilityLabel={`${d.name}: ${vaultStatusLabels[d.status]}${d.note ? `, ${d.note}` : ''}`}>
      <Text style={s.rowTitle}>{d.name}</Text>
      <Text style={d.status === 'ready' ? s.done : [s.note, d.status === 'check' ? s.bad : null]}>{vaultStatusLabels[d.status]}{d.note ? ` · ${d.note}` : ''}</Text>
    </View>)}
    {result.tool === 'search_benefits' && rows.length === 0 ? <Text style={s.note}>{String(result.data.notice ?? '조건에 맞는 후보가 없어요.')}</Text> : null}
    {checklist.map(task => <View key={task.id} style={s.listRow} accessibilityLabel={`${task.title}: ${task.done ? '완료' : '미완료'}`}>
      <Text style={s.rowTitle}>{task.title}</Text>
      <Text style={task.done ? s.done : s.note}>{task.done ? '완료' : '미완료'}</Text>
    </View>)}
    {live ? <View style={s.liveBlock} accessibilityLabel={live.label}>
      <View style={[s.badge, live.status === 'closed' ? s.badgeFail : null]}><Text style={s.badgeText}>{live.label}</Text></View>
      {live.quotes.map((q, i) => <View key={`q${i}`} style={s.quoteBlock}><Text style={s.quote}>“{q.text}”</Text></View>)}
      {live.changes.map(c => <Text key={c} style={s.body}>{c}</Text>)}
      {live.link ? <Pressable accessibilityRole="link" style={s.outline} onPress={() => openApplyUrl(live.link as string)}>
        <Text style={s.outlineText}>공식 페이지 열기</Text>
      </Pressable> : null}
    </View> : null}
    {issueLinks.map(row => <View key={row.name} style={s.listRow} accessibilityLabel={`${row.name}: ${row.issuer ?? '발급처 안내 없음'}`}>
      <Text style={s.rowTitle}>{row.name}{row.issuer ? ` · ${row.issuer}` : ''}</Text>
      <Text style={s.note}>{row.guidance}</Text>
      {row.url ? <Pressable accessibilityRole="link" accessibilityLabel={`${row.name} 발급하러 가기`} style={s.outline} onPress={() => openApplyUrl(row.url as string)}>
        <Text style={s.outlineText}>발급하러 가기</Text>
      </Pressable> : null}
    </View>)}
    {issueLinks.length ? <Text style={s.note}>로그인과 발급은 공식 사이트에서 직접 해요.</Text> : null}
    {issueLinks.some(r => !r.known) && notice ? <Pressable accessibilityRole="link" style={s.outline} onPress={() => openApplyUrl(notice)}>
      <Text style={s.outlineText}>공고문 열기</Text>
    </Pressable> : null}
    {passages.map(p => <View key={p.id} style={s.quoteBlock}><Text style={s.quote}>“{p.text}”</Text></View>)}
    {forms.map(f => <Pressable key={f.resourceId} accessibilityRole="button" style={s.outline}
      onPress={() => router.push(`/form-fill?programId=${result.program_id}&resourceId=${f.resourceId}`)}>
      <Text style={s.outlineText}>‘{f.label}’ 작성 도우미 열기</Text>
    </Pressable>)}
    {result.tool === 'open_form_fill' && forms.length === 0 && guidedRoute(result.program_id) ? <Pressable accessibilityRole="button" style={s.outline}
      onPress={() => router.push(guidedRoute(result.program_id) as never)}>
      <Text style={s.outlineText}>복사용 안내와 공식 양식 보기</Text>
    </Pressable> : null}
    {result.program_id && (result.tool === 'get_preparation' || verdict) ? <Pressable accessibilityRole="button" style={s.outline}
      onPress={() => { const route = actionRoute({ type: 'prepare_application', label: '', payload: { program_id: result.program_id } }); if (route) router.push(route as never); }}>
      <Text style={s.outlineText}>신청 준비 화면 열기</Text>
    </Pressable> : null}
  </View>;
}

/** Before/after, what runs, explicit 승인/취소. Nothing changes until 승인. */
export function ConfirmationCard({ confirmation, busy, onApprove, onReject, onCalendar, localFill }: {
  confirmation: AgentConfirmation; busy: boolean; onApprove: () => void; onReject: () => void;
  /** A card made on this device for values said in form context; never sent. */
  localFill?: LocalFill | null;
  /** Write an approved calendar event (again) on this device. */
  onCalendar?: () => void;
}) {
  const router = useRouter();
  const open = canDecide(confirmation);
  // An approved form card: open the form helper with the draft built on this device.
  const forms = approvedForms(confirmation);
  return <View style={[s.card, s.confirm]} accessibilityLabel={`승인 요청: ${confirmation.action}`}>
    <Text style={s.cardTitle}>{confirmation.title}</Text>
    <Text style={s.body}>{confirmation.action}</Text>
    <View style={s.compare}>
      <View style={s.compareCell}><Text style={s.note}>지금</Text><Text style={s.rowTitle}>{confirmation.before}</Text></View>
      <View style={s.compareCell}><Text style={s.note}>승인하면</Text><Text style={s.rowTitle}>{confirmation.after}</Text></View>
    </View>
    {confirmationNotes(confirmation, open).map(note => <Text key={note} style={s.note}>{note}</Text>)}
    {localFill ? localRows(localFill).map((row, i) => <View key={`fill-${i}`} style={s.listRow}><Text style={s.rowTitle}>{row}</Text></View>) : null}
    {localFill ? localFill.notes.map((note, i) => <Text key={`note-${i}`} style={s.note}>{note}</Text>) : null}
    {officialHandoffUrl(confirmation) ? <Pressable accessibilityRole="link" style={s.outline} onPress={() => openApplyUrl(officialHandoffUrl(confirmation) as string)}>
      <Text style={s.outlineText}>공식 신청 페이지 다시 열기</Text>
    </Pressable> : null}
    {confirmationStatusLabel(confirmation) ? <Text style={s.note}>{confirmationStatusLabel(confirmation)}</Text> : null}
    {forms.map(f => { const route = draftRoute(confirmation.program_id, f.resourceId); return route ? <Pressable key={f.resourceId} accessibilityRole="button"
      onPress={() => router.push(route as never)} style={s.outline}><Text style={s.outlineText}>‘{f.label}’ 초안 열기</Text></Pressable> : null; })}
    {onCalendar && canRetryCalendar(confirmation) ? <Pressable accessibilityRole="button" accessibilityLabel="이 기기 캘린더에 다시 넣기" disabled={busy}
      onPress={onCalendar} style={s.outline}><Text style={s.outlineText}>이 기기 캘린더에 넣기</Text></Pressable> : null}
    {open ? <View style={s.actions}>
      <Pressable accessibilityRole="button" accessibilityLabel={`${confirmation.action} 취소`} disabled={busy} onPress={onReject} style={[s.outline, s.flex]}>
        <Text style={s.outlineText}>취소</Text>
      </Pressable>
      <Pressable accessibilityRole="button" accessibilityLabel={`${confirmation.action} 승인`} disabled={busy} onPress={onApprove} style={[s.primary, s.flex, busy ? s.disabled : null]}>
        <Text style={s.primaryText}>{busy ? '처리 중…' : '승인'}</Text>
      </Pressable>
    </View> : null}
  </View>;
}

const s = StyleSheet.create({
  timeline: { gap: 6, paddingVertical: 4 },
  row: { flexDirection: 'row', alignItems: 'center', gap: 8 },
  status: { fontSize: 13, lineHeight: 20, fontWeight: '600', color: '#070428' },
  step: { flexDirection: 'row', alignItems: 'center', gap: 8, minHeight: 28 },
  stepIndex: { width: 22, height: 22, borderRadius: 11, backgroundColor: '#F7F9FA', textAlign: 'center', paddingTop: 3, lineHeight: 16, fontSize: 11, color: '#5D5D62' },
  stepLabel: { flex: 1, fontSize: 13, lineHeight: 20, color: '#070428' },
  stepStatus: { fontSize: 12, lineHeight: 18, color: '#239969', fontWeight: '600' },
  bad: { color: '#070428' },
  card: { borderRadius: 16, borderWidth: 1, borderColor: '#F1F1F1', padding: 14, gap: 10, backgroundColor: '#fff' },
  confirm: { borderColor: '#F75B2B', backgroundColor: '#F72B2B0A' },
  cardTitle: { fontSize: 14, lineHeight: 20, fontWeight: '700', color: '#070428' },
  body: { fontSize: 13, lineHeight: 20, color: '#070428' },
  note: { fontSize: 12, lineHeight: 18, color: '#5D5D62' },
  done: { fontSize: 12, lineHeight: 18, color: '#239969', fontWeight: '600' },
  badge: { alignSelf: 'flex-start', borderRadius: 8, paddingHorizontal: 8, paddingVertical: 4, backgroundColor: '#F7F9FA' },
  badgeFail: { backgroundColor: '#F72B2B0A' },
  badgeText: { fontSize: 12, lineHeight: 18, fontWeight: '700', color: '#070428' },
  listRow: { borderTopWidth: 1, borderTopColor: '#F1F1F1', paddingTop: 8, gap: 2, minHeight: 44, justifyContent: 'center' },
  rowTitle: { fontSize: 14, lineHeight: 20, fontWeight: '600', color: '#070428' },
  liveBlock: { gap: 8 },
  quoteBlock: { borderLeftWidth: 4, borderLeftColor: '#2A8F6652', paddingLeft: 10, gap: 2 },
  quote: { fontSize: 13, lineHeight: 20, color: '#070428' },
  compare: { flexDirection: 'row', gap: 8 },
  compareCell: { flex: 1, borderRadius: 10, backgroundColor: '#fff', borderWidth: 1, borderColor: '#F75B2B', padding: 10, gap: 2 },
  actions: { flexDirection: 'row', gap: 8 },
  flex: { flex: 1 },
  outline: { minHeight: 44, borderRadius: 10, borderWidth: 1, borderColor: '#F1F1F1', alignItems: 'center', justifyContent: 'center', paddingHorizontal: 14 },
  outlineText: { fontSize: 14, lineHeight: 20, color: '#070428', fontWeight: '600' },
  primary: { minHeight: 44, borderRadius: 10, backgroundColor: '#191A54', alignItems: 'center', justifyContent: 'center' },
  primaryText: { fontSize: 14, lineHeight: 20, color: '#fff', fontWeight: '700' },
  disabled: { opacity: 0.6 },
});
