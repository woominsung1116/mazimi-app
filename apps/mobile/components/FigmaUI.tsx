import React, { useRef, useState } from "react";
import { ActivityIndicator, Image, Keyboard, Modal, Platform, Pressable, ScrollView, StyleSheet, TextInput, useWindowDimensions, View, type ImageSourcePropType, type StyleProp, type TextStyle, type ViewStyle } from "react-native";
import { useRouter } from "expo-router";
import { useSafeAreaInsets } from "react-native-safe-area-context";
import Text from "./Text";
import Tappable from "./Tappable";
import { LinearGradient } from "expo-linear-gradient";
import { colors, fig } from "@/constants/theme";
import { useQuery } from "@tanstack/react-query";
import { api, displayTitle, programTypeLabel, summaryLine, type ApiProgram } from "@/lib/api";
import { useAuthStore } from "@/store/auth";
import { DEADLINE_TONE_COLORS, deadlineLabel, deadlineTone } from "@/lib/deadline";
import type { CardStatus } from "@/lib/verifiedSections";
import { categoryStyleOf, type CategoryStyle } from "@/constants/categoryStyle";
import { ALL_TAB, CAMPUS_TAB, FIELD_TABS, PROVIDER_TABS, fieldLabelOf, fieldTab, providerTab, type CategoryTab } from "@/constants/fields";

export { deadlineLabel };

export const figmaAssets = {
  logo: require("../assets/figma/logo.png"),
  gift: require("../assets/figma/gift.png"),
  scholarship: require("../assets/figma/scholarship.png"),
  benefit: require("../assets/figma/benefit.png"),
  books: require("../assets/figma/student-enrolled.png"),
  searchEmpty: require("../assets/figma/search-empty.png"),
  home: require("../assets/figma/icon-home.png"),
  search: require("../assets/figma/icon-search.png"),
  bookmark: require("../assets/figma/icon-bookmark.png"),
  archive: require("../assets/figma/icon-archive.png"),
  user: require("../assets/figma/icon-user.png"),
  bell: require("../assets/figma/icon-bell.png"),
  arrow: require("../assets/figma/icon-arrow.png"),
  back: require("../assets/figma/icon-back.png"),
  checkCircle: require("../assets/figma/icon-check-circle.png"),
  minusCircle: require("../assets/figma/icon-minus-circle.png"),
  info: require("../assets/figma/icon-info.png"),
  close: require("../assets/figma/icon-close.png"),
  kakao: require("../assets/figma/icon-kakao.png"),
  searchMono: require("../assets/figma/icon-search-mono.png"),
  xCircle: require("../assets/figma/icon-x-circle.png"),
  check: require("../assets/figma/icon-check.png"),
  link: require("../assets/figma/icon-link.png"),
  minus: require("../assets/figma/icon-minus.png"),
  pin: require("../assets/figma/icon-pin.png"),
  repeat: require("../assets/figma/icon-repeat.png"),
  document: require("../assets/figma/icon-document.png"),
  eyeOff: require("../assets/figma/icon-eye-off.png"),
  bookmarkMono: require("../assets/figma/icon-bookmark-mono.png"),
  setting: require("../assets/figma/icon-setting.png"),
  welcome: require("../assets/figma/welcome.png"),
} as const;

const C = fig.color;

/** Card icon box 34065:2933: the category glyph tinted with its accent on its tile (user decision 2026-10-31). */
export function CategoryIcon({ style }: { style: CategoryStyle }) {
  return <View style={[ui.providerIcon, { backgroundColor: style.tile, borderColor: style.tile }]}><Image source={style.glyph} style={{ width: K.cardIcon.icon, height: K.cardIcon.icon, tintColor: style.accent }} resizeMode="contain" /></View>;
}

