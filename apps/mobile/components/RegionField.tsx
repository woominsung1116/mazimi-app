import React, { useState } from "react";
import { Modal, Pressable, ScrollView, StyleSheet, TextInput, useWindowDimensions, View } from "react-native";
import { useSafeAreaInsets } from "react-native-safe-area-context";
import Text from "./Text";
import { fig } from "@/constants/theme";
import { FigmaIcon } from "./FigmaUI";
import { districtLevelHint, districtName, districtsOf, findRegion, hasDistrictChoice, REGIONS } from "@/constants/regions";

const normalized = (value: string) => value.replace(/\s/g, "");

/** 고른 지역을 한 줄로: "부산광역시 해운대구", "세종특별자치시", 시·군·구를 안 고르면 시·도만 */
export function regionDisplay(region: string, cityCode: string | null | undefined) {
  const sido = findRegion(region);
  if (!sido) return "";
  const district = hasDistrictChoice(sido.code) ? districtName(sido.code, cityCode) : "";
  return district ? `${sido.name} ${district.trim()}` : sido.name;
}

/**
 * 거주지역 입력 (v1.0.0 피드백 F3.1·F3.3). 칸은 최종 선택만 보여 주고, 누르면 아래에서 올라오는
 * 목록에서 시·도 → 시·군·구 순서로 고른다. 시·군·구가 하나뿐인 곳(세종)은 둘째 단계를 건너뛴다.
 * 시·도 목록의 회색 안내는 그 시·도에 실제로 있는 단위만 말한다 (서울 "구 선택", 경기 "시·군 선택").
 * 값은 서버가 읽는 region_code와 city_code다 (시·군·구를 안 고르면 "").
 */
export default function RegionField({ region, cityCode, onChange, invalid = false }: { region: string; cityCode: string; onChange: (region: string, cityCode: string) => void; invalid?: boolean }) {
  const insets = useSafeAreaInsets();
  const { height } = useWindowDimensions();
  const [open, setOpen] = useState(false);
  const [sido, setSido] = useState<string | null>(null);
  const [search, setSearch] = useState("");
  const selected = findRegion(region)?.code ?? "";
  const display = regionDisplay(region, cityCode);
  const close = () => { setOpen(false); setSido(null); setSearch(""); };
  const pickSido = (code: string) => {
    if (!hasDistrictChoice(code)) { onChange(code, ""); close(); return; }
    setSido(code); setSearch("");
  };
  const pickDistrict = (code: string) => { if (sido) onChange(sido, code); close(); };
  const step = sido ? findRegion(sido) : null;
  const key = normalized(search);
  const districts = sido ? districtsOf(sido).filter(([, name]) => normalized(name).includes(key)) : [];
  return <>
    <Pressable accessibilityRole="button" accessibilityLabel={display ? `거주지역, ${display}. 바꾸려면 누르세요` : "거주지역 선택"} onPress={() => setOpen(true)} style={({ pressed }) => [styles.field, invalid && styles.invalid, pressed && styles.pressed]}>
      <View style={{ flex: 1, gap: 4 }}>{!!display && <Text style={styles.fieldLabel}>거주지역</Text>}<Text style={[styles.fieldText, !display && styles.placeholder]} numberOfLines={1}>{display || "시·도 선택"}</Text></View>
      <FigmaIcon name="arrow" size={16} color={fig.color.grayC8} rotate={90} />
    </Pressable>
    <Modal visible={open} transparent animationType="slide" onRequestClose={() => sido ? setSido(null) : close()} statusBarTranslucent>
      <Pressable style={styles.backdrop} onPress={close} accessibilityLabel="닫기" />
      <View style={[styles.sheet, { maxHeight: height * 0.8, paddingBottom: Math.max(insets.bottom, 12) }]}>
        <View style={styles.handle} />
        <View style={styles.header}>
          {step ? <Pressable accessibilityRole="button" accessibilityLabel="시·도 다시 고르기" onPress={() => setSido(null)} hitSlop={8} style={styles.headerButton}><FigmaIcon name="back" size={20} color={fig.color.primaryBright} /></Pressable> : <View style={styles.headerButton} />}
          <Text style={styles.title}>{step ? step.name : "시·도 선택"}</Text>
          <Pressable accessibilityRole="button" accessibilityLabel="닫기" onPress={close} hitSlop={8} style={styles.headerButton}><FigmaIcon name="close" size={16} color={fig.color.grayC1} /></Pressable>
        </View>
        <Text style={styles.stepNote}>{step ? "시·군·구까지 고르면 지역 혜택을 더 정확히 찾아요. 모르면 전체로 두세요." : "주민등록상 주소를 골라 주세요."}</Text>
        {step && districtsOf(step.code).length > 12 && <TextInput value={search} onChangeText={setSearch} placeholder={`${step.label} 안에서 검색`} placeholderTextColor={fig.color.deepGray} accessibilityLabel="시·군·구 검색" style={styles.search} />}
        <ScrollView keyboardShouldPersistTaps="handled" contentContainerStyle={{ paddingBottom: 8 }}>
          {!step ? <View accessibilityRole="radiogroup" accessibilityLabel="시·도">
            {REGIONS.map(item => { const checked = selected === item.code; const hint = districtLevelHint(item.code); return <Pressable key={item.code} accessibilityRole="radio" accessibilityState={{ checked }} accessibilityLabel={hint ? `${item.name}, ${hint}` : item.name} onPress={() => pickSido(item.code)} style={({ pressed }) => [styles.row, pressed && styles.pressed]}>
              <View style={{ flex: 1, gap: 2 }}>
                <Text style={[styles.rowTitle, checked && styles.checked]}>{item.name}</Text>
                {!!hint && <Text style={styles.rowHint}>{hint}</Text>}
              </View>
              {checked && <FigmaIcon name="check" size={12} color={fig.color.primary} />}
              {!!hint && <FigmaIcon name="arrow" size={16} color={fig.color.grayE8} />}
            </Pressable>; })}
          </View> : <View accessibilityRole="radiogroup" accessibilityLabel={`${step.name} 시·군·구`}>
            <Pressable accessibilityRole="radio" accessibilityState={{ checked: selected === step.code && !districtName(step.code, cityCode) }} onPress={() => pickDistrict("")} style={({ pressed }) => [styles.row, pressed && styles.pressed]}>
              <View style={{ flex: 1, gap: 2 }}><Text style={styles.rowTitle}>{step.name} 전체</Text><Text style={styles.rowHint}>시·군·구는 고르지 않을게요</Text></View>
            </Pressable>
            {districts.map(([code, name]) => { const checked = selected === step.code && cityCode === code; return <Pressable key={code} accessibilityRole="radio" accessibilityState={{ checked }} accessibilityLabel={name.trim()} onPress={() => pickDistrict(code)} style={({ pressed }) => [styles.row, pressed && styles.pressed]}>
              <Text style={[styles.rowTitle, { flex: 1 }, checked && styles.checked]}>{name.trim()}</Text>
              {checked && <FigmaIcon name="check" size={12} color={fig.color.primary} />}
            </Pressable>; })}
            {!districts.length && <Text style={[styles.rowHint, { padding: 20 }]}>검색 결과가 없어요.</Text>}
          </View>}
        </ScrollView>
      </View>
    </Modal>
  </>;
}

