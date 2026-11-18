import React, { useCallback, useEffect, useRef, useState } from "react";
import { markFirstSignup, takeFirstSignup } from "@/lib/firstSignup";
import { ActivityIndicator, Image, Platform, Pressable, StyleSheet, View } from "react-native";
import { useLocalSearchParams, useRouter } from "expo-router";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import * as Linking from "expo-linking";
import * as WebBrowser from "expo-web-browser";
import * as Crypto from "expo-crypto";
import { useSafeAreaInsets } from "react-native-safe-area-context";
import Text from "@/components/Text";
import { FigmaIcon, ui } from "@/components/FigmaUI";
import { useAuthStore } from "@/store/auth";
import { tokenStorage } from "@/lib/tokenStorage";
import { clearPendingApplication, takePendingApplication } from "@/lib/pendingApplication";
import { rememberLoginReturn, takeLoginReturn } from "@/lib/navigation";
import BrandBackdrop from "@/components/BrandBackdrop";
import { colors, fig } from "@/constants/theme";
import { API_BASE_URL as API_URL } from "@/lib/apiBaseUrl";
type Provider = "kakao" | "google";
const verifierKey = (provider: Provider) => `mazimi_oauth_verifier_${provider}`;
const providerName = (provider: Provider) => provider === "kakao" ? "카카오" : "Google";
// 응답 없는 요청이 버튼을 붙잡지 않게 한다. 시간 안에 못 받으면 누를 때 다시 확인한다
const PROVIDERS_TIMEOUT_MS = 8000;
const providersKey = ["login-providers"];
async function fetchProviders(): Promise<Record<Provider, boolean>> {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), PROVIDERS_TIMEOUT_MS);
  try {
    const response = await fetch(`${API_URL}/api/v1/auth/providers`, { signal: controller.signal });
    if (!response.ok) throw new Error("로그인 방식을 불러오지 못했어요.");
    return await response.json() as Record<Provider, boolean>;
  } finally { clearTimeout(timer); }
}

