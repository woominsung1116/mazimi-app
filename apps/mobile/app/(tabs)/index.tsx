import React, { useEffect, useState } from "react";
import { ActivityIndicator, Image, RefreshControl, ScrollView, StyleSheet, useWindowDimensions, View } from "react-native";
import { useRouter } from "expo-router";
import { useQuery } from "@tanstack/react-query";
import Text from "@/components/Text";
import Tappable from "@/components/Tappable";
import LockedSection from "@/components/LockedSection";
import DeadlineBadge from "@/components/DeadlineBadge";
import { BenefitCard, CategoryTabs, EmptyState, figmaAssets, FigmaIcon, ScreenHeader, SectionTitle, ui } from "@/components/FigmaUI";
import { useLoginPrompt } from "@/components/LoginPromptDialog";
import { useMemberAction } from "@/components/MemberAction";
import { useSchoolSelection } from "@/lib/useSchoolSelection";
import { api, ApiRequestError, displayTitle, programTypeLabel, summaryLine } from "@/lib/api";
import { PROFILE_RETRY_TOAST, useBenefitProfile, useProfileStatus } from "@/lib/useBenefitProfile";
import { useAuthStore } from "@/store/auth";
import { useIsGuest } from "@/lib/guest";
import { SessionErrorBanner, useSessionProblem } from "@/components/SessionProblem";
import { IMMINENT_DAYS, isImminent } from "@/lib/deadline";
import { recommendationCardProgram } from "@/components/RecommendationCard";
import { loadEach } from "@/lib/programBatch";
import { cardStatus } from "@/lib/verifiedSections";
import { useCategorySupport, useRecommendations } from "@/lib/catalogue";
import { CATEGORY_STYLE, categoryStyleOf } from "@/constants/categoryStyle";
import { ALL_TAB, CAMPUS_TAB, fieldLabelOf, tabQuery, type CategoryTab } from "@/constants/fields";
import { fig } from "@/constants/theme";
import { useAgentEntryHidden } from "@/lib/useAgentEntry";
import { largestStatedAmount, withObjectParticle } from "@/lib/displayCopy";

/** Application states still in progress (홈 34065:6413 "신청 중인 혜택을 이어서 진행해 보세요"). */
const IN_PROGRESS = new Set(["planning", "applying", "submission_unconfirmed"]);

/** Home 마감 임박 request (contract §5); an older server ignores closing_within, so isImminent filters again. */
export const IMMINENT_QUERY = { status: "open", closing_within: IMMINENT_DAYS, sort: "deadline", per_page: 10 } as const;

