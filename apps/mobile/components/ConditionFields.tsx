import React from "react";
import { Linking, Pressable, StyleSheet, View } from "react-native";
import * as WebBrowser from "expo-web-browser";
import Text from "./Text";
import { FigmaIcon, FigmaSelect } from "./FigmaUI";
import FigmaSlider from "./FigmaSlider";
import { colors } from "@/constants/theme";
import { EMPLOYMENT_LABELS } from "@/lib/profileCodes";

// 값은 서버의 정규 코드(lib/profileCodes.ts)다. 이전 빌드가 저장한 한글 값도 코드로 읽는다
// 학적 상태와 뒤따르는 선택지는 components/EnrollmentPicker.tsx

/** 취업 상태 as the Figma input picker 34005:2937. */
export function EmploymentOptions({ value, onChange }: { value: string; onChange: (value: string) => void }) {
  return <FigmaSelect label="취업 상태" accessibilityLabel="취업 상태 선택" options={employmentOptions} value={value} onSelect={onChange} />;
}
const employmentOptions = [{ label: "선택 안 함", value: "" }, ...(["unemployed", "employed", "other"] as const).map(status => ({ label: EMPLOYMENT_LABELS[status], value: status }))];

const brackets = [{ label: "선택 안 함", value: "" }, ...Array.from({ length: 10 }, (_, i) => ({ label: `${i + 1}구간`, value: String(i + 1) }))];
/** 한국장학재단 학자금 지원구간 조회 화면 (v1.0.0 피드백 F3.2) */
export const KOSAF_BRACKET_URL = "https://www.kosaf.go.kr/ko/tuition.do?pg=tuition04_09_01&type=tuition";

/** 학자금 지원구간 on the Figma slider 34005:2897; the guide row 34005:2977 opens KOSAF. */
export function IncomeBracketSelect({ value, onChange }: { value: number | null; onChange: (value: number | null) => void }) {
  return <View style={{ gap: 8 }}>
    <FigmaSlider accessibilityLabel="학자금 지원구간 선택" steps={brackets} value={value ? String(value) : ""} onChange={v => onChange(v ? Number(v) : null)} />
    <KosafBracketLink />
  </View>;
}

/** 내 지원구간을 한국장학재단에서 확인하는 안내 줄 (Figma guide row 34005:2977) */
export function KosafBracketLink() {
  return <View style={styles.guide}>
    <FigmaIcon name="info" size={12} color={colors.skyblue} />
    <Text style={styles.kosafNote}>※ 한국장학재단 로그인 필요 (신규 산정 시 약 8주 소요)</Text>
    <Pressable accessibilityRole="link" accessibilityLabel="내 지원구간 확인하기, 한국장학재단 사이트가 열려요" onPress={() => void WebBrowser.openBrowserAsync(KOSAF_BRACKET_URL).catch(() => Linking.openURL(KOSAF_BRACKET_URL))} hitSlop={{ top: 14, bottom: 14, left: 12, right: 12 }} style={styles.kosaf}>
      <Text style={styles.kosafText}>내 지원구간 확인하기</Text><FigmaIcon name="link" size={12} color={colors.skyblue} />
    </Pressable>
  </View>;
}

const styles = StyleSheet.create({
  guide: { flexDirection: "row", flexWrap: "wrap", alignItems: "center", gap: 4, paddingVertical: 8, paddingLeft: 16, paddingRight: 12, borderRadius: 17, borderWidth: 1, borderColor: colors.decorativeBlob, backgroundColor: colors.decorativeBlob },
  kosaf: { flexDirection: "row", alignItems: "center", gap: 4 },
  kosafText: { fontSize: 11, lineHeight: 16, color: colors.skyblue },
  kosafNote: { fontSize: 11, lineHeight: 16, color: colors.textSecondary },
});
