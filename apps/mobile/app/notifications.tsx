import React, { useEffect, useRef } from "react";
import { Pressable, RefreshControl, ScrollView, View } from "react-native";
import { useRouter } from "expo-router";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import Text from "@/components/Text";
import { EmptyState, FigmaIcon, ScreenHeader, deadlineLabel, ui } from "@/components/FigmaUI";
import { StyleSheet } from "react-native";
import { fig } from "@/constants/theme";
import { api } from "@/lib/api";
import DeadlineBadge from "@/components/DeadlineBadge";
import { useAuthStore } from "@/store/auth";
import NonMemberState from "@/components/NonMemberState";
export default function NotificationsScreen() {
 const router = useRouter(); const user = useAuthStore(s => s.user); const client = useQueryClient();
 const query = useQuery({ queryKey: ["alerts", user?.id], queryFn: () => api.getAlerts({ limit: 50 }), enabled: !!user });
 // 화면에 보인 알림은 읽음으로 기록하고, 이번 목록에서는 새 알림 표시를 유지한다
 const markRead = useMutation({ mutationFn: api.markAlertsRead, onSuccess: () => client.setQueryData(["alerts-unread", user?.id], { unread: 0 }) });
 const marked = useRef(false);
 useEffect(() => { if (!marked.current && (query.data?.unread ?? 0) > 0) { marked.current = true; markRead.mutate(); } }, [query.data?.unread]);
 return <View style={ui.screen}><ScreenHeader title="알림" back /><ScrollView contentContainerStyle={{ paddingBottom: 32 }} refreshControl={<RefreshControl refreshing={query.isRefetching} onRefresh={() => { if (user) void query.refetch(); }} />}>{query.data?.items.map(item => { const unread = item.read_at === null; const due = deadlineLabel(item); return <Pressable key={item.id} accessibilityRole="button" accessibilityLabel={`${unread ? "새 알림, " : ""}${item.program_title} ${due}`} onPress={() => router.push(`/programs/${item.program_id}`)} style={[n.row, unread && n.unread]}><View style={n.avatar}><FigmaIcon name="bell" size={20} color={unread ? fig.color.primary : fig.color.secondary32} /></View><View style={{ flex: 1, gap: 8 }}><Text style={n.text}><Text style={{ color: fig.color.black }}>{item.program_title}</Text> 신청 일정을 확인해보세요</Text><View style={ui.row}><DeadlineBadge program={item} /><Text style={n.time}>{new Date(item.alert_date).toLocaleDateString("ko-KR")}</Text></View></View></Pressable>; })}{!user ? <NonMemberState title="로그인하고 알림을 받아보세요" description="관심 혜택의 마감 일정을 알려드려요." /> : !query.data?.items.length && <EmptyState loading={query.isLoading} title={query.isError ? "알림을 불러오지 못했어요" : "아직 도착한 알림이 없어요"} description="관심 혜택의 마감 일정을 확인해 보세요." action="관심 혜택 보기" onAction={() => router.push("/manage")} />}</ScrollView></View>;
}

// Figma 알림 34065:14563: rows 34065:14567, unread 34065:14574, avatar 34065:14569.
const n = StyleSheet.create({
  row: { paddingVertical: 16, paddingHorizontal: 12, minHeight: 96, flexDirection: "row", gap: 8, borderBottomWidth: 1, borderBottomColor: fig.color.grayF7, backgroundColor: fig.color.white },
  unread: { backgroundColor: fig.color.unread },
  avatar: { width: fig.component.alertRow.avatar, height: fig.component.alertRow.avatar, borderRadius: 24, backgroundColor: fig.color.secondary4, borderWidth: 1, borderColor: fig.color.secondary4, alignItems: "center", justifyContent: "center" },
  text: { ...fig.text.body13, color: fig.color.gray66 },
  time: { ...fig.text.meta12, color: fig.color.deepGray },
});