export default function HomeScreen() {
  const router = useRouter();
  const [category, setCategory] = useState<CategoryTab>(ALL_TAB);
  const school = useSchoolSelection();
  const activeCategory = category === CAMPUS_TAB && !school.showCampus ? ALL_TAB : category;
  useEffect(() => { if (!school.showCampus && category === CAMPUS_TAB) setCategory(ALL_TAB); }, [school.showCampus, category]);
  const profile = useBenefitProfile();
  const profileStatus = useProfileStatus();
  const user = useAuthStore(s => s.user);
  const restoring = useAuthStore(s => s.isLoading);
  const isGuest = useIsGuest();
  const sessionProblem = useSessionProblem();
  const login = useLoginPrompt();
  const categories = useCategorySupport();
  const recommend = useRecommendations(profile);
  // 409: saved conditions are incomplete; treat as "no conditions yet".
  const needsConditions = !profile || (recommend.error instanceof ApiRequestError && recommend.error.status === 409);
  // 회원만 맞춤 추천을 본다 (F7.1). 조건이 없으면 조건 입력으로, 비회원은 가입 안내로 보낸다
  const goRecommendation = () => {
    // A failed read is "try again", never "you have no conditions" (no detour to onboarding).
    if (profileStatus.state === "error") { member.say(PROFILE_RETRY_TOAST); profileStatus.retry(); return; }
    router.push("/recommendations");
    if (needsConditions) router.push("/onboarding?mode=edit");
  };
  // A restoring member's tap runs once the session and the saved conditions are known; only a guest is asked to log in.
  const member = useMemberAction({ reco: { run: goRecommendation, ready: profileStatus.state !== "loading" && (!profile || !recommend.isLoading) } });
  const openRecommendation = () => member.run("reco", () => login.prompt("로그인하고 조건을 저장하면 내 조건에 맞는 혜택을 찾아드려요."));
  // 34065:6413: a member with an application in progress gets the "이어서 진행" title and a link to it.
  const applications = useQuery({ queryKey: ["applications", user?.id], queryFn: api.getMyApplications, enabled: !!user });
  const ongoing = user ? applications.data?.items.find(item => IN_PROGRESS.has(item.display_status)) : undefined;
  const canList = !restoring && (!user || !!profile);
  const region = profile?.region_code;
  const deadlines = useQuery({ queryKey: ["home-imminent", region], queryFn: () => api.getPrograms({ ...IMMINENT_QUERY, region }), enabled: !restoring });
  // The top 4 recommendations render from their items. An item without a summary
  // (today's server) gets its programme read, at most 4 calls, so the card keeps
  // the summary it showed before; a failed read leaves the item's own fields.
  const topItems = (recommend.data?.items ?? []).slice(0, 4);
  const summaryIds = topItems.filter(item => !item.summary_display?.trim() && !item.summary?.trim()).map(item => item.program_id);
  const topPrograms = useQuery({ queryKey: ["home-recommended", summaryIds], queryFn: () => loadEach(summaryIds, api.getProgram).catch(() => []), enabled: !!profile && !activeCategory && summaryIds.length > 0, staleTime: 5 * 60_000 });
  const programById = new Map((topPrograms.data ?? []).map(p => [p.id, p]));
  const recommendedPrograms = topItems.map(item => programById.get(item.program_id) ?? recommendationCardProgram(item));
  const campus = useQuery({ queryKey: ["campus", school], queryFn: () => api.getCampusPrograms(school.school, school.enrollmentStatus), enabled: school.showCampus });
  // Members only: a guest draft left by an older build never turns the list into recommendations (F7.1).
  const showRecommended = !!user && !!profile && !activeCategory && !recommend.isError;
  const browse = useQuery({ queryKey: ["home-browse", activeCategory, region, categories], queryFn: () => api.getPrograms({ status: "open", region, ...tabQuery(activeCategory, categories !== false), per_page: 4 }), enabled: canList && activeCategory !== CAMPUS_TAB && categories !== undefined && !showRecommended });
  const imminent = (deadlines.data?.items ?? []).filter(p => isImminent(p)).slice(0, 10);
  const cards = (activeCategory === CAMPUS_TAB ? campus.data?.items ?? [] : showRecommended ? recommendedPrograms : canList ? browse.data?.items ?? [] : []).slice(0, 4);
  const recommendationById = new Map(recommend.data?.items.map(p => [p.program_id, p]));
  const sectionEnabled = recommend.data?.verified_section_enabled === true;
  // The query the card list shows; a failed one offers a retry, never "아직 없어요".
  const cardsQuery = activeCategory === CAMPUS_TAB ? campus : showRecommended ? recommend : browse;
  const cardsFailed = cardsQuery.isError;
  const candidateCount = recommend.isError ? undefined : recommend.data?.total_available;
  // 34065:6176 "지원금 최대": only from an amount a 조건 확인됨 candidate states; otherwise the hero keeps the count.
  const largest = recommend.isError ? null : largestStatedAmount(recommend.data);
  const refresh = () => { void deadlines.refetch(); if (user && profile) void recommend.refetch(); void cardsQuery.refetch(); };
  const listTitle = activeCategory ? "분야별 혜택" : showRecommended ? "추천 혜택" : "혜택 둘러보기";
  const { width } = useWindowDimensions();
  const slideWidth = Math.min(width, 520) - 32;
  const [activeSlide, setSlide] = useState(0);
  const agentHidden = useAgentEntryHidden();
  // Banner slides (34065:6237), always the frame's four (dots 34065:6259), in priority order.
  // 질문으로 혜택 찾기 only fills a free slot; /assistant opens it from its own card either way.
  const slides = [
    { key: "vault", label: "서류를 미리 준비하고 혜택을 놓치지 마세요", badge: "신청 준비", title: "서류를 미리 준비하고\n혜택을 놓치지 마세요", caption: "내 서류 보관함 바로가기", image: figmaAssets.benefit, onPress: () => router.push("/vault") },
    // 저장한 혜택, 다음 할 일 is every member's, independent of the 비서 flag.
    ...(user ? [{ key: "briefing", label: "저장한 혜택, 다음 할 일", badge: "다음 할 일", title: "저장한 혜택\n다음 할 일", caption: undefined, image: figmaAssets.books, onPress: () => router.push("/briefing") }] : []),
    ...(agentHidden ? [] : [{ key: "agent", label: "마지미 비서", badge: "비서", title: "마지미 비서\n무엇을 맡길까요?", caption: "찾기·조건 확인·준비 정리는 바로 해요", image: CATEGORY_STYLE.agent.image, onPress: () => router.push("/agent") }]),
    { key: "assistant", label: "복학·독립·취업 준비, 지금 필요한 혜택부터", badge: undefined, title: "복학·독립·취업 준비\n지금 필요한 혜택부터", caption: "상황별 후보 찾기 · 조건과 공통 서류 비교", image: figmaAssets.scholarship, onPress: () => router.push("/assistant") },
    { key: "search", label: "질문으로 혜택 찾기", badge: undefined, title: "질문으로\n혜택 찾기", caption: "예: 부산 사는 휴학생이 받을 수 있는 장학금?", image: figmaAssets.searchEmpty, onPress: () => router.push("/assistant-search") },
  ].slice(0, 4);
  const browseAll = () => router.navigate({ pathname: "/explore", params: { category: activeCategory } });
  // Figma 홈 34065:6155, top to bottom: title and link, hero, 마감 임박, banners, 내 조건 맞춤 추천.
  return <View style={ui.screen}>
    <ScreenHeader />
    {sessionProblem && <SessionErrorBanner inline />}
    <ScrollView showsVerticalScrollIndicator={false} refreshControl={<RefreshControl refreshing={deadlines.isRefetching} onRefresh={refresh} />} contentContainerStyle={{ paddingTop: 16, paddingBottom: 16, gap: 8 }}>
      {ongoing ? <View style={s.intro}><Text style={s.title}>신청 중인 혜택을 이어서 진행해 보세요.</Text><Tappable accessibilityRole="button" onPress={() => router.push(`/apply-assistant?programId=${ongoing.program_id}`)} style={s.introLink}><Text style={[s.linkText, { flexShrink: 1 }]} numberOfLines={1}>{withObjectParticle(ongoing.program_title)} 이어 진행해 주세요</Text><View style={s.linkArrow}><FigmaIcon name="arrow" size={8} color={fig.color.secondary} /></View></Tappable></View>
        : <View style={s.intro}><Text style={s.title}>내 조건으로 찾은{"\n"}혜택을 확인해보세요</Text><Tappable accessibilityRole="button" onPress={openRecommendation} style={s.introLink}><Text style={s.linkText}>맞춤 혜택 후보 확인</Text><View style={s.linkArrow}><FigmaIcon name="arrow" size={8} color={fig.color.secondary} /></View></Tappable></View>}
      <View style={s.heroSection}>
        <LockedSection locked={isGuest} message="로그인하면 내 조건에 맞는 혜택 수를 알려드려요">
          <Tappable accessibilityRole="button" accessibilityLabel="맞춤 혜택 확인" onPress={openRecommendation} style={s.hero}>
            <View style={{ flex: 1, gap: 8 }}>
              {largest ? <View style={{ gap: 4 }}><View><Text style={s.heroLabel}>지원금 최대</Text><Text style={s.heroAmount}>{largest}</Text></View><Text style={s.heroBody}>놓치지 말고{"\n"}간편하게 신청해보세요 !</Text></View>
                : <View style={{ gap: 4 }}><View><Text style={s.heroLabel}>나에게 맞는 혜택 후보</Text>{recommend.isFetching && candidateCount == null ? <ActivityIndicator color={fig.color.navy} style={{ alignSelf: "flex-start", marginVertical: 8 }} /> : <Text style={s.heroAmount}>{candidateCount != null ? `${candidateCount.toLocaleString()}건` : "어떤 혜택이 있을까요?"}</Text>}</View><Text style={s.heroBody}>조건과 준비 서류를{"\n"}함께 확인해보세요!</Text></View>}
              {largest ? <View style={s.heroNoteRow}><FigmaIcon name="info" size={12} color={fig.color.secondary32} /><Text style={s.heroNote}>실제 수령 금액은 달라질 수 있어요</Text></View> : null}
            </View>
            <Image source={figmaAssets.gift} style={{ width: 124, height: 124 }} resizeMode="contain" />
          </Tappable>
        </LockedSection>
      </View>
      <View style={s.section}><SectionTitle title="마감 임박 혜택" subtitle={`${IMMINENT_DAYS}일 안에 마감하는 혜택이에요`} onPress={() => router.navigate({ pathname: "/explore", params: { status: "open", sort: "deadline", category: ALL_TAB } })} />
        {imminent.length ? <ScrollView horizontal showsHorizontalScrollIndicator={false} contentContainerStyle={{ paddingHorizontal: 16, gap: 16 }}>
          {imminent.map(p => { const art = categoryStyleOf(p); return <Tappable key={p.id} onPress={() => router.push(`/programs/${p.id}`)} accessibilityRole="button" accessibilityLabel={`${displayTitle(p)} 상세 보기`} style={s.deadlineCard}>
            <View style={[s.art, { backgroundColor: art.tile }]}><Image source={art.image} style={s.artImage} resizeMode="contain" accessibilityIgnoresInvertColors /><DeadlineBadge program={p} style={s.deadline} /></View>
            <View style={{ gap: 8 }}><Text style={s.category}>{fieldLabelOf(p, programTypeLabel(p.program_type))}</Text><View style={{ gap: 4 }}><Text numberOfLines={1} style={s.smallTitle}>{displayTitle(p)}</Text><Text numberOfLines={2} style={ui.muted}>{summaryLine(p) || "신청 조건을 확인해보세요"}</Text></View></View>
          </Tappable>; })}
        </ScrollView> : <EmptyState loading={deadlines.isLoading} title={deadlines.isError ? "혜택을 불러오지 못했어요" : `${IMMINENT_DAYS}일 안에 마감하는 혜택이 없어요`} action={deadlines.isError ? "다시 시도" : "마감순 전체 보기"} onAction={() => { if (deadlines.isError) void deadlines.refetch(); else router.navigate({ pathname: "/explore", params: { status: "open", sort: "deadline", category: ALL_TAB } }); }} />}
      </View>
      {/* 34065:6235: one banner carousel with its page dots 34065:6259 */}
      <View style={s.banners}>
        <ScrollView horizontal pagingEnabled={false} snapToInterval={slideWidth + 12} decelerationRate="fast" showsHorizontalScrollIndicator={false} contentContainerStyle={{ gap: 12 }} onScroll={e => setSlide(Math.round(e.nativeEvent.contentOffset.x / (slideWidth + 12)))} scrollEventThrottle={64}>
          {slides.map(slide => <Tappable key={slide.key} accessibilityRole="button" accessibilityLabel={slide.label} onPress={slide.onPress} style={[s.banner, { width: slideWidth }]}>
            <View style={s.bannerText}>{slide.badge ? <View style={s.bannerBadge}><Text style={s.badgeText}>{slide.badge}</Text></View> : null}<View style={{ gap: 4 }}><Text style={s.bannerTitle} numberOfLines={2}>{slide.title}</Text>{slide.caption ? <Text style={ui.muted} numberOfLines={1}>{slide.caption}</Text> : null}</View></View>
            <Image source={slide.image} style={{ width: 92, height: 92 }} resizeMode="contain" />
          </Tappable>)}
        </ScrollView>
        <View style={s.dots} accessibilityElementsHidden importantForAccessibility="no-hide-descendants">{slides.map((slide, index) => <View key={slide.key} style={[s.dot, index === Math.min(activeSlide, slides.length - 1) && s.dotActive]} />)}</View>
      </View>
      <View style={s.section}>
        {/* Shown to everyone so the layout never shifts when a restore ends; a guest's tap opens the login dialog (not a second lock) */}
        <Tappable accessibilityRole="button" accessibilityLabel="내 조건 맞춤 추천 보기" onPress={openRecommendation} style={s.recoEntry}>
          <View style={{ flex: 1, gap: 4 }}><Text style={ui.muted}>나에게 맞는 지원 정책</Text><Text accessibilityRole="header" style={ui.sectionTitle}>내 조건 맞춤 추천</Text></View>
          <View style={ui.sectionChevron}><FigmaIcon name="arrow" size={12} color={fig.color.primary} /></View>
        </Tappable>
        <CategoryTabs value={activeCategory} showCampus={school.showCampus} onChange={setCategory} line={fig.color.grayF1} />
        <View style={{ paddingHorizontal: 16, paddingTop: 12 }}>{cards.map(p => { const match = recommendationById.get(p.id); return <BenefitCard key={p.id} home program={p} eligibility={match ? cardStatus(match, sectionEnabled) : "unknown"} />; })}{!cards.length && <EmptyState loading={!cardsFailed && (cardsQuery.isLoading || categories === undefined || (showRecommended && (recommend.isLoading || topPrograms.isFetching)))} title={cardsFailed ? "혜택을 불러오지 못했어요" : "이 조건에 맞는 혜택이 아직 없어요"} description={cardsFailed ? "연결을 확인한 뒤 다시 시도해 주세요." : undefined} action={cardsFailed ? "다시 시도" : "전체 혜택 보기"} onAction={() => { if (cardsFailed) void cardsQuery.refetch(); else router.push("/explore"); }} />}</View>
        {/* Figma 34065:6354: the round chevron between two lines opens the full list */}
        <Tappable accessibilityRole="button" accessibilityLabel={`${listTitle} 전체 보기`} onPress={browseAll} style={s.more}><View style={s.moreLine} /><View style={s.moreButton}><FigmaIcon name="arrow" size={12} color={fig.color.primary} rotate={90} /></View><View style={s.moreLine} /></Tappable>
      </View>
    </ScrollView>
    {member.toastElement}
    {login.dialog}
  </View>;
}

