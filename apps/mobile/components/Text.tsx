import React, { createContext, useContext } from "react";
import { Text as NativeText, StyleSheet, type TextProps } from "react-native";
import { fig } from "@/constants/theme";

// Nested text inherits size and colour from its parent, so only the outermost Text sets the defaults.
const Nested = createContext(false);

/** Figma uses Pretendard throughout, including Korean labels. Unstyled text is the Figma body 34065:2950 (13/20, Black). */
export default function Text({ style, children, ...props }: TextProps) {
  const nested = useContext(Nested);
  const flat = StyleSheet.flatten(style);
  const weight = Number(flat?.fontWeight ?? 400);
  const fontFamily = FAMILY[Math.min(700, Math.max(300, Math.round(weight / 100) * 100))];
  const defaults = nested ? null : [{ color: fig.color.black }, flat?.fontSize == null ? BODY : null];
  return <NativeText {...props} style={[{ fontFamily, includeFontPadding: false }, defaults, style, { fontFamily, fontWeight: "normal" }]}>
    {nested ? children : <Nested.Provider value>{children}</Nested.Provider>}
  </NativeText>;
}

// The five Pretendard cuts the .fig uses (Light 300 through Bold 700), all bundled in assets/fonts.
const FAMILY: Record<number, string> = { 300: "Pretendard-Light", 400: "Pretendard", 500: "Pretendard-Medium", 600: "Pretendard-SemiBold", 700: "Pretendard-Bold" };
const BODY = { fontSize: fig.text.body13.fontSize, lineHeight: fig.text.body13.lineHeight };
