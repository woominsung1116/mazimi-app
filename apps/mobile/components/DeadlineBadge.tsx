import React from "react";
import { View, StyleSheet, type StyleProp, type ViewStyle } from "react-native";
import Text from "@/components/Text";
import { DEADLINE_TONE_COLORS, deadlineLabel, deadlineTone } from "@/lib/deadline";

type DeadlineProgram = Parameters<typeof deadlineLabel>[0];

/** D-day pill. Label and colour come from lib/deadline (one rule app-wide: red from D-2 down). */
export default function DeadlineBadge({ program, deadline, style }: { program?: DeadlineProgram; deadline?: string; style?: StyleProp<ViewStyle> }) {
  const source: DeadlineProgram = program ?? { deadline_at: deadline ?? null };
  const tone = DEADLINE_TONE_COLORS[deadlineTone(source)];
  const label = deadlineLabel(source);
  return <View style={[styles.badge, { backgroundColor: tone.background }, style]} accessibilityLabel={label === "오늘 마감" ? label : /^D-\d+$/.test(label) ? `${label.slice(2)}일 후 마감` : label}>
    <Text style={[styles.text, { color: tone.onBackground }]}>{label}</Text>
  </View>;
}

const styles = StyleSheet.create({
  badge: { alignSelf: "flex-start", borderRadius: 8, paddingHorizontal: 8, paddingVertical: 4 },
  text: { fontSize: 10, lineHeight: 12, fontWeight: "600" },
});
