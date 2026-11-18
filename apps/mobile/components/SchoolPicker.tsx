import React, { useState } from "react";
import { Modal, Pressable, ScrollView, TextInput, View } from "react-native";
import { useQuery } from "@tanstack/react-query";
import { useSafeAreaInsets } from "react-native-safe-area-context";
import Text from "./Text";
import { CheckMark, FigmaIcon, ui } from "./FigmaUI";
import { api } from "@/lib/api";
import { regionLabel } from "@/constants/regions";
import bundledUniversities from "@/constants/universities.json";

export default function SchoolPicker({ value, onChange, homeRegion = "" }: { value: string; onChange: (value: string) => void; homeRegion?: string }) {
  const [open, setOpen] = useState(false);
  const [search, setSearch] = useState("");
  const [region, setRegion] = useState("");
  const insets = useSafeAreaInsets();
  const schools = useQuery({ queryKey: ["universities"], queryFn: api.getUniversities, staleTime: 86400000, enabled: open });
  const key = search.trim().toLocaleLowerCase().replace(/\s/g, "");
  const allSchools = schools.data?.items ?? bundledUniversities;
  const regions = Array.from(new Set(allSchools.map(s => s.region)));
  const items = allSchools.filter(s => (!region || s.region === region) && [s.name, ...(s.aliases ?? [])].some(n => n.toLocaleLowerCase().replace(/\s/g, "").includes(key)));
  const select = (name: string) => { onChange(name); setOpen(false); setSearch(""); };
  // 거주지역 대학이 있으면 그 지역부터 보여준다
  const show = () => { setRegion(regions.includes(homeRegion) ? homeRegion : ""); setOpen(true); };
  return <View style={{ gap: 8 }}>
    <Pressable style={{ minHeight: 54, borderRadius: 14, backgroundColor: "#F7F9FA", paddingHorizontal: 12, flexDirection: "row", alignItems: "center" }} accessibilityRole="button" accessibilityLabel="대학 선택" onPress={show}><Text style={{ flex: 1, fontSize: 12, lineHeight: 18, color: "#070428" }}>{value || "대학 선택 안 함"}</Text><FigmaIcon name="arrow" color="#8894A1" /></Pressable>
    <Text style={ui.muted}>선택하면 우리 대학 혜택도 볼 수 있어요.</Text>
    <Modal visible={open} animationType="slide" onRequestClose={() => setOpen(false)}>
      <View style={[ui.screen, { paddingTop: insets.top, paddingBottom: insets.bottom }]}>
        <View style={ui.header}><Text style={ui.headerTitle}>대학 선택</Text><Pressable accessibilityRole="button" onPress={() => setOpen(false)} style={[ui.iconButton, { marginLeft: "auto" }]}><Text>닫기</Text></Pressable></View>
        <TextInput value={search} onChangeText={setSearch} placeholder="학교 이름으로 검색" accessibilityLabel="학교 검색" style={{ margin: 16, padding: 14, backgroundColor: "#F7F9FA", borderRadius: 12, fontFamily: "Pretendard", fontSize: 13, lineHeight: 20 }} />
        <View style={[ui.row, { paddingHorizontal: 16, paddingBottom: 12 }]}>{[["", "전체"], ...regions.map(code => [code, regionLabel(code)])].map(([id, label]) => <Pressable key={id} accessibilityRole="tab" accessibilityState={{ selected: region === id }} onPress={() => setRegion(id)} style={[ui.outlineButton, region === id && { backgroundColor: "#EFFFF8", borderColor: "#34BB84" }]}><Text style={ui.outlineButtonText}>{label}</Text></Pressable>)}</View>
        <ScrollView keyboardShouldPersistTaps="handled" contentContainerStyle={ui.content}>
          <Pressable accessibilityRole="radio" accessibilityState={{ checked: !value }} onPress={() => select("")} style={{ paddingVertical: 18 }}><View style={{ flexDirection: "row", alignItems: "center", gap: 8 }}><Text>대학 선택 안 함</Text>{!value && <CheckMark />}</View></Pressable>
          {items.map(s => <Pressable key={s.id} accessibilityRole="radio" accessibilityState={{ checked: value === s.name }} onPress={() => select(s.name)} style={{ paddingVertical: 16, borderTopWidth: 1, borderTopColor: "#F1F1F1" }}><Text style={{ fontSize: 13, lineHeight: 20, color: value === s.name ? "#239969" : "#070428" }}>{s.name}</Text>{value === s.name && <CheckMark />}<Text style={ui.muted}>{regionLabel(s.region)}</Text></Pressable>)}
          {!items.length && <Text style={[ui.muted, { paddingVertical: 24 }]}>검색 결과가 없어요. 현재 {regions.map(regionLabel).join("·")} 대학을 지원해요.</Text>}
        </ScrollView>
      </View>
    </Modal>
  </View>;
}