export default function Login() {
  const router = useRouter();
  // 기능을 누르다 들어온 경우에만 원래 화면으로 돌아간다. 첫 실행 흐름은 그대로 온보딩으로 간다
  const returnParam = useLocalSearchParams<{ return?: string }>().return;
  const returning = useRef(false);
  returning.current = returnParam === "1" && router.canGoBack();
  const insets = useSafeAreaInsets();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const handledCode = useRef<string | null>(null);
  const client = useQueryClient();
  const providers = useQuery({ queryKey: providersKey, staleTime: 60000, queryFn: fetchProviders, retry: 1, retryDelay: 500 });
  const [pending, setPending] = useState<Provider | null>(null);
  // 카카오에서 돌아온 게 아니면 예전에 취소한 로그인의 복귀 경로를 버린다
  useEffect(() => { if (Platform.OS === "web" && !/[?&](code|error)=/.test(window.location.search)) takeLoginReturn(); }, []);
  const finish = useCallback(async (url: string) => {
    const parsed = Linking.parse(url);
    if (parsed.path !== "login" && parsed.hostname !== "login") return;
    const provider: Provider = parsed.queryParams?.provider === "google" ? "google" : "kakao";
    const code = parsed.queryParams?.code;
    const failure = parsed.queryParams?.error;
    if (Platform.OS === "web" && (code || failure)) window.history.replaceState(null, "", "/login");
    if (failure) {
      await tokenStorage.deleteItemAsync(verifierKey(provider));
      setError(failure === "cancelled" ? "로그인을 취소했어요. 다시 선택할 수 있어요." : "인증을 완료하지 못했어요. 다시 시도해 주세요.");
      return;
    }
    if (typeof code !== "string" || handledCode.current === code) return;
    handledCode.current = code;
    setBusy(true);
    try {
      const verifier = await tokenStorage.getItemAsync(verifierKey(provider))
        ?? (provider === "kakao" ? await tokenStorage.getItemAsync("mazimi_oauth_verifier") : null);
      if (!verifier) throw new Error("로그인을 시작한 브라우저에서 다시 시도해 주세요.");
      const response = await fetch(`${API_URL}/api/v1/auth/${provider}/exchange`, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ code, verifier }) });
      if (!response.ok) throw new Error("로그인이 만료됐어요. 다시 시도해 주세요.");
      const data = await response.json();
      const me = await fetch(`${API_URL}/api/v1/auth/me`, { headers: { Authorization: `Bearer ${data.token}` } });
      if (!me.ok) throw new Error("사용자 정보를 불러오지 못했어요.");
      const user = await me.json();
      await tokenStorage.setItemAsync("mazimi_auth_token", data.token);
      await tokenStorage.setItemAsync("mazimi_refresh_token", data.refresh_token);
      await tokenStorage.deleteItemAsync(verifierKey(provider));
      await tokenStorage.deleteItemAsync("mazimi_oauth_verifier");
      useAuthStore.setState({ token: data.token, refreshToken: data.refresh_token, user, isLoading: false });
      // 돌아갈 화면이 있으면 신청 화면을 새로 쌓지 않고 그 화면으로 돌아간다
      const next = data.is_new || returning.current ? null : await takePendingApplication();
      if (returning.current) await clearPendingApplication();
      // 다른 화면에서 로그인하러 왔으면 그 화면으로 돌려보낸다
      if (data.is_new) { markFirstSignup(); router.replace(returning.current ? "/onboarding?mode=edit" : "/onboarding"); return; }
      // An existing member: no welcome, even if an earlier sign-up was left unfinished.
      takeFirstSignup();
      if (next) { takeLoginReturn(); router.replace(next); }
      else if (returning.current) router.back();
      else router.replace(takeLoginReturn() ?? "/(tabs)");
    } catch (e) { setError(e instanceof Error ? e.message : "로그인에 실패했어요."); }
    finally { setBusy(false); }
  }, [router]);
  useEffect(() => {
    const sub = Linking.addEventListener("url", ({ url }) => void finish(url));
    void Linking.getInitialURL().then(url => { if (url) void finish(url); });
    return () => sub.remove();
  }, [finish]);
  // 버튼은 로그인 방식 확인을 기다리지 않는다. 확인이 아직이면 누른 뒤에 받아서 이어 간다
  const login = async (provider: Provider) => {
    if (busy) return;
    setError(""); setBusy(true); setPending(provider);
    try {
      const available = providers.data ?? await client.fetchQuery({ queryKey: providersKey, queryFn: fetchProviders, staleTime: 60000 }).catch(() => null);
      if (!available) { setError("로그인 서버에 연결하지 못했어요. 연결을 확인하고 다시 눌러 주세요."); return; }
      if (!available[provider]) { setError(`${providerName(provider)} 로그인은 아직 준비 중이에요.`); return; }
      const verifier = Array.from(Crypto.getRandomBytes(32), b => b.toString(16).padStart(2, "0")).join("");
      const challenge = await Crypto.digestStringAsync(Crypto.CryptoDigestAlgorithm.SHA256, verifier);
      await tokenStorage.setItemAsync(verifierKey(provider), verifier);
      const url = `${API_URL}/api/v1/auth/${provider}/start?client=${Platform.OS === "web" ? "web" : "mobile"}&challenge=${challenge}`;
      if (Platform.OS === "web") { if (returning.current) rememberLoginReturn(); window.location.assign(url); return; }
      const result = await WebBrowser.openAuthSessionAsync(url, "mazimi://login");
      if (result.type === "success") await finish(result.url);
      else await tokenStorage.deleteItemAsync(verifierKey(provider));
    } catch { setError("로그인을 시작하지 못했어요. 다시 시도해 주세요."); }
    finally { setBusy(false); setPending(null); }
  };
  const canGoBack = returning.current;
  return <View style={[ui.screen, { backgroundColor: colors.loginBackground, paddingTop: insets.top }]}>
    <BrandBackdrop />
    <View style={{ flexDirection: "row", alignItems: "center" }}>
      {canGoBack && <Pressable accessibilityRole="button" accessibilityLabel="뒤로 가기" disabled={busy} onPress={() => router.back()} style={[ui.iconButton, { marginLeft: 4 }]}><FigmaIcon name="back" size={20} color={fig.color.white} /></Pressable>}
      <Pressable accessibilityRole="button" disabled={busy} style={({ pressed }) => [styles.guest, { flex: 1 }, pressed && { opacity: 0.7 }]} onPress={() => { void clearPendingApplication().finally(() => canGoBack ? router.back() : router.replace("/(tabs)")); }}>
        <Text style={styles.guestText}>로그인 없이 둘러보기</Text><View style={styles.guestArrow}><FigmaIcon name="arrow" size={12} color={fig.color.white} /></View>
      </Pressable>
    </View>
    <View style={{ flex: 1 }} />
    <View style={[styles.footer, { paddingBottom: Math.max(insets.bottom, 16) + 12 }]}>
      {!!error && <Text accessibilityRole="alert" style={styles.feedback}>{error}</Text>}
      {providers.isError && <Pressable accessibilityRole="button" onPress={() => void providers.refetch()}><Text style={styles.feedback}>로그인 방식 다시 불러오기</Text></Pressable>}
      <Text style={styles.heading}>{busy ? "로그인 중…" : "소셜 계정으로 로그인"}</Text>
      <View style={styles.providers}>
        {(["kakao", "google"] as const).map(provider => {
          const name = providerName(provider);
          // 서버가 꺼 둔 방식만 흐리게 막는다. 확인 중이거나 확인에 실패해도 누를 수 있다
          const unavailable = providers.data?.[provider] === false;
          const label = unavailable ? `${name} 로그인 준비 중` : `${name}로 시작하기`;
          return <Pressable key={provider} onPress={() => void login(provider)} disabled={busy || unavailable} hitSlop={6} accessibilityRole="button" accessibilityLabel={label} accessibilityState={{ disabled: busy || unavailable, busy: pending === provider }} style={({ pressed }) => [styles.provider, unavailable && { opacity: 0.5 }, pressed && { opacity: 0.7, transform: [{ scale: 0.96 }] }]}>
            {provider === "kakao" ? <View style={[styles.circle, { backgroundColor: fig.color.kakao }]}><FigmaIcon name="kakao" size={16} color={fig.color.apple} /></View> : <Image source={require("../assets/auth/google-signin.png")} style={styles.circle} />}
            {pending === provider && <View style={styles.spinner}><ActivityIndicator color={fig.color.white} /></View>}
            {unavailable && <Text style={styles.unavailable}>준비 중</Text>}
          </Pressable>;
        })}
      </View>
      <Text style={styles.copyright}>© mazimi.corp.</Text>
    </View>
  </View>;
}
// Figma 로그인 32002:697: guest link 32002:846, caption 32002:755, provider circles 32002:797, copyright 32002:792.
const styles = StyleSheet.create({
  guest: { height: 56, flexDirection: "row", alignItems: "center", justifyContent: "flex-end", paddingHorizontal: 16, gap: 6 },
  guestText: { ...fig.text.semibold13, color: fig.color.white },
  guestArrow: { width: 20, height: 20, borderRadius: 14, padding: 4, backgroundColor: fig.color.whiteLine4, borderWidth: 1, borderColor: fig.color.whiteLine16, alignItems: "center", justifyContent: "center" },
  footer: { paddingHorizontal: 16, paddingTop: 24, gap: 20 },
  heading: { ...fig.text.medium13, color: fig.color.white, textAlign: "center" },
  providers: { flexDirection: "row", justifyContent: "center", gap: 8 },
  provider: { width: 44, height: 44, alignItems: "center", justifyContent: "center" },
  circle: { width: 40, height: 40, borderRadius: 20, justifyContent: "center", alignItems: "center" },
  spinner: { position: "absolute", width: 40, height: 40, borderRadius: 20, backgroundColor: fig.color.overlay, alignItems: "center", justifyContent: "center" },
  unavailable: { position: "absolute", top: 43, ...fig.text.caption10, color: fig.color.white },
  feedback: { ...fig.text.body13, color: fig.color.white, textAlign: "center" },
  copyright: { ...fig.text.body12, color: fig.color.white, opacity: 0.8, textAlign: "center" },
});
