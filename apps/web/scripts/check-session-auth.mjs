// End-to-end check of the site session: real Next server + real API + a
// scratch database. Only the OAuth provider is mocked: the test writes the
// handoff row the API's provider callback would write, then follows the
// redirect the API would send. Never run against production.
//
//   E2E_API=http://localhost:18497 E2E_PG_CONTAINER=web-auth-pg node scripts/check-session-auth.mjs
//
// Needs `npm run build` with NEXT_PUBLIC_API_BASE_URL=$E2E_API and
// API_INTERNAL_URL=$E2E_API, an API on $E2E_API (APP_ENV=local,
// KAKAO_CLIENT_ID set), and the tools/design Playwright install.
import assert from "node:assert/strict";
import path from "node:path";
import { createHash, randomBytes } from "node:crypto";
import { execFileSync, spawn } from "node:child_process";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";

const web = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const designRequire = createRequire(path.resolve(web, process.env.E2E_DESIGN_TOOLS ?? "../../tools/design/package.json"));
const { chromium } = designRequire("playwright");
const API = process.env.E2E_API ?? "http://localhost:18497";
const CONTAINER = process.env.E2E_PG_CONTAINER ?? "web-auth-pg";
// The API sends site logins back to http://localhost:3000 in local mode.
const base = "http://localhost:3000";
const sha = value => createHash("sha256").update(value).digest("hex");
const sql = query => execFileSync("docker", ["exec", "-i", CONTAINER, "psql", "-U", "wello", "-d", "wello", "-At", "-v", "ON_ERROR_STOP=1", "-c", query], { encoding: "utf8" }).trim();
const delay = ms => new Promise(resolve => setTimeout(resolve, ms));

function seedUser(role) {
  const id = sql(`INSERT INTO users(auth_provider,auth_provider_id,role,created_at,updated_at) VALUES('test','e2e-${randomBytes(8).toString("hex")}','${role}',now(),now()) RETURNING id`).split("\n")[0];
  sql(`INSERT INTO user_profiles(user_id,birth_year,region_code,nickname,updated_at) VALUES('${id}',2000,'busan','e2e-${role}',now())`);
  return id;
}

