import { LinkLabel } from "./FigmaUI";
import React from 'react';
import { Linking, Platform, Pressable, StyleSheet, View } from 'react-native';
import { useRouter } from 'expo-router';
import Text from './Text';
import { eligibilityLabels } from '@/lib/assistantTypes';
import { JOURNEY_GOALS, type Journey } from '@/lib/agent';
import { documentReadiness, readinessLabels, readinessNote, readinessSummary, type ReadinessRow } from '@/lib/documentReadiness';
import { useVaultDocuments } from '@/lib/vault';

/** Opens an issuer's official home page (HTTPS, fixed catalogue) outside the app. */
function openIssuer(url: string) {
  if (!url.startsWith('https://')) return;
  if (Platform.OS === 'web') window.open(url, '_blank', 'noopener,noreferrer');
  else void Linking.openURL(url).catch(() => {});
}

/**
 * 신청 준비 — one card over the turn's results: 조건 → 준비 서류 → 서식 →
 * 공식 신청 → 상태 기록. Documents are matched against the vault on this
 * device; nothing about them is sent. Each later step is a fixed goal the
 * server plans without a model, or an in-app screen. The user submits on
 * the official site.
 */
export default function AgentJourney({ journey, busy, onGoal }: {
  journey: Journey; busy: boolean; onGoal: (goal: string) => void;
}) {
  const router = useRouter();
  const vault = useVaultDocuments();
  const rows = documentReadiness(journey.tasks, vault);
  const summary = readinessSummary(rows);
  const held = journey.pendingCards > 0;
  const step = (n: number, title: string, body: React.ReactNode) => <View style={s.step} accessibilityLabel={`${n}단계 ${title}`}>
    <Text style={s.index}>{n}</Text>
    <View style={s.stepBody}><Text style={s.stepTitle}>{title}</Text>{body}</View>
  </View>;
  return <View style={s.card} accessibilityLabel="신청 준비 단계">
    <Text style={s.cardTitle}>신청 준비</Text>
    {step(1, '조건 확인', <Text style={s.note}>{journey.verdict ? eligibilityLabels[journey.verdict] : '조건 확인 결과가 없어요.'}</Text>)}
    {step(2, '준비 서류', rows.length ? <View style={{ gap: 8 }}>
      <Text style={s.note}>보관함 대조 {summary.have}/{summary.total}</Text>
      {rows.map(row => <DocumentRow key={row.taskId} row={row} />)}
      <Pressable accessibilityRole="button" onPress={() => router.push('/document-vault')} style={s.link}><LinkLabel style={s.linkText}>서류 보관함 열기</LinkLabel></Pressable>
    </View> : <Text style={s.note}>공고에 정리된 제출 서류가 없어요. 공식 공고를 확인해 주세요.</Text>)}
    {step(3, '서식 작성', <View style={{ gap: 6 }}>
      {/* No template: the form turn's own result links the guided copy sheet. */}
      <Pressable accessibilityRole="button" disabled={busy || held} onPress={() => onGoal(JOURNEY_GOALS.form)} style={[s.outline, (busy || held) && s.disabled]}><Text style={s.outlineText}>서식 작성 시작</Text></Pressable>
    </View>)}
    {step(4, '공식 신청', <View style={{ gap: 6 }}>
      <Text style={s.note}>제출은 공식 신청처에서 직접 해요.</Text>
      <Pressable accessibilityRole="button" disabled={busy || held} onPress={() => onGoal(JOURNEY_GOALS.official)} style={[s.outline, (busy || held) && s.disabled]}><Text style={s.outlineText}>공식 신청 페이지로 가기</Text></Pressable>
    </View>)}
    {step(5, '신청 상태 기록', <Pressable accessibilityRole="button" disabled={busy || held} onPress={() => onGoal(JOURNEY_GOALS.applied)} style={[s.outline, (busy || held) && s.disabled]}><Text style={s.outlineText}>신청을 마쳤어요 · 기록하기</Text></Pressable>)}
    {held ? <Text style={s.note} accessibilityLiveRegion="polite">위의 승인 카드를 먼저 승인하거나 취소하면 다음 단계로 갈 수 있어요.</Text> : null}
  </View>;
}

function DocumentRow({ row }: { row: ReadinessRow }) {
  const bad = row.status === 'missing' || row.status === 'stale';
  return <View style={s.doc} accessibilityLabel={`${row.title}: ${readinessLabels[row.status]}`}>
    <View style={s.docHead}>
      <Text style={s.docTitle}>{row.title}{row.done ? ' · 체크함' : ''}</Text>
      <Text style={[s.badge, bad ? s.badgeBad : s.badgeOk]}>{readinessLabels[row.status]}</Text>
    </View>
    {readinessNote(row) ? <Text style={s.note}>{readinessNote(row)}</Text> : null}
    {bad && row.issuer?.url ? <Pressable accessibilityRole="link" accessibilityLabel={`${row.issuer.name}에서 발급`} onPress={() => openIssuer(row.issuer!.url!)} style={s.link}>
      <LinkLabel style={s.linkText}>{`${row.issuer.name}에서 발급`}</LinkLabel>
    </Pressable> : null}
  </View>;
}

const s = StyleSheet.create({
  card: { borderRadius: 16, borderWidth: 1, borderColor: '#2A8F6614', padding: 14, gap: 12, backgroundColor: '#2A8F660A' },
  cardTitle: { fontSize: 14, lineHeight: 20, fontWeight: '700', color: '#070428' },
  step: { flexDirection: 'row', gap: 10 },
  index: { width: 22, height: 22, borderRadius: 11, backgroundColor: '#2A8F660A', textAlign: 'center', paddingTop: 3, lineHeight: 16, fontSize: 11, color: '#239969', fontWeight: '600' },
  stepBody: { flex: 1, gap: 4 },
  stepTitle: { fontSize: 14, lineHeight: 20, fontWeight: '700', color: '#070428' },
  note: { fontSize: 12, lineHeight: 18, color: '#5D5D62' },
  doc: { borderTopWidth: 1, borderTopColor: '#F1F1F1', paddingTop: 8, gap: 2 },
  docHead: { flexDirection: 'row', alignItems: 'center', gap: 8 },
  docTitle: { flex: 1, fontSize: 13, lineHeight: 20, fontWeight: '600', color: '#070428' },
  badge: { fontSize: 11, lineHeight: 16, fontWeight: '600', paddingHorizontal: 6, paddingVertical: 2, borderRadius: 6, overflow: 'hidden' },
  badgeOk: { color: '#239969', backgroundColor: '#2A8F660A' },
  badgeBad: { color: '#070428', backgroundColor: '#F72B2B0A' },
  link: { minHeight: 36, justifyContent: 'center' },
  linkText: { fontSize: 13, lineHeight: 20, color: '#239969', fontWeight: '600' },
  outline: { minHeight: 44, borderRadius: 10, borderWidth: 1, borderColor: '#F1F1F1', alignItems: 'center', justifyContent: 'center', paddingHorizontal: 14, backgroundColor: '#fff' },
  outlineText: { fontSize: 14, lineHeight: 20, color: '#070428', fontWeight: '600' },
  disabled: { opacity: 0.45 },
});
