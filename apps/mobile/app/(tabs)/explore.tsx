import React, { useEffect, useRef, useState } from "react";
import { ActivityIndicator, RefreshControl, ScrollView, StyleSheet, View } from "react-native";
import { useLocalSearchParams, useRouter } from "expo-router";
import { keepPreviousData, useInfiniteQuery, useIsFetching, useQuery } from "@tanstack/react-query";
import Text from "@/components/Text";
import Tappable from "@/components/Tappable";
import { fig } from "@/constants/theme";
import { BenefitCard, CategoryTabs, Dropdown, EmptyState, FigmaIcon, NotificationBell, ScreenHeader, SearchBar, ui } from "@/components/FigmaUI";
import { cardStatus } from "@/lib/verifiedSections";
import { api } from "@/lib/api";
import { useSchoolSelection } from "@/lib/useSchoolSelection";
import { PROFILE_RETRY_TOAST, useBenefitProfile, useProfileStatus } from "@/lib/useBenefitProfile";
import { enrollmentLabel } from "@/lib/profileCodes";
import { useLoginPrompt } from "@/components/LoginPromptDialog";
import { useMemberAction } from "@/components/MemberAction";
import { GUEST_LOCAL_REASON } from "@/lib/displayCopy";
import { findRegion } from "@/constants/regions";
import { useAuthStore } from "@/store/auth";
import { useCategorySupport, useRecommendations } from "@/lib/catalogue";
import { ALL_TAB, CAMPUS_TAB, FIELD_TABS, fieldTab, providerTab, tabFromLegacy, tabQuery, type CategoryTab } from "@/constants/fields";
import { ProgramListSkeleton } from "@/components/Skeleton";
import { chipQuery, corporateChips, countFilters, keptSelection } from "@/lib/corporateChips";

const categoryOptions = [{ label: "전체", value: "" }, ...FIELD_TABS.map(tab => ({ label: tab.label, value: fieldTab(tab.value) }))];
// 34065:3334 소득 구간: the programmes' own 기준 중위소득 bands (no 만원 data exists for the designed slider).
const incomeOptions = [{ label: "전체", value: "" }, { label: "중위소득 50% 이하", value: "0-50" }, { label: "중위소득 51~75%", value: "51-75" }, { label: "중위소득 76~100%", value: "76-100" }, { label: "중위소득 101~200%", value: "101-200" }, { label: "중위소득 200% 초과", value: "200+" }];
const statusOptions = [{ label: "전체", value: "" }, { label: "모집 중", value: "open" }, { label: "모집 예정", value: "upcoming" }];
// "deadline": dated rows first, rolling ones last (contract §5). An older server treats it as deadline_asc.
const sortOptions = [{ label: "마감 빠른 순", value: "deadline" }, { label: "지원금 높은 순", value: "benefit_desc" }];
const sortFromParam = (value: string | undefined) => value === "deadline_asc" ? "deadline" : value;