/** `rotate`: degrees, as the Figma instance is turned (the down chevron is the right one at 90). */
export function FigmaIcon({ name, size = 16, color = colors.textSecondary, rotate }: { name: keyof typeof figmaAssets; size?: number; color?: string; rotate?: number }) {
  return <Image source={figmaAssets[name]} style={[{ width: size, height: size, tintColor: color }, rotate ? { transform: [{ rotate: `${rotate}deg` }] } : null]} resizeMode="contain" />;
}

/**
 * Figma headers: logo 34065:6393 (no title), title 34065:3106 (a tab screen's name) and
 * back 34065:14632 (back arrow, centred title). `tabs`: CategoryTabs follow and draw the line.
 */
export function ScreenHeader({ title, back = false, action, tabs = false }: { title?: string; back?: boolean; action?: React.ReactNode; tabs?: boolean }) {
  const router = useRouter();
  const insets = useSafeAreaInsets();
  const line = tabs ? null : back ? fig.component.headerLine.back : fig.component.headerLine.logo;
  return <View style={{ backgroundColor: C.white, paddingTop: insets.top }}>
    <View style={[ui.header, line ? { borderBottomWidth: 1, borderBottomColor: line } : null]}>
      {back && <Tappable accessibilityRole="button" accessibilityLabel="뒤로 가기" onPress={() => router.canGoBack() ? router.back() : router.replace("/(tabs)")} style={[ui.iconButton, { marginLeft: -12, marginRight: 4 }]}><FigmaIcon name="back" size={20} color={C.primaryBright} /></Tappable>}
      {title ? <Text style={[ui.headerTitle, back && { position: "absolute", left: 60, right: 60, textAlign: "center" }]}>{title}</Text> : <Image source={figmaAssets.logo} style={{ width: 68, height: 20 }} accessibilityLabel="마지미" />}
      <View style={{ marginLeft: "auto", marginRight: -12 }}>{action ?? (!back && <NotificationBell />)}</View>
    </View>
  </View>;
}

// 읽지 않은 알림이 있으면 종에 점을 표시한다
export function NotificationBell() {
  const router = useRouter();
  const user = useAuthStore(s => s.user);
  const unread = useQuery({ queryKey: ["alerts-unread", user?.id], queryFn: async () => ({ unread: (await api.getAlerts({ limit: 1 })).unread ?? 0 }), enabled: !!user });
  const count = user ? unread.data?.unread ?? 0 : 0;
  return <Tappable onPress={() => router.push("/notifications")} accessibilityRole="button" accessibilityLabel={count ? `알림, 새 알림 ${count}개` : "알림"} style={ui.iconButton}>
    <View style={{ width: 20, height: 20 }}>
      <FigmaIcon name="bell" size={20} color="#C9CCCECC" />
      {count > 0 && <View style={ui.bellDot} />}
    </View>
  </Tappable>;
}

// Subset of mazimi_core::PROGRAM_TYPES with the same labels; a core test
// asserts every entry is a canonical type. The tabs no longer filter by
// program_type (see CategoryTabs); deep links that still carry one of these
// values are mapped by constants/fields.tabFromLegacy.
export const benefitCategories = [
  { label: "전체", value: "" }, { label: "정책", value: "policy" },
  { label: "장학금", value: "scholarship" }, { label: "주거", value: "housing" },
  { label: "생활", value: "benefit" }, { label: "기업혜택", value: "corporate_benefit" },
  { label: "복지", value: "welfare" },
  { label: "금융", value: "financial_product" }, { label: "취업", value: "employment" },
];

/**
 * F5.1 order: 전체 · 정책 장학금 기업혜택 (provider) | 주거 생활 교육 복지 금융 취업 (field).
 * Every tab is the Figma tab 34065:3116; the two axes are split by the Figma row divider 34065:4533.
 */
