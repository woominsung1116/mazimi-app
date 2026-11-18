import { deadlineLabel } from '@/lib/deadline';
import React, { useCallback, useState } from 'react';
import { ScrollView, View } from 'react-native';
import Tappable from '@/components/Tappable';
import { useFocusEffect, useRouter } from 'expo-router';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import Text from '@/components/Text';
import { LinkLabel, EmptyState, ScreenHeader, ui } from '@/components/FigmaUI';
import { assistantStyles as s } from '@/components/AssistantUI';
import { api, programTypeLabel } from '@/lib/api';
import { apiErrorText } from '@/lib/errorCopy';
import { assistantScenarios, eligibilityLabels, profileFieldList, type AssistantScenario, type EligibilityStatus } from '@/lib/assistantTypes';
import { useAuthStore } from '@/store/auth';
import NonMemberState from '@/components/NonMemberState';

export default function AssistantScreen() {
  const user = useAuthStore(state => state.user); const router = useRouter();
  return <View style={ui.screen}><ScreenHeader title="상황별 혜택 도우미" back />
    {user ? <Bundles key={user.id} userId={user.id} /> : <NonMemberState title="로그인하고 내 조건으로 찾아보세요" description="프로필에 저장한 조건으로 상황별 후보를 찾아요." />}
  </View>;
}

function Bundles({ userId }: { userId: string }) {
  const router = useRouter(); const client = useQueryClient();
  const [scenario, setScenario] = useState<AssistantScenario>('returning_student');
  const [selected, setSelected] = useState<string[]>([]);
  const query = useQuery({ queryKey: ['assistant-bundle', userId, scenario], queryFn: ({ signal }) => api.getAssistantBundle(scenario, signal), staleTime: 0, retry: 1 });
  useFocusEffect(useCallback(() => { void client.invalidateQueries({ queryKey: ['assistant-bundle', userId] }); }, [client, userId]));
  const data = query.isError ? undefined : query.data;
  // A refresh may remove an expired or newly ineligible candidate.
  const available = new Set(data?.groups.flatMap(group => group.items.map(item => item.program_id)) ?? []);
  const ids = selected.filter(id => available.has(id));
  function toggle(id: string) { setSelected(ids.includes(id) ? ids.filter(value => value !== id) : ids.length < 4 ? [...ids, id] : ids); }
  return <ScrollView contentContainerStyle={s.content}>
    <Text style={s.title}>지금 필요한 혜택부터{"\n"}하나씩 준비해요</Text>
    <Tappable accessibilityRole="button" onPress={() => router.push('/assistant-search')} style={[s.card, s.selected]}><LinkLabel style={s.heading} color="#070428">질문으로 혜택 찾기</LinkLabel><Text style={s.note}>예: 부산 사는 휴학생이 받을 수 있는 장학금?</Text></Tappable>
    <View style={s.chips}>{assistantScenarios.map(item => <Tappable key={item.id} accessibilityRole="button" accessibilityState={{ selected: scenario === item.id }} onPress={() => { setScenario(item.id); setSelected([]); }} style={[s.chip, scenario === item.id && s.selected]}><Text>{item.label}</Text></Tappable>)}</View>
    {!data ? <EmptyState loading={query.isLoading} title={query.isError ? '혜택 후보를 불러오지 못했어요' : '저장한 조건으로 확인 중이에요'} description={query.isError ? apiErrorText(query.error) : undefined} action="다시 확인" onAction={() => void query.refetch()} /> : <>
      {data.required_profile_fields.length > 0 ? <EmptyState title="먼저 내 조건을 알려주세요" description={profileFieldList(data.required_profile_fields)} action="내 조건 입력" onAction={() => router.push('/onboarding?mode=edit')} /> : <>
        <View style={[s.card, s.selected]}><Text style={s.heading}>혜택 비교 · {ids.length}/4개 선택</Text>
          <Tappable accessibilityRole="button" disabled={ids.length < 2 || query.isFetching} onPress={() => router.push({ pathname: '/compare', params: { ids: ids.join(',') } })} style={[ui.primaryButton, (ids.length < 2 || query.isFetching) && { opacity: 0.5 }]}><Text style={ui.primaryButtonText}>선택한 혜택 비교하기</Text></Tappable>
        </View>
        {data.groups.map(group => <View style={s.card} key={group.title}><Text style={s.heading}>{group.title}</Text><Text style={s.note}>조건 검토 후보 {group.total_candidates}건 중 {group.items.length}건</Text>
          {!group.items.length && <Text style={s.body}>이 분야에서 확인된 후보가 아직 없어요.</Text>}
          {group.items.map(item => <View style={s.row} key={item.program_id}><Text style={s.note}>{programTypeLabel(item.program_type)} · {eligibilityLabels[item.eligibility_status as EligibilityStatus] ?? '확인 필요'}</Text><Text style={s.heading}>{item.title}</Text>
            <Text style={s.note}>등록 마감: {item.deadline ? deadlineLabel({ deadline_at: item.deadline }) : '확인 필요'}</Text>
            {item.reasons.slice(0, 2).map(reason => <Text style={s.body} key={reason}>{reason}</Text>)}
            <Tappable accessibilityRole="checkbox" accessibilityLabel={`${item.title} 비교 선택`} accessibilityState={{ checked: ids.includes(item.program_id), disabled: !ids.includes(item.program_id) && ids.length >= 4 }} disabled={!ids.includes(item.program_id) && ids.length >= 4} onPress={() => toggle(item.program_id)} style={[s.chip, ids.includes(item.program_id) && s.selected]}><Text>{ids.includes(item.program_id) ? '비교에 담음' : ids.length >= 4 ? '최대 4개까지 선택할 수 있어요' : '비교에 담기'}</Text></Tappable>
            <Tappable accessibilityRole="button" onPress={() => router.push(`/benefit-assistant?programId=${item.program_id}`)} style={ui.outlineButton}><Text style={ui.outlineButtonText}>내 조건·준비 확인</Text></Tappable>
          </View>)}
        </View>)}
      </>}
      <Tappable accessibilityRole="button" onPress={() => router.push('/explore')} style={ui.outlineButton}><Text style={ui.outlineButtonText}>전체 혜택에서 더 찾아보기</Text></Tappable>
    </>}
  </ScrollView>;
}
