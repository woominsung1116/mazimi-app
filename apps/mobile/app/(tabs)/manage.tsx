import React, { useEffect, useState } from "react";
import { RefreshControl, ScrollView, View } from "react-native";
import { notify } from "@/lib/notify";
import NonMemberState from "@/components/NonMemberState";
import { useRouter } from "expo-router";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import Text from "@/components/Text";
import { BenefitCard, CategoryTabs, Dropdown, EmptyState, FigmaIcon, ScreenHeader, ui } from "@/components/FigmaUI";
import Tappable from "@/components/Tappable";
import { useConfirmDialog } from "@/components/ConfirmDialog";
import { effectiveDeadline } from "@/lib/deadline";
import { useRecommendations } from "@/lib/catalogue";
import { useBenefitProfile } from "@/lib/useBenefitProfile";
import { cardStatus, savedCardStatus } from "@/lib/verifiedSections";
import { withObjectParticle } from "@/lib/displayCopy";
import { StyleSheet } from "react-native";
import { fig } from "@/constants/theme";
import { useSchoolSelection } from "@/lib/useSchoolSelection";
import { api, displayTitle, type SavedProgram } from "@/lib/api";
import { invalidateAfterWrite } from "@/lib/writeEffects";
import { CAMPUS_TAB, matchesTab, type CategoryTab } from "@/constants/fields";
import { useAuthStore } from "@/store/auth";

