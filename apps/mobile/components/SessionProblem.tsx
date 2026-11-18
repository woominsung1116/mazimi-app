import React, { useState } from "react";
import { Pressable, StyleSheet, View } from "react-native";
import { useSafeAreaInsets } from "react-native-safe-area-context";
import Text from "@/components/Text";
import { EmptyState, FigmaIcon } from "@/components/FigmaUI";
import { colors } from "@/constants/theme";
import { useAuthStore } from "@/store/auth";
import { useRouter } from "expo-router";
import { signOut } from "@/lib/session";

// 저장된 로그인은 있는데 서버에 닿지 못한 상태. 손님 화면 대신 다시 시도를 보여 준다(재로그인 불필요).
export const SESSION_ERROR_TITLE = "연결에 문제가 있어요";

/** True while a stored session could not be restored because the API was unreachable. */
export function useSessionProblem(): boolean {
  return useAuthStore(s => !s.user && s.sessionError);
}

/** Full-screen state for screens that would otherwise show the guest "로그인하기" view. */
export function SessionErrorState() {
  const loading = useAuthStore(s => s.isLoading);
  const retry = useAuthStore(s => s.restoreSession);
  const router = useRouter();
  // A way out besides waiting: end this stored login (bounded, works offline) and log in again.
  const switchAccount = async () => { await signOut(); router.replace("/login"); };
  return <View>
    <EmptyState loading={loading} title={SESSION_ERROR_TITLE} description="로그인 정보는 그대로 있어요. 연결되면 이어서 쓸 수 있어요." action={loading ? undefined : "다시 시도"} onAction={() => void retry()} />
    {!loading && <Pressable accessibilityRole="button" onPress={() => void switchAccount()} style={styles.switch} hitSlop={8}><Text style={styles.switchText}>{SWITCH_ACCOUNT_LABEL}</Text></Pressable>}
  </View>;
}

export const SWITCH_ACCOUNT_LABEL = "다른 계정으로 로그인";

/**
 * Banner for every other screen. It sits below the 56px ScreenHeader (and the
 * detail page's 44px top buttons) so it never covers a back button, and it can be closed.
 * `inline`: sits in the screen's own flow (home), so it pushes content down instead of covering it.
 */
export function SessionErrorBanner({ inline = false }: { inline?: boolean } = {}) {
  const insets = useSafeAreaInsets();
  const loading = useAuthStore(s => s.isLoading);
  const retry = useAuthStore(s => s.restoreSession);
  const [closed, setClosed] = useState(false);
  if (closed) return null;
  return <View pointerEvents="box-none" style={inline ? styles.inline : [styles.wrap, { top: insets.top + 64 }]}>
    <View style={styles.banner}>
      <Pressable accessibilityRole="button" accessibilityLabel={`${SESSION_ERROR_TITLE}. 다시 시도`} disabled={loading} onPress={() => void retry()} style={styles.retry}>
        <Text style={styles.text}>{SESSION_ERROR_TITLE} · <Text style={styles.action}>{loading ? "다시 연결 중" : "다시 시도"}</Text></Text>
      </Pressable>
      <Pressable accessibilityRole="button" accessibilityLabel="연결 안내 닫기" onPress={() => setClosed(true)} style={styles.close} hitSlop={8}>
        <FigmaIcon name="close" size={16} color={colors.onPrimary} />
      </Pressable>
    </View>
  </View>;
}

const styles = StyleSheet.create({
  switch: { alignSelf: "center", minHeight: 44, justifyContent: "center", paddingHorizontal: 12 },
  switchText: { fontSize: 13, lineHeight: 20, color: colors.textSecondary, textDecorationLine: "underline" },
  wrap: { position: "absolute", left: 16, right: 16, alignItems: "center", zIndex: 100 },
  inline: { alignItems: "center", paddingHorizontal: 16, paddingTop: 8 },
  banner: { minHeight: 44, flexDirection: "row", alignItems: "center", paddingLeft: 16, borderRadius: 22, backgroundColor: "#070428" },
  retry: { minHeight: 44, justifyContent: "center" },
  close: { minWidth: 44, minHeight: 44, alignItems: "center", justifyContent: "center" },
  text: { color: "#FFFFFF", fontSize: 13, lineHeight: 20 },
  action: { color: colors.primary, fontWeight: "700", textDecorationLine: "underline" },
});