const C = fig.color, K = fig.component, T = fig.text;
const s = StyleSheet.create({
  // 34065:6157
  intro: { paddingHorizontal: 16, gap: 8, marginBottom: 8 }, title: { ...T.bold20, color: C.heading }, introLink: { flexDirection: "row", alignItems: "center", gap: 8, minHeight: 44, marginVertical: -12, alignSelf: "flex-start", borderRadius: 8 }, linkText: { ...T.body13, color: C.secondary },
  linkArrow: { width: 12, height: 12, borderRadius: 10, padding: 2, backgroundColor: C.secondary4, borderWidth: 0.5, borderColor: C.secondary4, alignItems: "center", justifyContent: "center" }, // 34065:6166
  // 34065:6168 / 34065:6169
  heroSection: { paddingHorizontal: 16, paddingVertical: 12, gap: 8 },
  hero: { backgroundColor: C.input8, borderRadius: K.hero.radius, paddingTop: 16, paddingHorizontal: 16, paddingBottom: 12, minHeight: 156, flexDirection: "row", alignItems: "flex-end", gap: 4, borderWidth: 1, borderColor: C.secondary4 }, heroLabel: { ...T.bold14, color: C.navy }, heroAmount: { ...T.bold24, color: C.navy }, heroBody: { ...T.body13, color: C.gray66 },
  heroNoteRow: { flexDirection: "row", alignItems: "center", gap: 4 }, heroNote: { ...T.caption10, color: C.gray99 },
  // 34065:6188
  section: { paddingVertical: 16 },
  deadlineCard: { width: K.tile.width, gap: 8, borderRadius: K.tile.radius },
  art: { height: K.tile.height, borderRadius: K.tile.radius, alignItems: "center", justifyContent: "center", paddingVertical: 12, paddingHorizontal: 28 }, artImage: { width: K.tile.image, height: K.tile.image }, deadline: { position: "absolute", top: 8, left: 8 }, // 34065:6201
  category: { ...T.badge11, color: C.secondary }, smallTitle: { ...T.bold14, color: C.black },
  // 34065:6235 / 34065:6237
  banners: { paddingHorizontal: 16, paddingVertical: 16, gap: 8 },
  dots: { flexDirection: "row", justifyContent: "center", gap: 4 }, dot: { width: 4, height: 4, borderRadius: 2, backgroundColor: C.deepGray }, dotActive: { width: 12, backgroundColor: C.primary }, // 34065:6259
  banner: { backgroundColor: C.secondary4, borderWidth: 1, borderColor: C.secondary4, borderRadius: K.banner.radius, padding: K.banner.padding, flexDirection: "row", alignItems: "center", gap: K.banner.gap, minHeight: 130 },
  bannerText: { flex: 1, gap: 4 }, bannerBadge: { alignSelf: "flex-start", borderRadius: K.badge.radius, backgroundColor: C.primary, paddingHorizontal: K.badge.paddingHorizontal, paddingVertical: K.badge.paddingVertical, boxShadow: `${fig.shadow.badge}, ${fig.shadow.badgeInner}` }, badgeText: { ...T.badge11, color: C.white }, bannerTitle: { ...T.bold16, color: C.black },
  // 34065:6266
  recoEntry: { flexDirection: "row", alignItems: "flex-end", gap: 12, paddingHorizontal: 16, marginBottom: 4, minHeight: 50 },
  more: { flexDirection: "row", alignItems: "center", gap: 36, paddingHorizontal: 28, minHeight: 44 }, moreLine: { flex: 1, height: 1, backgroundColor: C.secondary8 },
  moreButton: { width: 28, height: 28, borderRadius: 14, backgroundColor: C.secondary4, borderWidth: 1, borderColor: C.secondary8, alignItems: "center", justifyContent: "center" }, // 34065:6357
});