export function CategoryTabs({ value, onChange, showCampus = false, line = fig.component.tabs.lineColor }: { value: CategoryTab; onChange: (value: CategoryTab) => void; showCampus?: boolean; line?: string }) {
  const providers = [...PROVIDER_TABS.map(tab => ({ label: tab.label, value: providerTab(tab.value) })), ...(showCampus ? [{ label: "대학", value: CAMPUS_TAB }] : [])];
  const fields = FIELD_TABS.map(tab => ({ label: tab.label, value: fieldTab(tab.value) }));
  const tab = (item: { label: string; value: string }, label: string) => {
    const selected = value === item.value;
    return <Tappable key={item.value || "all"} onPress={() => onChange(item.value)} accessibilityRole="tab" accessibilityLabel={label} accessibilityState={{ selected }} style={[ui.tab, selected && ui.tabSelected]}>
      <Text style={[ui.tabText, selected && ui.tabTextSelected]}>{item.label}</Text>
    </Tappable>;
  };
  return <View style={[ui.tabs, { borderBottomColor: line }]}><ScrollView horizontal showsHorizontalScrollIndicator={false} contentContainerStyle={{ paddingHorizontal: 16, alignItems: "center" }}>
    {tab({ label: "전체", value: ALL_TAB }, "전체")}
    {providers.map(item => tab(item, `${item.label}, 제공 주체`))}
    <View style={ui.tabDivider} accessibilityElementsHidden importantForAccessibility="no" />
    {fields.map(item => tab(item, `${item.label}, 분야`))}
  </ScrollView></View>;
}

export type BenefitCardProgram = Pick<ApiProgram, "id" | "title" | "program_type"> & Partial<Pick<ApiProgram, "title_display" | "summary" | "provider_name" | "company_name" | "deadline_at" | "application_end_at" | "benefit_valid_until" | "effective_deadline" | "deadline_date" | "program_status" | "summary_display" | "provider_category" | "provider_categories" | "field_category" | "field_label" | "corporate_category">> & {
  /** Saved list only: false once the programme left the catalogue (its detail page is gone). */
  available?: boolean;
};

const statusText: Record<CardStatus, string> = { verified: "조건 확인됨", pass: "일부 조건 맞음", unknown: "공고 확인 필요" };

/**
 * `eligibility` "verified" only while the 조건 확인됨 section is on.
 * The body is the programme's own summary (34065:2930); without one the card is title and meta only.
 */
