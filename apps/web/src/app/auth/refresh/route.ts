import { createHash } from "node:crypto";
import { NextResponse, type NextRequest } from "next/server";
import {
  REFRESH_COOKIE,
  clearSessionCookies,
  isSameOriginCall,
  isTokenPair,
  noStore,
  postToApi,
  setSessionCookies,
} from "@/lib/server/session";

export const dynamic = "force-dynamic";

/** Names the presented refresh token without revealing it. */
const SESSION_FINGERPRINT_HEADER = "x-mazimi-session";
/** Clear the cookies only if the current refresh token has this fingerprint. */
const CLEAR_IF_HEADER = "x-mazimi-clear-if";

const status = (code: number) => noStore(new NextResponse(null, { status: code }));
const fingerprint = (token: string) => createHash("sha256").update(token).digest("hex").slice(0, 32);

/**
 * POST /auth/refresh — rotates the API refresh token held in the httpOnly
 * cookie.
 *
 * - 204 with new cookies.
 * - 401 when the API refuses the token. The cookies are kept and the
 *   response names the refused token by fingerprint: another tab may have
 *   rotated the jar in the meantime (browsers without Web Locks), and
 *   clearing here would sign that tab out.
 * - With `X-Mazimi-Clear-If: <fingerprint>` it does not call the API: if
 *   the jar still holds that refused token, clear and answer 401;
 *   otherwise answer 409 (the session moved on; retry the call).
 * - 503 (cookies kept) when the API is unreachable or rate-limited.
 */
export async function POST(request: NextRequest) {
  if (!isSameOriginCall(request)) return status(403);
  const refreshToken = request.cookies.get(REFRESH_COOKIE)?.value;
  const clearIf = request.headers.get(CLEAR_IF_HEADER);
  if (!refreshToken || (clearIf && clearIf === fingerprint(refreshToken))) {
    const response = status(401);
    clearSessionCookies(response);
    return response;
  }
  if (clearIf) return status(409);

  let upstream: Response;
  try {
    upstream = await postToApi(request, "/api/v1/auth/refresh", { refresh_token: refreshToken });
  } catch {
    return status(503);
  }
  if (upstream.status === 401) {
    const response = status(401);
    response.headers.set(SESSION_FINGERPRINT_HEADER, fingerprint(refreshToken));
    return response;
  }
  if (!upstream.ok) return status(503);
  const pair: unknown = await upstream.json().catch(() => null);
  if (!isTokenPair(pair)) return status(503);
  const response = status(204);
  setSessionCookies(response, pair.token, pair.refresh_token);
  return response;
}
