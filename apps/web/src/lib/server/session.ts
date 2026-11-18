// Server-only: imported by the route handlers under app/auth and app/bff.
import { NextResponse, type NextRequest } from "next/server";

// The site's session is the Rust API's own token pair, kept in httpOnly
// cookies that page scripts cannot read. Each cookie is scoped to the only
// path that uses it:
//   access  → /bff   (the proxy that attaches it as a Bearer header)
//   refresh → /auth  (refresh and logout)
//   oauth   → /auth/callback (the PKCE verifier between start and callback)
export const ACCESS_COOKIE = "__Secure-mazimi_at";
export const REFRESH_COOKIE = "__Secure-mazimi_rt";
export const OAUTH_COOKIE = "__Secure-mazimi_oauth";

// Expire the access cookie a minute before the 30-minute JWT so the proxy
// answers 401 (and the page refreshes) before the API would.
const ACCESS_MAX_AGE = 29 * 60;
const REFRESH_MAX_AGE = 30 * 24 * 60 * 60;
export const OAUTH_MAX_AGE = 10 * 60;

export const PROVIDERS = ["kakao", "google"] as const;
export type Provider = (typeof PROVIDERS)[number];
export function isProvider(value: string | null | undefined): value is Provider {
  return PROVIDERS.includes(value as Provider);
}

/** API origin for server-to-server calls (the compose service name in production). */
export function apiInternalBase(): string {
  return (process.env.API_INTERNAL_URL || process.env.NEXT_PUBLIC_API_BASE_URL || "http://localhost:8080").replace(/\/+$/, "");
}

/** API origin the browser is sent to for the provider redirect. */
export function apiPublicBase(): string {
  return (process.env.NEXT_PUBLIC_API_BASE_URL || "http://localhost:8080").replace(/\/+$/, "");
}

const base = { httpOnly: true, secure: true, sameSite: "lax" as const };

export function setSessionCookies(response: NextResponse, token: string, refreshToken: string) {
  response.cookies.set(ACCESS_COOKIE, token, { ...base, path: "/bff", maxAge: ACCESS_MAX_AGE });
  response.cookies.set(REFRESH_COOKIE, refreshToken, { ...base, path: "/auth", maxAge: REFRESH_MAX_AGE });
}

export function clearSessionCookies(response: NextResponse) {
  response.cookies.set(ACCESS_COOKIE, "", { ...base, path: "/bff", maxAge: 0 });
  response.cookies.set(REFRESH_COOKIE, "", { ...base, path: "/auth", maxAge: 0 });
}

export function setOAuthCookie(response: NextResponse, value: string) {
  response.cookies.set(OAUTH_COOKIE, value, { ...base, path: "/auth/callback", maxAge: OAUTH_MAX_AGE });
}

export function clearOAuthCookie(response: NextResponse) {
  response.cookies.set(OAUTH_COOKIE, "", { ...base, path: "/auth/callback", maxAge: 0 });
}

/** Header every cookie-authenticated call must carry (see `isSameOriginCall`). */
export const CSRF_HEADER = "x-mazimi-csrf";

/**
 * CSRF guard for cookie-authenticated routes. SameSite=Lax already withholds
 * the cookies from cross-site subrequests; on top of that the call must
 * (1) carry a custom header, which a cross-origin page can only send after a
 * CORS preflight this server never approves, (2) not be marked cross-site by
 * the browser, and (3) when it names an Origin, name this host.
 */
export function isSameOriginCall(request: NextRequest): boolean {
  if (request.headers.get(CSRF_HEADER) !== "1") return false;
  const site = request.headers.get("sec-fetch-site");
  if (site && site !== "same-origin") return false;
  const origin = request.headers.get("origin");
  if (origin) {
    try {
      if (new URL(origin).host !== request.headers.get("host")) return false;
    } catch {
      return false;
    }
  }
  return true;
}

/**
 * Headers that keep the API's per-IP rate limits per visitor rather than per
 * web container. Caddy overwrites X-Real-IP with the peer address.
 */
export function forwardedFor(request: NextRequest): Record<string, string> {
  const ip = request.headers.get("x-real-ip") || request.headers.get("x-forwarded-for")?.split(",")[0]?.trim();
  return ip ? { "x-forwarded-for": ip, "x-real-ip": ip } : {};
}

export { safeReturnPath } from "@/lib/returnPath";

interface TokenPair { token: string; refresh_token: string }
export function isTokenPair(value: unknown): value is TokenPair {
  const v = value as Partial<TokenPair> | null;
  return typeof v?.token === "string" && v.token.length > 0 && typeof v.refresh_token === "string" && v.refresh_token.length > 0;
}

/** JSON POST to the API from the server; never cached, never follows redirects. */
export function postToApi(request: NextRequest, path: string, body: unknown): Promise<Response> {
  return fetch(`${apiInternalBase()}${path}`, {
    method: "POST",
    headers: { "content-type": "application/json", ...forwardedFor(request) },
    body: JSON.stringify(body),
    cache: "no-store",
    redirect: "manual",
    signal: AbortSignal.timeout(15_000),
  });
}

export function noStore(response: NextResponse): NextResponse {
  response.headers.set("cache-control", "no-store");
  return response;
}

/**
 * Same-site redirect with a relative Location. Behind the reverse proxy the
 * request URL's host is the container's, so it is never used to build one.
 */
export function redirectTo(pathAndQuery: string): NextResponse {
  return noStore(new NextResponse(null, { status: 303, headers: { location: pathAndQuery } }));
}
