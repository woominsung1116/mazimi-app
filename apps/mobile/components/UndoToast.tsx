import React, { useCallback, useEffect, useRef, useState } from "react";
import { StyleSheet, View, type StyleProp, type ViewStyle } from "react-native";
import Text from "./Text";
import Tappable from "./Tappable";

// 화면 아래에 잠깐 뜨는 안내와 되돌리기 (v1.0.0 피드백 F6.4: 마감 알림을 켰어요 · 실행 취소).

export type ToastMessage = { text: string; undo?: () => unknown };
export const TOAST_MS = 4000;

export function useUndoToast() {
  const [toast, setToast] = useState<(ToastMessage & { key: number }) | null>(null);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const hide = useCallback(() => { if (timer.current) clearTimeout(timer.current); setToast(null); }, []);
  const show = useCallback((message: ToastMessage) => {
    if (timer.current) clearTimeout(timer.current);
    setToast({ ...message, key: Date.now() });
    timer.current = setTimeout(() => setToast(null), TOAST_MS);
  }, []);
  useEffect(() => () => { if (timer.current) clearTimeout(timer.current); }, []);
  return { toast, show, hide };
}

export default function UndoToast({ toast, onHide, style }: { toast: (ToastMessage & { key: number }) | null; onHide: () => void; style?: StyleProp<ViewStyle> }) {
  if (!toast) return null;
  return <View key={toast.key} style={[s.toast, style]} accessibilityLiveRegion="polite" accessibilityRole="alert">
    <Text style={s.text}>{toast.text}</Text>
    {toast.undo && <Tappable accessibilityRole="button" accessibilityLabel={`${toast.text} 실행 취소`} onPress={() => { const undo = toast.undo!; onHide(); return Promise.resolve(undo()).catch(() => {}); }} style={s.undo} feedback="opacity"><Text style={s.undoText}>실행 취소</Text></Tappable>}
  </View>;
}

const s = StyleSheet.create({
  toast: { position: "absolute", left: 16, right: 16, minHeight: 52, borderRadius: 14, backgroundColor: "#191A54", flexDirection: "row", alignItems: "center", paddingLeft: 16, paddingRight: 6, shadowColor: "#070428", shadowOpacity: 0.2, shadowRadius: 12, shadowOffset: { width: 0, height: 4 }, elevation: 6 },
  text: { flex: 1, fontSize: 14, lineHeight: 20, color: "#FFFFFF", fontWeight: "600" },
  undo: { minHeight: 44, minWidth: 44, paddingHorizontal: 12, borderRadius: 10, alignItems: "center", justifyContent: "center" },
  undoText: { fontSize: 14, lineHeight: 20, fontWeight: "700", color: "#34BB84" },
});
