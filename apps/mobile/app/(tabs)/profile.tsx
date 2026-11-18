import React from "react";
import { Pressable, ScrollView, StyleSheet, View } from "react-native";
import { notify } from "@/lib/notify";
import NonMemberState from "@/components/NonMemberState";
import { useMemberState } from "@/lib/guest";
import { useRouter } from "expo-router";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import Text from "@/components/Text";
import { FigmaToggle, FigmaIcon, ScreenHeader, ui } from "@/components/FigmaUI";
import { fig } from "@/constants/theme";
import { api, type NotificationPreferences } from "@/lib/api";
import { signOut } from "@/lib/session";
import { useAuthStore } from "@/store/auth";
import { useVaultDocuments } from "@/lib/vault";
import LockedSection from "@/components/LockedSection";
import { enablePush } from "@/lib/notifications";
import { districtName, findRegion, hasDistrictChoice } from "@/constants/regions";
import Constants from "expo-constants";
import * as Updates from "expo-updates";

const appVersion = Constants.expoConfig?.version ?? "1.0.0";
// 팀원끼리 APK를 돌려 쓸 때 어떤 빌드와 업데이트인지 확인할 수 있게 한다
const versionDetail = () => [`버전 ${appVersion}`, Updates.updateId ? `업데이트 ${Updates.updateId.slice(0, 8)}` : "기본 번들", Updates.channel ? `채널 ${Updates.channel}` : null].filter(Boolean).join("\n");

