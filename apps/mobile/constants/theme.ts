/**
 * Design tokens of 마지미 (공유용).fig, the app's design source.
 *
 * `fig` holds the Figma values: the local colour variables by their Figma names, the
 * text styles and the shared components (node ids in constants/figma-tokens.json,
 * which tools/design/export-tokens.mjs derives from the .fig). Screens use these or
 * the legacy aliases below, which point at the same values.
 * scripts/check-figma-tokens.cjs rejects any colour, font size, weight or line height
 * that is not in the .fig, and asserts these tokens against figma-tokens.json.
 */

import { type TextStyle, type ViewStyle } from "react-native";

// ---------------------------------------------------------------------------
// Colours: Figma variables (set "Color") first, then values the frames use directly.
// ---------------------------------------------------------------------------

const color = {
  primary: "#34BB84", // Colors/Primeary color
  primary2: "#EFFFF8", // Colors/Primeary color 2
  secondary: "#239969", // Colors/Secondary color
  secondary4: "#2A8F660A", // Colors/Secondary 4%
  secondary8: "#2A8F6614", // Colors/Secondary 8%
  secondary32: "#2A8F6652", // Colors/Secondary 32%
  black: "#070428", // Colors/Black
  navy: "#191A54", // Colors/Color/Navy
  blue: "#0095FF", // Colors/Color/Blue
  skyblue: "#4DAEEB", // Colors/Color/Skyblue
  orange: "#F75B2B", // Colors/Color/Orange
  red: "#FF2B4B", // Colors/Color/Red
  deepGray: "#8894A1", // Colors/DeepGray
  gray66: "#5D5D62", // Colors/Gray/Gray66
  gray99: "#919FA8", // Colors/Gray/Gray99
  grayC1: "#C1C1C1", // Colors/Gray/GrayC1
  grayC8: "#C9CCCE", // Colors/Gray/GrayC8
  grayE1: "#E1E1E1", // Colors/Gray/GrayE1
  grayE8: "#E8E8E8", // Colors/Gray/GrayE8
  grayF1: "#F1F1F1", // Colors/Gray/GrayF1
  grayF7: "#F7F9FA", // Colors/Gray/GrayF7
  white: "#FFFFFF", // Colors/White
  input2: "#0095FF05", // Colors/Input filed 2%
  input8: "#0095FF14", // Colors/Input filed 8%
  inputError: "#F72B2B0A", // Colors/Input filed -Error
  // Frame values without a variable
  title: "#111111", // card titles 34065:2937
  heading: "#052236", // 홈 title 34065:6162
  primaryBright: "#15D688", // back arrow 34065:14634, 모두 동의하기 32002:1035
  footnote: "#8EA0AC", // 34065:14597
  white92: "#FFFFFFEB", // header 34065:6393
  white96: "#FFFFFFF5", // bottom bar 34002:3829
  white98: "#FFFFFFFA", // tab bar 34065:6364
  whiteLine4: "#FFFFFF0A", // login arrow fill 32002:847
  whiteLine16: "#FFFFFF29", // login arrow stroke 32002:847
  filterLine: "#003E810A", // filter bar 34065:3281
  unread: "#EEF6F3", // White + Secondary 8%, 34065:14574
  disabled: "#7D8894", // DeepGray + 8% black, 34002:3961
  loginBackground: "#30AC7A", // Primary + 8% black, 32002:699
  overlay: "#11111199", // #111111 at 60%, 34002:3983
  kakao: "#FFE500", // 32002:798
  apple: "#222222", // 32002:819
  tileScholarship: "#E2F9E8", // #CEF5D8 + 40% white, 34065:6201
  tileBenefit: "#EBF9E2", // #DEF5CE + 40% white, 34065:6213
  tileGift: "#E2F9F9", // #CEF5F5 + 40% white, 34065:6225
} as const;

// ---------------------------------------------------------------------------
// Text styles: every size/weight/line-height pair the frames use.
// Text maps 400 → Regular, 500/600 → SemiBold, 700 → Bold (the bundled cuts).
// ---------------------------------------------------------------------------

const t = (fontSize: number, fontWeight: TextStyle["fontWeight"], lineHeight: number): TextStyle => ({ fontSize, fontWeight, lineHeight });
const text = {
  label9: t(9, "600", 12), // input label 34002:3854
  caption10: t(10, "400", 12), // 34065:6182
  badge10: t(10, "600", 12), // D-day 34065:6205
  body11: t(11, "400", 16), // tab label 34065:6369
  badge11: t(11, "600", 16), // 34065:2939
  meta12: t(12, "400", 16), // time 34065:14573
  body12: t(12, "400", 18), // 34065:2943
  medium12: t(12, "500", 19), // 34065:4532
  semibold12: t(12, "600", 18), // 34065:6311
  bold12: t(12, "700", 18), // 34065:2923
  body13: t(13, "400", 20), // 34065:2950
  medium13: t(13, "500", 20), // 32002:755
  semibold13: t(13, "600", 20), // 34065:3117
  semibold14: t(14, "600", 20),
  bold14: t(14, "700", 20), // 34002:3833
  body16: t(16, "400", 24), // 34002:4004
  medium16: t(16, "500", 24), // 34002:4008
  bold16: t(16, "700", 24), // 34065:2937
  bold18: t(18, "700", 28), // 34065:6194
  bold20: t(20, "700", 32), // 34002:3815
  bold24: t(24, "700", 36), // 34065:6177
} as const;

