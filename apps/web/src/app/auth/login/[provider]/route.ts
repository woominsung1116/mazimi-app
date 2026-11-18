import { createHash, randomBytes } from "node:crypto";
import { NextResponse, type NextRequest } from "next/server";
import { apiPublicBase, isProvider, noStore, redirectTo, safeReturnPath, setOAuthCookie } from "@/lib/server/session";

export const dynamic = "force-dynamic";

/**
 * GET /auth/login/{provider}?callbackUrl=/path
 *
 * Starts the API's server-side OAuth handoff. The PKCE verifier stays in an
 * httpOnly cookie scoped to /auth/callback; only its SHA-256 goes to the API,
 * so an exchange code is useless outside the browser that started sign-in.
 */
export async function GET(request: NextRequest, { params }: { params: Promise<{ provider: string }> }) {
  const { provider } = await params;
  if (!isProvider(provider)) return redirectTo("/login?error=failed");
  const verifier = randomBytes(32).toString("hex");
  const challenge = createHash("sha256").update(verifier).digest("hex");
  const returnTo = safeReturnPath(request.nextUrl.searchParams.get("callbackUrl"));
  const start = new URL(`${apiPublicBase()}/api/v1/auth/${provider}/start`);
  start.searchParams.set("client", "site");
  start.searchParams.set("challenge", challenge);
  const response = NextResponse.redirect(start);
  setOAuthCookie(response, JSON.stringify({ v: verifier, p: provider, r: returnTo }));
  return noStore(response);
}