export default function SavedScreen() {
  const router = useRouter();
  const user = useAuthStore(s => s.user);
  const client = useQueryClient();
  const [category, setCategory] = useState<CategoryTab>("");
  const school = useSchoolSelection();
  const activeCategory = category === CAMPUS_TAB && !school.showCampus ? "" : category;
  useEffect(() => { if (!school.showCampus && category === CAMPUS_TAB) setCategory(""); }, [school.showCampus, category]);
  const saved = useQuery({ queryKey: ["saved", user?.id], queryFn: api.getSaved, enabled: !!user });
  // A removed benefit leaves every category tab at once, before the server answers (origin/main f2df3882).
  // setBookmark(false) is idempotent, so a repeated tap never re-adds it.
  const savedKey = ["saved", user?.id];
  const remove = useMutation({ mutationFn: (id: string) => api.setBookmark(id, false),
    onMutate: async (id: string) => { await client.cancelQueries({ queryKey: savedKey }); const previous = client.getQueryData<{ items: SavedProgram[] }>(savedKey); client.setQueryData<{ items: SavedProgram[] }>(savedKey, old => old && { ...old, items: old.items.filter(p => p.id !== id) }); return { previous }; },
    onSettled: (_data, _error, id) => { void invalidateAfterWrite(client, 'bookmark', { userId: user?.id, programId: id }); },
    onError: (_error, _id, context) => { if (context?.previous) client.setQueryData(savedKey, context.previous); notify("저장 상태를 바꾸지 못했어요", "잠시 후 다시 시도해 주세요."); } });
  // 34059:2753 count row: 조건 확인됨만 보기 (only while the server sorts verified rows) and the sort.
  const [sort, setSort] = useState<"deadline" | "saved">("deadline");
  const [verifiedOnly, setVerifiedOnly] = useState(false);
  const recommend = useRecommendations(useBenefitProfile());
  const sectionEnabled = recommend.data?.verified_section_enabled === true;
  const verifiedIds = new Set((recommend.data?.items ?? []).filter(item => cardStatus(item, sectionEnabled) === "verified").map(item => item.program_id));
  const confirm = useConfirmDialog();
  // The checkbox only exists while the server sorts verified rows; when that turns off, so does the filter.
  useEffect(() => { if (recommend.data && !sectionEnabled && verifiedOnly) setVerifiedOnly(false); }, [recommend.data, sectionEnabled, verifiedOnly]);
  // The verified filter waits for the recommendations; until they load the list is unfiltered.
  const filtering = verifiedOnly && sectionEnabled && !!recommend.data;
  const inTab = (saved.data?.items ?? []).filter(p => matchesTab(p, activeCategory));
  const tabbed = filtering ? inTab.filter(p => verifiedIds.has(p.id)) : inTab;
  // 마감 빠른 순: open rows by deadline, undated after them, closed ones last; 저장 순 is the server's order.
  const rank = (p: SavedProgram) => p.available === false ? Infinity : (Date.parse(effectiveDeadline(p) ?? "") || Number.MAX_SAFE_INTEGER);
  const items = sort === "deadline" ? [...tabbed].sort((a, b) => rank(a) - rank(b)) : tabbed;
  // Figma 관심 혜택 34059:2750: title header, tabs, count row, cards with the X in the title row.
  return <View style={ui.screen}><ScreenHeader title="관심 혜택" tabs /><CategoryTabs value={activeCategory} showCampus={school.showCampus} onChange={setCategory} />
    <ScrollView contentContainerStyle={[ui.content, { paddingTop: 12 }]} refreshControl={<RefreshControl refreshing={saved.isRefetching} onRefresh={() => { if (user) void saved.refetch(); }} />}>
      <View style={[ui.row, { minHeight: 35, paddingVertical: 8 }]}><Text style={s.count}>전체 <Text style={s.countNumber}>{items.length}건</Text></Text>
        <View style={s.controls}>
          {sectionEnabled && <><Tappable accessibilityRole="checkbox" accessibilityState={{ checked: verifiedOnly }} onPress={() => setVerifiedOnly(v => !v)} style={s.control}><FigmaIcon name="checkCircle" size={16} color={verifiedOnly ? fig.color.primary : fig.color.grayE1} /><Text style={s.controlText}>조건 확인됨만 보기</Text></Tappable><View style={s.divider} /></>}
          <Dropdown accessibilityLabel="정렬" options={SORTS} value={sort} onSelect={value => setSort(value as "deadline" | "saved")} style={s.control}><Text style={s.sortText}>{SORTS.find(o => o.value === sort)?.label}</Text><FigmaIcon name="arrow" size={12} color={fig.color.primary} rotate={90} /></Dropdown>
        </View></View>
      {items.map(p => <BenefitCard key={p.id} program={p} eligibility={savedCardStatus(recommend.data?.items, sectionEnabled, p.id)} saved onSave={() => confirm.ask({ title: "관심 혜택 해제", message: `${withObjectParticle(displayTitle(p))}\n관심 목록에서 해제할까요?`, confirm: "해제", onConfirm: () => remove.mutate(p.id) })} />)}
      {!items.length && !user && <NonMemberState title="관심 혜택을 모아보세요" description="마음에 드는 혜택을 저장하고 신청 준비를 이어가세요." />}
      {!items.length && !!user && filtering && inTab.length > 0 && <EmptyState title="조건 확인된 관심 혜택이 없어요" description="확인이 필요한 혜택은 전체 목록에 있어요." action="전체 보기" onAction={() => setVerifiedOnly(false)} />}
      {!items.length && !!user && !(filtering && inTab.length > 0) && <EmptyState loading={saved.isLoading} title={saved.isError ? "관심 혜택을 불러오지 못했어요" : "아직 저장한 혜택이 없어요"} description="마음에 드는 혜택을 저장하고 신청 준비를 이어가세요." action={saved.isError ? "다시 시도" : "혜택 탐색하기"} onAction={() => { if (saved.isError) void saved.refetch(); else router.push("/explore"); }} />}
    </ScrollView>
    {confirm.dialog}
  </View>;
}

const SORTS = [{ label: "마감 빠른 순", value: "deadline" }, { label: "저장 순", value: "saved" }];

// 34059:2753
const s = StyleSheet.create({
  count: { ...fig.text.bold12, color: fig.color.black }, countNumber: { color: fig.color.primary },
  controls: { marginLeft: "auto", flexDirection: "row", alignItems: "center", gap: 8 },
  control: { flexDirection: "row", alignItems: "center", gap: 4, minHeight: 44, marginVertical: -12 },
  controlText: { ...fig.text.medium12, color: fig.color.deepGray }, sortText: { ...fig.text.medium12, color: fig.color.black },
  divider: { width: 1, height: 12, backgroundColor: fig.color.secondary8 },
});
