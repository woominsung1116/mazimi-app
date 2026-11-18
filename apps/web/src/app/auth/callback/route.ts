import type { NextRequest } from "next/server";
import {
  OAUTH_COOKIE,
  clearOAuthCookie,
  isProvider,
  isTokenPair,
  postToApi,
  redirectTo,
  safeReturnPath,
  setSessionCookies,
} from "@/lib/server/session";

export const dynamic = "force-dynamic";

interface PendingLogin { v: string; p: string; r: string }

function readPending(raw: string | undefined): PendingLogin | null {
  if (!raw) return null;
  try {
    const value = JSON.parse(raw) as Partial<PendingLogin>;
    return typeof value.v === "string" && /^[0-9a-f]{64}$/.test(value.v) && typeof value.p === "string" && typeof value.r === "string"
      ? (value as PendingLogin)
      : null;
  } catch {
    return null;
  }
}

/**
 * GET /auth/callback?provider=kakao&code=… (or &error=…)
 *
 * The API redirects here after the provider callback. This route redeems the
 * one-time code with the verifier from the httpOnly cookie, server to server,
 * and stores the API's token pair in httpOnly cookies. Tokens never reach
 * page scripts, the URL, or logs.
 */
export async function GET(request: NextRequest) {
  const query = request.nextUrl.searchParams;
  const pending = readPending(request.cookies.get(OAUTH_COOKIE)?.value);
  const provider = query.get("provider");
  const code = query.get("code");
  const fail = (reason: string) => {
    const target = new URLSearchParams({ error: reason });
    if (pending && pending.r !== "/dashboard") target.set("callbackUrl", safeReturnPath(pending.r));
    const response = redirectTo(`/login?${target}`);
    clearOAuthCookie(response);
    return response;
  };

  if (query.get("error")) return fail(query.get("error") === "cancelled" ? "cancelled" : "failed");
  if (!pending || !isProvider(provider) || pending.p !== provider) return fail("restart");
  if (!code || !/^[0-9a-f]{64}$/.test(code)) return fail("failed");

  let issued: unknown;
  try {
    const exchange = await postToApi(request, `/api/v1/auth/${provider}/exchange`, { code, verifier: pending.v });
    if (!exchange.ok) return fail("expired");
    issued = await exchange.json();
  } catch {
    return fail("failed");
  }
  if (!isTokenPair(issued)) return fail("failed");

  const isNew = (issued as { is_new?: unknown }).is_new === true;
  const response = redirectTo(isNew ? "/onboarding" : safeReturnPath(pending.r));
  setSessionCookies(response, issued.token, issued.refresh_token);
  clearOAuthCookie(response);
  return response;
}
