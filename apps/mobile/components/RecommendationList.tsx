import React, { useState } from "react";
import { FlatList, StyleSheet, View } from "react-native";
import { useRouter } from "expo-router";
import { useIsFetching } from "@tanstack/react-query";
import Text from "./Text";
import Tappable from "./Tappable";
import RecommendationCard from "./RecommendationCard";
import { Dropdown, EmptyState, FigmaIcon, ui } from "./FigmaUI";
import { fig } from "@/constants/theme";
import { ApiRequestError, displayTitle } from "@/lib/api";
import { useRecommendations } from "@/lib/catalogue";
import { matchesTab, type CategoryTab } from "@/constants/fields";
import { PROFILE_RETRY_TOAST, useBenefitProfile, useProfileStatus } from "@/lib/useBenefitProfile";
import { useLoginPrompt } from "./LoginPromptDialog";
import { useMemberAction } from "./MemberAction";
import { GUEST_RECOMMEND_REASON } from "@/lib/displayCopy";
import { useAuthStore } from "@/store/auth";
import { cardStatus, verifiedSections } from "@/lib/verifiedSections";

const SORTS = [{ label: "추천 순", value: "rank" }, { label: "마감 빠른 순", value: "deadline" }];

/** 추천 혜택 목록 (34065:5283). 홈과 같은 추천 쿼리를 써서 만료된 혜택이 다시 나타나지 않는다 */
export default function RecommendationList({ category, query: search }: { category: CategoryTab; query: string }) {
  const router = useRouter();
  const [verifiedOnly, setVerifiedOnly] = useState(false);
  const [sort, setSort] = useState<"rank" | "deadline">("rank");
  const login = useLoginPrompt();
  const user = useAuthStore(s => s.user);
  // Recommendations are members only (F7.1): a guest's device draft never counts as conditions.
  const draft = useBenefitProfile();
  const profile = user ? draft : null;
  const profileStatus = useProfileStatus();
  const restoring = useAuthStore(s => s.isLoading);
  const profilePending = useIsFetching({ queryKey: ["profile", "me", user?.id] }) > 0 || restoring;
  const query = useRecommendations(profile);
  // 409 profile_required: saved conditions are incomplete, same as having none.
  const incomplete = query.error instanceof ApiRequestError && query.error.status === 409;
  const key = search.toLocaleLowerCase();
  const filtered = (query.data?.items ?? []).filter(item => matchesTab(item, category) && (!key || displayTitle(item).toLocaleLowerCase().includes(key)));
  // 조건 확인됨 / 확인 필요 only while the server flag is on; otherwise one list as before.
  const sections = query.data ? verifiedSections(query.data, filtered) : null;
  const sectionEnabled = !!sections;
  // 34065:5283 count row: 조건 확인됨만 보기 keeps only rows the server verified (cardStatus "verified");
  // 마감 빠른 순 orders by deadline, undated last. The recommended order keeps the sections.
  const ranked = sections ? [...sections.verified, ...sections.needsCheck] : filtered;
  // Only offered while the server sorts verified rows; with the section off nothing is ever "verified".
  const onlyVerified = verifiedOnly && sectionEnabled;
  const shown = onlyVerified ? ranked.filter(item => cardStatus(item, sectionEnabled) === "verified") : ranked;
  const items = sort === "deadline" ? [...shown].sort((a, b) => (a.deadline ? Date.parse(a.deadline) : Infinity) - (b.deadline ? Date.parse(b.deadline) : Infinity)) : shown;
  const sectionStart = new Map<string, string>();
  if (sections && sort === "rank" && !onlyVerified) {
    if (sections.verified[0]) sectionStart.set(sections.verified[0].program_id, `조건 확인됨 ${sections.verified.length}건`);
    if (sections.needsCheck[0]) sectionStart.set(sections.needsCheck[0].program_id, `확인 필요 ${sections.needsCheck.length}건`);
  }
  // 조건이 없으면 회원은 조건 입력으로, 비회원은 로그인과 기기 저장 중에서 고르게 한다
  // A restoring member's tap is decided again once the saved conditions are known; only a guest is asked to log in.
  const member = useMemberAction({ conditions: { run: () => openConditions(), ready: profileStatus.state !== "loading" } });
  // A failed read is "try again", never a detour to onboarding.
  const openConditions = () => { if (profileStatus.state === "error") { member.say(PROFILE_RETRY_TOAST); profileStatus.retry(); return; } router.push(profile ? "/profile-edit" : "/onboarding?mode=edit"); };
  const editConditions = () => { if (profile) router.push("/profile-edit"); else member.run("conditions", () => login.prompt(GUEST_RECOMMEND_REASON)); };
  const header = query.data ? <>
    <View style={styles.result}><Text style={styles.count}>전체 <Text style={styles.countNumber}>{items.length}건</Text></Text>
      <View style={styles.controls}>
        {sectionEnabled && <>
          <Tappable accessibilityRole="checkbox" accessibilityState={{ checked: verifiedOnly }} onPress={() => setVerifiedOnly(v => !v)} style={styles.control}><FigmaIcon name="checkCircle" size={16} color={verifiedOnly ? fig.color.primary : fig.color.grayE1} /><Text style={styles.controlText}>조건 확인됨만 보기</Text></Tappable>
          <View style={styles.divider} />
        </>}
        <Dropdown accessibilityLabel="정렬" options={SORTS} value={sort} onSelect={value => setSort(value as "rank" | "deadline")} style={styles.control}><Text style={styles.sortText}>{SORTS.find(o => o.value === sort)?.label}</Text><FigmaIcon name="arrow" size={12} color={fig.color.primary} rotate={90} /></Dropdown>
      </View>
    </View>
  </> : null;
  return <><FlatList data={items} keyExtractor={item => item.program_id} renderItem={({ item }) => <>
    {sectionStart.has(item.program_id) && <Text accessibilityRole="header" style={styles.section}>{sectionStart.get(item.program_id)}</Text>}
    <RecommendationCard data={item} sectionEnabled={sectionEnabled} />
  </>}
    initialNumToRender={6} maxToRenderPerBatch={8} windowSize={7} showsVerticalScrollIndicator={false} contentContainerStyle={ui.content} keyboardShouldPersistTaps="handled"
    refreshing={query.isRefetching} onRefresh={() => { if (profile) void query.refetch(); }}
    ListHeaderComponent={header}
    ListEmptyComponent={onlyVerified && ranked.length ? <EmptyState title="조건 확인됨 혜택이 아직 없어요" description="확인이 필요한 혜택은 전체 목록에 있어요." action="전체 보기" onAction={() => setVerifiedOnly(false)} /> : <EmptyState loading={(!profile && profilePending) || (!!profile && query.isLoading)} title={incomplete ? "조건을 마저 입력해 주세요" : !user && !profilePending ? "로그인하고 맞춤 혜택을 받아보세요" : !profile ? profilePending ? "저장된 조건을 불러오고 있어요" : "맞춤 혜택을 찾아볼까요?" : query.isError ? "추천 혜택을 불러오지 못했어요" : query.isLoading ? "맞춤 혜택을 찾고 있어요" : search ? "검색어에 맞는 추천 혜택이 없어요" : "이 조건의 추천 혜택이 아직 없어요"} description={!profile ? "지역과 나이를 입력하면 받을 수 있는 혜택을 찾아드려요." : undefined} action={incomplete ? "내 조건 입력" : query.isError ? "다시 시도" : !profile && profilePending ? undefined : !profile ? "내 조건 입력" : undefined} onAction={() => incomplete ? editConditions() : query.isError ? void query.refetch() : editConditions()} />} />{login.dialog}{member.toastElement}</>;
}
// Count row and sections: 추천 혜택 목록 34065:5283 / 탐색 34065:2919.
const styles = StyleSheet.create({
  result: { flexDirection: "row", alignItems: "center", justifyContent: "space-between", minHeight: 35, paddingVertical: 8 },
  count: { ...fig.text.bold12, color: fig.color.black }, countNumber: { color: fig.color.primary },
  controls: { flexDirection: "row", alignItems: "center", gap: 8 },
  control: { flexDirection: "row", alignItems: "center", gap: 4, minHeight: 44, marginVertical: -12 },
  controlText: { ...fig.text.medium12, color: fig.color.deepGray }, sortText: { ...fig.text.medium12, color: fig.color.black },
  divider: { width: 1, height: 12, backgroundColor: fig.color.secondary8 },
  section: { ...fig.text.bold14, color: fig.color.black, marginTop: 4, marginBottom: 12 },
});
