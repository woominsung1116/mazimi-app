import { useFonts } from "expo-font";
import { Stack, usePathname } from "expo-router";
import { DefaultTheme, ThemeProvider } from "@react-navigation/native";
import * as SplashScreen from "expo-splash-screen";
import { StatusBar } from "expo-status-bar";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import React, { useEffect, useRef } from "react";
import {
  AppState,
  Platform,
  View,
  TouchableOpacity,
  StyleSheet,
  SafeAreaView,
} from "react-native";
import Text from "@/components/Text";
import { FigmaIcon } from "@/components/FigmaUI";
import { retryTimedOutRefresh, useAuthStore } from "../store/auth";
import { useConsentStore } from "../store/consent";
import BrandBackdrop from "@/components/BrandBackdrop";
import PushRouting from "@/components/PushRouting";
import { SessionErrorBanner, useSessionProblem } from "@/components/SessionProblem";
import { trackWebPath } from "@/lib/navigation";
import { purgeLegacyGuestData } from "@/lib/guest";
import { clearFormFillCache } from "@/lib/formFill/cache";
import { sweepVaultLeftovers } from "@/lib/vault";
import { colors, fig } from "@/constants/theme";

// Hold the native launch screen until the consent record and fonts are ready.
void SplashScreen.preventAutoHideAsync().catch(() => {});

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: 1000 * 60 * 5,
      retry: 2,
    },
  },
});

/** Restores auth session on app start. No navigation — just reads SecureStore. */
function SessionRestore() {
  const { restoreSession, user } = useAuthStore();
  const previousUser = useRef<string | null>(null);
  useEffect(() => {
    if (previousUser.current !== (user?.id ?? null)) queryClient.clear();
    previousUser.current = user?.id ?? null;
  }, [user?.id]);
  const restored = useRef(false);

  useEffect(() => {
    if (!restored.current) {
      restored.current = true;
      restoreSession();
      // A filled PDF left behind by a killed share sheet must not outlive the session.
      void clearFormFillCache();
      // Decrypted vault copies and plaintext leftovers from an interrupted save.
      void sweepVaultLeftovers().catch(() => {});
      // 예전 빌드가 기기에 남긴 비회원 조건을 지운다
      void purgeLegacyGuestData();
    }
  }, []);

  // After an outage the stored session comes back by itself when the app
  // returns to the foreground or the browser is online again.
  useEffect(() => {
    const retry = () => {
      const s = useAuthStore.getState();
      if (!s.user && s.sessionError && !s.isLoading) { void s.restoreSession(); return; }
      // A refresh that timed out moments ago may have rotated the token: finish it inside the server's grace window.
      if (s.user) void retryTimedOutRefresh();
    };
    const sub = AppState.addEventListener("change", state => { if (state === "active") retry(); });
    if (Platform.OS === "web" && typeof window !== "undefined") window.addEventListener("online", retry);
    return () => { sub.remove(); if (Platform.OS === "web" && typeof window !== "undefined") window.removeEventListener("online", retry); };
  }, []);

  return null;
}

// Screens that render SessionErrorState themselves (every NonMemberState screen,
// profile-edit and every onboarding step), home (it shows the banner in its own
// flow), and the auth screens. Kept in step with NonMemberState users by check-session.
// The navigator paints its scene container in the theme background; Figma's screens are White.
const NAV_THEME = { ...DefaultTheme, colors: { ...DefaultTheme.colors, background: fig.color.white, card: fig.color.white } };
const NO_SESSION_BANNER = new Set([
  "/", "/profile", "/manage", "/profile-edit", "/login", "/consent",
  "/agent", "/assistant", "/assistant-search", "/benefit-assistant", "/apply-assistant", "/compare", "/agent-history", "/briefing",
  "/notifications", "/applications", "/account",
]);
const showsSessionBanner = (pathname: string) => !NO_SESSION_BANNER.has(pathname) && !pathname.startsWith("/onboarding");

/**
 * Kicks off the one-time AsyncStorage read for consent state. Kept separate
 * from the gating decision itself (see RootLayout) so the loading trigger and
 * the render branch are easy to reason about independently.
 */
function useConsentGate() {
  const isLoaded = useConsentStore((s) => s.isLoaded);
  const hasRequiredConsent = useConsentStore((s) => s.hasRequiredConsent());
  const loadConsent = useConsentStore((s) => s.loadConsent);
  const started = useRef(false);

  useEffect(() => {
    if (!started.current) {
      started.current = true;
      loadConsent();
    }
  }, [loadConsent]);

  return { isLoaded, hasRequiredConsent };
}

