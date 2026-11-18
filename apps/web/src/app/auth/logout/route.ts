import { NextResponse, type NextRequest } from "next/server";
import { REFRESH_COOKIE, clearSessionCookies, isSameOriginCall, noStore, postToApi } from "@/lib/server/session";

export const dynamic = "force-dynamic";

/**
 * POST /auth/logout — revokes the refresh token at the API, then clears the
 * session cookies. The cookies are cleared even if the API is unreachable.
 */
export async function POST(request: NextRequest) {
  if (!isSameOriginCall(request)) return noStore(new NextResponse(null, { status: 403 }));
  const refreshToken = request.cookies.get(REFRESH_COOKIE)?.value;
  if (refreshToken) {
    try {
      await postToApi(request, "/api/v1/auth/logout", { refresh_token: refreshToken });
    } catch {
      // The access token expires within 30 minutes; the cookies go regardless.
    }
  }
  const response = noStore(new NextResponse(null, { status: 204 }));
  clearSessionCookies(response);
  return response;
}