export function BenefitCard({ program, saved, onSave, eligibility, home = false }: { program: BenefitCardProgram; saved?: boolean; onSave?: () => void; eligibility?: CardStatus; home?: boolean }) {
  const router = useRouter();
  const gone = program.available === false;
  const status = eligibility ?? "unknown";
  const urgent = deadlineTone(program) === "urgent";
  const category = categoryStyleOf(program);
  const summary = gone ? "마감되었거나 종료된 혜택이에요." : summaryLine(program);
  const provider = program.provider_name ?? program.company_name;
  // 34059:2829: only 조건 확인됨 gets the highlighted card.
  return <Tappable onPress={() => { if (!gone) router.push(`/programs/${program.id}`); }} accessibilityRole="button" accessibilityLabel={gone ? `${displayTitle(program)}, 마감·종료된 혜택` : `${displayTitle(program)} 상세 보기`} accessibilityState={{ disabled: gone }} style={[ui.benefitCard, home && { borderRadius: fig.component.card.radiusHome }, status === "verified" && !gone && ui.benefitCardVerified, gone && { opacity: 0.72 }]}>
    <View style={ui.cardTop}>
      <View style={ui.row}>
        <CategoryIcon style={category} />
        <View style={{ flex: 1, gap: 2 }}>
          <View style={ui.cardTitleRow}><Text style={[ui.cardTitle, { flex: 1 }]} numberOfLines={2}>{displayTitle(program)}</Text><View style={[ui.typeBadge, { backgroundColor: category.tile }]}><Text style={[ui.typeBadgeText, { color: category.accent }]}>{fieldLabelOf(program, programTypeLabel(program.program_type))}</Text></View></View>
          <Text style={ui.muted} numberOfLines={1}>{provider ? <>{provider}<Text style={ui.metaDot}>・</Text></> : null}<Text style={urgent ? { color: DEADLINE_TONE_COLORS.urgent.text } : undefined}>{deadlineLabel(program)}</Text></Text>
        </View>
        {onSave && <Tappable hitSlop={12} accessibilityRole="button" accessibilityLabel={saved ? "관심 혜택 해제" : "관심 혜택 저장"} style={ui.cardSave} onPress={e => { e.stopPropagation(); onSave(); }}><FigmaIcon name={saved ? "close" : "bookmarkMono"} size={16} color={C.grayC8} /></Tappable>}
      </View>
      {summary ? <Text style={ui.cardSummary} numberOfLines={2}>{summary}</Text> : null}
    </View>
    {!gone && <View style={ui.cardFooter}>
      {/* "pass" is not verified eligibility: neutral colours, never a success state. Only "verified" (조건 확인됨) is. */}
      <View style={[ui.cardFooterCell, { paddingLeft: 0 }]}><View style={ui.statusGroup}><View style={ui.statusCaption}><FigmaIcon name={status === "unknown" ? "minusCircle" : "checkCircle"} size={16} color={status === "verified" ? C.primary : status === "pass" ? C.grayE8 : C.grayE1} /><Text style={ui.statusLabel}>자격 상태</Text></View><View style={[ui.statusBadge, status === "verified" && ui.statusPass]}><Text style={[ui.statusText, status === "pass" && { color: C.gray66 }, status === "verified" && { color: C.primary }]}>{statusText[status]}</Text></View></View></View>
      {/* 서류 상태 34065:6314: no document status reaches the card, so it says where to check, never 구비/미구비 */}
      <FadeDivider /><View style={ui.cardFooterCell}><View style={ui.statusGroup}><View style={ui.statusCaption}><FigmaIcon name="minusCircle" size={16} color={C.grayE1} /><Text style={ui.statusLabel}>서류 상태</Text></View><View style={ui.statusBadge}><Text style={ui.statusText}>공고 확인</Text></View></View></View>
    </View>}
  </Tappable>;
}

/** The .fig search bar (34061:3131): rounded field, clear button, Primary search button. Used by 탐색 and 서류 보관함. */
export function SearchBar({ value, onChange, onSubmit, placeholder, label }: { value: string; onChange: (text: string) => void; onSubmit: () => void; placeholder: string; label: string }) {
  return <View style={searchStyles.bar}>
    <TextInput autoFocus style={searchStyles.input} value={value} onChangeText={onChange} placeholder={placeholder} placeholderTextColor={C.deepGray} accessibilityLabel={label} returnKeyType="search" onSubmitEditing={onSubmit} />
    {value !== "" && <Tappable accessibilityLabel="검색어 지우기" accessibilityRole="button" onPress={() => onChange("")} hitSlop={6} style={searchStyles.clear}><FigmaIcon name="xCircle" size={16} color={C.grayC8} /></Tappable>}
    <Tappable accessibilityRole="button" accessibilityLabel="검색" hitSlop={6} onPress={onSubmit} style={searchStyles.button}><FigmaIcon name="searchMono" size={16} color={C.white} /></Tappable>
  </View>;
}
const searchStyles = StyleSheet.create({
  bar: { marginHorizontal: 16, marginTop: 12, paddingVertical: 8, paddingLeft: 20, paddingRight: 8, borderRadius: 30, borderWidth: 1, borderColor: C.secondary4, backgroundColor: C.white, minHeight: 48, flexDirection: "row", alignItems: "center", gap: 8 },
  input: { flex: 1, fontFamily: "Pretendard", ...fig.text.body13, color: C.black, height: 32 },
  clear: { width: 32, height: 32, alignItems: "center", justifyContent: "center" },
  button: { width: 32, height: 32, borderRadius: 16, backgroundColor: C.primary, alignItems: "center", justifyContent: "center", boxShadow: "0px 12px 12px 0px #34BB841F" },
});

