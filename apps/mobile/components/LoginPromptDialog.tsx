import React, { useCallback, useMemo, useState } from "react";
import { Modal, Pressable, StyleSheet, View } from "react-native";
import { FigmaIcon } from "./FigmaUI";
import { useRouter } from "expo-router";
import Text from "./Text";
import Tappable from "./Tappable";
import { colors } from "@/constants/theme";
import { LOGIN_RETURN_PATH } from "@/lib/guest";

// 비회원이 찜·알림처럼 회원 기능을 누르면 가입·로그인을 권하는 창 (v1.0.0 피드백 F7.3).
// 가입과 로그인은 같은 소셜 로그인 한 번이라 버튼도 하나다.

export const LOGIN_PROMPT_TITLE = "회원만 쓸 수 있는 기능이에요";
export const LOGIN_PROMPT_CTA = "가입·로그인하고 계속하기";

export function useLoginPrompt(): { prompt: (reason: string) => void; dialog: React.JSX.Element } {
  const router = useRouter();
  const [reason, setReason] = useState<string | null>(null);
  const close = useCallback(() => setReason(null), []);
  const prompt = useCallback((next: string) => setReason(next), []);
  const login = useCallback(() => { setReason(null); router.push(LOGIN_RETURN_PATH); }, [router]);
  const dialog = useMemo(() => <LoginPromptDialog reason={reason} onClose={close} onLogin={login} />, [reason, close, login]);
  return { prompt, dialog };
}

export default function LoginPromptDialog({ reason, onClose, onLogin }: { reason: string | null; onClose: () => void; onLogin: () => void }) {
  return <Modal visible={reason !== null} transparent animationType="fade" onRequestClose={onClose} statusBarTranslucent>
    <View style={s.backdrop}>
      <Pressable style={StyleSheet.absoluteFill} onPress={onClose} accessibilityLabel="닫기" />
      <View style={s.card} accessibilityViewIsModal accessibilityRole="alert">
        <View style={s.icon}><FigmaIcon name="eyeOff" size={20} color={colors.primary} /></View>
        <Text style={s.title}>{LOGIN_PROMPT_TITLE}</Text>
        {!!reason && <Text style={s.body}>{reason}</Text>}
        <Text style={s.perks}>관심 혜택 모아보기 · 마감 알림 · 내 조건 맞춤 추천</Text>
        <Tappable accessibilityRole="button" onPress={onLogin} style={s.primary} feedback="opacity"><Text style={s.primaryText}>{LOGIN_PROMPT_CTA}</Text></Tappable>
        <Tappable accessibilityRole="button" onPress={onClose} style={s.secondary}><Text style={s.secondaryText}>나중에 할게요</Text></Tappable>
      </View>
    </View>
  </Modal>;
}

const s = StyleSheet.create({
  backdrop: { flex: 1, backgroundColor: "#11111199", alignItems: "center", justifyContent: "center", padding: 24 },
  card: { width: "100%", maxWidth: 360, backgroundColor: "#fff", borderRadius: 20, padding: 24, alignItems: "center" },
  icon: { width: 48, height: 48, borderRadius: 24, backgroundColor: colors.primaryFixed, borderWidth: 1, borderColor: "#2A8F6614", alignItems: "center", justifyContent: "center", marginBottom: 14 },
  title: { fontSize: 16, lineHeight: 24, fontWeight: "700", color: "#070428", textAlign: "center" },
  body: { fontSize: 13, lineHeight: 20, color: "#5D5D62", textAlign: "center", marginTop: 8 },
  perks: { fontSize: 12, lineHeight: 18, color: "#239969", textAlign: "center", marginTop: 12 },
  primary: { alignSelf: "stretch", minHeight: 48, borderRadius: 12, backgroundColor: "#191A54", alignItems: "center", justifyContent: "center", marginTop: 20 },
  primaryText: { fontSize: 14, lineHeight: 20, fontWeight: "700", color: "#fff" },
  secondary: { alignSelf: "stretch", minHeight: 44, borderRadius: 12, alignItems: "center", justifyContent: "center", marginTop: 6 },
  secondaryText: { fontSize: 13, lineHeight: 20, color: "#8894A1" },
});
