// Figma nodes whose values the app's shared tokens (apps/mobile/constants/theme.ts `fig`)
// reproduce. export-tokens.mjs records each one's summary in figma-tokens.json and
// apps/mobile/scripts/check-figma-tokens.cjs asserts the theme against it.
export const COMPONENTS = {
  // Bottom tab bar, 홈 34065:6155
  tabBar: "34065:6364", tabItem: "34065:6366", tabIconBox: "34065:6367", tabLabelActive: "34065:6369", tabItemInactive: "34065:6370", tabLabelInactive: "34065:6373", homeIndicatorArea: "34065:6363",
  // Headers
  headerLogo: "34065:6393", headerLogoMark: "34065:6395", bell: "34065:6405", bellDot: "34065:6407",
  headerTitle: "34065:3106", headerTitleText: "34065:3108", headerSearch: "34065:3182",
  headerBack: "34065:14632", headerBackIcon: "34065:14634", headerBackTitle: "34065:14654", headerBackAction: "34065:14636",
  // Category tabs and filters, 탐색 34065:2917
  tabsRow: "34065:3115", tabSelected: "34065:3116", tabSelectedText: "34065:3117", tabText: "34065:3119",
  filterBar: "34065:3281", filterPill: "34065:3283", filterLabel: "34065:3285", filterValue: "34065:3287",
  listCount: "34065:2923", listCountNumber: "34065:2924", listToggleText: "34065:4532", listDivider: "34065:4533", listSortText: "34065:2927",
  // Benefit card: 탐색/관심 (radius 16) and 홈 (radius 20)
  card: "34065:2930", cardHome: "34065:6288", cardSelected: "34059:2829", cardTop: "34065:2931", cardIcon: "34065:2933", cardTitle: "34065:2937",
  typeBadge: "34065:2938", typeBadgeText: "34065:2939", typeBadgeScholarship: "34059:2837", cardMeta: "34065:2943", cardMetaDot: "34065:2944", cardSummary: "34065:2950",
  cardFooter: "34065:2951", cardFooterCell: "34065:2952", statusLabel: "34065:6311", statusPass: "34065:6312", statusPassText: "34065:6313", statusUnknown: "34065:6345", statusUnknownText: "34065:6346",
  cardClose: "34059:3452", moreButton: "34065:6357",
  // 홈 sections
  homeTitle: "34065:6162", homeLink: "34065:6165", homeLinkArrow: "34065:6166",
  hero: "34065:6169", heroLabel: "34065:6176", heroAmount: "34065:6177", heroBody: "34065:6178", heroNote: "34065:6182",
  sectionKicker: "34065:6191", sectionTitle: "34065:6194", sectionChevron: "34065:6196",
  deadlineTile: "34065:6201", deadlineBadgeUrgent: "34065:6204", deadlineBadge: "34065:6216", deadlineBadgeText: "34065:6205", deadlineCategory: "34065:6207", deadlineTitle: "34065:6209", deadlineSummary: "34065:6210",
  banner: "34065:6237", bannerBadge: "34065:6240", bannerBadgeText: "34065:6241", bannerTitle: "34065:6243", bannerCaption: "34065:6244",
  // 온보딩 (나이 34002:3810, 지역 시트 34002:3934, 학적 34003:2694)
  stepTitle: "34002:3815", stepSubtitle: "34002:3816", input: "34002:3818", inputLabel: "34002:3854", inputValue: "34002:3855", inputPlaceholder: "34002:3945",
  infoBox: "34002:3868", infoText: "34002:3872", bottomBar: "34002:3829", buttonPrimary: "34002:3832", buttonPrimaryText: "34002:3833", buttonDisabled: "34002:3961",
  overlay: "34002:3983", sheet: "34002:3984", sheetHandle: "34002:3985", sheetTitle: "34002:3989", sheetDescription: "34002:3990", sheetOption: "34002:4003", sheetOptionText: "34002:4004", sheetOptionSelected: "34002:4007", sheetOptionSelectedText: "34002:4008", sheetBar: "34002:4016",
  choiceSelected: "34003:2744", choice: "34003:2750", choiceArt: "34003:2746", choiceTitleSelected: "34003:2749", choiceTextSelected: "34003:2804", choiceTitle: "34003:2808", choiceText: "34003:2809",
  // 로그인 32002:697
  loginBackground: "32002:699", loginMark: "34002:3571", loginTagline: "32002:722", loginCaption: "32002:755", loginKakao: "32002:798", loginGoogle: "32002:801", loginGuest: "32002:840", loginGuestArrow: "32002:847", loginCopyright: "32002:792",
  // 알림 34065:14563
  alertRow: "34065:14567", alertRowUnread: "34065:14574", alertAvatar: "34065:14569", alertText: "34065:14572", alertTime: "34065:14573", alertFootnote: "34065:14597",
};
