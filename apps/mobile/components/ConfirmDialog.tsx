import React, { useCallback, useMemo, useState } from "react";
import { Modal, Pressable, StyleSheet, View } from "react-native";
import Text from "./Text";
import Tappable from "./Tappable";
import { FigmaIcon } from "./FigmaUI";
import { fig } from "@/constants/theme";

/** `tone`: Orange for 해제·삭제 (34068:5425), Primary for a positive action like 알림 받기 (34065:12491). */
export interface ConfirmRequest { title: string; message: string; confirm: string; onConfirm: () => void; tone?: "danger" | "primary" }

/** The .fig confirm pop-up (관심 혜택 해제 34068:5410, 서류 삭제 34068:4574, 마감일 알림 설정 34065:12476): header with close, body, 취 소 / action. */
export function useConfirmDialog(): { ask: (request: ConfirmRequest) => void; dialog: React.JSX.Element } {
  const [request, setRequest] = useState<ConfirmRequest | null>(null);
  const close = useCallback(() => setRequest(null), []);
  const dialog = useMemo(() => <ConfirmDialog request={request} onClose={close} />, [request, close]);
  return { ask: setRequest, dialog };
}

export default function ConfirmDialog({ request, onClose }: { request: ConfirmRequest | null; onClose: () => void }) {
  const confirm = () => { const run = request?.onConfirm; onClose(); run?.(); };
  return <Modal visible={request !== null} transparent animationType="fade" onRequestClose={onClose} statusBarTranslucent>
    <View style={s.backdrop}>
      <Pressable style={StyleSheet.absoluteFill} onPress={onClose} accessibilityLabel="닫기" />
      {request && <View style={s.card} accessibilityViewIsModal accessibilityRole="alert">
        <View style={s.header}>
          <View style={s.headerSide} />
          <Text style={s.title} accessibilityRole="header">{request.title}</Text>
          <Tappable accessibilityRole="button" accessibilityLabel="닫기" hitSlop={14} onPress={onClose} style={s.headerSide}><FigmaIcon name="close" size={16} color={fig.color.black} /></Tappable>
        </View>
        <View style={s.body}><Text style={s.message}>{request.message}</Text></View>
        <View style={s.buttons}>
          <Tappable accessibilityRole="button" onPress={onClose} style={[s.button, s.cancel]}><Text style={s.cancelText}>취 소</Text></Tappable>
          <Tappable accessibilityRole="button" onPress={confirm} style={[s.button, request.tone === "primary" ? s.primary : s.danger]} feedback="opacity"><Text style={s.actionText}>{request.confirm}</Text></Tappable>
        </View>
      </View>}
    </View>
  </Modal>;
}

// pop_up 34068:5410: 300 wide, radius 12; header 34068:5411 (12/20), body 34068:5417 (8/24), buttons 34068:5421 (12/16/16, gap 8, 42 high, radius 8).
const C = fig.color, T = fig.text;
const s = StyleSheet.create({
  backdrop: { flex: 1, backgroundColor: C.overlay, alignItems: "center", justifyContent: "center", padding: 24 },
  card: { width: 300, maxWidth: "100%", backgroundColor: C.white, borderRadius: 12, overflow: "hidden" },
  header: { flexDirection: "row", alignItems: "center", paddingVertical: 12, paddingHorizontal: 20, minHeight: 44 },
  headerSide: { width: 16, height: 16, alignItems: "center", justifyContent: "center" },
  title: { flex: 1, ...T.bold14, color: C.black, textAlign: "center", marginHorizontal: 10 },
  body: { paddingVertical: 8, paddingHorizontal: 24 },
  message: { ...T.body13, color: C.gray66, textAlign: "center" },
  buttons: { flexDirection: "row", gap: 8, paddingTop: 12, paddingHorizontal: 16, paddingBottom: 16 },
  button: { flex: 1, minHeight: 44, borderRadius: 8, alignItems: "center", justifyContent: "center" },
  cancel: { backgroundColor: C.secondary4, borderWidth: 1, borderColor: C.secondary8 },
  cancelText: { ...T.bold12, color: C.gray66 },
  danger: { backgroundColor: C.orange },
  primary: { backgroundColor: C.primary },
  actionText: { ...T.bold12, color: C.white },
});
