import { useState } from "react";
import { Pressable, ScrollView, TextInput, View } from "react-native";
import { notify } from "@/lib/notify";
import { useRouter } from "expo-router";
import { useQueryClient } from "@tanstack/react-query";
import Text from "@/components/Text";
import { ScreenHeader, ui } from "@/components/FigmaUI";
import { api } from "@/lib/api";
import { clearAccountVault } from "@/lib/vault";
import { useAuthStore } from "@/store/auth";
import NonMemberState from "@/components/NonMemberState";

export default function AccountScreen() {
  const { user, logout } = useAuthStore();
  const router = useRouter();
  const cache = useQueryClient();
  const [confirmation, setConfirmation] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const remove = async () => {
    if (!user || confirmation !== "탈퇴" || busy) return;
    setBusy(true); setError("");
    // Server first: an irreversible local wipe must never precede a failed deletion.
    try { await api.deleteAccount(); }
    catch { setError("계정 삭제를 완료하지 못했어요. 이 기기의 보관 서류와 계정은 그대로 유지됩니다. 연결을 확인하고 다시 시도해 주세요."); setBusy(false); return; }
    // The account is gone; local cleanup is best-effort and the session always ends.
    let vaultError = false;
    try { await clearAccountVault(user.id); } catch { vaultError = true; }
    await logout(); cache.clear(); setBusy(false);
    if (vaultError) notify("계정은 삭제됐어요", "이 기기의 보관 서류를 정리하지 못했어요. 기기 저장공간에서 직접 삭제해 주세요.");
    router.replace("/login");
  };
  return <View style={ui.screen}><ScreenHeader title="회원 탈퇴" back /><ScrollView contentContainerStyle={[ui.content, { paddingTop: 28, gap: 20 }]}>
    <Text style={ui.cardTitle}>마지미 계정을 삭제할까요?</Text>
    <Text style={[ui.cardSummary, { fontSize: 13, lineHeight: 20 }]}>프로필, 관심 혜택, 신청 기록과 이 기기의 보관 서류를 삭제해요. 삭제된 기록은 복구할 수 없어요. 외부 기관에 접수한 신청은 취소되지 않아요.</Text>
    <Text style={ui.cardSummary}>다른 기기나 외부 앱에 따로 보관한 서류는 해당 기기에서 삭제해 주세요. 복구용 백업은 기본 7일 보관 후 정기 정리돼요.</Text>
    {user ? <><Text style={ui.cardSummary}>계속하려면 아래에 ‘탈퇴’를 입력해 주세요.</Text>
      <TextInput accessibilityLabel="탈퇴 확인" value={confirmation} onChangeText={setConfirmation} editable={!busy} autoCorrect={false} placeholder="탈퇴" style={{ borderWidth: 1, borderColor: "#F1F1F1", borderRadius: 10, padding: 14, fontSize: 16, lineHeight: 24, color: "#070428" }} />
      {!!error && <Text accessibilityRole="alert" style={{ color: "#F75B2B", lineHeight: 20 }}>{error}</Text>}
      <Pressable accessibilityRole="button" disabled={confirmation !== "탈퇴" || busy} onPress={() => void remove()} style={{ backgroundColor: confirmation === "탈퇴" && !busy ? "#F75B2B" : "#E1E1E1", padding: 16, borderRadius: 10, alignItems: "center" }}><Text style={{ color: "#fff", fontWeight: "700" }}>{busy ? "삭제 중…" : "계정과 내 기록 삭제"}</Text></Pressable>
    </> : <NonMemberState title="로그인 후 계정을 삭제할 수 있어요" />}
  </ScrollView></View>;
}
