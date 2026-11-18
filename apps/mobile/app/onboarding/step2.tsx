import React, { useState } from "react";
import { useLocalSearchParams, useRouter } from "expo-router";
import OnboardingFrame from "@/components/OnboardingFrame";
import BirthDatePicker from "@/components/BirthDatePicker";
import { useOnboardingStore } from "@/store/onboarding";
import { validBirthDate } from "@/lib/birthDate";

// 나이 대신 생년월일을 받는다. 나이는 해마다 바뀌고, 만 나이로 판정하는 혜택이 있다 (피드백 F2.5)
export default function BirthDate() {
  const router = useRouter();
  const { mode } = useLocalSearchParams<{ mode?: string }>();
  const { birthDate, birthYear, setBirthDate } = useOnboardingStore();
  const [showError, setShowError] = useState(false);
  const valid = validBirthDate(birthDate);
  return <OnboardingFrame title="생년월일을 알려주세요" description="만 나이로 혜택 자격을 정확히 판정해요." disabled={!valid} onBlocked={() => setShowError(true)} next={() => router.push(mode === "edit" ? "/onboarding/school?mode=edit" : "/onboarding/school")}>
    <BirthDatePicker value={birthDate} onChange={setBirthDate} fallbackYear={birthYear || null} showError={showError} />
  </OnboardingFrame>;
}