interface ErrorBoundaryState {
  hasError: boolean;
  error: Error | null;
}

class GlobalErrorBoundary extends React.Component<
  { children: React.ReactNode },
  ErrorBoundaryState
> {
  constructor(props: { children: React.ReactNode }) {
    super(props);
    this.state = { hasError: false, error: null };
  }

  static getDerivedStateFromError(error: Error): ErrorBoundaryState {
    return { hasError: true, error };
  }

  componentDidCatch(error: Error, info: React.ErrorInfo) {
    console.error("[GlobalErrorBoundary] Unhandled error:", error);
    console.error("[GlobalErrorBoundary] Component stack:", info.componentStack);
  }

  handleRestart = () => {
    this.setState({ hasError: false, error: null });
  };

  render() {
    if (this.state.hasError) {
      return (
        <SafeAreaView style={errorStyles.container}>
          <View style={errorStyles.content}>
            <View style={errorStyles.emoji}><FigmaIcon name="info" size={32} color={colors.error} /></View>
            <Text style={errorStyles.title}>앱에 문제가 발생했어요</Text>
            <Text style={errorStyles.subtitle}>
              잠시 후 다시 시도해 주세요.
            </Text>
            <TouchableOpacity
              style={errorStyles.button}
              onPress={this.handleRestart}
              activeOpacity={0.8}
            >
              <Text style={errorStyles.buttonText}>다시 시작하기</Text>
            </TouchableOpacity>
          </View>
        </SafeAreaView>
      );
    }
    return this.props.children;
  }
}

const errorStyles = StyleSheet.create({
  container: {
    flex: 1,
    backgroundColor: colors.background,
  },
  content: {
    flex: 1,
    alignItems: "center",
    justifyContent: "center",
    paddingHorizontal: 32,
  },
  emoji: {
    fontSize: 24, fontWeight: "700", lineHeight: 36,
    marginBottom: 16,
  },
  title: {
    fontSize: 20, lineHeight: 32,
    fontWeight: "700",
    color: colors.textPrimary,
    textAlign: "center",
    marginBottom: 8,
  },
  subtitle: {
    fontSize: 13, lineHeight: 20,
    color: colors.textMuted,
    textAlign: "center",
    marginBottom: 32,
  },
  button: {
    backgroundColor: colors.primary,
    paddingVertical: 14,
    paddingHorizontal: 32,
    borderRadius: 12,
    minHeight: 44,
    alignItems: "center",
    justifyContent: "center",
  },
  buttonText: {
    color: "#ffffff",
    fontSize: 16, lineHeight: 24,
    fontWeight: "700",
  },
});