export function EmptyState({ title, description, action, onAction, loading = false }: { title: string; description?: string; action?: string; onAction?: () => void; loading?: boolean }) {
  return <View style={ui.empty}>{loading ? <ActivityIndicator color={colors.primary} /> : <FigmaIcon name="searchMono" size={28} color={C.secondary32} />}<Text style={ui.emptyTitle}>{title}</Text>{description && <Text style={ui.emptyDescription}>{description}</Text>}{action && onAction && <Tappable accessibilityRole="button" onPress={onAction} style={ui.outlineButton}><Text style={ui.outlineButtonText}>{action}</Text></Tappable>}</View>;
}

export type MenuOption = { label: string; value: string; description?: string };

export function Dropdown({ options, value, onSelect, children, accessibilityLabel, style }: { options: MenuOption[]; value: string; onSelect: (value: string) => void; children: React.ReactNode; accessibilityLabel: string; style?: StyleProp<ViewStyle> }) {
  const anchor = useRef<View>(null);
  const picked = useRef<string | null>(null);
  const [menu, setMenu] = useState<{ x: number; top: number; bottom: number; width: number } | null>(null);
  const screen = useWindowDimensions();
  // iOS는 모달이 닫히는 중에 띄운 Alert를 버리므로 닫힌 뒤에 선택을 반영한다
  const flush = () => { const value = picked.current; picked.current = null; if (value !== null) onSelect(value); };
  const open = () => { picked.current = null; Keyboard.dismiss(); anchor.current?.measureInWindow((x, y, width, height) => setMenu({ x, top: y, bottom: y + height, width })); };
  const width = menu ? Math.min(Math.max(menu.width, 180), screen.width - 32) : 0;
  // 아래 공간이 모자라면 버튼 위로 펼친다
  const below = menu ? screen.height - menu.bottom - 24 : 0;
  const above = menu ? menu.top - 48 : 0;
  const up = below < 240 && above > below;
  const place = menu && (up ? { bottom: screen.height - menu.top + 4, maxHeight: above } : { top: menu.bottom + 4, maxHeight: below });
  return <>
    <Tappable ref={anchor} accessibilityRole="button" accessibilityLabel={accessibilityLabel} accessibilityState={{ expanded: !!menu }} onPress={open} style={style}>{children}</Tappable>
    <Modal visible={!!menu} transparent animationType="none" onRequestClose={() => setMenu(null)} onDismiss={flush} statusBarTranslucent>
      <Pressable style={StyleSheet.absoluteFill} onPress={() => setMenu(null)} accessibilityLabel="닫기" />
      {menu && <View style={[ui.menu, place, { left: Math.min(menu.x, screen.width - width - 16), width }]}>
        <ScrollView bounces={false} keyboardShouldPersistTaps="handled">{options.map(option => { const selected = option.value === value; return <Tappable key={option.value} accessibilityRole="radio" accessibilityState={{ checked: selected }} onPress={() => { picked.current = option.value; setMenu(null); if (Platform.OS !== "ios") flush(); }} style={[ui.menuOption, selected && { backgroundColor: C.secondary4 }]}>
          <View style={{ flex: 1 }}><Text style={[ui.menuOptionText, selected && { color: C.secondary, fontWeight: "700" }]}>{option.label}</Text>{option.description && <Text style={ui.muted}>{option.description}</Text>}</View>
          {selected && <FigmaIcon name="checkCircle" size={16} color={colors.primary} />}
        </Tappable>; })}</ScrollView>
      </View>}
    </Modal>
  </>;
}