export default function ProfileScreen() {
  const router = useRouter();
  const user = useAuthStore(s => s.user);
  // Only a guest sees the guest view; a restoring member sees loading, an unconfirmed one the retry state.
  const memberState = useMemberState();
  const client = useQueryClient();
  const profile = useQuery({ queryKey: ["profile", "me", user?.id], queryFn: api.getProfile, enabled: !!user });
  const saved = useQuery({ queryKey: ["saved", user?.id], queryFn: api.getSaved, enabled: !!user });
  const applications = useQuery({ queryKey: ["applications", user?.id], queryFn: api.getMyApplications, enabled: !!user });
  const preferences = useQuery({ queryKey: ["notification-preferences", user?.id], queryFn: api.getNotificationPreferences, enabled: !!user });
  const documents = useVaultDocuments();
  const update = useMutation({ mutationFn: api.saveNotificationPreferences, onSuccess: data => client.setQueryData(["notification-preferences", user?.id], data), onError: () => notify("설정을 저장하지 못했어요", "연결을 확인하고 다시 시도해 주세요.") });
  const p = profile.data?.profile;
  const residence = p ? [findRegion(p.region_code ?? "")?.label, hasDistrictChoice(p.region_code ?? "") && districtName(p.region_code ?? "", p.city_code)].filter(Boolean).join(" ") : "";
  const conditionCount = p ? [p.birth_year, p.region_code, p.enrollment_status, p.employment_status, p.income_bracket].filter(v => v != null && v !== "").length : 0;
  // 서버에 조건이 없으면 처음 입력하는 단계별 온보딩으로 보낸다
  const edit = () => router.push(p || profile.isLoading ? "/profile-edit" : "/onboarding?mode=edit");
  // Figma rows 32026:3609 (chevron) and 32026:3630 (버전 정보: blue value, no chevron).
  const row = (label: string, onPress: () => void, value?: string, version = false) => <Pressable key={label} accessibilityRole="button" style={({ pressed }) => [s.row, pressed && s.pressed]} onPress={onPress}><Text style={s.label}>{label}</Text>{value && <Text style={version ? s.version : ui.muted}>{value}</Text>}{!version && <FigmaIcon name="arrow" size={16} color={fig.color.grayE8} />}</Pressable>;
  return <View style={ui.screen}><ScreenHeader /><ScrollView showsVerticalScrollIndicator={false} contentContainerStyle={{ paddingBottom: 24 }}>
    {user ? <View style={s.profile}><Text style={ui.muted}>조건 <Text style={{ fontWeight: "700" }}>{conditionCount}개</Text><Text style={s.dot}>・</Text>추천 조건 {conditionCount >= 4 ? "충분" : "입력 중"}</Text><Pressable onPress={edit} style={[ui.row, { marginTop: 4, marginBottom: 16 }]} accessibilityRole="button"><Text style={s.hello}>안녕하세요, <Text style={s.name}>{p?.nickname || "마지미 회원"} 님</Text></Text><View style={s.setting}><FigmaIcon name="setting" size={10} color={fig.color.deepGray} /></View></Pressable>
      <View style={s.stats}>{[{ label: "관심 혜택", count: saved.data?.items.length, path: "/manage" }, { label: "신청 이력", count: applications.data?.total, path: "/applications" }, { label: "서류 보관함", count: documents.length, path: "/vault" }].map((item, i) => <Pressable key={item.label} onPress={() => router.push(item.path as "/manage")} style={s.stat} accessibilityRole="button"><Text style={[s.count, i === 0 && { color: fig.color.secondary }]}>{item.count ?? "—"}</Text><Text style={s.statLabel}>{item.label}</Text></Pressable>).flatMap((cell, i) => i ? [<View key={`d${i}`} style={s.statDivider} />, cell] : [cell])}</View>
    </View> : memberState !== "guest" ? <NonMemberState /> : <View style={s.profile}>
      {/* 비회원에게 임시 이름(익명 번호 등)을 붙이지 않는다. 기기에 남기는 정보도 없다 */}
      <Text style={s.greeting}>안녕하세요, 마지미예요</Text>
      <Text style={[ui.muted, { marginTop: 4, marginBottom: 16 }]}>로그인하면 관심 혜택과 신청 이력, 내 서류를 모아볼 수 있어요.</Text>
      <LockedSection message="관심 혜택 · 신청 이력 · 서류 보관함" minHeight={150}>
        <View style={[s.stats, { borderWidth: 0 }]}>{["관심 혜택", "신청 이력", "서류 보관함"].map((label, i) => <View key={label} style={[s.stat, i > 0 && { borderLeftWidth: 1, borderLeftColor: fig.color.grayF1 }]}><Text style={s.count}>—</Text><Text style={s.statLabel}>{label}</Text></View>)}</View>
      </LockedSection>
    </View>}
    <View style={s.divider} />
    {user ? <View style={s.section}><Text style={s.heading}>내 정보</Text>{row("내 정보 수정", edit, residence || undefined)}</View>
      : null}
    {/* 32026:3613 고객 센터: FAQ. 문의하기 waits for a real support channel (help: none runs today). */}
    <View style={s.section}><Text style={s.heading}>고객 센터</Text>{row("자주 묻는 질문", () => router.push("/help"))}</View>
    {/* 32026:3626 약관 및 정책: 버전 정보, the alert toggles (34065:4964, no line), 약관 */}
    <View style={s.section}><Text style={s.heading}>약관 및 정책</Text>{row("버전 정보", () => notify("마지미", versionDetail()), `v ${appVersion}`, true)}
      {user && ([{ key: "deadline", label: "마감 임박 알림" }, { key: "new_program", label: "새 혜택 알림" }, { key: "profile_update", label: "내 정보 변경 알림" }] as const).map(item => <View key={item.key} style={[s.row, s.toggleRow]}><Text style={s.label}>{item.label}</Text><FigmaToggle accessibilityLabel={item.label} value={preferences.data?.[item.key] ?? false} disabled={!preferences.data || update.isPending} onValueChange={value => { if (value) void enablePush().then(granted => { if (!granted) notify("알림 권한이 꺼져 있어요", "기기 설정에서 마지미 알림을 허용해야 푸시 알림을 받을 수 있어요."); }); if (preferences.data) update.mutate({ ...preferences.data, [item.key]: value } as NotificationPreferences); }} /></View>)}
      {row("이용약관", () => router.push("/terms"))}{row("개인정보처리방침", () => router.push("/privacy-policy"))}
    </View>
    {user && <Pressable accessibilityRole="button" style={s.logout} onPress={async () => { await signOut(); client.clear(); }}><Text style={s.logoutText}>로그아웃</Text></Pressable>}
    {/* No row in the frame: a quiet last line in the 로그아웃 style 32026:3644 */}
    {user && <Pressable accessibilityRole="button" style={s.leave} onPress={() => router.push("/account")}><Text style={s.logoutText}>회원 탈퇴</Text></Pressable>}
  </ScrollView></View>;
}
// Figma 마이페이지 32026:3562
// Figma rows 32026:3609: 20px text, 12px to the GrayF7 line, 12px to the next row. The 12px above
// the text is padding (not margin) so each row is a 44px target; the heading gives up the same 12px.
const C = fig.color, T = fig.text;
const s = StyleSheet.create({
  profile: { paddingHorizontal: 16, paddingTop: 24 },
  dot: { color: C.grayE1 }, hello: { ...T.bold18, fontWeight: "300", color: C.deepGray }, name: { fontWeight: "700", color: C.black },
  setting: { width: 18, height: 18, borderRadius: 10, padding: 4, backgroundColor: C.secondary4, borderWidth: 0.5, borderColor: C.secondary4, alignItems: "center", justifyContent: "center" }, // 32026:3572
  stats: { minHeight: 68, borderRadius: 16, borderWidth: 1, borderColor: C.secondary4, backgroundColor: C.secondary4, flexDirection: "row", alignItems: "center", boxShadow: "inset 0px 1px 1px 0px #FFFFFF33" }, // 32026:3576
  stat: { flex: 1, alignItems: "center", gap: 4, paddingVertical: 10, paddingHorizontal: 16 }, statDivider: { width: 1, height: 16, backgroundColor: C.grayF1 },
  count: { ...T.bold16, color: C.black }, statLabel: { ...T.body11, color: C.deepGray },
  divider: { height: 4, backgroundColor: C.grayF7, marginTop: 16 }, // bottom stroke of 32026:3564
  section: { paddingTop: 16 }, heading: { paddingHorizontal: 16, ...T.bold16, color: C.black, marginBottom: 4 },
  row: { minHeight: 44, flexDirection: "row", alignItems: "center", paddingHorizontal: 16, paddingVertical: 12, gap: 8, borderBottomWidth: 1, borderBottomColor: C.grayF7 },
  label: { flex: 1, ...T.body13, color: C.black }, version: { ...T.body13, color: C.blue }, pressed: { backgroundColor: C.secondary4 },
  toggleRow: { borderBottomWidth: 0 },
  greeting: { ...T.bold18, color: C.black }, logout: { minHeight: 56, justifyContent: "center", alignItems: "center" }, leave: { minHeight: 44, justifyContent: "center", alignItems: "center", marginBottom: 16 }, logoutText: { fontSize: 12, fontWeight: "500", lineHeight: 16, color: C.deepGray },
});
