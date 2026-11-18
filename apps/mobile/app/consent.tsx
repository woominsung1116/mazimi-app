import React, { useState } from "react";
import { Pressable, ScrollView, StyleSheet, View } from "react-native";
import { useRouter } from "expo-router";
import { useSafeAreaInsets } from "react-native-safe-area-context";
import Text from "@/components/Text";
import BrandBackdrop from "@/components/BrandBackdrop";
import { FigmaIcon, ui } from "@/components/FigmaUI";
import { useConsentStore } from "@/store/consent";
import { colors, fig } from "@/constants/theme";

/** Figma 32002:906. Only the two required agreements are recorded here.
 * Sensitive document consent stays separate at the point of upload. */
export default function ConsentGateScreen() {
  const router = useRouter();
  const insets = useSafeAreaInsets();
  const agreeRequired = useConsentStore(s => s.agreeRequired);
  const [terms, setTerms] = useState(false);
  const [privacy, setPrivacy] = useState(false);
  const [busy, setBusy] = useState(false);
  const all = terms && privacy;
  async function continueToLogin() {
    if (!all || busy) return;
    setBusy(true);
    await agreeRequired();
    router.replace("/login");
  }
  const agreement = (label: string, checked: boolean, toggle: () => void, route?: "/terms" | "/privacy-policy") => (
    <View style={[styles.row, !route && styles.all]}>
      <Pressable accessibilityRole="checkbox" accessibilityLabel={label} accessibilityState={{ checked }} onPress={toggle} style={styles.check}>
        <FigmaIcon name="checkCircle" size={24} color={checked ? fig.color.primary : fig.color.grayE1} />
        <Text style={[styles.label, !route && styles.labelAll]}>{label}{route && <Text style={{ color: colors.textMuted }}> (필수)</Text>}</Text>
      </Pressable>
      {route && <Pressable accessibilityRole="link" accessibilityLabel={`${label} 보기`} onPress={() => router.push(route)} style={ui.iconButton}><View style={{ opacity: 0.24 }}><FigmaIcon name="arrow" size={16} color={fig.color.deepGray} /></View></Pressable>}
    </View>
  );
  return <View style={styles.root}>
    <BrandBackdrop />
    <View style={[styles.sheet, { paddingBottom: Math.max(insets.bottom, 16), maxHeight: "90%" }]}>
      <View style={styles.handle} />
      <ScrollView showsVerticalScrollIndicator={false} contentContainerStyle={styles.content}>
        <Text style={styles.title}>약관 동의</Text>
        <Text style={styles.description}>서비스 이용을 위해 약관에 동의해 주세요</Text>
        {agreement("모든 약관에 동의합니다.", all, () => { setTerms(!all); setPrivacy(!all); })}
        {agreement("이용약관 동의", terms, () => setTerms(!terms), "/terms")}
        {agreement("개인정보처리방침 동의", privacy, () => setPrivacy(!privacy), "/privacy-policy")}
        <Text style={styles.note}>건강보험 등 민감정보 서류는 업로드할 때 별도로 동의를 받아요.</Text>
      </ScrollView>
      <View style={styles.actions}>
        <Pressable accessibilityRole="button" accessibilityLabel="동의하고 시작하기" accessibilityState={{ disabled: !all || busy }} disabled={!all || busy} onPress={() => void continueToLogin()} style={[ui.primaryButton, styles.button, styles.agree, (!all || busy) && { backgroundColor: fig.color.disabled }]}><Text style={styles.buttonText}>{busy ? "시작 중…" : "동의하기"}</Text></Pressable>
        <Pressable accessibilityRole="button" onPress={() => { setTerms(true); setPrivacy(true); }} style={[ui.primaryButton, styles.button, styles.agreeAll]}><Text style={styles.buttonText}>모두 동의하기</Text></Pressable>
      </View>
    </View>
  </View>;
}
// Figma 32002:1000: sheet, rows 32002:1007 / 32002:1013, buttons 32002:1033 / 32002:1035.
const styles = StyleSheet.create({
  root: { flex: 1, backgroundColor: colors.loginBackground, justifyContent: "flex-end" },
  sheet: { backgroundColor: fig.color.white, borderTopLeftRadius: fig.component.sheet.radius, borderTopRightRadius: fig.component.sheet.radius, borderWidth: 1, borderColor: fig.color.grayF1 },
  handle: { width: fig.component.sheet.handleWidth, height: fig.component.sheet.handleHeight, borderRadius: 100, backgroundColor: fig.color.grayE8, alignSelf: "center", marginTop: 12, marginBottom: 12 },
  content: { paddingHorizontal: 16 },
  title: { ...fig.text.bold24, color: fig.color.black },
  description: { ...fig.text.body13, color: fig.color.gray99, marginTop: 8, marginBottom: 20 },
  row: { minHeight: 48, flexDirection: "row", alignItems: "center", paddingLeft: 12, borderRadius: 8 },
  all: { backgroundColor: fig.color.secondary4 },
  check: { flex: 1, minHeight: 48, flexDirection: "row", alignItems: "center", gap: 8 },
  label: { flex: 1, ...fig.text.body13, color: fig.color.black }, labelAll: { fontWeight: "500" },
  note: { ...fig.text.body11, color: fig.color.deepGray, paddingVertical: 8, paddingHorizontal: 12 },
  actions: { flexDirection: "row", gap: 8, paddingHorizontal: 16, paddingVertical: 8, backgroundColor: fig.color.white96, boxShadow: fig.shadow.bar },
  button: { flex: 1, boxShadow: "0px 5px 10px 0px #1A1A1A1A" },
  agree: { backgroundColor: fig.color.black }, agreeAll: { backgroundColor: fig.color.primaryBright },
  buttonText: { ...fig.text.semibold14, color: fig.color.white },
});
