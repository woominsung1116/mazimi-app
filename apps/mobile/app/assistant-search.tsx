import { deadlineLabel } from '@/lib/deadline';
import React, { useState } from 'react';
import { Pressable, ScrollView, TextInput, View, StyleSheet } from 'react-native';
import { useRouter } from 'expo-router';
import { useMutation } from '@tanstack/react-query';
import Text from '@/components/Text';
import { EmptyState, ScreenHeader, ui } from '@/components/FigmaUI';
import { assistantStyles as s } from '@/components/AssistantUI';
import { api, programTypeLabel } from '@/lib/api';
import { eligibilityLabels, profileFieldList, type EligibilityStatus } from '@/lib/assistantTypes';
import { useAuthStore } from '@/store/auth';
import NonMemberState from '@/components/NonMemberState';

const EXAMPLES = ['부산 사는 휴학생이 받을 수 있는 장학금?', '서울 월세 지원 알려줘', '대학생 할인 혜택'];

/** Global assistant entry: one question across all benefits. Rule-parsed
 * filters narrow the same recommendation list; nothing is generated. */
export default function AssistantSearchScreen() {
  const user = useAuthStore(state => state.user); const router = useRouter();
  return <View style={ui.screen}><ScreenHeader title="질문으로 혜택 찾기" back />
    {user ? <Search key={user.id} userId={user.id} /> : <NonMemberState title="로그인하고 질문으로 찾아보세요" description="저장한 조건과 질문에 적은 조건으로 후보를 찾아요." />}
  </View>;
}

function Search({ userId }: { userId: string }) {
  const router = useRouter();
  const [question, setQuestion] = useState('');
  const search = useMutation({ mutationFn: (text: string) => api.searchAssistant(text), retry: false });
  const submit = (text: string) => { const trimmed = text.trim(); if (trimmed && !search.isPending) { setQuestion(trimmed); search.mutate(trimmed); } };
  const data = useAuthStore.getState().user?.id === userId ? search.data : undefined;
  const filters = data ? [data.intent.region_label, data.intent.enrollment_label, ...data.intent.program_types.map(programTypeLabel), ...data.intent.keywords].filter((v): v is string => !!v) : [];
  return <ScrollView contentContainerStyle={s.content} keyboardShouldPersistTaps="handled">
    <Text style={s.title}>궁금한 혜택을{"\n"}한 문장으로 물어보세요</Text>
    <TextInput accessibilityLabel="혜택 찾기 질문" placeholder="찾는 혜택을 물어보세요" maxLength={500} value={question} onChangeText={setQuestion} editable={!search.isPending} onSubmitEditing={() => submit(question)} style={local.input} />
    <Pressable accessibilityRole="button" disabled={!question.trim() || search.isPending} onPress={() => submit(question)} style={[ui.primaryButton, (!question.trim() || search.isPending) && { opacity: 0.5 }]}><Text style={ui.primaryButtonText}>{search.isPending ? '찾는 중…' : '찾기'}</Text></Pressable>
    {!data && <View style={s.chips}>{EXAMPLES.map(text => <Pressable key={text} accessibilityRole="button" disabled={search.isPending} onPress={() => submit(text)} style={s.chip}><Text>{text}</Text></Pressable>)}</View>}
    {search.isError && <Text accessibilityRole="alert" style={s.notice}>{search.error.message}</Text>}
    {data && <>
      {filters.length > 0 && <View style={s.chips} accessibilityLabel="질문에서 찾은 조건">{filters.map(label => <View key={label} style={[s.chip, s.selected]}><Text>{label}</Text></View>)}</View>}
      <Text style={s.note}>{data.notice}</Text>
      {data.required_profile_fields.length > 0 ? <EmptyState title="먼저 내 조건을 알려주세요" description={profileFieldList(data.required_profile_fields)} action="내 조건 입력" onAction={() => router.push('/onboarding?mode=edit')} /> : <>
        {data.not_found && !data.unsupported && <View style={s.card}><Text style={s.heading}>조건에 맞는 혜택을 찾지 못했어요</Text><Text style={s.body}>키워드를 바꾸거나 전체 혜택에서 찾아보세요.</Text></View>}
        {data.results.map(item => <View key={item.program_id} style={s.card}>
          <Text style={s.note}>{programTypeLabel(item.program_type)} · {eligibilityLabels[item.eligibility_status as EligibilityStatus] ?? '확인 필요'}</Text>
          <Text style={s.heading}>{item.title}</Text>
          <Text style={s.note}>등록 마감: {item.deadline ? deadlineLabel({ deadline_at: item.deadline }) : '확인 필요'}</Text>
          {item.matched_keywords.length > 0 && <Text style={s.body}>질문과 맞는 말: {item.matched_keywords.join(' · ')}</Text>}
          {item.youth_fit_label ? <Text style={s.body}>{item.youth_fit_label}</Text> : null}
          {item.reasons.slice(0, 2).map(reason => <Text style={s.body} key={reason}>{reason}</Text>)}
          <Pressable accessibilityRole="button" onPress={() => router.push(`/benefit-assistant?programId=${item.program_id}`)} style={ui.outlineButton}><Text style={ui.outlineButtonText}>이 혜택에 대해 물어보기</Text></Pressable>
        </View>)}
        {data.total_candidates > data.results.length && <Text style={s.note}>조건에 맞는 {data.total_candidates}개 중 상위 {data.results.length}개를 보여 드려요.</Text>}
      </>}
      <Pressable accessibilityRole="button" onPress={() => router.push('/explore')} style={ui.outlineButton}><Text style={ui.outlineButtonText}>전체 혜택에서 더 찾아보기</Text></Pressable>
    </>}
  </ScrollView>;
}

const local = StyleSheet.create({
  input: { minHeight: 52, borderWidth: 1, borderColor: '#F1F1F1', borderRadius: 12, padding: 12, fontSize: 13, lineHeight: 20 },
});
