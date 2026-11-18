import React, { useEffect, useState } from "react";
import { StyleSheet, View } from "react-native";
import Text from "./Text";
import { Dropdown, FigmaIcon } from "./FigmaUI";
import { onboardingStyle as s } from "./OnboardingFrame";
import { colors } from "@/constants/theme";
import { BIRTH_DATE_MESSAGES, birthDateProblem, daysInMonth, formatBirthDate, kstToday, manAge, MIN_MAN_AGE, parseBirthDate } from "@/lib/birthDate";

type Parts = { year: number | null; month: number | null; day: number | null };
const partsOf = (value: string, fallbackYear?: number | null): Parts => {
  const parsed = parseBirthDate(value);
  return parsed ?? { year: fallbackYear ?? null, month: null, day: null };
};

/**
 * 연·월·일을 하나씩 고르는 생년월일 선택. 네이티브와 웹에서 같은 드롭다운을 쓴다 (네이티브 모듈 없음).
 * `value`는 "YYYY-MM-DD" 또는 "" (덜 골랐을 때). `fallbackYear`는 예전에 저장한 출생연도로, 연도만 미리 골라 둔다.
 */
export default function BirthDatePicker({ value, onChange, fallbackYear, showError = false }: { value: string; onChange: (value: string) => void; fallbackYear?: number | null; showError?: boolean }) {
  const today = kstToday();
  const [parts, setParts] = useState<Parts>(() => partsOf(value, fallbackYear));
  // 밖에서 값이 바뀌면 (저장된 프로필을 불러오면) 따라간다
  useEffect(() => { if (parseBirthDate(value)) setParts(partsOf(value)); }, [value]);
  const update = (patch: Partial<Parts>) => {
    const next = { ...parts, ...patch };
    // 2월 30일처럼 없는 날이 되면 그 달의 마지막 날로 맞춘다
    if (next.year && next.month && next.day) next.day = Math.min(next.day, daysInMonth(next.year, next.month));
    setParts(next);
    onChange(next.year && next.month && next.day ? formatBirthDate({ year: next.year, month: next.month, day: next.day }) : "");
  };
  const latestYear = today.year - MIN_MAN_AGE;
  const years = Array.from({ length: latestYear - 1920 + 1 }, (_, i) => latestYear - i).map(y => ({ label: `${y}년`, value: String(y) }));
  const months = Array.from({ length: 12 }, (_, i) => ({ label: `${i + 1}월`, value: String(i + 1) }));
  const dayCount = parts.year && parts.month ? daysInMonth(parts.year, parts.month) : 31;
  const days = Array.from({ length: dayCount }, (_, i) => ({ label: `${i + 1}일`, value: String(i + 1) }));
  const chosen = parts.year && parts.month && parts.day ? formatBirthDate({ year: parts.year, month: parts.month, day: parts.day }) : "";
  const problem = chosen ? birthDateProblem(chosen, today) : showError ? "invalid" : null;
  const birth = parseBirthDate(chosen);
  const field = (label: string, options: { label: string; value: string }[], current: number | null, key: keyof Parts, unit: string) =>
    <Dropdown accessibilityLabel={`생년월일 ${label} 선택`} options={options} value={current ? String(current) : ""} onSelect={v => update({ [key]: Number(v) })} style={[s.input, styles.select, !!problem && !current && styles.invalid]}>
      <Text style={[styles.value, !current && styles.placeholder]}>{current ? `${current}${unit}` : label}</Text>
      <View style={{ transform: [{ rotate: "90deg" }] }}><FigmaIcon name="arrow" color="#8894A1" /></View>
    </Dropdown>;
  return <View style={{ gap: 8 }}>
    <View style={styles.row} accessibilityLabel="생년월일">
      <View style={{ flex: 1.4 }}>{field("연도", years, parts.year, "year", "년")}</View>
      <View style={{ flex: 1 }}>{field("월", months, parts.month, "month", "월")}</View>
      <View style={{ flex: 1 }}>{field("일", days, parts.day, "day", "일")}</View>
    </View>
    {problem ? <Text accessibilityRole="alert" style={styles.error}>{BIRTH_DATE_MESSAGES[problem]}</Text>
      : birth ? <Text style={styles.age}>만 {manAge(birth, today)}세</Text> : null}
  </View>;
}

const styles = StyleSheet.create({
  row: { flexDirection: "row", gap: 8 },
  select: { flexDirection: "row", alignItems: "center", gap: 4 },
  value: { flex: 1, fontSize: 13, lineHeight: 20, color: colors.textPrimary },
  placeholder: { color: "#8894A1" },
  invalid: { borderWidth: 1, borderColor: "#F75B2B" },
  error: { fontSize: 12, lineHeight: 18, color: "#F75B2B" },
  age: { fontSize: 12, lineHeight: 18, color: colors.onPrimaryFixedVariant, fontWeight: "600" },
});
