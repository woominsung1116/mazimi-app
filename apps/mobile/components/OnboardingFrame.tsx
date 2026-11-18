import React, { useEffect } from "react";
import { ActivityIndicator, KeyboardAvoidingView, Platform, Pressable, ScrollView, StyleSheet, View } from "react-native";
import { useRouter } from "expo-router";
import { useSafeAreaInsets } from "react-native-safe-area-context";
import Text from "./Text";
import { FigmaIcon, ui } from "./FigmaUI";
import { colors, fig } from "@/constants/theme";
import { useGuest } from "@/lib/guest";
import { SessionErrorState, useSessionProblem } from "./SessionProblem";

/** `onBlocked`: called when the person taps 다음 while `disabled` (to show why). Without it a disabled button ignores taps. */
/** `onSkip`: the header's 건너뛰기 (34005:2959), only on a step whose every field is optional. */
export default function OnboardingFrame({ title, description, children, next, disabled = false, label = "다음", busy = false, onBlocked, onSkip }: { title: string; description: string; children: React.ReactNode; next: () => void; disabled?: boolean; label?: string; busy?: boolean; onBlocked?: () => void; onSkip?: () => void }) {
  const insets = useSafeAreaInsets();
  const router = useRouter();
  // 조건 입력은 회원만 한다. 비회원이 링크로 들어오면 로그인 화면으로 보낸다
  // 저장된 로그인이 있는데 서버에 닿지 못했으면 로그인 화면이 아니라 다시 시도를 보여 준다
  const { isGuest } = useGuest();
  const sessionProblem = useSessionProblem();
  useEffect(() => { if (isGuest) router.replace("/login"); }, [isGuest, router]);
  if (isGuest) return <View style={styles.root} />;
  if (sessionProblem) return <View style={[styles.root, { paddingTop: insets.top }]}><SessionErrorState /></View>;
  return <KeyboardAvoidingView behavior={Platform.OS === "ios" ? "padding" : undefined} style={[styles.root, { paddingTop: insets.top }]}>
    <View style={styles.header}><Pressable accessibilityRole="button" accessibilityLabel="뒤로 가기" onPress={() => router.canGoBack() ? router.back() : router.replace("/login")} style={ui.iconButton}><FigmaIcon name="back" size={20} color={fig.color.primaryBright} /></Pressable>{onSkip && <Pressable accessibilityRole="button" accessibilityLabel="건너뛰기" disabled={busy} onPress={onSkip} style={styles.skip}><Text style={styles.skipText}>건너뛰기</Text><View style={styles.skipArrow}><FigmaIcon name="arrow" size={12} color={fig.color.primary} /></View></Pressable>}</View>
    <ScrollView keyboardShouldPersistTaps="handled" showsVerticalScrollIndicator={false} contentContainerStyle={styles.content}>
      <View style={{ gap: 4 }}><Text style={onboardingStyle.title}>{title}</Text><Text style={onboardingStyle.description}>{description}</Text></View>
      {children}
    </ScrollView>
    <View style={[styles.footer, { paddingBottom: insets.bottom + 8 }]}>
      <Pressable accessibilityRole="button" accessibilityState={{ disabled: disabled || busy, busy }} disabled={busy || (disabled && !onBlocked)} onPress={disabled ? onBlocked : next} style={({ pressed }) => [styles.button, (disabled || busy) && styles.disabled, pressed && !disabled && styles.pressed]}>{busy ? <ActivityIndicator color={fig.color.white} /> : <Text style={ui.primaryButtonText}>{label}</Text>}</Pressable>
    </View>
  </KeyboardAvoidingView>;
}
export function OnboardingNote({ children }: { children: React.ReactNode }) {
  return <View style={styles.note}><FigmaIcon name="info" size={12} color={fig.color.skyblue} /><Text style={onboardingStyle.note}>{children}</Text></View>;
}
// Figma 나이 34002:3810: back header 34002:3825, content 34002:3812, bottom bar 34002:3829,
// button 34002:3832 (disabled 34002:3961), input 34002:3818, note 34002:3868, 학적 cards 34003:2744.
const C = fig.color, K = fig.component, T = fig.text;
const styles = StyleSheet.create({
  root: { flex: 1, backgroundColor: C.white },
  header: { height: K.header.height, backgroundColor: C.white92, borderBottomWidth: 1, borderBottomColor: K.headerLine.back, paddingHorizontal: 4, flexDirection: "row", justifyContent: "space-between", alignItems: "center" },
  skip: { flexDirection: "row", alignItems: "center", gap: 6, minHeight: 44, paddingHorizontal: 12 }, skipText: { ...T.semibold13, color: C.primary }, // 34005:2959
  skipArrow: { width: 20, height: 20, borderRadius: 14, padding: 4, borderWidth: 1, borderColor: C.secondary8, backgroundColor: C.secondary4, alignItems: "center", justifyContent: "center" },
  content: { paddingHorizontal: 16, paddingTop: 24, gap: 24, paddingBottom: 32 },
  footer: { paddingHorizontal: K.bottomBar.paddingHorizontal, paddingTop: 8, backgroundColor: C.white96, borderTopWidth: 1, borderTopColor: C.grayF1, borderTopLeftRadius: K.bottomBar.radius, borderTopRightRadius: K.bottomBar.radius, boxShadow: fig.shadow.bar },
  button: { height: K.button.height, borderRadius: K.button.radius, backgroundColor: C.primary, alignItems: "center", justifyContent: "center" },
  disabled: { backgroundColor: C.disabled },
  pressed: { backgroundColor: C.secondary },
  note: { flexDirection: "row", alignItems: "center", gap: 4, backgroundColor: C.secondary4, borderWidth: 1, borderColor: C.secondary4, borderRadius: K.infoBox.radius, paddingVertical: K.infoBox.paddingVertical, paddingLeft: K.infoBox.paddingLeft, paddingRight: K.infoBox.paddingRight },
});
/** The bottom bar and button (34002:3829), for a step without the frame's back header (회원가입을 환영합니다 28023:1832). */
export const onboardingFooter = { bar: styles.footer, button: styles.button };
export const onboardingStyle = StyleSheet.create({
  title: { ...T.bold20, color: C.black },
  description: { ...T.body13, color: C.deepGray },
  input: { height: K.input.height, borderRadius: K.input.radius, backgroundColor: C.secondary4, paddingHorizontal: K.input.paddingHorizontal, ...T.body12, fontFamily: "Pretendard", color: C.black },
  note: { flexShrink: 1, ...T.body11, color: C.gray66 },
  label: { ...T.semibold13, color: C.black },
  option: { minHeight: 48, borderRadius: K.choice.radius, backgroundColor: C.input2, padding: 12, justifyContent: "center", borderWidth: 1, borderColor: C.secondary8 },
  selected: { borderColor: C.primary, backgroundColor: C.secondary4 },
});