const results = [];
// Fresh client IPs per run: the API's auth limiter allows 5 calls per IP.
const net = `10.${1 + (process.pid % 250)}.${Math.floor(Math.random() * 250)}`;
const admin = seedUser("admin"), member = seedUser("user");
const server = spawn(process.execPath, [path.join(web, "node_modules/next/dist/bin/next"), "start", "--hostname", "localhost", "--port", "3000"], { cwd: web, stdio: "ignore", env: { ...process.env, NEXT_PUBLIC_API_BASE_URL: API, API_INTERNAL_URL: API } });
let browser;
try {
  for (let i = 0; i < 100 && !(await fetch(base).then(r => r.ok, () => false)); i++) await delay(150);
  browser = await chromium.launch({ headless: true });

  /** Sign in through the real /auth/login → API start → /auth/callback chain. */
  async function signIn(userId, ip, returnTo) {
    // A distinct client IP per account keeps each under the API's auth limiter.
    const context = await browser.newContext({ serviceWorkers: "block", extraHTTPHeaders: { "x-real-ip": ip } });
    const seen = [];
    context.on("request", request => seen.push({ url: request.url(), authorization: request.headers().authorization ?? null }));
    // Playwright does not route redirect hops, so the first hop is taken
    // apart: the real Next login route (sets the verifier cookie), then the
    // real API start (stores the handoff, redirects to Kakao). The Kakao leg
    // is replaced by what the API's callback would do for this state.
    await context.route(`${base}/auth/login/**`, async route => {
      const login = await route.fetch({ maxRedirects: 0 });
      const startUrl = login.headers().location;
      assert.ok(startUrl.startsWith(`${API}/api/v1/auth/kakao/start?client=site&challenge=`), "the site starts a site-client handoff");
      const started = await fetch(startUrl, { redirect: "manual", headers: { "x-real-ip": ip } });
      const kakao = new URL(started.headers.get("location"));
      assert.equal(kakao.origin, "https://kauth.kakao.com", "the API sends the browser to Kakao");
      const state = kakao.searchParams.get("state");
      const code = randomBytes(32).toString("hex");
      const updated = sql(`UPDATE oauth_handoffs SET callback_used=true, exchange_hash='${sha(code)}', user_id='${userId}', is_new=false, expires_at=now()+interval '2 minutes' WHERE state_hash='${sha(state)}' AND client='site' AND NOT callback_used RETURNING 1`);
      assert.equal(updated.split("\n")[0], "1", "the API stored a site handoff for this state");
      await route.fulfill({ response: login, headers: { ...login.headers(), location: `${base}/auth/callback?provider=kakao&code=${code}` } });
    });
    const page = await context.newPage();
    page.setDefaultTimeout(15000);
    await page.goto(`${base}/login?callbackUrl=${encodeURIComponent(returnTo)}`);
    await page.getByRole("link", { name: /카카오 1-Tap 로그인/ }).click();
    await page.waitForURL(`${base}${returnTo}`);
    return { context, page, seen };
  }

  const cookieMap = async context => Object.fromEntries((await context.cookies()).map(c => [c.name, c]));

  // ── A guest is sent to sign in, and the header offers it everywhere ──
  const guest = await browser.newContext({ serviceWorkers: "block", extraHTTPHeaders: { "x-real-ip": `${net}.4` } });
  const guestPage = await guest.newPage();
  await guestPage.goto(`${base}/dashboard`);
  await guestPage.waitForURL(`${base}/login?callbackUrl=%2Fdashboard`);
  await guestPage.goto(`${base}/programs`);
  const headerLogin = guestPage.getByRole("banner").getByRole("link", { name: "로그인" });
  assert.equal(await headerLogin.getAttribute("href"), "/login?callbackUrl=%2Fprograms");
  await guest.close();
  results.push("A signed-out visitor on a member page is sent to /login with a return path; the header shows 로그인 on every page.");

  // ── Admin signs in and lands on the admin console ──
  const a = await signIn(admin, `${net}.1`, "/admin");
  await a.page.getByText("Admin", { exact: true }).waitFor();
  assert.equal(await a.page.getByRole("heading", { name: "로그인 필요" }).count(), 0);
  const jar = await cookieMap(a.context);
  for (const name of ["__Secure-mazimi_at", "__Secure-mazimi_rt"]) {
    assert.ok(jar[name]?.httpOnly && jar[name].secure && jar[name].sameSite === "Lax", `${name} is httpOnly, Secure, SameSite=Lax`);
  }
  assert.equal(jar["__Secure-mazimi_at"].path, "/bff");
  assert.equal(jar["__Secure-mazimi_rt"].path, "/auth");
  assert.equal(jar["__Secure-mazimi_oauth"], undefined, "the verifier cookie is consumed");
  assert.deepEqual(await a.page.evaluate(() => [document.cookie, Object.keys(localStorage)]), ["", []], "no token is readable by page scripts");
  assert.ok(a.seen.some(r => r.url === `${base}/bff/api/v1/auth/me`), "the guard asks through the proxy");
  assert.ok(a.seen.every(r => r.authorization === null), "the browser never sends a bearer token");
  results.push("Admin signs in via start/exchange, gets httpOnly path-scoped cookies, and the admin console renders.");

  // ── An authenticated user call succeeds ──
  const dashboard = a.page.waitForResponse(r => r.url() === `${base}/bff/api/v1/dashboard`);
  await a.page.goto(`${base}/dashboard`);
  assert.equal((await dashboard).status(), 200);
  results.push("Dashboard call through /bff returns 200 for the signed-in session.");

  // ── CSRF: the proxy refuses calls without the header or from another origin ──
  assert.equal(await a.page.evaluate(() => fetch("/bff/api/v1/auth/me").then(r => r.status)), 403, "missing CSRF header");
  for (const headers of [{ origin: "https://evil.example" }, { "sec-fetch-site": "cross-site" }]) {
    const res = await a.context.request.get(`${base}/bff/api/v1/auth/me`, { headers: { "x-mazimi-csrf": "1", ...headers } });
    assert.equal(res.status(), 403, `cross-origin call refused: ${JSON.stringify(headers)}`);
  }
  assert.equal(await a.page.evaluate(() => fetch("/bff/api/v1/auth/refresh", { headers: { "X-Mazimi-Csrf": "1" } }).then(r => r.status)), 404, "token routes are not proxied");
  assert.equal(await a.page.evaluate(() => fetch("/bff/api/v1/../../auth/refresh", { headers: { "X-Mazimi-Csrf": "1" } }).then(r => r.status)), 404, "only /api/v1 is reachable");
  results.push("Proxy refuses calls without the CSRF header, with a foreign Origin or Sec-Fetch-Site, and never proxies token routes.");

  // ── Expired access cookie: one refresh, then the call succeeds ──
  const oldRefresh = jar["__Secure-mazimi_rt"].value;
  await a.context.clearCookies({ name: "__Secure-mazimi_at" });
  const refreshed = a.page.waitForResponse(r => r.url() === `${base}/auth/refresh`);
  const retried = a.page.waitForResponse(r => r.url() === `${base}/bff/api/v1/dashboard` && r.status() === 200);
  await a.page.reload();
  assert.equal((await refreshed).status(), 204);
  await retried;
  const rotated = await cookieMap(a.context);
  assert.ok(rotated["__Secure-mazimi_at"], "a new access cookie");
  assert.notEqual(rotated["__Secure-mazimi_rt"].value, oldRefresh, "the refresh token rotated");
  results.push("A missing access cookie triggers one refresh (rotation) and the original call is retried successfully.");

  // ── Member: the admin console says forbidden and the API answers 403 ──
  const m = await signIn(member, `${net}.2`, "/admin");
  await m.page.getByRole("heading", { name: "접근 권한 없음" }).waitFor();
  assert.equal(await m.page.getByRole("link", { name: "데이터 동기화" }).count(), 0);
  assert.equal(await m.page.evaluate(() => fetch("/bff/api/v1/admin/stats", { headers: { "X-Mazimi-Csrf": "1" } }).then(r => r.status)), 403);
  assert.equal(await a.page.evaluate(() => fetch("/bff/api/v1/admin/stats", { headers: { "X-Mazimi-Csrf": "1" } }).then(r => r.status)), 200);
  results.push("Non-admin sees 접근 권한 없음 and gets 403 from admin routes; the admin gets 200.");

  // ── Onboarding saves the profile for a signed-in visitor ──
  await m.page.goto(`${base}/onboarding`);
  await m.page.getByRole("banner").getByRole("button", { name: "로그아웃" }).waitFor();
  await m.page.getByRole("button", { name: "부산 선택", exact: true }).click();
  await m.page.locator("select").selectOption("2001");
  await m.page.getByRole("button", { name: "다음 단계로 이동", exact: true }).click();
  await m.page.waitForURL("**/onboarding/step2");
  await m.page.getByRole("button", { name: "재학", exact: true }).click();
  await m.page.getByRole("button", { name: "맞춤 추천 보기", exact: true }).click();
  await m.page.waitForURL("**/preview");
  await m.page.getByRole("button", { name: "이 조건 저장하고 내 대시보드 보기" }).click();
  await m.page.waitForURL(`${base}/dashboard`);
  assert.equal(sql(`SELECT birth_year||'/'||enrollment_status FROM user_profiles WHERE user_id='${member}'`), "2001/enrolled", "the onboarding answers are the profile");
  results.push("Onboarding's last step saves the profile through /bff for a signed-in visitor and opens the dashboard.");

  // ── Request bodies are capped at 1 MiB, also when chunked ──
  const accessCookie = (await cookieMap(m.context))["__Secure-mazimi_at"].value;
  const post = (body, extra = {}) => fetch(`${base}/bff/api/v1/profile`, { method: "POST", headers: { cookie: `__Secure-mazimi_at=${accessCookie}`, "x-mazimi-csrf": "1", "content-type": "application/json", "x-real-ip": `${net}.9`, ...extra }, body, duplex: "half" });
  const chunked = (bytes) => new ReadableStream({ start(controller) { for (let sent = 0; sent < bytes; sent += 65536) controller.enqueue(new Uint8Array(Math.min(65536, bytes - sent)).fill(32)); controller.close(); } });
  assert.equal((await post(chunked(1024 * 1024 + 1))).status, 413, "a chunked body past 1 MiB is refused");
  assert.equal((await post(" ".repeat(1024 * 1024 + 1))).status, 413, "a declared body past 1 MiB is refused");
  assert.notEqual((await post(chunked(16))).status, 413, "a small chunked body is forwarded");
  results.push("The proxy refuses bodies over 1 MiB whether declared or chunked, and forwards small ones.");

  // ── A refused refresh clears cookies only if the jar still holds that token ──
  const memberRefresh = (await cookieMap(m.context))["__Secure-mazimi_rt"].value;
  const mismatch = await m.context.request.post(`${base}/auth/refresh`, { headers: { "x-mazimi-csrf": "1", "x-mazimi-clear-if": "0".repeat(32) } });
  assert.equal(mismatch.status(), 409, "another token in the jar: nothing is cleared");
  assert.equal((await cookieMap(m.context))["__Secure-mazimi_rt"]?.value, memberRefresh);
  // Revoke the member's token behind the page's back (as another tab's logout would).
  const revoked = await fetch(`${API}/api/v1/auth/logout`, { method: "POST", headers: { "content-type": "application/json", "x-real-ip": `${net}.2` }, body: JSON.stringify({ refresh_token: memberRefresh }) });
  assert.equal(revoked.status, 200);
  await m.context.clearCookies({ name: "__Secure-mazimi_at" });
  const refusedRefresh = m.page.waitForResponse(r => r.url() === `${base}/auth/refresh` && !r.request().headers()["x-mazimi-clear-if"]);
  const conditionalClear = m.page.waitForResponse(r => r.url() === `${base}/auth/refresh` && !!r.request().headers()["x-mazimi-clear-if"]);
  await m.page.reload();
  const refused = await refusedRefresh;
  assert.equal(refused.status(), 401);
  assert.ok(refused.headers()["x-mazimi-session"], "the refusal names the refused token");
  assert.equal((await conditionalClear).status(), 401);
  // The session is over: the member page sends the visitor to sign in again.
  await m.page.waitForURL(`${base}/login?callbackUrl=%2Fdashboard`);
  assert.equal((await cookieMap(m.context))["__Secure-mazimi_rt"], undefined, "the refused token is cleared once confirmed current");
  results.push("A refused refresh keeps the cookies until the page confirms the jar still holds that token; then it signs out and sends the visitor to /login with a return path.");

  // ── Logout revokes the refresh token and closes the console ──
  const loggedOut = a.page.waitForResponse(r => r.url() === `${base}/auth/logout`);
  await a.page.getByRole("banner").getByRole("button", { name: "로그아웃" }).click();
  assert.equal((await loggedOut).status(), 204);
  await a.page.waitForURL(`${base}/`);
  const after = await cookieMap(a.context);
  assert.equal(after["__Secure-mazimi_at"], undefined);
  assert.equal(after["__Secure-mazimi_rt"], undefined);
  assert.equal(sql(`SELECT count(*) FROM refresh_tokens WHERE user_id='${admin}' AND revoked_at IS NULL`), "0", "no live refresh token after logout");
  await a.page.goto(`${base}/admin`);
  await a.page.getByRole("heading", { name: "로그인 필요" }).waitFor();
  results.push("The header 로그아웃 revokes the refresh token at the API, clears both cookies, and the admin guard closes.");

  // ── Hostile inputs on the login routes ──
  const probe = await browser.newContext({ extraHTTPHeaders: { "x-real-ip": `${net}.3` } });
  const returnTo = async raw => {
    await probe.clearCookies();
    const start = await probe.request.get(`${base}/auth/login/kakao?callbackUrl=${encodeURIComponent(raw)}`, { maxRedirects: 0 });
    assert.equal(start.status(), 307);
    assert.ok(start.headers().location.startsWith(`${API}/api/v1/auth/kakao/start?client=site&challenge=`));
    const pending = JSON.parse(decodeURIComponent((await cookieMap(probe))["__Secure-mazimi_oauth"].value));
    assert.equal(pending.v.length, 64);
    return pending.r;
  };
  const hostile = ["//evil.example/x", "/.//evil.com", "/a/..//evil.com", "/%2e//evil.com", "/./\\evil.com", "/\\evil.com", "https://evil.com", "/auth/logout"];
  for (const raw of hostile) assert.equal(await returnTo(raw), "/dashboard", `server drops ${raw}`);
  assert.equal(await returnTo("/programs?region=busan#top"), "/programs?region=busan#top", "a same-site path is kept");
  const loginPage = await probe.newPage();
  for (const raw of hostile) {
    await loginPage.goto(`${base}/login?callbackUrl=${encodeURIComponent(raw)}`);
    const href = await loginPage.getByRole("link", { name: /카카오 1-Tap 로그인/ }).getAttribute("href");
    assert.equal(href, "/auth/login/kakao?callbackUrl=%2Fdashboard", `login page drops ${raw}`);
  }
  await probe.clearCookies();
  const noVerifier = await probe.request.get(`${base}/auth/callback?provider=kakao&code=${"a".repeat(64)}`, { maxRedirects: 0 });
  assert.equal(noVerifier.status(), 303);
  assert.equal(noVerifier.headers().location, "/login?error=restart", "a code without this browser's verifier is refused");
  assert.equal((await cookieMap(probe))["__Secure-mazimi_at"], undefined);
  results.push("Off-site return paths (incl. dot-segment and backslash forms) are dropped on server and page, and a callback without this browser's verifier sets no session.");

  console.log(JSON.stringify({ checked_at: new Date().toISOString(), scope: "Next apps/web + local API + scratch DB; provider mocked", results }, null, 2));
} finally {
  await browser?.close();
  server.kill("SIGTERM");
  sql(`DELETE FROM users WHERE id IN ('${admin}','${member}')`);
}
