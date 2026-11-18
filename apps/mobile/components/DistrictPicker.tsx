import React, { useState } from "react";
import { Modal, Pressable, ScrollView, TextInput, View } from "react-native";
import { useSafeAreaInsets } from "react-native-safe-area-context";
import Text from "./Text";
import { CheckMark, FigmaIcon, ui } from "./FigmaUI";
import { districtName, districtsOf, regionLabel } from "@/constants/regions";

const normalized = (value: string) => value.replace(/\s/g, "");

/** 선택한 시·도의 시·군·구를 고른다. 값은 서버가 읽는 city_code다 */
export default function DistrictPicker({ region, value, onChange }: { region: string; value: string; onChange: (code: string) => void }) {
  const [open, setOpen] = useState(false);
  const [search, setSearch] = useState("");
  const insets = useSafeAreaInsets();
  const options = districtsOf(region);
  // 세종 has a single 시·군·구; it is sent without asking (constants/regions cityCodeFor).
  if (options.length <= 1) return null;
  const selected = districtName(region, value);
  const key = normalized(search);
  const items = options.filter(([, name]) => normalized(name).includes(key));
  const select = (code: string) => { onChange(code); setOpen(false); setSearch(""); };
  return <View style={{ gap: 8 }}>
    <Pressable style={{ minHeight: 54, borderRadius: 14, backgroundColor: "#F7F9FA", paddingHorizontal: 12, flexDirection: "row", alignItems: "center" }} accessibilityRole="button" accessibilityLabel="시·군·구 선택" onPress={() => setOpen(true)}><Text style={{ flex: 1, fontSize: 12, lineHeight: 18, color: selected ? "#070428" : "#8894A1" }}>{selected || "시·군·구 선택 안 함"}</Text><FigmaIcon name="arrow" color="#8894A1" /></Pressable>
    <Text style={ui.muted}>선택하면 시·군·구 단위 혜택도 확인할 수 있어요.</Text>
    <Modal visible={open} animationType="slide" onRequestClose={() => setOpen(false)}>
      <View style={[ui.screen, { paddingTop: insets.top, paddingBottom: insets.bottom }]}>
        <View style={ui.header}><Text style={ui.headerTitle}>{regionLabel(region)} 시·군·구 선택</Text><Pressable accessibilityRole="button" onPress={() => setOpen(false)} style={[ui.iconButton, { marginLeft: "auto" }]}><Text>닫기</Text></Pressable></View>
        <TextInput value={search} onChangeText={setSearch} placeholder="시·군·구 이름으로 검색" accessibilityLabel="시·군·구 검색" style={{ margin: 16, padding: 14, backgroundColor: "#F7F9FA", borderRadius: 12, fontFamily: "Pretendard", fontSize: 13, lineHeight: 20 }} />
        <ScrollView keyboardShouldPersistTaps="handled" contentContainerStyle={ui.content}>
          <View accessibilityRole="radiogroup" accessibilityLabel="시·군·구">
          <Pressable accessibilityRole="radio" accessibilityState={{ checked: !selected }} onPress={() => select("")} style={{ paddingVertical: 18 }}><View style={{ flexDirection: "row", alignItems: "center", gap: 8 }}><Text>선택 안 함</Text>{!selected && <CheckMark />}</View></Pressable>
          {items.map(([code, name]) => <Pressable key={code} accessibilityRole="radio" accessibilityState={{ checked: value === code }} onPress={() => select(code)} style={{ paddingVertical: 16, borderTopWidth: 1, borderTopColor: "#F1F1F1" }}><Text style={{ fontSize: 13, lineHeight: 20, color: value === code ? "#239969" : "#070428" }}>{name}</Text>{value === code && <CheckMark />}</Pressable>)}</View>
          {!items.length && <Text style={[ui.muted, { paddingVertical: 24 }]}>검색 결과가 없어요.</Text>}
        </ScrollView>
      </View>
    </Modal>
  </View>;
}