/** Section header 34065:6189: kicker, title and the round chevron 34065:6196. */
export function SectionTitle({ title, subtitle, onPress, accessibilityLabel }: { title: string; subtitle?: string; onPress?: () => void; accessibilityLabel?: string }) {
  return <View style={ui.sectionHeader}>
    <View style={{ flex: 1, gap: fig.component.section.kickerGap }}>{subtitle && <Text style={ui.muted}>{subtitle}</Text>}<Text style={ui.sectionTitle}>{title}</Text></View>
    {onPress && <Tappable accessibilityRole="button" accessibilityLabel={accessibilityLabel ?? `${title} 전체 보기`} onPress={onPress} style={ui.sectionChevronTarget}><View style={ui.sectionChevron}><FigmaIcon name="arrow" size={12} color={C.primary} /></View></Tappable>}
  </View>;
}

/** Figma toggle 34065:4967: 36x20, radius 10, 2px inset, Blue on / GrayE1 off, 16px white knob. Replaces RN Switch. */
export function FigmaToggle({ value, onValueChange, disabled = false, accessibilityLabel }: { value: boolean; onValueChange: (value: boolean) => void; disabled?: boolean; accessibilityLabel: string }) {
  return <Tappable feedback="opacity" accessibilityRole="switch" accessibilityLabel={accessibilityLabel} accessibilityState={{ checked: value, disabled }} aria-checked={value} disabled={disabled} hitSlop={12} onPress={() => onValueChange(!value)} style={[ui.toggle, { backgroundColor: value ? C.blue : C.grayE1, alignItems: value ? "flex-end" : "flex-start" }, disabled && { opacity: 0.6 }]}>
    <View style={ui.toggleKnob} />
  </Tappable>;
}

/** Figma input 34005:2937 as a picker: 9/12 label over the 12/18 value, Secondary 32% down chevron. */
export function FigmaSelect({ label, options, value, onSelect, accessibilityLabel, placeholder = "선택 안 함" }: { label: string; options: MenuOption[]; value: string; onSelect: (value: string) => void; accessibilityLabel: string; placeholder?: string }) {
  const chosen = options.find(option => option.value === value && option.value !== "");
  // Screen readers hear the label and the current choice, as the visible field shows them.
  return <Dropdown accessibilityLabel={`${accessibilityLabel}, ${chosen?.label ?? placeholder}`} options={options} value={value} onSelect={onSelect} style={ui.select}>
    <View style={{ flex: 1, gap: 4 }}><Text style={ui.selectLabel}>{label}</Text><Text style={[ui.selectValue, !chosen && { color: C.deepGray }]}>{chosen?.label ?? placeholder}</Text></View>
    <FigmaIcon name="arrow" size={16} color={C.secondary32} rotate={90} />
  </Dropdown>;
}

/** A text link with the Figma arrow (34065:6163 / 34065:2926), in place of a "›" glyph. */
export function LinkLabel({ children, style, color = C.secondary }: { children: React.ReactNode; style?: StyleProp<TextStyle>; color?: string }) {
  return <View style={ui.linkLabel}><Text style={[ui.linkLabelText, { color }, style]}>{children}</Text><FigmaIcon name="arrow" size={12} color={color} /></View>;
}

/** A list line with a drawn 4px dot, centred on the text's first line, in place of a "•" glyph. */
export function BulletRow({ children, style, color = C.primary }: { children: React.ReactNode; style?: StyleProp<TextStyle>; color?: string }) {
  const lineHeight = StyleSheet.flatten(style)?.lineHeight ?? fig.text.body13.lineHeight ?? 20;
  return <View style={ui.bulletRow}><View style={[ui.bulletDot, { backgroundColor: color, marginTop: (lineHeight - 4) / 2 }]} /><Text style={[{ flex: 1 }, style]}>{children}</Text></View>;
}

/** A selected option's Figma check (icon-check-mono 34060:4233), in place of a "✓" glyph. */
export function CheckMark({ color = C.primary }: { color?: string }) {
  return <FigmaIcon name="check" size={12} color={color} />;
}

