import { Alert, Platform } from "react-native";
import type { Href, Router } from "expo-router";

// 딥링크 등으로 이전 화면이 없을 때는 홈으로 보낸다
export function goBack(router: Router) {
  if (router.canGoBack()) router.back();
  else router.replace("/(tabs)");
}

// 로그인이 필요한 기능은 바로 로그인 화면으로 넘기지 않고 이유를 먼저 알린다
// return=1이면 로그인 화면이 끝난 뒤 이 화면으로 돌아온다
export function requireLogin(router: Router, message: string) {
  if (Platform.OS === "web") { if (window.confirm(`로그인이 필요해요\n${message}`)) router.push("/login?return=1"); return; }
  Alert.alert("로그인이 필요해요", message, [
    { text: "취소", style: "cancel" },
    { text: "로그인", onPress: () => router.push("/login?return=1") },
  ]);
}


// 웹 로그인은 카카오로 페이지를 떠났다 돌아오므로 돌아갈 경로를 세션에 남긴다
const LAST_PATH = "mazimi_last_path";
const LOGIN_RETURN = "mazimi_login_return";
const session = () => { try { return Platform.OS === "web" ? window.sessionStorage : null; } catch { return null; } };
/** An in-app path: never absolute, protocol-relative or a backslash trick. */
export const samePath = (path: unknown): path is Href => typeof path === "string" && path.startsWith("/") && !path.startsWith("//") && !path.startsWith("/\\");

export function trackWebPath() {
  const path = Platform.OS === "web" ? window.location.pathname + window.location.search : "";
  if (path && !/^\/(login|consent|onboarding)/.test(path)) session()?.setItem(LAST_PATH, path);
}
export function rememberLoginReturn() {
  const path = session()?.getItem(LAST_PATH);
  if (path) session()?.setItem(LOGIN_RETURN, JSON.stringify({ path, expiresAt: Date.now() + 60 * 60 * 1000 }));
}
export function takeLoginReturn(): Href | null {
  const raw = session()?.getItem(LOGIN_RETURN);
  session()?.removeItem(LOGIN_RETURN);
  try {
    const { path, expiresAt } = JSON.parse(raw ?? "null") ?? {};
    return samePath(path) && expiresAt > Date.now() ? path : null;
  } catch { return null; }
}
