// Local Next build + isolated HTTP fixtures. Never calls a production API.
// Uses the existing tools/design Playwright installation; run npm run build first.
import assert from "node:assert/strict";
import net from "node:net";
import path from "node:path";
import { spawn } from "node:child_process";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
const web = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const designRequire = createRequire(path.resolve(web, "../../tools/design/package.json"));
const { chromium } = designRequire("playwright");
const freePort = net.createServer();
await new Promise(resolve => freePort.listen(0, "127.0.0.1", resolve));
const port = freePort.address().port;
await new Promise(resolve => freePort.close(resolve));
const server = spawn(process.execPath, [path.join(web, "node_modules/next/dist/bin/next"), "start", "--hostname", "127.0.0.1", "--port", String(port)], { cwd: web, stdio: "ignore" });
const base = `http://127.0.0.1:${port}`;
let browser;
const delay = ms => new Promise(resolve => setTimeout(resolve, ms));
const deferred = () => { let resolve; const promise = new Promise(done => { resolve = done; }); return { promise, resolve }; };
const failures = [], results = [];
try {
  let ready = false;
  for (let attempt = 0; attempt < 100; attempt++) {
    if (server.exitCode !== null) throw new Error("Local Next server exited before startup");
    try { if ((await fetch(base)).ok) { ready = true; break; } } catch { /* Server is still starting. */ }
    await delay(100);
  }
  assert(ready, "Local Next server should start");
  browser = await chromium.launch({ headless: true, ...(process.env.MAZIMI_CHROMIUM ? { executablePath: process.env.MAZIMI_CHROMIUM } : {}) });
  const context = await browser.newContext({ serviceWorkers: "block" });
  context.on("page", page => page.on("pageerror", error => failures.push(error.message)));
  // The admin guard asks /bff/api/v1/auth/me; `session` is the fixture account.
  // Signed out until the admin section (the header probes it on every page).
  let session = "signed-out";
  await context.route("**/auth/refresh", route => route.fulfill({ status: 401 }));
  const busanRequested = deferred(), busanResponse = deferred(), busanCompleted = deferred();
  const adminRequested = deferred(), adminResponse = deferred(), adminCompleted = deferred();
  let failProgramList = false, failAdmin = true;
  // POST /recommend/preview is a read. The held 수영구 request fails once released.
  const previewBodies = [], suyeongRequested = deferred(), suyeongResponse = deferred();
  async function previewRoute(route, headers) {
    const body = route.request().postDataJSON(); previewBodies.push(body);
    const city = body.city_code ?? null;
    if (city === "26500") { suyeongRequested.resolve(); await suyeongResponse.promise; return route.fulfill({ status: 503, headers, contentType: "application/json", body: JSON.stringify({ error: "Temporary fixture outage" }) }); }
    const item = { program_id: `preview-${city ?? "all"}`, program_type: "scholarship", title: `미리보기 ${city ?? "전체"}`, match_score: 80, reasons: [], missing_checks: [], official_url: null, benefit_amount_monthly: null, benefit_amount_semester: null };
    return route.fulfill({ status: 200, headers, contentType: "application/json", body: JSON.stringify({ total_available: 1, items: [item] }) });
  }
  await context.route("**/api/v1/**", async route => {
    const request = route.request(), url = new URL(request.url());
    const headers = { "access-control-allow-origin": "*", "access-control-allow-headers": "content-type,authorization", "access-control-allow-methods": "GET,OPTIONS" };
    if (request.method() === "OPTIONS") return route.fulfill({ status: 204, headers });
    if (url.pathname.endsWith("/recommend/preview")) return previewRoute(route, headers);
    if (request.method() !== "GET") { failures.push("Unexpected mutation intercepted"); return route.fulfill({ status: 405, headers }); }
    const reply = (body, status = 200) => route.fulfill({ status, headers, contentType: "application/json", body: JSON.stringify(body) });
    if (url.pathname.endsWith("/alerts")) return reply({ total: 0, items: [] });
    // Onboarding prefills a signed-in member's saved nickname.
    if (url.pathname.endsWith("/profile")) return reply({ user_id: "fixture-user", profile: { user_id: "fixture-user", nickname: "저장된닉네임" } });
    if (url.pathname.endsWith("/programs")) {
      const region = url.searchParams.get("region");
      const name = region === "busan" ? "부산 응답 fixture" : region === "daegu" ? "대구 응답 fixture" : "전체 응답 fixture";
      if (region === "busan") { busanRequested.resolve(); await busanResponse.promise; }
      try { await reply(failProgramList ? { error: "Temporary fixture outage" } : { total: 1, items: [{ id: "fixture-program", title: name, program_type: "scholarship", regions: region ? [region] : [], provider_name: "가짜 제공처", deadline_at: null, benefit_amount_monthly: 10000 }] }, failProgramList ? 503 : 200); }
      catch (error) { if (region !== "busan") throw error; /* A superseded fetch is deliberately aborted. */ }
      finally { if (region === "busan") busanCompleted.resolve(); }
      return;
    }
    if (url.pathname.endsWith("/auth/me")) {
      if (session === "old-admin") {
        adminRequested.resolve(); await adminResponse.promise;
        try { await reply({ role: "admin" }); } catch { /* The previous credential's fetch was aborted. */ }
        finally { adminCompleted.resolve(); }
        return;
      }
      if (session === "signed-out") return reply({ error: "Session expired" }, 401);
      return reply(failAdmin && session === "outage" ? { error: "Temporary fixture outage" } : { role: "user" }, failAdmin && session === "outage" ? 503 : 200);
    }
    failures.push(`Unexpected API path ${url.pathname}`);
    return reply({ error: "Unexpected fixture path" }, 404);
  });
  const page = await context.newPage(); page.setDefaultTimeout(15000);
  await page.goto(base + "/programs"); await page.getByText("전체 응답 fixture", { exact: true }).waitFor();
  await page.getByRole("button", { name: "부산", exact: true }).click(); await busanRequested.promise;
  assert.equal(await page.getByText("전체 응답 fixture", { exact: true }).count(), 0, "previous filter must not remain visible while loading");
  await page.getByRole("button", { name: "대구", exact: true }).click(); await page.getByText("대구 응답 fixture", { exact: true }).waitFor();
  busanResponse.resolve(); await busanCompleted.promise; await page.waitForTimeout(100);
  assert.equal(await page.getByText("대구 응답 fixture", { exact: true }).count(), 1);
  assert.equal(await page.getByText("부산 응답 fixture", { exact: true }).count(), 0);
  results.push("Late Busan response cannot replace the newer Daegu filter result; previous results hide while loading.");
  failProgramList = true; await page.getByRole("button", { name: "전체 지역", exact: true }).click(); await page.getByRole("alert").filter({ hasText: "불러오지 못했어요" }).waitFor();
  assert.equal(await page.getByText("등록된 프로그램이 없어요", { exact: true }).count(), 0);
  failProgramList = false; await page.getByRole("button", { name: "다시 시도", exact: true }).click(); await page.getByText("전체 응답 fixture", { exact: true }).waitFor();
  results.push("Program API outage renders an error, and retry succeeds without claiming there are no programs.");
  session = "old-admin"; await page.goto(base + "/admin"); await adminRequested.promise;
  // Another tab signs in as someone else or out; returning focus re-checks.
  const changeSession = async next => { session = next; await page.evaluate(() => window.dispatchEvent(new Event("focus"))); };
  await changeSession("current-member"); await page.getByRole("heading", { name: "접근 권한 없음", exact: true }).waitFor();
  adminResponse.resolve(); await adminCompleted.promise; await page.waitForTimeout(100);
  assert.equal(await page.getByRole("heading", { name: "접근 권한 없음", exact: true }).count(), 1);
  assert.equal(await page.getByRole("link", { name: "데이터 동기화", exact: true }).count(), 0);
  await changeSession("signed-out"); await page.getByRole("heading", { name: "로그인 필요", exact: true }).waitFor();
  results.push("Late admin response cannot authorize a different member; signing out elsewhere closes the admin guard on focus.");
  await changeSession("outage"); await page.getByRole("heading", { name: "권한을 확인하지 못했어요", exact: true }).waitFor();
  failAdmin = false; await page.getByRole("button", { name: "다시 시도", exact: true }).click(); await page.getByRole("heading", { name: "접근 권한 없음", exact: true }).waitFor();
  results.push("Admin authorization outage fails closed and can be retried without confusing a 503 with logout.");
  await page.goto(base + "/onboarding");
  await page.locator("#nickname").fill("마지미");
  await page.waitForTimeout(300);
  assert.equal(await page.locator("#nickname").inputValue(), "마지미", "a late saved nickname never replaces what was typed");
  await page.getByRole("button", { name: "부산 선택", exact: true }).click(); await page.locator("select").selectOption("2000");
  await page.getByRole("button", { name: "다음 단계로 이동", exact: true }).click(); await page.waitForURL("**/onboarding/step2");
  await page.getByRole("button", { name: "재학", exact: true }).click(); await page.getByRole("button", { name: "맞춤 추천 보기", exact: true }).click();
  await page.getByText("미리보기 전체", { exact: true }).waitFor();
  const district = page.getByRole("combobox", { name: "시·군·구 (선택)" }), summary = page.getByTestId("preview-summary");
  assert.equal(await summary.getAttribute("aria-live"), "polite");
  const hint = await district.getAttribute("aria-describedby");
  assert.match(await page.locator(`[id="${hint}"]`).innerText(), /시·군·구 단위 혜택/);
  await district.selectOption("26500"); await suyeongRequested.promise;
  await page.getByRole("status").filter({ hasText: "불러오고 있어요" }).waitFor();
  assert.match(await summary.innerText(), /^부산 \/ 재학/, "the label names the district the shown list was fetched with");
  const stale = page.getByRole("alert").filter({ hasText: "이전 결과" });
  suyeongResponse.resolve(); await stale.waitFor();
  assert.equal(await district.inputValue(), "26500", "a failed refetch keeps the user's choice");
  assert.equal(await page.getByText("미리보기 전체", { exact: true }).count(), 1, "the previous list stays while the error shows");
  await district.selectOption("26410"); await page.getByText("미리보기 26410", { exact: true }).waitFor();
  assert.match(await summary.innerText(), /^부산 금정구 \/ 재학/);
  assert.equal(await stale.count(), 0, "a new choice clears the error");
  assert.deepEqual(previewBodies.map(b => b.city_code ?? null), [null, "26500", "26410"]);
  results.push("Preview district change shows loading, labels the fetched district, and keeps the choice when a refetch fails.");
  assert.deepEqual(failures, []);
  console.log(JSON.stringify({ checked_at: new Date().toISOString(), scope: "Next apps/web only; isolated HTTP fixtures; no production writes", results, errors: failures }, null, 2));
} finally {
  await browser?.close(); server.kill("SIGTERM");
}
