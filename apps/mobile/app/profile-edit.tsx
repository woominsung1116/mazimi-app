import React, { useEffect, useRef, useState } from "react";
import { KeyboardAvoidingView, Platform, Pressable, ScrollView, StyleSheet, TextInput, View } from "react-native";
import { useRouter } from "expo-router";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useSafeAreaInsets } from "react-native-safe-area-context";
import Text from "@/components/Text";
import { EmptyState, ScreenHeader, ui } from "@/components/FigmaUI";
import { onboardingStyle as s } from "@/components/OnboardingFrame";
import RegionField from "@/components/RegionField";
import SchoolPicker from "@/components/SchoolPicker";
import { EmploymentOptions, IncomeBracketSelect } from "@/components/ConditionFields";
import EnrollmentPicker from "@/components/EnrollmentPicker";
import { api, ApiRequestError } from "@/lib/api";
import { NICKNAME_MAX, saveConditions, validNickname, type ConditionDraft } from "@/lib/conditions";
import { BIRTH_DATE_HINTS, birthDateSave } from "@/lib/birthDate";
import BirthDatePicker from "@/components/BirthDatePicker";
import { pickEnrollment, toEmploymentCode, toEnrollmentCode } from "@/lib/profileCodes";
import { goBack } from "@/lib/navigation";
import { findRegion } from "@/constants/regions";
import OptionalConditions from "@/components/OptionalConditions";
import { savedOptionalAnswers } from "@/lib/optionalAnswers";
import { colors } from "@/constants/theme";
import { getBirthYear, useOnboardingStore } from "@/store/onboarding";
import { useAuthStore } from "@/store/auth";
import { useIsGuest } from "@/lib/guest";
import { SessionErrorState, useSessionProblem } from "@/components/SessionProblem";

