import React from "react";
import { Modal, Pressable, ScrollView, StyleSheet, View } from "react-native";
import { useSafeAreaInsets } from "react-native-safe-area-context";
import Text from "./Text";
import Tappable from "./Tappable";
import { displayTitle, type ApiProgram } from "@/lib/api";
import { OFFICIAL_LINK_LABEL, summarySections } from "@/lib/displayCopy";

export { OFFICIAL_LINK_LABEL };

// "혜택 요약" (v1.0.0 피드백 F5.7): 정보 · 조건 · 서류 · 신청 방법을 이미 받은 상세 데이터로만 한 장에 모은다.
// 새로 지어낸 문장 없이 공고 내용을 옮기고, 맨 아래에 공식 안내 링크를 둔다 (F6.7).

export default function BenefitSummarySheet({ program, visible, onClose, conditions, applicationText, onOfficial }: { program: ApiProgram; visible: boolean; onClose: () => void; conditions: string[]; applicationText: string; onOfficial: () => unknown }) {
  const insets = useSafeAreaInsets();
  return <Modal visible={visible} transparent animationType="slide" onRequestClose={onClose} statusBarTranslucent>
    <View style={s.backdrop}>
      <Pressable style={StyleSheet.absoluteFill} onPress={onClose} accessibilityLabel="혜택 요약 닫기" />
      <View style={[s.sheet, { paddingBottom: Math.max(insets.bottom, 16) }]} accessibilityViewIsModal>
        <View style={s.grip} />
        <View style={s.header}><Text style={s.title} numberOfLines={2}>혜택 요약</Text><Tappable accessibilityRole="button" accessibilityLabel="닫기" onPress={onClose} style={s.close}><Text style={s.closeText}>닫기</Text></Tappable></View>
        <Text style={s.programTitle} numberOfLines={2}>{displayTitle(program)}</Text>
        <ScrollView style={{ flexGrow: 0 }} contentContainerStyle={{ paddingBottom: 12 }}>
          {summarySections(program, conditions, applicationText).map(section => <View key={section.title} style={s.section}>
            <Text style={s.sectionTitle}>{section.title}</Text>
            {section.lines.map((line, index) => <View key={index} style={s.line}><View style={s.bullet} /><Text style={s.lineText}>{line}</Text></View>)}
          </View>)}
        </ScrollView>
        <Tappable accessibilityRole="link" onPress={onOfficial} style={s.official} feedback="opacity"><Text style={s.officialText}>{OFFICIAL_LINK_LABEL}</Text></Tappable>
      </View>
    </View>
  </Modal>;
}

const s = StyleSheet.create({
  backdrop: { flex: 1, backgroundColor: "#11111199", justifyContent: "flex-end" },
  sheet: { maxHeight: "86%", backgroundColor: "#fff", borderTopLeftRadius: 24, borderTopRightRadius: 24, paddingHorizontal: 20 },
  grip: { width: 48, height: 4, borderRadius: 2, backgroundColor: "#E8E8E8", alignSelf: "center", marginTop: 10, marginBottom: 8 },
  header: { flexDirection: "row", alignItems: "center", minHeight: 44 },
  title: { flex: 1, fontSize: 18, lineHeight: 28, fontWeight: "700", color: "#070428" },
  close: { minHeight: 44, minWidth: 44, paddingHorizontal: 8, alignItems: "center", justifyContent: "center", borderRadius: 10 },
  closeText: { fontSize: 13, lineHeight: 20, color: "#8894A1" },
  programTitle: { fontSize: 13, lineHeight: 20, color: "#5D5D62", marginBottom: 8 },
  section: { paddingVertical: 14, borderTopWidth: 1, borderTopColor: "#F1F1F1" },
  sectionTitle: { fontSize: 14, lineHeight: 20, fontWeight: "700", color: "#239969", marginBottom: 8 },
  line: { flexDirection: "row", gap: 8, paddingVertical: 3 },
  bullet: { width: 5, height: 5, borderRadius: 3, backgroundColor: "#2A8F6614", marginTop: 8 },
  lineText: { flex: 1, fontSize: 13, lineHeight: 20, color: "#070428" },
  official: { minHeight: 48, borderRadius: 12, backgroundColor: "#191A54", alignItems: "center", justifyContent: "center", marginTop: 8 },
  officialText: { fontSize: 14, lineHeight: 20, fontWeight: "700", color: "#fff" },
});
