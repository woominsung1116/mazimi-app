import React,{useState} from "react";
import { takeFirstSignup } from "@/lib/firstSignup";
import { StyleSheet, View } from "react-native";
import { useLocalSearchParams, useRouter } from "expo-router";
import { useQueryClient } from "@tanstack/react-query";
import Text from "@/components/Text";
import OnboardingFrame,{onboardingStyle as s} from "@/components/OnboardingFrame";
import { useOnboardingStore,getBirthYear } from "@/store/onboarding";
import { useAuthStore } from "@/store/auth";
import { saveConditions, validNickname } from "@/lib/conditions";
import { validBirthDate } from "@/lib/birthDate";
import { EmploymentOptions, IncomeBracketSelect } from "@/components/ConditionFields";
import OptionalConditions from "@/components/OptionalConditions";
import { EMPTY_OPTIONAL_ANSWERS, PROFILE_CONDITION_QUESTIONS_ENABLED } from "@/lib/optionalAnswers";
import { takePendingApplication } from "@/lib/pendingApplication";
import { takeLoginReturn } from "@/lib/navigation";
import { findRegion } from "@/constants/regions";
import { toEmploymentCode } from "@/lib/profileCodes";
export default function Employment(){const router=useRouter();const {mode}=useLocalSearchParams<{mode?:string}>();const client=useQueryClient();const user=useAuthStore(s=>s.user);const local=useOnboardingStore();const [busy,setBusy]=useState(false);const [error,setError]=useState("");
// A draft persisted by an older build may still hold a Korean label.
const employmentStatus=toEmploymentCode(local.employmentStatus)??"";
const save = async () => {
  if (busy) return;
  setBusy(true); setError("");
  try {
    const birthYear = local.birthYear || getBirthYear(local.age);
    // Send the user back to the step that owns the invalid value.
    const step = !validNickname(local.nickname) ? "" : !findRegion(local.region) ? "/step1" : !validBirthDate(local.birthDate) ? "/step2" : null;
    if (step !== null) { router.replace(mode === "edit" ? `/onboarding${step}?mode=edit` : `/onboarding${step}`); return; }
    if (!await saveConditions(client, user?.id, { ...local, birthYear })) { router.replace(mode === "edit" ? "/onboarding/step1?mode=edit" : "/onboarding/step1"); return; }
    // Saved: the first-sign-up mark is used here or dropped, never left for a later edit.
    const firstSignup = takeFirstSignup();
    // 수정 모드는 온보딩 5단계를 닫고 진입했던 화면으로 돌아간다
    if (mode === "edit" && router.canDismiss()) { router.dismiss(5); return; }
    const next = (user ? await takePendingApplication() : null) ?? takeLoginReturn() ?? "/(tabs)";
    // Only a real first sign-up (login's is_new) sees 회원가입을 환영합니다 (28023:1751), once.
    // (A pending application's href object goes straight on.)
    if (firstSignup && typeof next === "string") router.replace({ pathname: "/onboarding/welcome", params: { next } }); else router.replace(next);
  } catch { setError("정보를 저장하지 못했어요. 연결을 확인하고 다시 시도해 주세요."); }
  finally { setBusy(false); }
};
// Figma 소득 34005:2822: 소득 구간 (sliders and their guide rows), then 가구 정보 (input pickers).
// Every field is optional, so the header offers 건너뛰기; it saves what is filled, like the button.
return <OnboardingFrame title="소득 및 취업정보를 알려주세요" description="입력하면 더 정확한 혜택 추천이 가능합니다." next={()=>void save()} onSkip={()=>void save()} busy={busy} label="맞춤 혜택 보기">
<View style={st.section}><Text style={s.label}>학자금 지원구간 (선택)</Text><IncomeBracketSelect value={local.incomeBracket} onChange={local.setIncomeBracket} /><Text style={s.note}>지원구간을 모르면 선택하지 않고 넘어가도 괜찮아요.</Text>
<OptionalConditions part="income" value={local.optional ?? EMPTY_OPTIONAL_ANSWERS} onChange={local.setOptional} /></View>
<View style={st.section}><Text style={s.label}>{PROFILE_CONDITION_QUESTIONS_ENABLED ? "가구 정보 (선택)" : "취업 상태 (선택)"}</Text><EmploymentOptions value={employmentStatus} onChange={local.setEmploymentStatus} />
<OptionalConditions part="household" value={local.optional ?? EMPTY_OPTIONAL_ANSWERS} onChange={local.setOptional} /></View>
{!!error&&<Text style={{color:"#070428",fontSize:13,lineHeight:20}}>{error}</Text>}</OnboardingFrame>}
// Sections 34005:2896 / 34005:2911: a Secondary 8% line under each.
const st = StyleSheet.create({ section: { gap: 8, paddingBottom: 12, borderBottomWidth: 1, borderBottomColor: "#2A8F6614" } });