/** 저장된 조건을 한 화면에서 바로 고친다. 처음 입력하는 사람은 단계별 온보딩을 쓴다 */
export default function ProfileEdit() {
  const router = useRouter();
  const insets = useSafeAreaInsets();
  const client = useQueryClient();
  const user = useAuthStore(st => st.user);
  const restoring = useAuthStore(st => st.isLoading);
  const profile = useQuery({
    queryKey: ["profile", "me", user?.id], queryFn: api.getProfile, enabled: !!user,
    retry: (count, error) => !(error instanceof ApiRequestError && error.status === 404) && count < 2,
  });
  const notFound = profile.error instanceof ApiRequestError && profile.error.status === 404;
  // Changed only through functional updaters so updates in one batch compose.
  const [draft, setDraft] = useState<ConditionDraft | null>(null);
  const [birthDate, setBirthDate] = useState("");
  const [birthDateTouched, setBirthDateTouched] = useState(false);
  const [schoolType, setSchoolType] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const initialized = useRef(false);
  // 조건은 회원만 저장한다. 비회원이 링크로 들어오면 로그인 화면으로 보낸다.
  // 저장된 로그인이 있는데 서버에 닿지 못한 회원(sessionProblem)은 비회원이 아니다
  const guest = useIsGuest();
  const sessionProblem = useSessionProblem();
  useEffect(() => { if (guest) router.replace("/login"); }, [guest, router]);
  useEffect(() => {
    // 세션 복원 중에는 회원인지 알 수 없어 기기 값으로 채우지 않는다
    if (initialized.current || restoring || !user) return;
    if (!profile.data && !notFound) return;
    initialized.current = true;
    const p = profile.data?.profile;
    const local = useOnboardingStore.getState();
    const next: ConditionDraft = p
      ? { nickname: p?.nickname ?? "", region: p?.region_code ?? "", cityCode: p?.city_code ?? "", birthYear: p?.birth_year ?? 0, enrollmentStatus: toEnrollmentCode(p?.enrollment_status) ?? "", schoolName: p?.school_name ?? "", employmentStatus: toEmploymentCode(p?.employment_status) ?? "", incomeBracket: p?.income_bracket ?? null, optional: savedOptionalAnswers(p), replaceOptional: true }
      : { nickname: local.nickname, region: local.region, cityCode: local.cityCode, birthYear: local.birthYear || getBirthYear(local.age), enrollmentStatus: toEnrollmentCode(local.enrollmentStatus) ?? "", schoolName: local.schoolName, employmentStatus: toEmploymentCode(local.employmentStatus) ?? "", incomeBracket: local.incomeBracket, optional: savedOptionalAnswers(local.optional), replaceOptional: true };
    setDraft(next);
    // 출생연도만 저장된 회원은 연도를 미리 골라 둔 채 월·일을 고르게 한다
    setBirthDate(p?.birth_date ?? "");
    setSchoolType(p?.school_type ?? "");
  }, [user, restoring, profile.data, notFound]);

  if (guest) return <View style={ui.screen} />;
  if (sessionProblem) return <View style={ui.screen}><ScreenHeader title="내 정보 수정" back /><SessionErrorState /></View>;
  if (!draft) return <View style={ui.screen}><ScreenHeader title="내 정보 수정" back /><EmptyState loading={!profile.isError} title={profile.isError ? "저장된 정보를 불러오지 못했어요" : "저장된 정보를 불러오고 있어요"} action={profile.isError ? "다시 시도" : undefined} onAction={() => void profile.refetch()} /></View>;

  const set = (patch: Partial<ConditionDraft>) => setDraft(current => current && { ...current, ...patch });
  // 출생연도만 저장된 회원도 저장을 막지 않는다. 생년월일을 건드렸을 때만 끝까지 고르게 한다 (M5)
  const dob = birthDateSave({ birthDate, storedYear: draft.birthYear, touched: birthDateTouched });
  // 닉네임은 필수다 (비우면 서버가 기존 닉네임을 그대로 두기도 한다)
  const nicknameMissing = !validNickname(draft.nickname);
  const canSave = dob.ok && !!findRegion(draft.region) && !nicknameMissing && !busy;
  const save = async () => {
    if (!canSave) return;
    setBusy(true); setError("");
    try { if (await saveConditions(client, user?.id, { ...draft, birthYear: dob.birthYear, birthDate: dob.birthDate, schoolType })) goBack(router); }
    catch { setError("정보를 저장하지 못했어요. 연결을 확인하고 다시 시도해 주세요."); }
    finally { setBusy(false); }
  };
  return <KeyboardAvoidingView behavior={Platform.OS === "ios" ? "padding" : undefined} style={ui.screen}>
    <ScreenHeader title="내 정보 수정" back />
    <ScrollView keyboardShouldPersistTaps="handled" showsVerticalScrollIndicator={false} contentContainerStyle={styles.content}>
      <View style={styles.field}><Text style={s.label}>닉네임</Text><TextInput value={draft.nickname} onChangeText={nickname => set({ nickname })} placeholder="닉네임을 입력해 주세요" placeholderTextColor="#8894A1" accessibilityLabel="닉네임 (필수)" maxLength={NICKNAME_MAX} style={[s.input, nicknameMissing && { borderWidth: 1, borderColor: "#F75B2B" }]} />
        {nicknameMissing && <Text accessibilityRole="alert" style={styles.error}>닉네임을 입력해 주세요. (1–{NICKNAME_MAX}자)</Text>}</View>
      <View style={styles.field}><Text style={s.label}>거주지역</Text><RegionField region={draft.region} cityCode={draft.cityCode} onChange={(region, cityCode) => set({ region, cityCode })} /></View>
      <View style={styles.field}><Text style={s.label}>생년월일</Text><BirthDatePicker value={birthDate} onChange={value => { setBirthDate(value); setBirthDateTouched(true); }} fallbackYear={draft.birthYear || null} showError={dob.showError} />
        {dob.hint && <Text style={styles.hint}>{BIRTH_DATE_HINTS[dob.hint]}</Text>}</View>
      <View style={styles.field}><Text style={s.label}>학적 상태</Text>
        <EnrollmentPicker compact value={{ status: draft.enrollmentStatus, schoolType }} onChange={next => { setSchoolType(next.schoolType); setDraft(current => current && pickEnrollment(current, next.status)); }} />
      </View>
      {!!draft.enrollmentStatus && <View style={styles.field}><Text style={s.label}>대학 (선택)</Text><SchoolPicker value={draft.schoolName} onChange={schoolName => set({ schoolName })} homeRegion={findRegion(draft.region)?.code} /></View>}
      <View style={styles.field}><Text style={s.label}>취업 상태 (선택)</Text><EmploymentOptions value={draft.employmentStatus} onChange={employmentStatus => set({ employmentStatus })} /></View>
      <View style={styles.field}><Text style={s.label}>학자금 지원구간 (선택)</Text><IncomeBracketSelect value={draft.incomeBracket} onChange={incomeBracket => set({ incomeBracket })} /></View>
      <OptionalConditions value={draft.optional ?? savedOptionalAnswers(null)} onChange={optional => set({ optional })} />
      {!!error && <Text style={styles.error}>{error}</Text>}
    </ScrollView>
    <View style={[styles.footer, { paddingBottom: insets.bottom + 8 }]}>
      <Pressable accessibilityRole="button" accessibilityState={{ disabled: !canSave }} disabled={!canSave} onPress={() => void save()} style={[styles.button, !canSave && { backgroundColor: "#8894A1" }]}><Text style={ui.primaryButtonText}>{busy ? "저장 중…" : "저장"}</Text></Pressable>
    </View>
  </KeyboardAvoidingView>;
}
const styles = StyleSheet.create({
  content: { paddingHorizontal: 16, paddingTop: 20, paddingBottom: 32, gap: 28 },
  field: { gap: 10 },
  error: { color: "#F75B2B", fontSize: 12, lineHeight: 18 },
  hint: { color: "#8894A1", fontSize: 12, lineHeight: 18 },
  footer: { paddingHorizontal: 15, paddingTop: 8, borderTopWidth: 1, borderTopColor: "#F1F1F1" },
  button: { height: 48, borderRadius: 12, backgroundColor: colors.primary, alignItems: "center", justifyContent: "center" },
});
