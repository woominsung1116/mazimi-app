import React, { useState } from 'react';
import { Pressable, ScrollView, StyleSheet, View } from 'react-native';
import { useRouter } from 'expo-router';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import Text from '@/components/Text';
import { FigmaToggle, EmptyState, ScreenHeader, ui } from '@/components/FigmaUI';
import { api } from '@/lib/api';
import type { ProactiveSettings } from '@/lib/agentTypes';
import { runStatusLabels } from '@/lib/agent';
import { useAuthStore } from '@/store/auth';
import NonMemberState from '@/components/NonMemberState';

const PROACTIVE: { key: keyof ProactiveSettings; label: string; description: string }[] = [
  { key: 'weekly_briefing', label: '주간 할 일 브리핑', description: '월요일 오전, 이번 주 마감과 준비 중인 혜택 개수를 알려요.' },
  { key: 'change_alerts', label: '공고 변경 알림', description: '저장한 혜택의 마감·조건·서류가 바뀌면 알려요.' },
  { key: 'resume_suggestions', label: '멈춘 준비 다시 알림', description: '일주일 넘게 멈춘 신청 준비를 마감 전에 알려요.' },
];

export default function AgentHistoryScreen() {
  const user = useAuthStore(s => s.user);
  const router = useRouter();
  return <View style={ui.screen}><ScreenHeader title="비서 기록" back />
    {!user ? <NonMemberState title="로그인하고 비서 대화 기록을 확인하세요" /> : <History userId={user.id} />}
  </View>;
}

function History({ userId }: { userId: string }) {
  const client = useQueryClient();
  const [open, setOpen] = useState<string | null>(null);
  const [confirming, setConfirming] = useState<string | null>(null);
  const [error, setError] = useState('');
  const list = useQuery({ queryKey: ['agent-conversations', userId], queryFn: ({ signal }) => api.listAgentConversations(signal) });
  const detail = useQuery({ queryKey: ['agent-conversation', userId, open], queryFn: ({ signal }) => api.getAgentConversation(open as string, signal), enabled: !!open });
  const proactive = useQuery({ queryKey: ['agent-proactive', userId], queryFn: () => api.getAgentProactive() });
  const remove = useMutation({
    mutationFn: (id: string) => api.deleteAgentConversation(id),
    onSuccess: (_, id) => { if (open === id) setOpen(null); setConfirming(null); void client.invalidateQueries({ queryKey: ['agent-conversations', userId] }); },
    onError: (e: Error) => setError(e.message),
  });
  const save = useMutation({
    mutationFn: (settings: ProactiveSettings) => api.saveAgentProactive(settings),
    onSuccess: () => void client.invalidateQueries({ queryKey: ['agent-proactive', userId] }),
    onError: (e: Error) => setError(e.message),
  });
  const settings = proactive.data?.settings;
  return <ScrollView contentContainerStyle={s.content}>
    <Text style={s.heading}>대화 기록</Text>
    <Text style={s.note}>요청은 연락처·긴 숫자를 가린 뒤 저장되고, {list.data?.retention_days ?? 30}일 동안 활동이 없으면 자동으로 지워져요. 지운 기록은 되살릴 수 없어요.</Text>
    {list.isLoading ? <Text style={s.note}>불러오는 중…</Text> : null}
    {list.data?.items.length === 0 ? <Text style={s.note}>아직 기록이 없어요.</Text> : null}
    {list.data?.items.map(item => <View key={item.id} style={s.card}>
      <Pressable accessibilityRole="button" accessibilityState={{ expanded: open === item.id }} onPress={() => setOpen(open === item.id ? null : item.id)} style={s.row}>
        <View style={{ flex: 1 }}>
          <Text style={s.title}>{item.title || '제목 없는 대화'}</Text>
          <Text style={s.note}>{item.updated_at.slice(0, 10)}{item.last_status ? ` · ${runStatusLabels[item.last_status]}` : ''}{item.pending_confirmations ? ` · 승인 대기 ${item.pending_confirmations}건` : ''}</Text>
        </View>
        <Text style={s.link}>{open === item.id ? '접기' : '보기'}</Text>
      </Pressable>
      {open === item.id && detail.data ? <View style={s.messages}>
        {detail.data.messages.map(m => <Text key={m.id} style={m.role === 'user' ? s.mine : s.theirs}>{m.role === 'user' ? '나: ' : '비서: '}{m.content}</Text>)}
      </View> : null}
      {confirming === item.id ? <View style={s.row}>
        <Text style={[s.note, { flex: 1 }]}>이 대화와 실행 기록을 지울까요?</Text>
        <Pressable accessibilityRole="button" onPress={() => setConfirming(null)} style={s.small}><Text style={s.link}>취소</Text></Pressable>
        <Pressable accessibilityRole="button" accessibilityLabel={`${item.title} 삭제 확인`} disabled={remove.isPending} onPress={() => remove.mutate(item.id)} style={s.small}><Text style={s.danger}>삭제</Text></Pressable>
      </View> : <Pressable accessibilityRole="button" accessibilityLabel={`${item.title} 삭제`} onPress={() => setConfirming(item.id)} style={s.small}><Text style={s.danger}>삭제</Text></Pressable>}
    </View>)}
    <Text style={[s.heading, { marginTop: 16 }]}>먼저 알려 주기</Text>
    <Text style={s.note}>모두 꺼져 있어요. 켠 항목만, 알림 설정에서 푸시를 허용했을 때 오전 9시~오후 9시 사이에 하루 최대 1건 보내요.</Text>
    {settings ? PROACTIVE.map(option => <View key={option.key} style={[s.card, s.row]}>
      <View style={{ flex: 1 }}>
        <Text style={s.title}>{option.label}</Text>
        <Text style={s.note}>{option.description}</Text>
      </View>
      <FigmaToggle accessibilityLabel={option.label} value={settings[option.key]} disabled={save.isPending}
        onValueChange={value => save.mutate({ ...settings, [option.key]: value })} />
    </View>) : null}
    {error ? <Text accessibilityRole="alert" style={s.danger}>{error}</Text> : null}
  </ScrollView>;
}

const s = StyleSheet.create({
  content: { padding: 16, paddingBottom: 48, gap: 10 },
  heading: { fontSize: 16, lineHeight: 24, fontWeight: '700', color: '#070428' },
  card: { borderRadius: 14, borderWidth: 1, borderColor: '#F1F1F1', padding: 12, gap: 8 },
  row: { flexDirection: 'row', alignItems: 'center', gap: 8, minHeight: 44 },
  title: { fontSize: 14, lineHeight: 20, fontWeight: '600', color: '#070428' },
  note: { fontSize: 12, lineHeight: 18, color: '#5D5D62' },
  link: { fontSize: 13, lineHeight: 20, color: '#239969', fontWeight: '600' },
  danger: { fontSize: 13, lineHeight: 20, color: '#F75B2B', fontWeight: '600' },
  small: { minHeight: 44, minWidth: 44, justifyContent: 'center', alignItems: 'center', alignSelf: 'flex-start' },
  messages: { gap: 6, borderTopWidth: 1, borderTopColor: '#F1F1F1', paddingTop: 8 },
  mine: { fontSize: 13, lineHeight: 20, color: '#070428', fontWeight: '600' },
  theirs: { fontSize: 13, lineHeight: 20, color: '#070428' },
});
