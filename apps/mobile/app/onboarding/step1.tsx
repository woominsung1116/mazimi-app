import React from "react";
import { useLocalSearchParams, useRouter } from "expo-router";
import OnboardingFrame, { OnboardingNote } from "@/components/OnboardingFrame";
import { useOnboardingStore } from "@/store/onboarding";
import RegionField from "@/components/RegionField";
import { findRegion } from "@/constants/regions";
export default function Region() {
  const router = useRouter();
  const { mode } = useLocalSearchParams<{ mode?: string }>();
  const { region, setRegion, cityCode, setCityCode } = useOnboardingStore();
  return <OnboardingFrame title="거주지역을 알려주세요" description="주민등록상 거주지를 시·도부터 골라 주세요." disabled={!findRegion(region)} next={() => router.push(mode === "edit" ? "/onboarding/step2?mode=edit" : "/onboarding/step2")}>
    <RegionField region={region} cityCode={cityCode} onChange={(next, city) => { setRegion(next); setCityCode(city); }} />
    <OnboardingNote>지역별로 혜택이 다르기 때문에 정확한 추천을 위해 필요해요.</OnboardingNote>
  </OnboardingFrame>;
}