/** Figma DROP_SHADOW as a CSS box shadow (React Native 0.76+ and react-native-web). */
const shadow = {
  card: "0px 8px 12px 2px #00000005", // 34065:2930
  bar: "0px -5px 20px 0px #00000005", // 34002:3829
  badge: "0px 7px 12px 3px #0000000F", // 34065:6240
  badgeInner: "inset 4px 4px 14px 0px #FFFFFF33", // 34065:2938
} as const;

// ---------------------------------------------------------------------------
// Shared components (node ids in tools/design/figma-components.mjs).
// ---------------------------------------------------------------------------

const component = {
  header: { height: 56, paddingHorizontal: 16, backgroundColor: color.white92 }, // 34065:6393
  headerLine: { logo: color.secondary4, back: color.grayF1 }, // bottom stroke of 34065:6393 / 34065:14632
  // A header laid over scrolling content (detail 34065:11165). Figma draws it White 92% with a
  // background blur of 8, which reads as solid white. The app draws no blur (see
  // docs/design/figma-restore-20261026.md), and 92% alone lets the scrolled text show through
  // the title, so it is opaque White.
  floatingHeader: { backgroundColor: color.white },
  tabBar: { height: 52, paddingVertical: 4, backgroundColor: color.white98, borderTopColor: color.grayF1, radius: 12, item: 44, iconBox: 24, icon: 16, inactiveOpacity: 0.6 }, // 34065:6364
  tabs: { height: 44, paddingHorizontal: 16, paddingVertical: 12, underline: 2, lineColor: color.secondary4 }, // 34065:3115
  card: { radius: 16, radiusHome: 20, padding: 16, paddingBottom: 8, gap: 8, borderColor: color.grayF1 }, // 34065:2930, 34065:6288
  cardIcon: { size: 48, radius: 16, icon: 24 }, // 34065:2933
  badge: { radius: 8, paddingHorizontal: 8, paddingVertical: 4 }, // 34065:2938
  filterPill: { height: 34, radius: 16, paddingHorizontal: 12 }, // 34065:3283
  input: { height: 54, radius: 12, paddingHorizontal: 12, paddingVertical: 10 }, // 34002:3818
  infoBox: { radius: 17, paddingLeft: 16, paddingRight: 12, paddingVertical: 8 }, // 34002:3868
  button: { height: 48, radius: 12, paddingHorizontal: 36 }, // 34002:3832
  bottomBar: { height: 64, radius: 16, paddingHorizontal: 16 }, // 34002:3829
  sheet: { radius: 20, handleWidth: 60, handleHeight: 4 }, // 34002:3984
  section: { kickerGap: 4, chevron: 20 }, // 34065:6189
  banner: { radius: 20, padding: 16, gap: 20 }, // 34065:6237
  hero: { radius: 20 }, // 34065:6169
  tile: { width: 156, height: 124, radius: 12, image: 100 }, // 34065:6201
  choice: { radius: 16, art: 100, artImage: 88 }, // 34003:2744
  alertRow: { avatar: 48 }, // 34065:14567
} as const;

export const fig = { color, text, shadow, component } as const;

// ---------------------------------------------------------------------------
// Legacy aliases. Older screens read these names; every value is a Figma value.
// ---------------------------------------------------------------------------