export default function RootLayout() {
  const pathname = usePathname();
  useEffect(() => { trackWebPath(); }, [pathname]);
  const { isLoaded, hasRequiredConsent } = useConsentGate();
  const sessionProblem = useSessionProblem();
  const [fontsLoaded, fontError] = useFonts({
    "Pretendard-Light": require("../assets/fonts/Pretendard-Light.otf"),
    Pretendard: require("../assets/fonts/Pretendard-Regular.otf"),
    "Pretendard-Medium": require("../assets/fonts/Pretendard-Medium.otf"),
    "Pretendard-SemiBold": require("../assets/fonts/Pretendard-SemiBold.otf"),
    "Pretendard-Bold": require("../assets/fonts/Pretendard-Bold.otf"),
  });

  // Avoid a false "not agreed" flash while the AsyncStorage read resolves —
  // render nothing but the background until we actually know the state.
  if (!isLoaded || (!fontsLoaded && !fontError)) {
    return (
      <GlobalErrorBoundary>
        <View style={consentLoadingStyles.root}>
          <StatusBar style="light" />
          <BrandBackdrop splash />
        </View>
      </GlobalErrorBoundary>
    );
  }

  return (
    <GlobalErrorBoundary>
      <QueryClientProvider client={queryClient}>
        <StatusBar style={pathname === "/login" || pathname === "/consent" ? "light" : "dark"} backgroundColor={pathname === "/login" || pathname === "/consent" ? colors.loginBackground : colors.background} />
        <SessionRestore />
        {hasRequiredConsent && <PushRouting />}
        <View style={{ flex: 1 }} onLayout={() => { void SplashScreen.hideAsync().catch(() => {}); }}>
        <ThemeProvider value={NAV_THEME}>
        <Stack
          screenOptions={{
            headerStyle: { backgroundColor: "#FFFFFF" },
            headerTintColor: colors.textPrimary,
            // The stack title style takes no line height; the title is drawn as Figma bold 16/24 text instead.
            headerTitle: ({ children }) => <Text numberOfLines={1} style={{ ...fig.text.bold16, color: colors.textPrimary }}>{children}</Text>,
            headerShadowVisible: false,
            contentStyle: { backgroundColor: "#FFFFFF" },
          }}
        >
          {/* 필수 동의(이용약관+개인정보) 전에는 이 화면만 보여진다. 뒤로가기로 빠져나갈
              수 없도록 gestureEnabled: false. 동의 완료 시 hasRequiredConsent가 true로
              바뀌면서 아래 그룹으로 자동 전환된다. */}
          <Stack.Protected guard={!hasRequiredConsent}>
            <Stack.Screen
              name="consent"
              options={{ headerShown: false, gestureEnabled: false }}
            />
          </Stack.Protected>

          <Stack.Protected guard={hasRequiredConsent}>
            <Stack.Screen name="(tabs)" options={{ headerShown: false }} />
            {/* 연타나 반복 진입으로 같은 화면이 여러 장 쌓이지 않게 한다 */}
            <Stack.Screen name="login" options={{ headerShown: false }} dangerouslySingular />
            <Stack.Screen name="notifications" options={{ headerShown: false }} dangerouslySingular />
            <Stack.Screen name="application/[id]" options={{ headerShown: false }} />
            <Stack.Screen name="applications" options={{ headerShown: false }} />
            <Stack.Screen name="help" options={{ headerShown: false }} />
            <Stack.Screen name="benefit-assistant" options={{ headerShown: false }} dangerouslySingular />
            <Stack.Screen name="assistant" options={{ headerShown: false }} dangerouslySingular />
            <Stack.Screen name="assistant-search" options={{ headerShown: false }} dangerouslySingular />
            <Stack.Screen name="agent-history" options={{ headerShown: false }} dangerouslySingular />
            <Stack.Screen
              name="onboarding/index"
              options={{ headerShown: false }}
              dangerouslySingular
            />
            <Stack.Screen
              name="onboarding/step1"
              options={{ headerShown: false }}
              dangerouslySingular
            />
            <Stack.Screen
              name="onboarding/step2"
              options={{ headerShown: false }}
              dangerouslySingular
            />
            <Stack.Screen
              name="onboarding/step3"
              options={{ headerShown: false }}
              dangerouslySingular
            />
            <Stack.Screen name="onboarding/school" options={{ headerShown: false }} dangerouslySingular />
            <Stack.Screen name="onboarding/welcome" options={{ headerShown: false, gestureEnabled: false }} dangerouslySingular />
            <Stack.Screen name="profile-edit" options={{ headerShown: false }} dangerouslySingular />
            <Stack.Screen name="recommendations" options={{ headerShown: false }} />
            <Stack.Screen name="briefing" options={{ headerShown: false }} />
            <Stack.Screen
              name="programs/[id]"
              options={{ headerShown: false }}
              dangerouslySingular={(_, params) => typeof params.id === "string" ? params.id : undefined}
            />
            <Stack.Screen
              name="compare"
              options={{ title: "혜택 비교", headerShown: false }}
            />
            <Stack.Screen
              name="document-vault"
              options={{ headerShown: false }}
            />
            <Stack.Screen
              name="apply-assistant"
              options={{ headerShown: false }}
            />
            <Stack.Screen name="form-fill" options={{ headerShown: false }} />
          </Stack.Protected>

          {/* 정책 전문은 동의 게이트 화면과 로그인 화면 양쪽에서 링크로 열어야 하므로
              항상(동의 여부와 무관하게) 접근 가능해야 한다. */}
          <Stack.Screen
            name="privacy-policy"
            options={{ headerShown: false }}
          />
          <Stack.Screen
            name="terms"
            options={{ headerShown: false }}
          />
          <Stack.Screen name="account" options={{ headerShown: false }} />
        </Stack>
        </ThemeProvider>
        {sessionProblem && showsSessionBanner(pathname) && <SessionErrorBanner />}
        </View>
      </QueryClientProvider>
    </GlobalErrorBoundary>
  );
}

const consentLoadingStyles = StyleSheet.create({
  root: {
    flex: 1,
    backgroundColor: colors.primary,
  },
});
