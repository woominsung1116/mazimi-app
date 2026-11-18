// Keep the existing route stable, but replace speculative payout simulations
// with an authenticated, read-only comparison of source-backed records.
import React, { useCallback, useState } from 'react';
import { Linking, Pressable, ScrollView, View } from 'react-native';
import { useFocusEffect, useLocalSearchParams, useRouter } from 'expo-router';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import Text from '@/components/Text';
import { EmptyState, ScreenHeader, ui } from '@/components/FigmaUI';
import { assistantStyles as s } from '@/components/AssistantUI';
import { api } from '@/lib/api';
import { apiErrorText } from '@/lib/errorCopy';
import { eligibilityLabels, safeOfficialUrl } from '@/lib/assistantTypes';
import { useAuthStore } from '@/store/auth';
import NonMemberState from '@/components/NonMemberState';

const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
export default function ComparisonScreen() {
  const params = useLocalSearchParams<{ ids?: string }>(); const router = useRouter();
  const user = useAuthStore(state => state.user);
  const ids = typeof params.ids === 'string' ? [...new Set(params.ids.split(','))] : [];
  const valid = ids.length >= 2 && ids.length <= 4 && ids.every(id => uuid.test(id));
  return <View style={ui.screen}><ScreenHeader title="혜택 비교" back />
    {!user ? <NonMemberState title="로그인하고 내 조건으로 비교하세요" /> : !valid ?
      <EmptyState title="비교할 혜택을 2~4개 골라주세요" description="조건·마감·준비 서류를 함께 확인할 수 있어요." action="상황별 혜택 고르기" onAction={() => router.replace('/assistant')} /> :
      <Comparison key={`${user.id}:${ids.join(',')}`} userId={user.id} ids={ids} />}
  </View>;
}

function Comparison({ userId, ids }: { userId: string; ids: string[] }) {
  const router = useRouter(); const client = useQueryClient(); const [message, setMessage] = useState('');
  const selection = ids.join(',');
  const query = useQuery({ queryKey: ['assistant-compare', userId, selection], queryFn: ({ signal }) => api.compareBenefits(ids, signal), staleTime: 0, retry: 1 });
  useFocusEffect(useCallback(() => { void client.invalidateQueries({ queryKey: ['assistant-compare', userId, selection] }); }, [client, userId, selection]));
  const data = query.isError ? undefined : query.data;
  if (!data) return <View style={s.content}><EmptyState loading={query.isLoading} title={query.isError ? '현재 비교할 수 없는 혜택이 있어요' : '조건과 서류를 확인하고 있어요'} description={query.isError ? apiErrorText(query.error) : undefined} action="다시 확인" onAction={() => void query.refetch()} /><Pressable accessibilityRole="button" onPress={() => router.replace('/assistant')} style={ui.outlineButton}><Text style={ui.outlineButtonText}>혜택 다시 고르기</Text></Pressable></View>;
  async function openSource(value: string | null) { const url = safeOfficialUrl(value); if (!url) { setMessage('확인된 공식 링크가 아니라서 열지 않았어요.'); return; } try { await Linking.openURL(url); } catch { setMessage('공식 공고를 열지 못했어요. 잠시 후 다시 시도해 주세요.'); } }
  const names = new Map(data.programs.map(program => [program.program_id, program.title]));
  return <ScrollView contentContainerStyle={s.content}>
    <Text style={s.title}>같이 살펴보고{"\n"}따로 확인할 조건을 찾아요</Text>
    <View style={[s.card, s.selected]}><Text style={s.heading}>중복 수혜 · 확인 필요</Text><Text style={s.body}>{data.compatibility_notice}</Text><Text style={s.note}>가능하다고 확인되지 않았다는 뜻이며, 모두 불가능하다는 뜻은 아니에요.</Text></View>
    {data.programs.map(program => {
      const amounts = [program.amounts.monthly != null ? `월 ${program.amounts.monthly.toLocaleString()}원` : null, program.amounts.semester != null ? `학기 ${program.amounts.semester.toLocaleString()}원` : null, program.amounts.once != null ? `1회 ${program.amounts.once.toLocaleString()}원` : null].filter(Boolean);
      return <View style={s.card} key={program.program_id}>
        <Text style={s.heading}>{program.title}</Text><Text style={s.body}>자격 상태 · {eligibilityLabels[program.eligibility.status]}</Text><Text style={s.note}>{program.eligibility.summary}</Text>
        <Text style={s.body}>공고 등록 금액: {amounts.length ? amounts.join(' / ') : '정액 정보 없음'}</Text><Text style={s.note}>기록된 지원 규모이며 실제 지급액이나 전액 수령을 보장하지 않아요.</Text>
        <Text style={s.body}>등록 마감: {program.deadline_at ? `${new Date(program.deadline_at).toLocaleString('ko-KR', { timeZone: 'Asia/Seoul' })} (한국 시간)` : '확인 필요'}</Text>
        <Text style={s.body}>미확인 준비 {program.remaining_task_ids.length}개</Text>
        {program.preparation.progress_reset && <Text style={s.notice}>공고가 바뀌어 준비 목록을 다시 확인해야 해요.</Text>}
        <Pressable accessibilityRole="button" onPress={() => router.push(`/benefit-assistant?programId=${program.program_id}`)} style={ui.primaryButton}><Text style={ui.primaryButtonText}>조건·준비 자세히 보기</Text></Pressable>
        {safeOfficialUrl(program.source.official_url) ? <Pressable accessibilityRole="link" onPress={() => void openSource(program.source.official_url)} style={ui.outlineButton}><Text style={ui.outlineButtonText}>공식 공고 확인</Text></Pressable> : <Text style={s.notice}>확인된 공식 링크가 없어요.</Text>}
      </View>;
    })}
    <View style={s.card}><Text style={s.heading}>함께 준비할 서류 후보</Text><Text style={s.note}>{data.documents_notice}</Text>
      {!data.shared_documents.length && <Text style={s.body}>등록된 목록에서 공통 서류를 확인하지 못했어요. 서류가 필요 없다는 뜻은 아니에요.</Text>}
      {data.shared_documents.map(doc => <View key={doc.name} style={s.row}><Text style={s.heading}>{doc.name}</Text>{doc.uses.map((use, index) => <View key={`${use.program_id}:${use.document_id}:${index}`}><Text style={s.body}>{names.get(use.program_id)} · {use.required === true ? '필수' : use.required === false ? '선택' : '필수 여부 확인 필요'}</Text></View>)}</View>)}
    </View>
    {!!message && <Text accessibilityRole="alert" style={s.notice}>{message}</Text>}
    <Pressable accessibilityRole="button" onPress={() => router.back()} style={ui.outlineButton}><Text style={ui.outlineButtonText}>선택으로 돌아가기</Text></Pressable>
  </ScrollView>;
}
