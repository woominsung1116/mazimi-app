// Shared by the login page (client) and /auth/login + /auth/callback (server).

export const DEFAULT_RETURN_PATH = "/dashboard";
const PROBE = "http://site.invalid";

/**
 * A same-site path to return to after sign-in, or the dashboard.
 *
 * Validation runs on the URL parser's normalised result, not the raw text:
 * `/.//evil.com`, `/a/..//evil.com` and `/%2e//evil.com` all normalise to a
 * pathname starting with `//`, which a browser reads as another host. Raw
 * backslashes and control characters are refused outright, and the auth
 * routes themselves are never a return target.
 */
export function safeReturnPath(raw: string | null | undefined): string {
  if (!raw || !raw.startsWith("/") || /[\\\u0000-\u001f\u007f]/.test(raw)) return DEFAULT_RETURN_PATH;
  let url: URL;
  try {
    url = new URL(raw, PROBE);
  } catch {
    return DEFAULT_RETURN_PATH;
  }
  const path = url.pathname;
  if (url.origin !== PROBE || path.startsWith("//") || path.startsWith("/\\")) return DEFAULT_RETURN_PATH;
  if (path === "/login" || path.startsWith("/auth/") || path.startsWith("/bff/")) return DEFAULT_RETURN_PATH;
  return `${path}${url.search}${url.hash}`;
}