export default function ExploreScreen() {
  const router = useRouter();
  const params = useLocalSearchParams<{ category?: string; view?: string; status?: string; sort?: string }>();
  // 맞춤 추천 is its own page (Figma 34065:5283); an old ?view=recommend link opens it.
  // 홈에서 맞춤 추천으로 들어오면 이미 열린 탭이어도 추천 목록으로 바꾼다
  useEffect(() => { if (params.view !== "recommend") return; router.setParams({ view: undefined }); router.push("/recommendations"); }, [params.view]);
  const [category, setCategory] = useState<CategoryTab>(tabFromLegacy(params.category));
  // 다른 화면에서 분야를 골라 들어오면 이미 열린 탭이어도 그 분야로 바꾼다
  useEffect(() => { if (params.category === undefined) return; setCategory(tabFromLegacy(params.category)); setCorporateCategory(""); router.setParams({ category: undefined }); }, [params.category]);
  const [search, setSearch] = useState("");
  const [showSearch, setShowSearch] = useState(false);
  const [query, setQuery] = useState("");
  const [status, setStatus] = useState("");
  const [incomeBand, setIncomeBand] = useState("");
  const user = useAuthStore(s => s.user);
  const restoring = useAuthStore(s => s.isLoading);
  const [localOnly, setLocalOnly] = useState(false);
  const [corporateCategory, setCorporateCategory] = useState("");
  const [sort, setSort] = useState("deadline");
  // 홈의 마감 임박처럼 모집 상태와 정렬을 정해서 들어오면 그대로 적용한다
  useEffect(() => {
    if (params.status === undefined && params.sort === undefined) return;
    if (statusOptions.some(o => o.value === params.status)) setStatus(params.status!);
    const nextSort = sortFromParam(params.sort); if (sortOptions.some(o => o.value === nextSort)) setSort(nextSort!);
    router.setParams({ status: undefined, sort: undefined });
  }, [params.status, params.sort]);
  const profile = useBenefitProfile();
  const profileStatus = useProfileStatus();
  const login = useLoginPrompt();
  const school = useSchoolSelection();
  const activeCategory = category === CAMPUS_TAB && !school.showCampus ? ALL_TAB : category;
  const campus = activeCategory === CAMPUS_TAB;
  const corporate = activeCategory === providerTab("corporate");
  const categories = useCategorySupport();
  useEffect(() => { if (!school.showCampus && category === CAMPUS_TAB) setCategory(ALL_TAB); }, [school.showCampus, category]);
  useEffect(() => { const timer = setTimeout(() => { setQuery(search.trim()); }, 300); return () => clearTimeout(timer); }, [search]);
  const changeCategory = (value: CategoryTab) => { setCategory(value); setCorporateCategory(""); };
  const filters = { ...tabQuery(activeCategory, categories !== false), q: query || undefined, status: status || undefined, income_band: incomeBand || undefined, region: localOnly ? profile?.region_code : undefined, corporate_category: chipQuery(corporateCategory, corporate, categories), sort, per_page: 20 };
  // Keep a selected local filter closed while the profile is restoring.
  // Falling back to an unfiltered request would show other cities as "내 지역".
  // 필터나 검색어가 바뀌면 query key가 바뀌어 첫 페이지부터 다시 불러온다
  const programs = useInfiniteQuery({ queryKey: ["programs", filters, campus ? school : null], initialPageParam: 1, queryFn: ({ pageParam }) => campus ? api.getCampusPrograms(school.school, school.enrollmentStatus, query, pageParam) : api.getPrograms({ ...filters, page: pageParam }), getNextPageParam: (last, pages) => last.has_next ? pages.length + 1 : undefined, enabled: (campus || categories !== undefined) && (campus || !localOnly || !!profile) });
  // A list waiting on the category probe is loading, not empty.
  const listLoading = programs.isLoading || (!campus && categories === undefined);
  const loaded = programs.data?.pages.flatMap(page => page.items) ?? [];
  const total = programs.data?.pages[0]?.total;
  // 기업혜택 chips by category, not by company, counted under the list's own search, status and region.
  // An older server without the category axis gets no chips. While new counts load the old ones stay, without numbers.
  const chipFilters = countFilters(filters);
  const corporateCategories = useQuery({ queryKey: ["corporate-categories", user?.id, chipFilters], queryFn: () => api.getCorporateCategories(chipFilters), enabled: corporate && categories === true && (!localOnly || !!profile), placeholderData: keepPreviousData });
  const freshCounts = corporateCategories.isPlaceholderData ? undefined : corporateCategories.data;
  const chips = corporateChips(corporateCategories.data, categories, !!freshCounts);
  useEffect(() => { const kept = keptSelection(corporateCategory, freshCounts); if (kept !== corporateCategory) setCorporateCategory(kept); }, [freshCounts, corporateCategory]);
  const matches = useRecommendations(profile);
  // Card wording from `verified` while the 조건 확인됨 section is on, else from eligibility_status as before.
  const sectionEnabled = matches.data?.verified_section_enabled === true;
  const statusById = new Map(matches.data?.items.map(p => [p.program_id, cardStatus(p, sectionEnabled)]));
  const statusOf = (id: string) => statusById.get(id) ?? "unknown";
  const items = (localOnly && !profile && !campus ? [] : loaded).filter((p, i, list) => list.findIndex(other => other.id === p.id) === i);
  const loadMore = () => { if (programs.hasNextPage && !programs.isFetchingNextPage) void programs.fetchNextPage(); };
  // 저장된 조건을 불러오는 중에는 조건이 없는 회원으로 보지 않는다
  const profilePending = useIsFetching({ queryKey: ["profile", "me", user?.id] }) > 0 || restoring;
  const regionLabel = profile ? findRegion(profile.region_code)?.label : undefined;
  const targetOptions = [{ label: "전체", value: "" }, { label: regionLabel ? `내 지역 (${regionLabel})` : "내 지역", value: "local", description: profile ? undefined : profilePending ? "저장된 조건을 불러오고 있어요" : "조건을 입력하면 선택할 수 있어요" }];
  // 내 지역을 고르다 조건을 입력하러 갔으면 돌아왔을 때 바로 적용한다
  const wantLocal = useRef(false);
  useEffect(() => { if (!wantLocal.current || !profile) return; wantLocal.current = false; setLocalOnly(true); }, [profile]);
  // 조건이 없으면 회원은 조건 입력으로, 비회원은 로그인과 기기 저장 중에서 고르게 한다
  const selectTarget = (value: string) => {
    if (value === "local" && !profile) {
      // A restoring member (or one whose conditions are still loading) is kept and decided again with the real profile; only a guest is asked to log in.
      if (!user || profileStatus.state === "loading") { member.run("local", () => login.prompt(GUEST_LOCAL_REASON)); return; }
      if (profileStatus.state === "error") { member.say(PROFILE_RETRY_TOAST); profileStatus.retry(); return; }
      wantLocal.current = true;
      router.push("/onboarding?mode=edit");
      return;
    }
    setLocalOnly(value === "local");
  };
  const member = useMemberAction({ local: { run: () => selectTarget("local"), ready: profileStatus.state !== "loading" } });
  const chevron = <FigmaIcon name="arrow" size={10} color={fig.color.grayC8} />;
  // Figma 탐색 34065:2917: title header, tabs, filter bar, then the list with its count and sort row.
  return <View style={ui.screen}><ScreenHeader title="탐색" tabs action={<View style={{ flexDirection: "row" }}><Tappable style={ui.iconButton} accessibilityRole="button" accessibilityLabel="혜택 검색 열기" accessibilityState={{ expanded: showSearch }} onPress={() => { setShowSearch(!showSearch); if (showSearch) setSearch(""); }}><FigmaIcon name="searchMono" size={20} color={fig.color.grayC8} /></Tappable><NotificationBell /></View>} />
    <CategoryTabs value={activeCategory} showCampus={school.showCampus} onChange={changeCategory} />
    {showSearch && <SearchBar value={search} onChange={setSearch} onSubmit={() => setQuery(search.trim())} placeholder="혜택명이나 기업명으로 검색" label="혜택 검색" />}
    <>
    {!campus && <View style={s.filters}><ScrollView horizontal showsHorizontalScrollIndicator={false} contentContainerStyle={{ gap: 8, paddingHorizontal: 16 }}>
      <Dropdown accessibilityLabel="대상 필터" options={targetOptions} value={localOnly ? "local" : ""} onSelect={selectTarget} style={[s.filter, localOnly && s.filterActive]}><Text style={s.filterLabel}>대상</Text><Text style={s.filterValue}>{localOnly ? targetOptions[1].label : "전체"}</Text>{chevron}</Dropdown>
      {/* 34065:3289 카테고리: the field axis of the tabs above */}
      <Dropdown accessibilityLabel="카테고리 필터" options={categoryOptions} value={FIELD_TABS.some(t => fieldTab(t.value) === activeCategory) ? activeCategory : ""} onSelect={value => changeCategory(value || ALL_TAB)} style={[s.filter, FIELD_TABS.some(t => fieldTab(t.value) === activeCategory) && s.filterActive]}><Text style={s.filterLabel}>카테고리</Text><Text style={s.filterValue}>{FIELD_TABS.find(t => fieldTab(t.value) === activeCategory)?.label ?? "전체"}</Text>{chevron}</Dropdown>
      <Dropdown accessibilityLabel="모집 상태 필터" options={statusOptions} value={status} onSelect={setStatus} style={[s.filter, !!status && s.filterActive]}><Text style={s.filterLabel}>모집 상태</Text><Text style={s.filterValue}>{statusOptions.find(o => o.value === status)?.label}</Text>{chevron}</Dropdown>
      <Dropdown accessibilityLabel="소득 구간 필터" options={incomeOptions} value={incomeBand} onSelect={setIncomeBand} style={[s.filter, !!incomeBand && s.filterActive]}><Text style={s.filterLabel}>소득 구간</Text><Text style={s.filterValue}>{incomeOptions.find(o => o.value === incomeBand)?.label}</Text>{chevron}</Dropdown>
    </ScrollView></View>}
    {corporate && chips.length > 1 && <ScrollView horizontal showsHorizontalScrollIndicator={false} style={{ flexGrow: 0 }} contentContainerStyle={s.chips}>{chips.map(c => <Tappable key={c.code} style={[s.filter, corporateCategory === c.code && s.filterActive]} onPress={() => setCorporateCategory(c.code)} accessibilityRole="button" accessibilityState={{ selected: corporateCategory === c.code }}><Text style={[s.filterValue, corporateCategory === c.code && s.filterValueActive]}>{c.label}{c.count > 0 ? ` ${c.count}` : ""}</Text></Tappable>)}</ScrollView>}
    {campus && <View style={{ paddingHorizontal: 16, paddingTop: 12, gap: 2 }}><Text style={s.campusName}>{school.school}</Text><Text style={ui.muted}>{enrollmentLabel(school.enrollmentStatus) || "학적 미선택"}</Text></View>}
    <ScrollView showsVerticalScrollIndicator={false} contentContainerStyle={[ui.content, { paddingTop: 12 }]} scrollEventThrottle={200} onScroll={({ nativeEvent: e }) => { if (e.layoutMeasurement.height + e.contentOffset.y >= e.contentSize.height - 400) loadMore(); }} refreshControl={<RefreshControl refreshing={programs.isRefetching && !programs.isFetchingNextPage} onRefresh={() => void programs.refetch()} />}>
      <View style={s.resultRow}><Text style={s.count}>전체 <Text style={s.countNumber}>{listLoading ? "확인 중" : `${total ?? "—"}건`}</Text></Text>
        {!campus && <Dropdown accessibilityLabel="정렬" options={sortOptions} value={sort} onSelect={setSort} style={s.sort}><Text style={s.sortText}>{sortOptions.find(o => o.value === sort)?.label}</Text><FigmaIcon name="arrow" size={12} color={fig.color.primary} rotate={90} /></Dropdown>}
      </View>
      {campus ? <>
        <Text style={s.sectionTitle}>우리 학교 혜택</Text>
        {items.filter(p => p.program_type === 'campus_benefit').map(p => <BenefitCard key={p.id} program={p} eligibility={statusOf(p.id)} />)}
        {!programs.isLoading && !programs.isError && !items.some(p => p.program_type === 'campus_benefit') && <Text style={[ui.muted, { marginBottom: 20 }]}>현재 조건으로 확인된 학교 전용 혜택은 아직 없어요.</Text>}
        {items.some(p => p.program_type === 'corporate_benefit') && <Text style={s.sectionTitle}>대학생 공통 혜택</Text>}
        {items.filter(p => p.program_type === 'corporate_benefit').map(p => <BenefitCard key={p.id} program={p} eligibility={statusOf(p.id)} />)}
      </> : items.map(p => <BenefitCard key={p.id} program={p} eligibility={statusOf(p.id)} />)}
      {!items.length && listLoading && <ProgramListSkeleton count={3} />}
      {!items.length && !listLoading && <EmptyState title={programs.isError ? "혜택을 불러오지 못했어요" : "조건에 맞는 혜택이 없어요"} description={programs.isError ? "연결을 확인한 뒤 다시 시도해 주세요." : "검색어나 필터를 바꿔서 다시 찾아보세요."} action={programs.isError ? "다시 시도" : "필터 초기화"} onAction={() => { if (programs.isError) { void programs.refetch(); return; } setSearch(""); changeCategory(""); setStatus(""); setIncomeBand(""); setLocalOnly(false); setSort("deadline"); }} />}
      {programs.isFetchingNextPage && <ActivityIndicator color={fig.color.primary} style={{ paddingVertical: 16 }} />}
      {programs.isFetchNextPageError && <Tappable accessibilityRole="button" onPress={loadMore} style={[ui.outlineButton, { marginVertical: 12 }]}><Text style={ui.outlineButtonText}>더 불러오지 못했어요. 다시 시도</Text></Tappable>}
    </ScrollView>
    </>
    {member.toastElement}
    {login.dialog}
  </View>;
}
const C = fig.color, K = fig.component, T = fig.text;
const s = StyleSheet.create({
  // 34065:3281 / 34065:3283
  filters: { backgroundColor: C.white92, borderBottomWidth: 1, borderBottomColor: C.filterLine, paddingVertical: 8 },
  filter: { flexDirection: "row", alignItems: "center", gap: 8, height: K.filterPill.height, paddingHorizontal: K.filterPill.paddingHorizontal, borderRadius: K.filterPill.radius, borderWidth: 1, borderColor: C.secondary8, backgroundColor: C.white },
  filterActive: { backgroundColor: C.secondary8, borderColor: C.secondary8 },
  filterLabel: { ...T.body12, color: C.deepGray }, filterValue: { ...T.semibold12, color: C.gray66 }, filterValueActive: { color: C.secondary },
  chips: { paddingHorizontal: 16, paddingVertical: 8, gap: 8 },
  campusName: { ...T.bold14, color: C.black },
  sectionTitle: { ...T.bold16, color: C.black, marginBottom: 12, marginTop: 8 },
  // 34065:2919
  resultRow: { flexDirection: "row", alignItems: "center", justifyContent: "space-between", paddingVertical: 8, minHeight: 35 },
  count: { ...T.bold12, color: C.black }, countNumber: { color: C.primary },
  sort: { flexDirection: "row", alignItems: "center", gap: 4, minHeight: 44, marginVertical: -12 }, sortText: { ...T.medium12, color: C.black },
});
