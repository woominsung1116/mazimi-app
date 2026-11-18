import React, { useEffect } from "react";
import { BackHandler, Image, Pressable, StyleSheet, View } from "react-native";
import { useLocalSearchParams, useRouter } from "expo-router";
import { useSafeAreaInsets } from "react-native-safe-area-context";
import Text from "@/components/Text";
import { figmaAssets, ScreenHeader, ui } from "@/components/FigmaUI";
import { onboardingFooter } from "@/components/OnboardingFrame";
import { fig } from "@/constants/theme";
import { samePath } from "@/lib/navigation";

/** 회원가입을 환영합니다 (Figma 28023:1751): after a new member's last onboarding step. */
export default function Welcome() {
  const router = useRouter();
  const insets = useSafeAreaInsets();
  const { next } = useLocalSearchParams<{ next?: string }>();
  const start = () => router.replace(samePath(next) ? next : "/(tabs)");
  // The saved onboarding steps are behind this screen: Android back goes on, like 시작하기.
  useEffect(() => { const sub = BackHandler.addEventListener("hardwareBackPress", () => { start(); return true; }); return () => sub.remove(); });
  return <View style={ui.screen}>
    <ScreenHeader action={<View />} />
    <View style={s.content}>
      <View style={s.art}><Image source={figmaAssets.welcome} style={s.image} resizeMode="contain" accessibilityIgnoresInvertColors /></View>
      <Text accessibilityRole="header" style={s.title}>회원가입을 환영합니다 !</Text>
      <Text style={s.body}>아래 버튼을 누르고{"\n"}마지미를 시작해보세요.</Text>
    </View>
    <View style={[onboardingFooter.bar, { paddingBottom: insets.bottom + 8 }]}>
      <Pressable accessibilityRole="button" onPress={start} style={onboardingFooter.button}><Text style={ui.primaryButtonText}>시작하기</Text></Pressable>
    </View>
  </View>;
}

// 28023:1752 content (24/16), 28023:1753 centred column (gap 8), image 239 (28023:1829), title Bold 20/32, body Regular 13/20 Deep gray.
const s = StyleSheet.create({
  content: { flex: 1, paddingVertical: 24, paddingHorizontal: 16, alignItems: "center", justifyContent: "center", gap: 8 },
  art: { paddingHorizontal: 52, alignItems: "center" },
  image: { width: 239, height: 239 },
  title: { ...fig.text.bold20, color: fig.color.black, textAlign: "center" },
  body: { ...fig.text.body13, color: fig.color.deepGray, textAlign: "center" },
});
