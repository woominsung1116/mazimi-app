import React, { useCallback } from 'react';
import { Pressable, StyleSheet, View } from 'react-native';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { useFocusEffect, useRouter } from 'expo-router';
import Text from './Text';
import { LinkLabel, ui } from './FigmaUI';
import { api } from '@/lib/api';
import { fig } from '@/constants/theme';
import { useAuthStore } from '@/store/auth';

export default function AssistantBriefingCard() {
  const user = useAuthStore(s => s.user);
  const router = useRouter();
  const client = useQueryClient();
  useFocusEffect(useCallback(() => {
    if (user?.id) void client.invalidateQueries({ queryKey: ['assistant-briefing', user.id] });
  }, [client, user?.id]));
  const query = useQuery({ queryKey: ['assistant-briefing', user?.id], queryFn: ({ signal }) => api.getAssistantBriefing(signal), enabled: !!user, staleTime: 30_000, retry: 1 });
  if (!user) return null;
  return <View style={s.card}>
    <Text style={s.title}>저장한 혜택, 다음 할 일</Text>
    {query.isError ? <Pressable accessibilityRole="button" onPress={() => void query.refetch()} style={s.item}><LinkLabel>다음 할 일을 불러오지 못했어요. 다시 시도</LinkLabel></Pressable> :
      !query.data ? <Text style={[ui.muted, s.item]}>남은 준비를 확인하고 있어요…</Text> :
      !query.data.items.length ? <Pressable accessibilityRole="button" onPress={() => router.push('/explore')} style={s.item}><LinkLabel>관심 혜택을 저장하고 준비를 시작해 보세요</LinkLabel></Pressable> :
      query.data.items.map(item => <Pressable key={item.program_id} accessibilityRole="button" onPress={() => router.push(`/benefit-assistant?programId=${item.program_id}`)} style={s.item}>
        <Text style={s.reason}>{item.reason}</Text><Text numberOfLines={2} style={s.name}>{item.title}</Text><LinkLabel style={ui.muted} color="#8894A1">{item.next_task}</LinkLabel>
      </Pressable>)}
  </View>;
}
// No frame of its own: the 홈 benefit card 34065:6288 with its divider and text styles.
const s = StyleSheet.create({
  card: { padding: 16, borderRadius: fig.component.card.radiusHome, backgroundColor: fig.color.white, borderWidth: 1, borderColor: fig.color.grayF1, boxShadow: fig.shadow.card, gap: 4 },
  title: { ...fig.text.bold16, color: fig.color.black },
  item: { paddingTop: 12, marginTop: 8, borderTopWidth: 1, borderColor: fig.color.grayF7, gap: 4 },
  reason: { ...fig.text.badge11, color: fig.color.secondary },
  name: { ...fig.text.bold14, color: fig.color.black },
});