/** Card footer divider 34065:6307: a 1px Secondary color line fading out at both ends. */
export function FadeDivider() {
  return <LinearGradient colors={["#23996900", "#23996952", "#23996900"]} style={{ width: 1, alignSelf: "stretch" }} accessibilityElementsHidden importantForAccessibility="no" />;
}

const K = fig.component;
const T = fig.text;
export const ui = StyleSheet.create({
  screen: { flex: 1, backgroundColor: C.white },
  content: { paddingHorizontal: 16, paddingBottom: 32 },
  // Headers 34065:6393 / 34065:3106 / 34065:14632
  header: { height: K.header.height, paddingHorizontal: K.header.paddingHorizontal, flexDirection: "row", alignItems: "center", backgroundColor: K.header.backgroundColor },
  headerTitle: { ...T.bold16, color: C.black },
  iconButton: { minWidth: 44, minHeight: 44, alignItems: "center", justifyContent: "center" },
  bellDot: { position: "absolute", top: 0, right: 0, width: 4, height: 4, borderRadius: 2, backgroundColor: C.red }, // 34065:6407
  row: { flexDirection: "row", alignItems: "center", gap: 8 },
  muted: { ...T.body12, color: C.deepGray },
  bulletRow: { flexDirection: "row", gap: 8 }, bulletDot: { width: 4, height: 4, borderRadius: 2, flexShrink: 0 },
  linkLabel: { flexDirection: "row", alignItems: "center", gap: 4 },
  select: { minHeight: K.input.height, borderRadius: K.input.radius, backgroundColor: C.secondary4, paddingHorizontal: K.input.paddingHorizontal, paddingVertical: K.input.paddingVertical, flexDirection: "row", alignItems: "center", gap: 8 }, // 34005:2937
  selectLabel: { ...T.label9, color: C.deepGray }, selectValue: { ...T.body12, color: C.black },
  toggle: { width: 36, height: 20, borderRadius: 10, padding: 2, justifyContent: "center" }, toggleKnob: { width: 16, height: 16, borderRadius: 8, backgroundColor: C.white }, // 34065:4967
  linkLabelText: { ...T.semibold12 },
  // Section header 34065:6189
  sectionHeader: { flexDirection: "row", alignItems: "flex-end", gap: 12, paddingHorizontal: 16, marginBottom: 12 },
  sectionTitle: { ...T.bold18, color: C.black },
  sectionChevronTarget: { width: 44, height: 44, marginRight: -12, marginBottom: -8, alignItems: "center", justifyContent: "center" },
  sectionChevron: { width: K.section.chevron, height: K.section.chevron, borderRadius: 10, padding: 4, backgroundColor: C.input2, borderWidth: 1, borderColor: C.secondary4, alignItems: "center", justifyContent: "center" }, // 34065:6196
  // Category tabs 34065:3115
  tabs: { height: K.tabs.height, borderBottomWidth: 1, borderBottomColor: K.tabs.lineColor, backgroundColor: C.white92 },
  tab: { minWidth: 57, height: K.tabs.height, paddingHorizontal: K.tabs.paddingHorizontal, alignItems: "center", justifyContent: "center", borderBottomWidth: K.tabs.underline, borderBottomColor: "transparent" },
  tabSelected: { borderBottomColor: C.navy },
  tabText: { ...T.body13, color: C.deepGray },
  tabTextSelected: { ...T.semibold13, color: C.navy },
  tabDivider: { width: 1, height: 12, backgroundColor: C.secondary8, marginHorizontal: 8 }, // 34065:4533
  // Benefit card 34065:2930 (home 34065:6288 has radius 20)
  benefitCard: { borderWidth: 1, borderColor: K.card.borderColor, borderRadius: K.card.radius, paddingTop: K.card.padding, paddingHorizontal: K.card.padding, paddingBottom: K.card.paddingBottom, gap: K.card.gap, backgroundColor: C.white, boxShadow: fig.shadow.card, marginBottom: 16 },
  benefitCardVerified: { backgroundColor: C.secondary4, borderColor: C.secondary32 }, // 34059:2829
  cardTop: { gap: 12, paddingBottom: 12, borderBottomWidth: 1, borderBottomColor: C.grayF7 }, // 34065:2931
  providerIcon: { width: K.cardIcon.size, height: K.cardIcon.size, borderRadius: K.cardIcon.radius, backgroundColor: C.secondary4, borderWidth: 1, borderColor: C.secondary4, alignItems: "center", justifyContent: "center" }, // 34065:2933
  cardTitleRow: { flexDirection: "row", alignItems: "flex-start", gap: 8 },
  cardTitle: { ...T.bold16, color: C.title },
  metaDot: { color: C.grayE1 },
  typeBadge: { backgroundColor: C.navy, borderRadius: K.badge.radius, paddingHorizontal: K.badge.paddingHorizontal, paddingVertical: K.badge.paddingVertical, alignSelf: "flex-start", boxShadow: fig.shadow.badgeInner }, // 34065:2938
  typeBadgeText: { ...T.badge11, color: C.white },
  cardSummary: { ...T.body13, color: C.gray66 },
  cardFooter: { flexDirection: "row", alignItems: "center", minHeight: 44 }, // 34065:2951
  cardFooterCell: { flex: 1, paddingVertical: 4, paddingHorizontal: 12, justifyContent: "center" }, // 34065:2952
  statusGroup: { flexDirection: "row", alignItems: "center", justifyContent: "space-between", gap: 4 },
  statusCaption: { flexDirection: "row", alignItems: "center", gap: 4 },
  cardSave: { width: 20, height: 20, padding: 2, alignItems: "center", justifyContent: "center", alignSelf: "flex-start" }, // 34059:3452
  statusLabel: { ...T.semibold12, color: C.black },
  statusBadge: { backgroundColor: C.grayF7, borderRadius: K.badge.radius, paddingHorizontal: K.badge.paddingHorizontal, paddingVertical: K.badge.paddingVertical, boxShadow: fig.shadow.badgeInner }, // 34065:6345
  statusPass: { backgroundColor: C.primary2 }, // 34065:6312
  countBadge: { backgroundColor: C.grayF7, borderRadius: K.badge.radius, paddingHorizontal: K.badge.paddingHorizontal, paddingVertical: K.badge.paddingVertical },
  countText: { ...T.badge11, color: C.deepGray },
  statusText: { ...T.badge11, color: C.deepGray },
  // Empty and loading states (no frame): 서류 검색 34061:3117 text styles
  empty: { paddingVertical: 36, paddingHorizontal: 20, alignItems: "center", gap: 8 },
  emptyTitle: { ...T.bold16, color: C.black, textAlign: "center" },
  emptyDescription: { ...T.body13, color: C.deepGray, textAlign: "center" },
  // Buttons: secondary 34068:5423, primary 34002:3832
  outlineButton: { minHeight: 44, paddingHorizontal: 20, borderRadius: 8, borderWidth: 1, borderColor: C.secondary4, backgroundColor: C.secondary4, alignItems: "center", justifyContent: "center" },
  outlineButtonText: { ...T.bold12, color: C.gray66 },
  primaryButton: { height: K.button.height, backgroundColor: C.primary, borderRadius: K.button.radius, justifyContent: "center", alignItems: "center", paddingHorizontal: 16 },
  primaryButtonText: { ...T.bold14, color: C.white },
  // Menus reuse the popup 34068:5410
  menu: { position: "absolute", backgroundColor: C.white, borderRadius: 12, borderWidth: 1, borderColor: C.grayF1, paddingVertical: 4, boxShadow: fig.shadow.card },
  menuOption: { minHeight: 44, flexDirection: "row", alignItems: "center", gap: 8, paddingHorizontal: 16, paddingVertical: 10 },
  menuOptionText: { ...T.body13, color: C.black },
});