export const colors = {
  primary: color.primary,
  loginBackground: color.loginBackground,
  primaryContainer: color.secondary,
  primaryFixed: color.primary2,
  primaryFixedDim: color.secondary32,
  onPrimary: color.white,
  onPrimaryContainer: color.white,
  onPrimaryFixed: color.heading,
  onPrimaryFixedVariant: color.secondary,

  secondary: color.gray66,
  secondaryContainer: color.grayF7,
  secondaryFixed: color.grayF7,
  secondaryFixedDim: color.grayE1,
  onSecondary: color.white,
  onSecondaryContainer: color.gray66,

  tertiary: color.deepGray,
  tertiaryContainer: color.gray66,
  tertiaryFixed: color.grayF7,
  tertiaryFixedDim: color.grayE1,

  surface: color.white,
  surfaceBright: color.white,
  surfaceDim: color.grayE1,
  surfaceContainer: color.grayF7,
  surfaceContainerLow: color.grayF7,
  surfaceContainerHigh: color.grayF7,
  surfaceContainerHighest: color.grayE1,
  surfaceContainerLowest: color.white,
  surfaceVariant: color.grayE1,
  surfaceTint: color.primary,

  onSurface: color.black,
  onSurfaceVariant: color.gray66,
  onBackground: color.black,
  inverseSurface: color.navy,
  inverseOnSurface: color.white,
  inversePrimary: color.primary,

  outline: color.deepGray,
  outlineVariant: color.grayF1,

  // Figma marks unmet conditions, rejections and urgent deadlines in Orange.
  error: color.orange,
  errorContainer: color.inputError,
  onError: color.white,
  onErrorContainer: color.orange,

  background: color.white,

  card: color.white,
  textPrimary: color.black,
  textSecondary: color.gray66,
  textMuted: color.deepGray,
  textDisabled: color.grayC8,
  textInverse: color.white,

  tabBarBackground: color.white98,
  tabBarActive: color.primary,
  tabBarActiveBackground: color.primary2,
  tabBarInactive: color.gray66,

  overlay: color.overlay,
  overlayLight: color.secondary4,

  decorativeBlob: color.secondary4,
  skyblue: color.skyblue,
} as const;

export type ColorToken = keyof typeof colors;

export const typography = {
  fontFamily: {
    heading: "Pretendard-Bold",
    body: "Pretendard",
  },

  // Sizes the frames use: 9 10 11 12 13 14 16 18 20 24.
  fontSize: {
    xs: 11,
    sm: 13,
    base: 14,
    md: 16,
    lg: 18,
    xl: 20,
    "2xl": 24,
    "3xl": 24,
    "4xl": 24,
    "5xl": 24,
  },

  fontWeight: {
    regular: "400" as const,
    medium: "500" as const,
    semibold: "600" as const,
    bold: "700" as const,
    extrabold: "700" as const,
    black: "700" as const,
  },

  letterSpacing: {
    tight: 0,
    normal: 0,
    wide: 0,
  },

  styles: {
    heroTitle: { ...text.bold24, color: color.black },
    pageTitle: { ...text.bold20, color: color.black },
    sectionTitle: { ...text.bold16, color: color.black },
    cardTitle: { ...text.bold16, color: color.title },
    bodyBase: { ...text.body13, color: color.gray66 },
    bodySm: { ...text.body12, color: color.gray66 },
    label: { ...text.semibold13, color: color.black },
    caption: { ...text.body12, color: color.deepGray },
    captionSm: { ...text.body11, color: color.deepGray },
    tabLabel: text.body11,
    buttonLabel: text.bold14,
    buttonLabelSm: text.semibold13,
    inputText: { ...text.body12, color: color.black },
    inputPlaceholder: { ...text.body12, color: color.deepGray },
    badge: text.badge11,
  },
} as const;

export const spacing = {
  0: 0,
  0.5: 2,
  1: 4,
  1.5: 6,
  2: 8,
  2.5: 10,
  3: 12,
  3.5: 14,
  4: 16,
  5: 20,
  6: 24,
  7: 28,
  8: 32,
  9: 36,
  10: 40,
  12: 48,
  14: 56,
  16: 64,
  20: 80,
  24: 96,
} as const;

export type SpacingToken = keyof typeof spacing;

// Radii the frames use: 8 (badge), 12 (input, button, tile), 16 (card, pill), 20 (home card, sheet), 24, 100 (pill).
export const borderRadius = {
  none: 0,
  sm: 4,
  md: 8,
  lg: 16,
  xl: 20,
  "2xl": 24,
  full: 100,
} as const;

export type BorderRadiusToken = keyof typeof borderRadius;

type ShadowStyle = Pick<ViewStyle, "boxShadow">;

export const shadows = {
  none: {} as ShadowStyle,
  card: { boxShadow: shadow.card } as ShadowStyle,
  cardMd: { boxShadow: shadow.card } as ShadowStyle,
  cardLg: { boxShadow: shadow.card } as ShadowStyle,
  primaryButton: {} as ShadowStyle,
  floating: { boxShadow: shadow.bar } as ShadowStyle,
  header: {} as ShadowStyle,
} as const;

export type ShadowToken = keyof typeof shadows;

