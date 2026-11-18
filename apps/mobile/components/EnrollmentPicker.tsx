import React from "react";
import { Image, Pressable, StyleSheet, View, type ImageSourcePropType } from "react-native";
import Text from "./Text";
import { onboardingStyle as s } from "./OnboardingFrame";
import { fig } from "@/constants/theme";
import { toEnrollmentCode } from "@/lib/profileCodes";

// 학적 상태 (v1.0.0 피드백 F3.4). 상태마다 뒤따르는 선택지를 1–2개씩 둔다.
// 값은 서버의 정규 코드 그대로다: enrollment_status enrolled | on_leave | graduated | research_student | null,
// school_type university | graduate (mazimi_core::canonical_school_type). 새 코드는 없다.
// "해당 없음"은 학생이 아니라는 뜻 하나만 남겼다 (예전 "학적을 완료"는 졸업과 헷갈렸다).

export type Enrollment = { status: string; schoolType: string };
type FollowUp = { label: string; patch: Partial<Enrollment> };
type Choice = { value: string; title: string; description: string; image: ImageSourcePropType; question?: string; followUps?: FollowUp[] };

export const ENROLLMENT_CHOICES: Choice[] = [
  { value: "enrolled", title: "재학 중", description: "학교에 다니고 있어요", image: require("../assets/figma/student-enrolled.png"),
    question: "어느 과정에 다니나요? (선택)", followUps: [{ label: "대학 (전문대 포함)", patch: { schoolType: "university" } }, { label: "대학원", patch: { schoolType: "graduate" } }] },
  { value: "on_leave", title: "휴학 중", description: "잠시 학업을 쉬고 있어요", image: require("../assets/figma/student-leave.png"),
    question: "어느 과정을 휴학했나요? (선택)", followUps: [{ label: "대학 (전문대 포함)", patch: { schoolType: "university" } }, { label: "대학원", patch: { schoolType: "graduate" } }] },
  { value: "graduated", title: "졸업", description: "학교를 마쳤어요", image: require("../assets/figma/student-graduated.png"),
    question: "지금 상황을 골라 주세요", followUps: [{ label: "졸업했어요", patch: { status: "graduated" } }, { label: "수료 후 연구생이에요", patch: { status: "research_student" } }] },
  { value: "", title: "해당 없음", description: "지금은 학생이 아니에요", image: require("../assets/figma/student-none.png") },
];

const SCHOOL_TYPES = ["university", "graduate"];

/** 카드에서 보이는 상태: 수료 후 연구생은 졸업 아래의 세부 상태다 */
export function enrollmentCard(status: string) {
  const code = toEnrollmentCode(status) ?? "";
  return code === "research_student" ? "graduated" : code;
}

/** 상태 카드를 골랐을 때의 값. 과정 질문이 없는 상태에서는 과정 답을 비운다 (졸업은 마친 과정을 그대로 둔다) */
export function chooseEnrollment(card: string, current: Enrollment): Enrollment {
  if (card === "enrolled" || card === "on_leave") return { status: card, schoolType: SCHOOL_TYPES.includes(current.schoolType) ? current.schoolType : "" };
  if (card === "graduated") return { status: enrollmentCard(current.status) === "graduated" ? toEnrollmentCode(current.status) ?? "graduated" : "graduated", schoolType: current.schoolType };
  return { status: "", schoolType: "" };
}

function isPicked(followUp: FollowUp, value: Enrollment) {
  if (followUp.patch.status) return toEnrollmentCode(value.status) === followUp.patch.status;
  return value.schoolType === followUp.patch.schoolType;
}

