import React from "react";
import { Platform, Pressable, StyleSheet, View } from "react-native";
import Text from "./Text";
import { fig } from "@/constants/theme";

export type SliderStep = { label: string; value: string };

/**
 * Figma slider 34005:2897: a 4px Secondary 8% track, the chosen part in Primary, a 20px white
 * knob, the first step's label on the left and the chosen one on the right (13/20).
 * Each step is a tap target. Native screen readers adjust it one step at a time; react-native-web has no
 * accessibility actions, so on web each step is a named radio instead.
 */
const web = Platform.OS === "web";
export default function FigmaSlider({ steps, value, onChange, accessibilityLabel }: { steps: SliderStep[]; value: string; onChange: (value: string) => void; accessibilityLabel: string }) {
  const index = Math.max(0, steps.findIndex(step => step.value === value));
  const share = steps.length > 1 ? index / (steps.length - 1) : 0;
  const move = (delta: number) => { const next = steps[Math.min(steps.length - 1, Math.max(0, index + delta))]; if (next) onChange(next.value); };
  return <View>
    <View {...(web ? {} : { accessibilityRole: "adjustable" as const, accessibilityLabel, accessibilityValue: { text: steps[index]?.label }, accessibilityActions: [{ name: "increment" }, { name: "decrement" }], onAccessibilityAction: (e: { nativeEvent: { actionName: string } }) => move(e.nativeEvent.actionName === "increment" ? 1 : -1) })} style={styles.frame}>
      <View style={styles.track}>
        <View style={styles.base} />
        <View style={[styles.fill, { width: `${share * 100}%` }]} />
        <View pointerEvents="none" style={[styles.knob, { left: `${share * 100}%` }]} />
        <View style={styles.zones} accessibilityRole={web ? "radiogroup" : undefined} accessibilityLabel={web ? accessibilityLabel : undefined}>{steps.map(step => <Pressable key={step.value || "none"} {...(web ? { accessibilityRole: "radio" as const, accessibilityLabel: step.label, accessibilityState: { checked: step.value === steps[index]?.value } } : { accessibilityElementsHidden: true, importantForAccessibility: "no" as const })} onPress={() => onChange(step.value)} style={styles.zone} />)}</View>
      </View>
    </View>
    <View style={styles.labels}>
      <Text style={styles.min}>{steps[0]?.label}</Text>
      <Text style={styles.current}>{steps[index]?.label}</Text>
    </View>
  </View>;
}

const styles = StyleSheet.create({
  frame: { paddingVertical: 8, paddingHorizontal: 6 },
  track: { height: 20, justifyContent: "center" },
  base: { position: "absolute", left: 0, right: 0, height: 4, borderRadius: 3, backgroundColor: fig.color.secondary8 },
  fill: { position: "absolute", left: 0, height: 4, borderRadius: 3, backgroundColor: fig.color.primary },
  knob: { position: "absolute", width: 20, height: 20, marginLeft: -10, borderRadius: 10, backgroundColor: fig.color.white, borderWidth: 1, borderColor: fig.color.secondary8, boxShadow: "0px 4px 4px 0px #0845691F" },
  zones: { ...StyleSheet.absoluteFillObject, flexDirection: "row", marginVertical: -12 },
  zone: { flex: 1 },
  labels: { flexDirection: "row", justifyContent: "space-between", paddingHorizontal: 6, gap: 8 },
  min: { ...fig.text.semibold13, color: fig.color.deepGray }, current: { ...fig.text.semibold13, color: fig.color.black, textAlign: "right" },
});
