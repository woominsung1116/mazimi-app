import React from "react";
import { Image, StyleSheet, useWindowDimensions, View } from "react-native";
import Text from "./Text";
import { colors } from "@/constants/theme";

/** Figma 28015:2403 / 32002:697. System bars remain native. */
export default function BrandBackdrop({ splash = false }: { splash?: boolean }) {
  const { height } = useWindowDimensions();
  return <View pointerEvents="none" style={[StyleSheet.absoluteFill, { backgroundColor: splash ? colors.primary : colors.loginBackground }]}>
    <View style={[styles.brand, { top: height * (349 / 812) }]}>
      <Image source={require("../assets/figma/logo-white.png")} accessibilityLabel="마지미" resizeMode="contain" style={styles.logo} />
      <Text style={styles.tagline}>당신에게 꼭 맞는 혜택이 마중 나오는 순간</Text>
    </View>
  </View>;
}

const styles = StyleSheet.create({
  brand: { position: "absolute", left: 16, right: 16, alignItems: "center", gap: 16 },
  logo: { width: 163, height: 48 },
  tagline: { fontSize: 13, lineHeight: 20, color: colors.onPrimary, textAlign: "center" }, // 32002:722
});
