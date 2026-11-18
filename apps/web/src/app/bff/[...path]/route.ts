import { NextResponse, type NextRequest } from "next/server";
import { ACCESS_COOKIE, apiInternalBase, forwardedFor, isSameOriginCall, noStore } from "@/lib/server/session";

export const dynamic = "force-dynamic";

// Matches the API's DefaultBodyLimit; larger bodies are refused before read.
const MAX_BODY_BYTES = 1024 * 1024;
const FORWARDED_REQUEST_HEADERS = ["accept", "content-type", "idempotency-key"];
const FORWARDED_RESPONSE_HEADERS = ["content-type", "content-disposition", "retry-after"];

/**
 * Read at most `limit` bytes. A chunked body has no Content-Length, so the
 * cap is enforced while streaming; past it the reader is cancelled and the
 * result is null.
 */
async function readCapped(stream: ReadableStream<Uint8Array> | null, limit: number): Promise<Uint8Array<ArrayBuffer> | null> {
  if (!stream) return new Uint8Array(new ArrayBuffer(0));
  const reader = stream.getReader();
  const chunks: Uint8Array[] = [];
  let size = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    size += value.byteLength;
    if (size > limit) {
      await reader.cancel().catch(() => undefined);
      return null;
    }
    chunks.push(value);
  }
  const body = new Uint8Array(new ArrayBuffer(size));
  let offset = 0;
  for (const chunk of chunks) {
    body.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return body;
}

const reply = (status: number, error: string) => noStore(NextResponse.json({ error }, { status }));

/**
 * /bff/api/v1/… — same-origin proxy for authenticated API calls. It attaches
 * the access token from the httpOnly cookie as a Bearer header, so page
 * scripts never see it. Only /api/v1 resources are reachable, the token
 * routes are excluded (they live under /auth), and browser cookies are never
 * forwarded to the API.
 */
async function proxy(request: NextRequest, { params }: { params: Promise<{ path: string[] }> }) {
  if (!isSameOriginCall(request)) return reply(403, "Forbidden");
  const { path } = await params;
  const safeSegment = (segment: string) => segment !== "" && segment !== "." && segment !== ".." && !/[/\\]/.test(segment);
  if (path.length < 3 || path[0] !== "api" || path[1] !== "v1" || !path.every(safeSegment)) return reply(404, "Not Found");
  if (path[2] === "auth" && path.slice(3).join("/") !== "me") return reply(404, "Not Found");

  const token = request.cookies.get(ACCESS_COOKIE)?.value;
  if (!token) return reply(401, "Session expired");

  const headers: Record<string, string> = { authorization: `Bearer ${token}`, ...forwardedFor(request) };
  for (const name of FORWARDED_REQUEST_HEADERS) {
    const value = request.headers.get(name);
    if (value) headers[name] = value;
  }
  let body: Uint8Array<ArrayBuffer> | undefined;
  if (request.method !== "GET" && request.method !== "HEAD") {
    if (Number(request.headers.get("content-length") ?? 0) > MAX_BODY_BYTES) return reply(413, "Payload too large");
    const read = await readCapped(request.body, MAX_BODY_BYTES);
    if (read === null) return reply(413, "Payload too large");
    body = read;
  }

  const target = `${apiInternalBase()}/${path.map(encodeURIComponent).join("/")}${request.nextUrl.search}`;
  let upstream: Response;
  try {
    upstream = await fetch(target, { method: request.method, headers, body, cache: "no-store", redirect: "manual", signal: request.signal });
  } catch {
    return reply(502, "API unavailable");
  }
  const out = new NextResponse(upstream.status === 204 || request.method === "HEAD" ? null : upstream.body, { status: upstream.status });
  for (const name of FORWARDED_RESPONSE_HEADERS) {
    const value = upstream.headers.get(name);
    if (value) out.headers.set(name, value);
  }
  out.headers.set("x-content-type-options", "nosniff");
  return noStore(out);
}

export { proxy as GET, proxy as POST, proxy as PUT, proxy as DELETE };