export const layout = {
  pagePadding: 16,
  pageVerticalPadding: 16,

  cardPadding: component.card.padding,
  cardPaddingLg: component.card.padding,

  cardGap: 16,
  cardGapMd: 12,
  cardGapLg: 16,

  sectionGap: 24,
  sectionGapLg: 24,

  touchTargetMin: 44,
  buttonHeightSm: 44,
  buttonHeightMd: component.button.height,
  buttonHeightLg: component.button.height,

  inputHeight: component.input.height,
  inputPaddingHorizontal: component.input.paddingHorizontal,

  tabBarHeight: component.tabBar.height,
  tabBarPaddingBottom: component.tabBar.paddingVertical,
  tabBarPaddingTop: component.tabBar.paddingVertical,
  tabBarIconSize: component.tabBar.icon,
  tabBarTopRadius: component.tabBar.radius,

  headerHeight: component.header.height,

  bottomSafeArea: 34,

  maxContentWidth: 520,

  avatarSm: 32,
  avatarMd: 40,
  avatarLg: 48,
  avatarXl: 64,

  progressDotSize: 10,
  progressDotSizeActive: 12,
  progressBarHeight: 10,

  badgePaddingHorizontal: component.badge.paddingHorizontal,
  badgePaddingVertical: component.badge.paddingVertical,
} as const;

export const componentStyles = {
  /** Benefit card 34065:2930. */
  card: {
    backgroundColor: color.white,
    borderRadius: component.card.radius,
    borderWidth: 1,
    borderColor: component.card.borderColor,
    padding: component.card.padding,
    boxShadow: shadow.card,
  } as ViewStyle,

  cardLg: {
    backgroundColor: color.white,
    borderRadius: component.card.radiusHome,
    borderWidth: 1,
    borderColor: component.card.borderColor,
    padding: component.card.padding,
    boxShadow: shadow.card,
  } as ViewStyle,

  /** Tinted panel (banner 34065:6237). */
  section: {
    backgroundColor: color.secondary4,
    borderRadius: component.card.radius,
    borderWidth: 1,
    borderColor: color.secondary4,
    padding: component.card.padding,
  } as ViewStyle,

  /** Input 34002:3818. */
  inputBase: {
    backgroundColor: color.secondary4,
    borderRadius: component.input.radius,
    height: component.input.height,
    paddingHorizontal: component.input.paddingHorizontal,
    ...typography.styles.inputText,
  } as ViewStyle,

  inputFocused: {
    backgroundColor: color.secondary8,
  } as ViewStyle,

  /** Primary button 34002:3832. */
  buttonPrimary: {
    backgroundColor: color.primary,
    borderRadius: component.button.radius,
    height: component.button.height,
    alignItems: "center" as const,
    justifyContent: "center" as const,
  } as ViewStyle,

  buttonSecondary: {
    backgroundColor: color.white,
    borderRadius: component.button.radius,
    borderWidth: 1,
    borderColor: color.grayF1,
    height: component.button.height,
    alignItems: "center" as const,
    justifyContent: "center" as const,
  } as ViewStyle,

  /** Disabled button 34002:3961. */
  buttonDisabled: {
    backgroundColor: color.disabled,
    borderRadius: component.button.radius,
    height: component.button.height,
    alignItems: "center" as const,
    justifyContent: "center" as const,
  } as ViewStyle,

  /** Region option 34002:4007 / 34002:4003. */
  chipActive: {
    backgroundColor: color.secondary8,
    borderRadius: borderRadius.full,
    paddingHorizontal: 12,
    paddingVertical: 4,
  } as ViewStyle,

  chipInactive: {
    backgroundColor: color.white,
    borderRadius: borderRadius.full,
    borderWidth: 1,
    borderColor: color.secondary8,
    paddingHorizontal: 12,
    paddingVertical: 4,
  } as ViewStyle,

  /** Status badge 34065:6345. */
  tagDefault: {
    backgroundColor: color.grayF7,
    borderRadius: component.badge.radius,
    paddingHorizontal: component.badge.paddingHorizontal,
    paddingVertical: component.badge.paddingVertical,
  } as ViewStyle,

  /** Status badge 34065:6312. */
  tagPrimary: {
    backgroundColor: color.primary2,
    borderRadius: component.badge.radius,
    paddingHorizontal: component.badge.paddingHorizontal,
    paddingVertical: component.badge.paddingVertical,
  } as ViewStyle,

  selectedState: {
    backgroundColor: color.secondary4,
  } as ViewStyle,

  tabActivePill: {
    backgroundColor: color.primary2,
    borderRadius: borderRadius.full,
    paddingHorizontal: 12,
    paddingVertical: 4,
  } as ViewStyle,
} as const;

export const gradients = {
  primaryCta: {
    colors: [color.primary, color.primary],
    start: { x: 0, y: 0 },
    end: { x: 1, y: 0 },
  },
  surfaceHero: {
    colors: [color.white, color.primary2],
    start: { x: 0, y: 0 },
    end: { x: 0, y: 1 },
  },
} as const;

export const theme = {
  colors,
  typography,
  spacing,
  borderRadius,
  shadows,
  layout,
  componentStyles,
  gradients,
  fig,
} as const;

export type Theme = typeof theme;
export default theme;
