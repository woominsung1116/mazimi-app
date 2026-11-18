import React, { useState } from "react";
import { Pressable, RefreshControl, ScrollView, StyleSheet, View } from "react-native";
import { useRouter } from "expo-router";
import { useQuery } from "@tanstack/react-query";
import Text from "@/components/Text";
import { CategoryIcon, Dropdown, EmptyState, FigmaIcon, ScreenHeader, ui } from "@/components/FigmaUI";
import { CATEGORY_STYLE } from "@/constants/categoryStyle";
import { fig } from "@/constants/theme";
import type { ApplicationDisplayStatus } from "@/lib/api";
import { api } from "@/lib/api";
import { useAuthStore } from "@/store/auth";
import NonMemberState from "@/components/NonMemberState";
/** The status label comes from the server (`display_label`); every client shows the same honest wording. */
export default function ApplicationsScreen() {
 const router = useRouter(); const user = useAuthStore(s => s.user);
 const query = useQuery({ queryKey: ["applications", user?.id], queryFn: api.getMyApplications, enabled: !!user });
 const [sort, setSort] = useState<"latest" | "oldest">("latest");
 // Sorted by the date each card shows (신청 날짜, else the last record), newest or oldest first.
 const shown = (item: { applied_at: string | null; updated_at: string }) => Date.parse(item.applied_at ?? item.updated_at) || 0;
 const items = [...(query.data?.items ?? [])].sort((a, b) => sort === "latest" ? shown(b) - shown(a) : shown(a) - shown(b));
 // Figma 신청 이력 34068:3699: count row 34068:3800, cards with status badge, "기관 ・ 날짜 신청", the programme summary.
 return <View style={ui.screen}><ScreenHeader title="신청 이력" back /><ScrollView contentContainerStyle={[ui.content, { paddingTop: 12 }]} refreshControl={<RefreshControl refreshing={query.isRefetching} onRefresh={() => { if (user) void query.refetch(); }} />}>
   {!!items.length && <View style={a.countRow}><Text style={a.count}>전체 <Text style={a.countNumber}>{items.length}건</Text></Text><Dropdown accessibilityLabel="정렬" options={SORTS} value={sort} onSelect={value => setSort(value as "latest" | "oldest")} style={a.sort}><Text style={a.sortText}>{SORTS.find(o => o.value === sort)?.label}</Text><FigmaIcon name="arrow" size={12} color={fig.color.primary} rotate={90} /></Dropdown></View>}
   {items.map(item => <Pressable key={item.program_id} onPress={() => router.push(`/application/${item.program_id}`)} style={[ui.benefitCard, { paddingBottom: 16 }]}>
   <View style={ui.row}>{/* an application row carries no category: the default style */}<CategoryIcon style={CATEGORY_STYLE.default} /><View style={{ flex: 1, gap: 2 }}><View style={ui.cardTitleRow}><Text style={[ui.cardTitle, { flex: 1 }]} numberOfLines={2}>{item.program_title}</Text><View style={[ui.typeBadge, { backgroundColor: statusColor(item.display_status) }]}><Text style={ui.typeBadgeText}>{item.display_label}</Text></View></View><Text style={ui.muted} numberOfLines={1}>{item.provider_name ? <>{item.provider_name}<Text style={ui.metaDot}>・</Text></> : null}{dateLabel(item.applied_at ?? item.updated_at)} {item.applied_at ? "신청" : "기록"}</Text></View></View>
   {item.summary_display ? <Text style={ui.cardSummary} numberOfLines={2}>{item.summary_display}</Text> : null}</Pressable>)}{!user ? <NonMemberState title="로그인하고 신청 이력을 모아보세요" /> : !items.length && <EmptyState loading={query.isLoading} title={query.isError ? "신청 이력을 불러오지 못했어요" : "아직 신청 이력이 없어요"} action="혜택 탐색하기" onAction={() => router.push("/explore")} />}</ScrollView></View>;
}

const SORTS = [{ label: "최신 순", value: "latest" }, { label: "오래된 순", value: "oldest" }];
/** "2025.05.12" (34068:3829). */
const dateLabel = (iso: string) => { const d = new Date(iso); return `${d.getFullYear()}.${String(d.getMonth() + 1).padStart(2, "0")}.${String(d.getDate()).padStart(2, "0")}`; };
const a = StyleSheet.create({
  countRow: { flexDirection: "row", alignItems: "center", justifyContent: "space-between", minHeight: 35, paddingVertical: 8 },
  count: { ...fig.text.bold12, color: fig.color.black }, countNumber: { color: fig.color.primary },
  sort: { flexDirection: "row", alignItems: "center", gap: 4, minHeight: 44, marginVertical: -12 }, sortText: { ...fig.text.medium12, color: fig.color.black },
});

// Figma 신청 이력 34068:3699 badges: 완료 Blue, 준비 중 Primary. No status here is a 반려 (Orange).
function statusColor(status: ApplicationDisplayStatus): string {
  if (status === "applied" || status === "waiting" || status === "received") return fig.color.blue;
  // 포기 is the member's own choice, not a 반려 (Orange): it reads as neutral like an unconfirmed submission.
  if (status === "abandoned" || status === "submission_unconfirmed") return fig.color.deepGray;
  return fig.color.primary;
}
