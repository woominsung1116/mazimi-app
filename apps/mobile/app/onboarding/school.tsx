import React from "react";
import { View } from "react-native";
import { useLocalSearchParams, useRouter } from "expo-router";
import Text from "@/components/Text";
import OnboardingFrame, { onboardingStyle as s } from "@/components/OnboardingFrame";
import SchoolPicker from "@/components/SchoolPicker";
import EnrollmentPicker from "@/components/EnrollmentPicker";
import { useOnboardingStore } from "@/store/onboarding";
import { toEnrollmentCode } from "@/lib/profileCodes";
import { findRegion } from "@/constants/regions";

export default function School() {
  const router = useRouter();
  const { mode } = useLocalSearchParams<{ mode?: string }>();
  const { region, enrollmentStatus: stored, chooseEnrollment, schoolName, setSchoolName, schoolType } = useOnboardingStore();
  // A draft persisted by an older build may still hold a Korean label.
  const enrollmentStatus = toEnrollmentCode(stored) ?? "";
  return <OnboardingFrame title="현재 학적 상태를 선택해주세요" description="정확한 추천을 위해 선택해 주세요" next={() => router.push(mode === "edit" ? "/onboarding/step3?mode=edit" : "/onboarding/step3")}>
    <EnrollmentPicker value={{ status: enrollmentStatus, schoolType }} onChange={next => chooseEnrollment(next.status, next.schoolType)} />
    {!!enrollmentStatus && <View style={{ gap: 12 }}><Text style={s.label}>대학 (선택)</Text><SchoolPicker value={schoolName} onChange={setSchoolName} homeRegion={findRegion(region)?.code} /></View>}
  </OnboardingFrame>;
}
