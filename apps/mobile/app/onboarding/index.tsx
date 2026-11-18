import React, { useEffect, useRef, useState } from "react";
import { Pressable, TextInput } from "react-native";
import { useLocalSearchParams, useRouter } from "expo-router";
import { useQuery } from "@tanstack/react-query";
import Text from "@/components/Text";
import OnboardingFrame,{onboardingStyle as s} from "@/components/OnboardingFrame";
import { useOnboardingStore } from "@/store/onboarding";
import { useAuthStore } from "@/store/auth";
import { api, ApiRequestError } from "@/lib/api";
import { NICKNAME_MAX, validNickname } from "@/lib/conditions";
import { savedOptionalAnswers } from "@/lib/optionalAnswers";

export default function Onboarding() {
  const router = useRouter();
  const { mode } = useLocalSearchParams<{ mode?: string }>();
  const user = useAuthStore(s => s.user);
  const { nickname, setNickname } = useOnboardingStore();
  const initializedFor = useRef<string | null>(null);
  const profile = useQuery({
    queryKey: ["profile", "me", user?.id], queryFn: api.getProfile, enabled: !!user,
    retry: (count, error) => !(error instanceof ApiRequestError && error.status === 404) && count < 2,
  });
  useEffect(() => {
    const p = profile.data?.profile;
    if (!user || !p || initializedFor.current === user.id) return;
    initializedFor.current = user.id;
    useOnboardingStore.setState({
      nickname: p.nickname ?? "", region: p.region_code ?? "", cityCode: p.city_code ?? "",
      birthYear: p.birth_year ?? 0, birthDate: p.birth_date ?? "",
      age: p.birth_year ? String(new Date().getFullYear() - p.birth_year) : "",
      enrollmentStatus: p.enrollment_status ?? "", schoolName: p.school_name ?? "", schoolType: p.school_type ?? "",
      employmentStatus: p.employment_status ?? "", incomeBracket: p.income_bracket,
      optional: savedOptionalAnswers(p),
    });
  }, [profile.data, user]);
  const failed = profile.isError && !(profile.error instanceof ApiRequestError && profile.error.status === 404);
  // The step waits until the session restore (/auth/me) and the saved profile have both settled:
  // before that a typed nickname could be overwritten by the profile, or a 다음 tap could land
  // on the button just as it turns busy and do nothing. Busy uses the frame's own spinner.
  const restoring = useAuthStore(s => s.isLoading);
  const loading = restoring || (!!user && profile.isLoading);
  // 닉네임은 필수다. 비어 있으면 다음 단계로 넘어가지 않고 이유를 바로 아래에 보여 준다
  const [showError, setShowError] = useState(false);
  const missing = !validNickname(nickname);
  return <OnboardingFrame title="어떻게 불러드릴까요?" description="마지미에서 사용할 닉네임을 알려주세요." busy={loading} disabled={failed || missing} onBlocked={failed ? undefined : () => setShowError(true)} next={() => router.push(mode === "edit" ? "/onboarding/step1?mode=edit" : "/onboarding/step1")}>
    <TextInput value={nickname} onChangeText={value => { setNickname(value); if (validNickname(value)) setShowError(false); }} onBlur={() => setShowError(missing)} editable={!loading && !failed} placeholder="닉네임을 입력해 주세요" placeholderTextColor="#8894A1" accessibilityLabel="닉네임 (필수)" aria-invalid={showError && missing} maxLength={NICKNAME_MAX} style={[s.input, showError && missing && { borderWidth: 1, borderColor: "#F75B2B" }]} />
    {showError && missing && <Text accessibilityRole="alert" style={{ marginTop: -20, fontSize: 12, lineHeight: 18, color: "#F75B2B" }}>닉네임을 입력해 주세요. (1–{NICKNAME_MAX}자)</Text>}
    {failed && <Pressable accessibilityRole="button" onPress={() => void profile.refetch()}><Text style={s.note}>저장된 정보를 불러오지 못했어요. 눌러서 다시 시도해 주세요.</Text></Pressable>}
  </OnboardingFrame>;
}
