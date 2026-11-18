import React from "react";
import { Platform, Pressable, StyleSheet, View, type StyleProp, type ViewStyle } from "react-native";
import { FigmaIcon } from "./FigmaUI";
import Text from "./Text";
import { colors } from "@/constants/theme";
import { useGuest } from "@/lib/guest";

// 비회원에게 회원 기능 자리를 흐리게 보여 주고 로그인으로 안내한다 (v1.0.0 피드백 F2.4·F4.1).
// 흐림: 웹은 CSS blur, Android 12+는 RN filter blur. iOS는 RN이 blur를 지원하지 않아
// 불투명도를 높인 덮개로 가린다. 네이티브 모듈을 더하지 않아 OTA 업데이트로도 배포된다.
const previewFilter: ViewStyle = Platform.OS === "web"
  ? ({ filter: "blur(5px)" } as unknown as ViewStyle)
  : Platform.OS === "android" ? { filter: [{ blur: 6 }] } : {};
const veil = Platform.OS === "ios" ? "#FFFFFFCC" : "#FFFFFF7A";

export type LockedSectionProps = {
  /** 회원에게 보이는 실제 내용. 비회원에게는 흐린 미리보기로만 깔린다. 없으면 자리표시 막대를 깐다 */
  children?: React.ReactNode;
  /** 잠금 안내 한 줄. 무엇이 열리는지 말한다 (예: "로그인하면 내 조건 맞춤 추천을 볼 수 있어요") */
  message?: string;
  /** 버튼 글자. 기본 "로그인 필요" */
  label?: string;
  /** 비회원 여부를 직접 정할 때. 기본은 useGuest().isGuest (세션 복원 중에는 잠그지 않는다) */
  locked?: boolean;
  /** 버튼 동작. 기본은 첫 로그인 화면(/login?return=1)을 열고, 로그인하면 이 화면으로 돌아온다 */
  onLogin?: () => void;
  /** 잠긴 상태의 최소 높이. 내용이 짧아도 안내가 잘리지 않게 한다 */
  minHeight?: number;
  style?: StyleProp<ViewStyle>;
};

/** 비회원에게 잠금 덮개(흐림 + 자물쇠 + "로그인 필요")를 씌운다. 회원에게는 children을 그대로 그린다 */
export default function LockedSection({ children, message, label = "로그인 필요", locked, onLogin, minHeight = 132, style }: LockedSectionProps) {
  const guest = useGuest();
  const isLocked = locked ?? guest.isGuest;
  if (!isLocked) return <>{children}</>;
  const login = onLogin ?? guest.openLogin;
  return <View style={[styles.frame, { minHeight }, style]}>
    {/* 미리보기는 보이기만 한다. 누를 수 없고 화면 읽기에서도 빠진다 */}
    <View pointerEvents="none" accessibilityElementsHidden importantForAccessibility="no-hide-descendants" aria-hidden style={[styles.preview, previewFilter]}>
      {children ?? <PlaceholderLines />}
    </View>
    {/* 덮개 어디를 눌러도 로그인 화면이 열린다. 버튼 모양은 누를 곳을 알려 준다 */}
    <Pressable accessibilityRole="button" accessibilityLabel={message ? `${label}. ${message}` : label} onPress={login} style={[StyleSheet.absoluteFill, styles.veil, { backgroundColor: veil }]}>
      {({ pressed }) => <>
        <View style={styles.lock}><FigmaIcon name="eyeOff" size={16} color={colors.primary} /></View>
        {!!message && <Text style={styles.message}>{message}</Text>}
        <View style={[styles.cta, pressed && styles.ctaPressed]}>
          <Text style={styles.ctaText}>{label}</Text>
          <FigmaIcon name="arrow" size={12} color={colors.onPrimary} />
        </View>
      </>}
    </Pressable>
  </View>;
}

function PlaceholderLines() {
  return <View style={{ padding: 16, gap: 10 }}>
    {[0.55, 0.85, 0.7, 0.4].map((width, i) => <View key={i} style={[styles.line, { width: `${width * 100}%` }, i === 0 && { height: 14, backgroundColor: "#2A8F6614" }]} />)}
  </View>;
}

const styles = StyleSheet.create({
  frame: { position: "relative", overflow: "hidden", borderRadius: 20, borderWidth: 1, borderColor: "#F1F1F1", backgroundColor: "#F7F9FA" },
  preview: { opacity: 0.7 },
  veil: { alignItems: "center", justifyContent: "center", padding: 16, gap: 8 },
  lock: { width: 40, height: 40, borderRadius: 20, backgroundColor: colors.primaryFixed, borderWidth: 1, borderColor: "#2A8F6614", alignItems: "center", justifyContent: "center" },
  message: { fontSize: 13, lineHeight: 20, color: colors.textSecondary, textAlign: "center" },
  cta: { minHeight: 44, minWidth: 120, paddingHorizontal: 18, borderRadius: 22, backgroundColor: colors.primary, flexDirection: "row", alignItems: "center", justifyContent: "center", gap: 4 },
  ctaPressed: { backgroundColor: colors.onPrimaryFixedVariant },
  ctaText: { fontSize: 13, lineHeight: 20, fontWeight: "600", color: "#FFFFFF" },
  line: { height: 10, borderRadius: 5, backgroundColor: "#F7F9FA" },
});
