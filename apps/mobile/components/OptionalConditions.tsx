import React from "react";
import { Linking, Pressable, StyleSheet, View } from "react-native";
import Text from "./Text";
import { FigmaIcon, FigmaSelect } from "./FigmaUI";
import FigmaSlider from "./FigmaSlider";
import { colors } from "@/constants/theme";
import { INCOME_MEDIAN_BANDS, MEDIAN_INCOME_TABLE_URL, OPTIONAL_COPY as T, INCOME_QUESTION_ENABLED, PROFILE_CONDITION_QUESTIONS_ENABLED, type IncomeMedianBand, type OptionalAnswers } from "@/lib/optionalAnswers";

// 선택 안 함 is how a question is skipped: it saves the same null the old 건너뛰기 did.
const yesNo = (yes: string, no: string) => [{ label: "선택 안 함", value: "" }, { label: yes, value: "yes" }, { label: no, value: "no" }];
const childOptions = [{ label: "선택 안 함", value: "" }, ...[0, 1, 2, 3, 4, 5].map(n => ({ label: n === 5 ? "5명 이상" : `${n}명`, value: String(n) }))];
const bandSteps = [{ label: "선택 안 함", value: "" }, ...INCOME_MEDIAN_BANDS.map(b => ({ label: b.label, value: b.value }))];
const flag = (value: boolean | null | undefined) => value === true ? "yes" : value === false ? "no" : "";

/**
 * 무주택 / 혼인·자녀 (`household`, PROFILE_CONDITION_QUESTIONS_ENABLED) and 소득 구간
 * (`income`, INCOME_QUESTION_ENABLED): each optional. Figma 소득 34005:2822 draws the income
 * question on its slider (34005:2897) and the household ones as input pickers (34005:2937).
 * `part` splits them between the frame's 소득 구간 and 가구 정보 sections. Nothing when both are off.
 */
export default function OptionalConditions({ value, onChange, household = PROFILE_CONDITION_QUESTIONS_ENABLED, income = INCOME_QUESTION_ENABLED, part = "all" }: { value: OptionalAnswers; onChange: (next: OptionalAnswers) => void; household?: boolean; income?: boolean; part?: "all" | "income" | "household" }) {
  const showHousehold = household && part !== "income";
  const showIncome = income && part !== "household";
  if (!showHousehold && !showIncome) return null;
  const set = (patch: Partial<OptionalAnswers>) => onChange({ ...value, ...patch });
  const married = value.marital_status === "married";
  return <View style={styles.wrap}>
    <Text style={styles.note}>{showHousehold ? T.intro : T.incomeIntro}</Text>
    {showHousehold && <>
      <FigmaSelect label={T.homelessTitle} accessibilityLabel={`${T.homelessTitle} 선택`} options={yesNo(T.homelessYes, T.homelessNo)} value={flag(value.homeless_household)} onSelect={v => set({ homeless_household: v === "" ? null : v === "yes" })} />
      <Text style={styles.note}>{T.homelessHelp}</Text>
      <FigmaSelect label={T.maritalTitle} accessibilityLabel={`${T.maritalTitle} 선택`} options={[{ label: "선택 안 함", value: "" }, { label: T.unmarried, value: "unmarried" }, { label: T.married, value: "married" }]} value={value.marital_status ?? ""} onSelect={v => set(v === "" ? { marital_status: null, newlywed: null, children_count: null } : v === "married" ? { marital_status: "married" } : { marital_status: "unmarried", newlywed: null })} />
      <Text style={styles.note}>{T.maritalHelp}</Text>
      {married && <FigmaSelect label={T.newlyweds} accessibilityLabel={T.newlyweds} options={yesNo("예", "아니요")} value={flag(value.newlywed)} onSelect={v => set({ newlywed: v === "" ? null : v === "yes" })} />}
      <FigmaSelect label={T.childrenTitle} accessibilityLabel="자녀 수 선택" options={childOptions} value={value.children_count == null ? "" : String(value.children_count)} onSelect={v => set({ children_count: v === "" ? null : Number(v) })} />
    </>}
    {showIncome && <View style={{ gap: 8 }}>
      <Text style={styles.label}>{T.incomeTitle}</Text>
      <FigmaSlider accessibilityLabel="기준 중위소득 구간 선택" steps={bandSteps} value={value.income_median_band ?? ""} onChange={v => set({ income_median_band: v ? (v as IncomeMedianBand) : null })} />
      {/* Guide row 34005:2977 */}
      <View style={styles.guide}>
        <View style={{ paddingTop: 2 }}><FigmaIcon name="info" size={12} color={colors.skyblue} /></View>
        <View style={styles.guideBody}>
          <Text style={styles.note}>{T.incomeHelp}</Text>
          <Pressable accessibilityRole="link" onPress={() => void Linking.openURL(MEDIAN_INCOME_TABLE_URL).catch(() => undefined)} hitSlop={{ top: 14, bottom: 14, left: 12, right: 12 }} style={styles.link}><Text style={styles.linkText}>{T.incomeLink}</Text><FigmaIcon name="link" size={12} color={colors.skyblue} /></Pressable>
        </View>
      </View>
    </View>}
  </View>;
}

const styles = StyleSheet.create({
  wrap: { gap: 16 },
  label: { fontSize: 13, lineHeight: 20, fontWeight: "600", color: colors.textPrimary },
  note: { flexShrink: 1, fontSize: 11, lineHeight: 16, color: colors.textSecondary },
  guide: { flexDirection: "row", alignItems: "flex-start", gap: 4, paddingVertical: 8, paddingLeft: 16, paddingRight: 12, borderRadius: 17, borderWidth: 1, borderColor: colors.decorativeBlob, backgroundColor: colors.decorativeBlob },
  guideBody: { flex: 1, gap: 4 },
  link: { flexDirection: "row", alignItems: "center", gap: 4 },
  linkText: { fontSize: 11, lineHeight: 16, color: colors.skyblue },
});