/** 학적 상태 카드 + 뒤따르는 선택지. `compact`는 내 정보 수정 화면의 칩 모양이다 */
export default function EnrollmentPicker({ value, onChange, compact = false }: { value: Enrollment; onChange: (next: Enrollment) => void; compact?: boolean }) {
  const card = enrollmentCard(value.status);
  const active = ENROLLMENT_CHOICES.find(choice => choice.value === card);
  return <View style={{ gap: 16 }}>
    <View accessibilityRole="radiogroup" accessibilityLabel="학적 상태" style={styles.grid}>
      {ENROLLMENT_CHOICES.map(choice => { const checked = active === choice; return <Pressable key={choice.title} accessibilityRole="radio" accessibilityLabel={`${choice.title}, ${choice.description}`} accessibilityState={{ checked }} onPress={() => { if (!checked) onChange(chooseEnrollment(choice.value, value)); }} style={({ pressed }) => [compact ? [s.option, styles.chip] : styles.card, checked && (compact ? s.selected : styles.selected), pressed && !checked && styles.pressed]}>
        {!compact && <View style={styles.imageCircle}><Image source={choice.image} resizeMode="contain" style={styles.image} /></View>}
        <Text style={[compact ? styles.chipTitle : styles.title, checked && styles.selectedText]}>{choice.title}</Text>
        {!compact && <Text style={[styles.description, checked && styles.selectedText]}>{choice.description}</Text>}
      </Pressable>; })}
    </View>
    {active?.followUps && <View style={styles.followUp}>
      <Text style={s.label}>{active.question}</Text>
      <View accessibilityRole="radiogroup" accessibilityLabel={active.question} style={styles.row}>
        {active.followUps.map(followUp => { const checked = isPicked(followUp, value); return <Pressable key={followUp.label} accessibilityRole="radio" accessibilityLabel={followUp.label} accessibilityState={{ checked }} onPress={() => {
          // 과정 질문은 선택이라 다시 누르면 비운다. 졸업 세부 상태는 둘 중 하나가 늘 골라져 있다
          if (followUp.patch.schoolType) onChange({ ...value, schoolType: checked ? "" : followUp.patch.schoolType });
          else onChange({ ...value, ...followUp.patch });
        }} style={({ pressed }) => [s.option, styles.segment, checked && s.selected, pressed && !checked && styles.pressed]}>
          <Text style={[styles.segmentText, checked && styles.selectedText]}>{followUp.label}</Text>
        </Pressable>; })}
      </View>
    </View>}
  </View>;
}

// Figma 학적 34003:2694: cards 34003:2744 (picked) / 34003:2750, art circle 34003:2746.
const C = fig.color, K = fig.component, T = fig.text;
const styles = StyleSheet.create({
  grid: { flexDirection: "row", flexWrap: "wrap", gap: 12 },
  card: { width: "47%", flexGrow: 1, minHeight: 178, borderWidth: 1, borderColor: C.secondary8, borderRadius: K.choice.radius, backgroundColor: C.input2, alignItems: "center", justifyContent: "center", padding: 12, gap: 12 },
  chip: { width: "47%", flexGrow: 1, alignItems: "center" },
  selected: { borderColor: C.primary, backgroundColor: C.secondary4 },
  pressed: { backgroundColor: C.secondary8 },
  imageCircle: { width: K.choice.art, height: K.choice.art, borderRadius: 56, padding: 6, backgroundColor: C.secondary4, alignItems: "center", justifyContent: "center" },
  image: { width: K.choice.artImage, height: K.choice.artImage },
  title: { ...T.semibold13, color: C.black, textAlign: "center" },
  chipTitle: { ...T.body13, color: C.black },
  description: { ...T.body11, color: C.deepGray, textAlign: "center", marginTop: -8 },
  selectedText: { color: C.secondary },
  followUp: { gap: 12, padding: 16, borderRadius: K.choice.radius, backgroundColor: C.secondary4, borderWidth: 1, borderColor: C.secondary4 },
  row: { flexDirection: "row", gap: 8 },
  segment: { flex: 1, alignItems: "center", backgroundColor: C.white },
  segmentText: { ...T.body13, color: C.black, textAlign: "center" },
});