// Field: Figma input 34002:3942 (label 9/12, value 12/18). Sheet: 거주지역 sheet 34002:3984
// (overlay, 20px top corners, 60px handle, 18/28 title, 16/24 options; the picked one is Secondary 8% in Primary).
const C = fig.color, K = fig.component, T = fig.text;
const styles = StyleSheet.create({
  field: { minHeight: K.input.height, borderRadius: K.input.radius, backgroundColor: C.secondary4, paddingHorizontal: K.input.paddingHorizontal, paddingVertical: K.input.paddingVertical, flexDirection: "row", alignItems: "center", gap: 8 },
  invalid: { borderWidth: 1, borderColor: C.orange, backgroundColor: C.inputError },
  fieldLabel: { ...T.label9, color: C.deepGray },
  fieldText: { ...T.body12, color: C.black },
  placeholder: { color: C.deepGray },
  pressed: { backgroundColor: C.secondary8 },
  backdrop: { ...StyleSheet.absoluteFillObject, backgroundColor: C.overlay },
  sheet: { position: "absolute", left: 0, right: 0, bottom: 0, backgroundColor: C.white, borderTopLeftRadius: K.sheet.radius, borderTopRightRadius: K.sheet.radius },
  handle: { alignSelf: "center", width: K.sheet.handleWidth, height: K.sheet.handleHeight, borderRadius: 100, backgroundColor: C.grayE1, marginTop: 12 },
  header: { height: 52, flexDirection: "row", alignItems: "center", paddingHorizontal: 8 },
  headerButton: { width: 44, height: 44, alignItems: "center", justifyContent: "center" },
  title: { flex: 1, textAlign: "center", ...T.bold18, color: C.black },
  stepNote: { paddingHorizontal: 20, paddingBottom: 8, ...T.body12, color: C.deepGray },
  search: { marginHorizontal: 16, marginBottom: 8, height: 44, paddingHorizontal: 12, borderRadius: K.input.radius, backgroundColor: C.secondary4, fontFamily: "Pretendard", ...T.body13, color: C.black },
  row: { minHeight: 48, flexDirection: "row", alignItems: "center", gap: 8, paddingHorizontal: 20, paddingVertical: 8, borderBottomWidth: 1, borderBottomColor: C.grayF7 },
  rowTitle: { ...T.body16, color: C.deepGray },
  rowHint: { ...T.body12, color: C.deepGray },
  checked: { ...T.medium16, color: C.primary },
});
