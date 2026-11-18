// Runs exclusively against deterministic browser fixtures; never submits to an institution.
import { chromium } from "playwright";
import fs from "node:fs";
import path from "node:path";
import http from "node:http";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const web = path.resolve(process.argv[2] ?? path.join(root, "releases/mobile-figma"));
const output = path.resolve(process.argv[3] ?? path.join(root, "docs/design/validation/figma-20261001"));
fs.mkdirSync(output, { recursive: true });
const mime = { ".html": "text/html", ".js": "text/javascript", ".css": "text/css", ".png": "image/png", ".otf": "font/otf", ".ttf": "font/ttf" };
const server = http.createServer((req, res) => {
  const local = path.resolve(web, "." + decodeURIComponent(new URL(req.url, "http://localhost").pathname));
  if (!local.startsWith(web + path.sep) && local !== web) { res.writeHead(403).end(); return; }
  const file = [local, local + ".html", path.join(local, "index.html"), path.join(web, "index.html")].find(p => fs.existsSync(p) && fs.statSync(p).isFile());
  res.setHeader("Content-Type", mime[path.extname(file)] ?? "application/octet-stream");
  fs.createReadStream(file).pipe(res);
});
await new Promise(resolve => server.listen(0, "127.0.0.1", resolve));
const base = `http://127.0.0.1:${server.address().port}`;
const programs = [
  { id: "11111111-1111-4111-8111-111111111111", title: "부산 청년 월세 지원", program_type: "policy", provider_name: "부산광역시", summary: "청년의 주거비 부담을 덜어주는 월세 지원 혜택입니다." },
  { id: "22222222-2222-4222-8222-222222222222", title: "한국장학재단 국가장학금", program_type: "scholarship", provider_name: "한국장학재단", summary: "소득과 학적 조건에 따라 대학생의 등록금을 지원합니다." },
  { id: "33333333-3333-4333-8333-333333333333", title: "대학생 소프트웨어 이용 혜택", program_type: "corporate_benefit", provider_name: "테스트 기업", summary: "대학생이 사용할 수 있는 소프트웨어 혜택을 확인해 보세요." },
].map(p => ({ ...p, program_status: "active", application_end_at: new Date(Date.now() + 7 * 86400000).toISOString(), deadline_at: new Date(Date.now() + 7 * 86400000).toISOString(), benefit_amount_monthly: 200000, benefit_amount_semester: null, regions: ["busan"], min_age: 19, max_age: 39, required_documents: [], application_method: "공식 홈페이지에서 온라인 신청", official_url: "https://example.org/notice", application_policy: { can_prepare: true, reason: "", destination: { url: "https://example.org/notice", label: "공식 신청 안내" }, workflow: { no_application_required: false, channels: [], needs_confirmation: false } } }));
const profile = { nickname: "마지미", birth_year: 2002, region_code: "busan", enrollment_status: "재학", school_name: "부산대학교", income_bracket: 3, employment_status: "미취업" };
const consent = { required: { termsAgreed: true, privacyAgreed: true, agreedAt: new Date().toISOString(), termsVersion: "2026-03-21", privacyVersion: "2026-09-26" }, healthInsurance: { agreed: false } };
const results = [], errors = [];
const browser = await chromium.launch({ headless: true, ...(process.env.MAZIMI_CHROMIUM ? { executablePath: process.env.MAZIMI_CHROMIUM } : {}) });
let ctx;
async function context(signedIn = false, width = 375, options = {}) {
  const c = await browser.newContext({ viewport: { width, height: 812 } });
  const state = c.fixtureState = { profile: { ...profile, ...options.profile }, profileStatus: options.profileStatus ?? 200, requests: [], profileWrites: [] };
  if (signedIn) await c.addInitScript(({ consent, draft }) => { if (!location.protocol.startsWith("http")) return; localStorage.setItem("mazimi_consent_v1", JSON.stringify(consent)); sessionStorage.setItem("mazimi_auth_token", "local-ui-fixture-only"); if (draft) localStorage.setItem("mazimi-onboarding-v1", JSON.stringify({ state: draft, version: 0 })); }, { consent, draft: options.draft });
  await c.route("**/api/v1/**", async route => {
    const u = new URL(route.request().url());
    const p = u.pathname.replace("/api/v1", "");
    const headers = { "access-control-allow-origin": "*", "access-control-allow-headers": "content-type,authorization", "access-control-allow-methods": "GET,POST,PUT,DELETE,OPTIONS" };
    if (route.request().method() === "OPTIONS") return route.fulfill({ status: 204, headers });
    state.requests.push({ path: p, query: Object.fromEntries(u.searchParams), method: route.request().method(), body: route.request().postDataJSON() });
    // Per-path response delay (ms) for race checks, e.g. { "/auth/me": 2500 }.
    if (options.delays?.[p]) await new Promise(resolve => setTimeout(resolve, options.delays[p]));
    let json;
    if (p === "/auth/providers") json = { kakao: true, google: false };
    else if (p === "/auth/kakao/exchange") json = { token: "local-ui-fixture-only", refresh_token: "local-ui-refresh", is_new: options.isNew === true };
    else if (p === "/auth/me") json = { id: "fixture-user", nickname: "마지미", image: null };
    else if (p === "/profile") {
      if (route.request().method() === "POST") { const body = route.request().postDataJSON(); state.profileWrites.push(body); state.profile = { ...state.profile, ...body }; state.profileStatus = 200; }
      else if (options.profileGate) await options.profileGate;
      if (state.profileStatus !== 200) return route.fulfill({ status: state.profileStatus, contentType: "application/json", headers, body: JSON.stringify({ error: "Profile not found" }) });
      json = { profile: state.profile, completeness: 80 };
    }
    // Members read GET /recommendations (lib/api.ts); the preview is the older server's fallback.
    else if (p === "/recommend/preview" || p === "/recommendations") json = { total_available: 3, estimated_monthly: 200000, estimated_semester: 0, verified_section_enabled: true, items: programs.map(item => ({ program_id: item.id, title: item.title, program_type: item.program_type, deadline: item.deadline_at, reasons: [item.summary], missing_checks: [], eligibility_status: options.unverified ? "unknown" : "pass", verified: !options.unverified, match_score: 80, benefit_amount_monthly: 200000, benefit_amount_semester: null })) };
    else if (p === "/programs" || p === "/campus/programs") {
      const catalogue = options.regionalFixture ? [...programs, { ...programs[0], id: "44444444-4444-4444-8444-444444444444", title: "용인시 전용 테스트 지원", regions: ["gyeonggi"] }] : programs;
      const items = catalogue.filter(item => (!u.searchParams.get("program_type") || item.program_type === u.searchParams.get("program_type")) && (!u.searchParams.get("region") || item.regions.includes(u.searchParams.get("region"))));
      // options.categories: a server with the provider/field axis (lib/catalogue.ts probes for provider_category).
      json = { items: options.categories ? items.map(item => ({ ...item, provider_category: item.program_type === "corporate_benefit" ? "corporate" : "policy" })) : items, total: items.length, has_next: false, page: 1 };
    }
    else if (p === "/campus/universities") json = { items: [{ id: "pnu", name: "부산대학교", aliases: ["부산대"], region: "busan" }] };
    else if (p === "/programs/corporate-categories") json = { items: [{ code: "dev", label: "개발·소프트웨어", count: 2 }, { code: "design", label: "디자인·창작", count: 1 }], total: 3 };
    else if (/^\/programs\/[^/]+$/.test(p)) json = programs.find(item => item.id === p.split("/").pop()) ?? programs[0];
    else if (p.includes("/saved")) json = { items: programs };
    else if (p === "/assistant/agent") json = { enabled: options.agent ?? true, model_planner_available: false, retention_days: 30, run_timeout_secs: 60, goals: [] };
    else if (p.includes("notification-preferences")) json = { deadline: true, new_program: false, profile_update: true, channels: { in_app: true, push: false, kakao: false, email: false } };
    else json = { items: [], total: 0 };
    await route.fulfill({ status: 200, contentType: "application/json", headers, body: JSON.stringify(json) });
  });
  c.on("page", page => page.on("pageerror", e => errors.push(e.message)));
  return c;
}
// RegionField (components/RegionField.tsx) is a picker sheet: 시·도, then 시·군·구 or "전체".
async function pickBusan(page) {
  const field = page.getByRole("button", { name: /^거주지역/ });
  // A miss names where the walk is (URL, screen text, last API calls) instead of a bare timeout.
  try { await field.waitFor({ timeout: 30000 }); } catch (error) {
    const requests = page.context().fixtureState?.requests.slice(-8).map(r => `${r.method} ${r.path}`) ?? [];
    throw new Error(`pickBusan: no 거주지역 field at ${page.url()}\nscreen: ${(await page.evaluate(() => document.body.innerText).catch(() => "")).replace(/\s+/g, " ").slice(0, 500)}\nrequests: ${requests.join(", ")}`, { cause: error });
  }
  await field.click(); await page.getByRole("radio", { name: /^부산광역시/ }).click();
  const whole = page.getByRole("radio", { name: /부산광역시 전체/ }); if (await whole.count()) await whole.click();
}
// BirthDatePicker: three Figma dropdowns, 연도 / 월 / 일 (step2 asks for a birth date, not an age).
async function pickBirth(page, year, month = 3, day = 15) {
  for (const [field, value] of [["연도", `${year}년`], ["월", `${month}월`], ["일", `${day}일`]]) { await page.getByRole("button", { name: `생년월일 ${field} 선택`, exact: true }).click(); await page.getByRole("radio", { name: value, exact: true }).click(); }
}
async function shot(page, name) { await page.waitForTimeout(500); await page.evaluate(() => document.fonts.ready); await page.screenshot({ path: path.join(output, name + ".png") }); }
try {
  ctx = await context(); const page = await ctx.newPage(); page.setDefaultTimeout(15000);
  await page.goto(base); await page.getByText("약관 동의", { exact: true }).waitFor();
  const start = page.getByRole("button", { name: "동의하고 시작하기", exact: true });
  assert(await start.isDisabled()); await shot(page, "consent");
  await page.getByRole("link", { name: "이용약관 동의 보기" }).click(); await page.waitForURL("**/terms"); await page.getByText("이용약관", { exact: true }).first().waitFor(); await page.goBack();
  await page.getByRole("checkbox", { name: "이용약관 동의", exact: true }).click(); assert(await start.isDisabled());
  await page.getByRole("checkbox", { name: "개인정보처리방침 동의", exact: true }).click(); assert(await start.isEnabled()); await start.click();
  await page.getByRole("button", { name: "카카오로 시작하기" }).waitFor();
  assert(await page.getByRole("button", { name: "Google 로그인 준비 중" }).isDisabled()); await shot(page, "login");
  results.push("Fresh install still requires both legal consents; policy links and explicit login/guest choice work.");
  // A guest goes from login straight to the tabs (app/login.tsx); the recommendation entry asks them to log in.
  await page.getByRole("button", { name: "로그인 없이 둘러보기" }).click();
  await page.getByText("로그인하면 내 조건에 맞는 혜택 수를 알려드려요", { exact: true }).waitFor(); await shot(page, "home-guest");
  assert.equal(new URL(page.url()).pathname, "/"); assert.equal(await page.getByRole("textbox", { name: "닉네임 (필수)", exact: true }).count(), 0, "no onboarding for a guest");
  await page.getByRole("button", { name: "맞춤 혜택 후보 확인", exact: true }).click(); await page.getByText(/로그인하고 조건을 저장하면/).first().waitFor();
  results.push("A guest lands on the tabs without onboarding; 맞춤 혜택 asks them to log in.");
  await ctx.close();
  // Onboarding is where 홈 sends a member with no saved conditions (goRecommendation in app/(tabs)/index.tsx).
  ctx = await context(true, 375, { profileStatus: 404 }); const onboarding = await ctx.newPage(); onboarding.setDefaultTimeout(15000);
  await onboarding.goto(base); await onboarding.getByRole("button", { name: "맞춤 혜택 확인", exact: true }).click();
  await onboarding.getByRole("textbox", { name: "닉네임 (필수)", exact: true }).fill("디자인 테스트"); await onboarding.getByRole("button", { name: "다음", exact: true }).click();
  await pickBusan(onboarding); await shot(onboarding, "onboarding-region"); await onboarding.getByRole("button", { name: "다음", exact: true }).click();
  // Without a date 다음 stays on the step and says why (OnboardingFrame onBlocked).
  await onboarding.getByRole("button", { name: "다음", exact: true }).click(); await onboarding.getByRole("alert").first().waitFor(); assert.match(onboarding.url(), /step2/);
  // Under 만 14 (lib/birthDate.ts MIN_MAN_AGE): the youngest listed year with a birthday still ahead this year.
  const today = new Date();
  if (!(today.getMonth() === 11 && today.getDate() === 31)) {
    await pickBirth(onboarding, today.getFullYear() - 14, 12, 31);
    await onboarding.getByText("만 14세 이상만 이용할 수 있어요.", { exact: true }).waitFor();
    await onboarding.getByRole("button", { name: "다음", exact: true }).click(); assert.match(onboarding.url(), /step2/, "under 14 stays on the step");
  }
  await pickBirth(onboarding, new Date().getFullYear() - 24); await shot(onboarding, "onboarding-birth"); await onboarding.getByRole("button", { name: "다음", exact: true }).click();
  await onboarding.getByRole("radio", { name: /^재학 중/ }).click(); await shot(onboarding, "onboarding-school");
  await onboarding.getByRole("button", { name: "대학 선택", exact: true }).click(); await onboarding.getByRole("radio", { name: /부산대학교/ }).click();
  await onboarding.getByRole("button", { name: "다음", exact: true }).click(); await shot(onboarding, "onboarding-income");
  // 재학 leaves 취업 상태 empty: the field reads 선택 안 함, nothing is pre-shown, and the save carries no answer.
  await onboarding.getByRole("button", { name: "취업 상태 선택, 선택 안 함", exact: true }).waitFor();
  assert.equal(await onboarding.getByRole("button", { name: "취업 상태 선택, 미취업", exact: true }).count(), 0, "onboarding: 미취업 is not pre-shown");
  await onboarding.getByRole("button", { name: "맞춤 혜택 보기", exact: true }).click();
  // Entered from 홈 (mode=edit): conditions only, no 회원가입을 환영합니다.
  await onboarding.waitForFunction(() => !location.pathname.includes("onboarding"));
  assert.equal(ctx.fixtureState.profileWrites.length, 1); assert.equal(ctx.fixtureState.profileWrites[0].region_code, "busan");
  // H1: 맞춤 혜택 보기 with 취업 상태 untouched saves no answer.
  assert.equal(ctx.fixtureState.profileWrites[0].employment_status, null, "onboarding: an untouched 취업 상태 is sent as null");
  await onboarding.goto(base); await onboarding.getByRole("button", { name: "맞춤 혜택 확인", exact: true }).filter({ hasText: "월 20만원" }).waitFor();
  await onboarding.getByRole("tab", { name: "대학, 제공 주체", exact: true }).first().waitFor({ timeout: 8000 }); await shot(onboarding, "home");
  await onboarding.getByRole("button", { name: "맞춤 혜택 확인", exact: true }).click(); await onboarding.getByText("추천 혜택 목록", { exact: true }).waitFor(); await shot(onboarding, "recommend");
  results.push("홈 sends a member without conditions to onboarding; an empty or under-14 birth date stays on the step; region, birth date, school and university are saved and show the campus tab.");
  // M6: with amounts only on 확인 필요 rows the hero keeps the count frame, never "지원금 최대".
  await ctx.close(); ctx = await context(true, 375, { unverified: true }); const countHero = await ctx.newPage();
  // Wait for the loaded count ("3건"): the count frame also shows while the recommendations load.
  await countHero.goto(base); await countHero.getByRole("button", { name: "맞춤 혜택 확인", exact: true }).filter({ hasText: "나에게 맞는 혜택 후보" }).filter({ hasText: "3건" }).waitFor();
  assert.equal(await countHero.getByText("지원금 최대", { exact: true }).count(), 0, "no amount from unconfirmed rows");
  results.push("홈 hero quotes an amount only from 조건 확인됨 rows; unconfirmed amounts leave the count frame.");
  await ctx.close(); ctx = await context(true); const member = await ctx.newPage();
  for (const route of ["explore", "manage", "vault", "profile", "notifications", "applications", "help", "terms", "privacy-policy", "account", "programs/" + programs[0].id]) {
    await member.goto(base + "/" + route); await member.waitForTimeout(450); await shot(member, route.startsWith("programs") ? "detail" : route);
    assert.equal(await member.getByText("앱에 문제가 발생했어요", { exact: true }).count(), 0, route);
    const overflow = await member.evaluate(() => document.documentElement.scrollWidth > innerWidth + 1); assert.equal(overflow, false, route + " horizontal overflow");
  }
  await member.setViewportSize({ width: 320, height: 640 });
  for (const route of ["explore", "onboarding/school", "login"]) { await member.goto(base + "/" + route); await member.waitForTimeout(300); assert.equal(await member.evaluate(() => document.documentElement.scrollWidth > innerWidth + 1), false, route + " narrow overflow"); }
  await shot(member, "login-320");
  results.push("Main tabs, detail, notifications and account/legal screens render at 375px; key screens also fit 320px.");
  await ctx.close();
  // 기업혜택 browses by category chips (GET /programs/corporate-categories), not one chip per company.
  // The list request each step checks is the last one sent; the one-row capability probe is not a list read.
  const lastList = () => ctx.fixtureState.requests.filter(r => r.path === "/programs" && r.query.per_page !== "1").at(-1)?.query;
  const lastCounts = () => ctx.fixtureState.requests.filter(r => r.path === "/programs/corporate-categories").at(-1)?.query;
  // A server without the category axis (probe finds no provider_category): no chips, no counts read, no chip sent.
  ctx = await context(true); const older = await ctx.newPage(); older.setDefaultTimeout(15000);
  await older.goto(base + "/explore"); await older.getByRole("tab", { name: "기업혜택, 제공 주체", exact: true }).click(); await older.waitForTimeout(600);
  assert.equal(lastList()?.program_type, "corporate_benefit", "older server lists by program_type");
  assert.equal(lastList()?.corporate_category, undefined);
  assert.equal(await older.getByRole("button", { name: /^(전체|개발·소프트웨어|디자인·창작)/ }).count(), 0, "no chips on an older server");
  assert.equal(lastCounts(), undefined, "no counts read on an older server");
  assert(!ctx.fixtureState.requests.some(r => r.path === "/brands"), "no brand list read");
  await ctx.close();
  ctx = await context(true, 375, { categories: true }); const corporate = await ctx.newPage(); corporate.setDefaultTimeout(15000);
  await corporate.goto(base + "/explore"); await corporate.getByRole("tab", { name: "기업혜택, 제공 주체", exact: true }).click();
  await corporate.getByRole("button", { name: "디자인·창작 1", exact: true }).waitFor();
  assert.equal(await corporate.getByRole("button", { name: "전체", exact: true }).count(), 1, "전체 chip");
  assert.equal(await corporate.getByRole("button", { name: /모든 기업|테스트 기업/ }).count(), 0, "no company chips");
  assert(!ctx.fixtureState.requests.some(r => r.path === "/brands"), "no brand list read");
  assert.equal(lastList().provider, "corporate"); assert.equal(lastList().corporate_category, undefined);
  await corporate.getByRole("button", { name: "디자인·창작 1", exact: true }).click(); await corporate.waitForTimeout(400);
  assert.equal(lastList().corporate_category, "design", "chip filters the list"); assert.equal(lastList().provider, "corporate");
  await shot(corporate, "explore-corporate");
  await corporate.getByRole("button", { name: "혜택 검색 열기", exact: true }).click();
  await corporate.getByRole("textbox", { name: "혜택 검색", exact: true }).fill("테스트 기업"); await corporate.waitForTimeout(800);
  assert.equal(lastList().q, "테스트 기업", "company search still runs"); assert.equal(lastList().corporate_category, "design", "inside the chip");
  assert.equal(lastCounts().q, "테스트 기업", "counts follow the search");
  await corporate.getByRole("tab", { name: "정책, 제공 주체", exact: true }).click(); await corporate.waitForTimeout(400);
  assert.equal(lastList().provider, "policy"); assert.equal(lastList().corporate_category, undefined, "other tabs send no chip");
  results.push("기업혜택 shows 전체 plus category chips with counts, no company chips, and none on a server without the category axis; a chip sends corporate_category, search runs inside it with matching counts, and other tabs drop it.");
  await ctx.close();
  // 비서 off (GET /assistant/agent enabled false): no 비서 slide, and 다음 할 일 still opens from 홈.
  ctx = await context(true, 375, { agent: false }); const agentOff = await ctx.newPage(); agentOff.setDefaultTimeout(15000);
  await agentOff.goto(base); await agentOff.getByRole("button", { name: "저장한 혜택, 다음 할 일", exact: true }).waitFor(); await agentOff.waitForTimeout(400);
  assert.equal(await agentOff.getByRole("button", { name: "마지미 비서", exact: true }).count(), 0, "no 비서 slide while it is off");
  await agentOff.getByRole("button", { name: "저장한 혜택, 다음 할 일", exact: true }).click(); await agentOff.waitForURL("**/briefing");
  await agentOff.getByText("저장한 혜택, 다음 할 일", { exact: true }).waitFor();
  results.push("With 비서 off a member sees no 비서 slide and still opens 다음 할 일 from 홈.");
  await ctx.close();
  let releaseProfile;
  ctx = await context(true, 375, { regionalFixture: true, profileGate: new Promise(resolve => { releaseProfile = resolve; }), draft: { region: "gyeonggi", age: "31", birthYear: 1995, schoolName: "이전 대학", enrollmentStatus: "재학" } });
  const delayed = await ctx.newPage();
  await delayed.goto(base); await delayed.getByRole("button", { name: "맞춤 혜택 확인", exact: true }).waitFor();
  await delayed.waitForTimeout(250);
  assert(ctx.fixtureState.requests.some(r => r.path === "/profile"));
  // Before the saved profile arrives only region-free reads run (the capability probe and the nationwide 마감 임박 list):
  // nothing is scoped by the old guest draft's region, and no recommendation is asked for.
  const early = ctx.fixtureState.requests;
  assert.equal(early.filter(r => r.path === "/programs" && r.query.region).length, 0, "member catalogue must wait for authoritative profile");
  assert.equal(early.filter(r => r.path === "/recommendations" || r.path === "/recommend/preview").length, 0, "recommendations wait for the saved profile");
  const released = ctx.fixtureState.requests.length; releaseProfile();
  await delayed.getByRole("button", { name: "맞춤 혜택 확인", exact: true }).filter({ hasText: "월 20만원" }).waitFor();
  assert.equal(await delayed.getByText(/월 지원금 예상 합계/).count(),0);
  await delayed.getByRole("tab", { name: "장학금, 제공 주체", exact: true }).click();
  await delayed.waitForTimeout(250);
  // Every catalogue read after the profile arrives is scoped to the saved region (the one-row capability probe is not a catalogue read).
  const memberRequests = ctx.fixtureState.requests.slice(released).filter(r => r.path === "/programs" && r.query.per_page !== "1");
  assert(memberRequests.length >= 2); assert(memberRequests.every(r => r.query.region === "busan"));
  // A member's list is GET /recommendations, read from the saved profile on the server; no preview is built from the device draft.
  assert(ctx.fixtureState.requests.slice(released).some(r => r.path === "/recommendations"), "member recommendations read");
  assert.equal(ctx.fixtureState.requests.filter(r => r.path === "/recommend/preview").length, 0, "no preview from the old guest draft");
  assert.equal(await delayed.getByText("용인시 전용 테스트 지원", { exact: true }).count(), 0);
  await delayed.goto(base + "/explore"); await delayed.getByText("용인시 전용 테스트 지원", { exact: true }).waitFor();
  await delayed.getByRole("button", { name: "대상 필터", exact: true }).click();
  await delayed.getByRole("radio", { name: "내 지역 (부산)", exact: true }).click();
  await delayed.waitForTimeout(250); assert.equal(await delayed.getByText("용인시 전용 테스트 지원", { exact: true }).count(), 0);
  assert(ctx.fixtureState.requests.some(r => r.path === "/programs" && r.query.region === "busan"));
  results.push("Delayed member profile never uses a previous guest draft; home, category and local explore requests use Busan and hide the Yongin fixture.");
  await ctx.close();
  ctx = await context(true, 375, { profile: { region_code: "gyeonggi", city_code: "yongin", school_year: 3, school_type: "university", major_group: "engineering" }, draft: { nickname: "old", region: "daegu", age: "20", birthYear: 2006, schoolName: "", enrollmentStatus: "" } });
  const editing = await ctx.newPage();
  await editing.goto(base + "/profile-edit");
  await editing.getByText("내 정보 수정", { exact: true }).waitFor();
  await editing.waitForFunction(() => document.querySelector('input[aria-label="닉네임 (필수)"]')?.value === "마지미");
  await editing.getByRole("textbox", { name: "닉네임 (필수)", exact: true }).fill("수정 회원"); await pickBusan(editing);
  assert.match(await editing.getByRole("button", { name: "생년월일 연도 선택", exact: true }).innerText(), new RegExp(`${profile.birth_year}년`));
  await editing.getByRole("radio", { name: /^해당 없음/ }).click(); await editing.getByRole("button", { name: "저장", exact: true }).click();
  await editing.waitForFunction(() => !location.pathname.includes("profile-edit"));
  assert.equal(ctx.fixtureState.profileWrites.length, 1);
  const savedProfile = ctx.fixtureState.profileWrites[0];
  assert.equal(savedProfile.region_code, "busan"); assert.equal(savedProfile.city_code, null); assert.equal(savedProfile.nickname, "수정 회원");
  for (const field of ["school_name", "school_year", "school_type", "major_group"]) assert.equal(savedProfile[field], null, field);
  assert.equal(await editing.getByRole("tab", { name: "대학, 제공 주체", exact: true }).count(), 0);
  results.push("Editing conditions starts from the saved member profile, saves nickname, clears old city/school metadata and immediately hides the campus tab.");
  // H1 on 내 정보 수정: what 저장 sends with 취업 상태 untouched, picked, then cleared (and the 학적 follow-up toggled after).
  await ctx.close(); ctx = await context(true, 375, { profile: { enrollment_status: "graduated", employment_status: null } });
  const employment = await ctx.newPage();
  const openEdit = async () => { await employment.goto(base + "/profile-edit"); await employment.waitForFunction(() => document.querySelector('input[aria-label="닉네임 (필수)"]')?.value === "마지미"); };
  const saveEdit = async count => { await employment.getByRole("button", { name: "저장", exact: true }).click(); await employment.waitForFunction(() => !location.pathname.includes("profile-edit")); assert.equal(ctx.fixtureState.profileWrites.length, count); return ctx.fixtureState.profileWrites[count - 1]; };
  const employmentField = async name => { await employment.getByRole("button", { name: `취업 상태 선택, ${name}`, exact: true }).waitFor(); assert.equal(await employment.getByRole("button", { name: /^취업 상태 선택,/ }).count(), 1); };
  await openEdit(); await employment.getByRole("radio", { name: /^휴학 중/ }).click();
  await employmentField("선택 안 함"); // 휴학 pre-shows nothing
  assert.equal((await saveEdit(1)).employment_status, null, "profile edit: an untouched 취업 상태 is sent as null");
  await openEdit(); await employment.getByRole("button", { name: /^취업 상태 선택,/ }).click(); await employment.getByRole("radio", { name: "미취업", exact: true }).click();
  assert.equal((await saveEdit(2)).employment_status, "unemployed", "profile edit: the pick is sent");
  await openEdit(); await employment.getByRole("button", { name: "취업 상태 선택, 미취업", exact: true }).click(); await employment.getByRole("radio").filter({ hasText: /^선택 안 함$/ }).click(); // the menu row (the 지원구간 slider step of that name has no text)
  await employmentField("선택 안 함");
  // The 과정 follow-up of 휴학 on and off: 선택 안 함 stays, nothing reappears.
  for (let i = 0; i < 2; i++) { await employment.getByRole("radio", { name: "대학 (전문대 포함)", exact: true }).click(); await employmentField("선택 안 함"); }
  assert.equal((await saveEdit(3)).employment_status, null, "profile edit: 선택 안 함 clears and stays");
  results.push("취업 상태 at the save boundary: 재학/휴학 pre-show nothing (선택 안 함); onboarding and 내 정보 수정 send null when untouched, the pick when picked, null when cleared, also after the 학적 follow-up is toggled.");
  // A real first sign-up: Kakao returns with a code and the exchange says is_new. The steps end on
  // 회원가입을 환영합니다 (28023:1751); 시작하기 goes home (samePath next).
  await ctx.close(); ctx = await context(false, 375, { profileStatus: 404, isNew: true });
  await ctx.addInitScript(consentValue => { if (!location.protocol.startsWith("http")) return; localStorage.setItem("mazimi_consent_v1", JSON.stringify(consentValue)); sessionStorage.setItem("mazimi_oauth_verifier_kakao", "local-ui-verifier"); }, consent);
  const signup = await ctx.newPage();
  await signup.goto(base + "/login?code=local-ui-code"); await signup.getByRole("textbox", { name: "닉네임 (필수)", exact: true }).fill("첫 회원"); await signup.getByRole("button", { name: "다음", exact: true }).click();
  await pickBusan(signup); await signup.getByRole("button", { name: "다음", exact: true }).click();
  await pickBirth(signup, new Date().getFullYear() - 24); await signup.getByRole("button", { name: "다음", exact: true }).click(); await signup.getByRole("button", { name: "다음", exact: true }).click(); await signup.getByRole("button", { name: "맞춤 혜택 보기", exact: true }).click();
  await signup.getByText("회원가입을 환영합니다 !", { exact: true }).waitFor(); await shot(signup, "onboarding-welcome");
  await signup.getByRole("button", { name: "시작하기", exact: true }).click();
  await signup.getByRole("button", { name: "맞춤 혜택 확인", exact: true }).waitFor();
  assert(!new URL(signup.url()).pathname.includes("onboarding"), "시작하기 leaves onboarding for home");
  results.push("A first sign-up (is_new) ends onboarding on 회원가입을 환영합니다 and 시작하기 goes home; an existing member never sees it.");
  await ctx.close(); ctx = await context(true, 375, { profileStatus: 404 }); const freshMember = await ctx.newPage();
  await freshMember.goto(base + "/onboarding"); await freshMember.getByRole("textbox", { name: "닉네임 (필수)", exact: true }).fill("새 회원"); await freshMember.getByRole("button", { name: "다음", exact: true }).click();
  await pickBusan(freshMember); await freshMember.getByRole("button", { name: "다음", exact: true }).click();
  await pickBirth(freshMember, new Date().getFullYear() - 24); await freshMember.getByRole("button", { name: "다음", exact: true }).click(); await freshMember.getByRole("button", { name: "다음", exact: true }).click(); await freshMember.getByRole("button", { name: "맞춤 혜택 보기", exact: true }).click();
  // Opened directly (no sign-up in this session, like 신청 도우미's "저장된 프로필 확인하기"): no 회원가입을 환영합니다.
  await freshMember.getByRole("button", { name: "맞춤 혜택 확인", exact: true }).waitFor();
  assert.equal(await freshMember.getByText("회원가입을 환영합니다 !", { exact: true }).count(), 0, "an existing member never sees the welcome");
  assert.equal(ctx.fixtureState.profileWrites.length, 1); assert.equal(ctx.fixtureState.profileWrites[0].region_code, "busan");
  await freshMember.goto(base + "/profile"); await freshMember.getByRole("button", { name: "로그아웃", exact: true }).click(); await freshMember.getByText("로그인하면 관심 혜택과 신청 이력, 내 서류를 모아볼 수 있어요.", { exact: true }).waitFor();
  assert.equal(await freshMember.evaluate(() => sessionStorage.getItem("mazimi_auth_token")), null);
  // Logout removes the draft, or leaves it empty: either way no personal condition stays on the device.
  const clearedDraft = await freshMember.evaluate(() => JSON.parse(localStorage.getItem("mazimi-onboarding-v1") ?? "null")?.state ?? null);
  if (clearedDraft) { assert.equal(clearedDraft.region, ""); assert.equal(clearedDraft.schoolName, ""); assert.equal(clearedDraft.nickname, ""); }
  results.push("A new member with HTTP 404 Profile not found can save onboarding; logout removes the session and personal condition draft.");
  // Onboarding race (CI, PR #26): the nickname step was interactive before /auth/me and /profile
  // settled, so a 다음 tap could land on the button as it turned busy and do nothing. Both orders:
  // while either request is pending the step is not editable; once both settle the walk goes on.
  for (const [label, delays, profileStatus] of [["/auth/me slower than /profile", { "/auth/me": 2500, "/profile": 300 }, 404], ["/profile slower than /auth/me", { "/auth/me": 300, "/profile": 2500 }, 404], ["saved profile arrives late", { "/auth/me": 300, "/profile": 2500 }, 200]]) {
    await ctx.close(); ctx = await context(true, 375, { profileStatus, delays }); const racing = await ctx.newPage();
    await racing.goto(base + "/onboarding", { waitUntil: "domcontentloaded" });
    const nickname = racing.getByRole("textbox", { name: "닉네임 (필수)", exact: true });
    await nickname.waitFor();
    assert.equal(await nickname.isEditable(), false, `${label}: the nickname step waits for the session and profile`);
    await racing.waitForFunction(() => { const input = document.querySelector('input[aria-label="닉네임 (필수)"]'); return input && !input.disabled && !input.readOnly; }, null, { timeout: 15000 });
    if (profileStatus === 200) assert.equal(await nickname.inputValue(), "마지미", `${label}: the saved nickname is filled in, not overwritten later`);
    else await nickname.fill("경합 회원");
    await racing.getByRole("button", { name: "다음", exact: true }).click();
    await pickBusan(racing);
  }
  results.push("The onboarding nickname step waits for /auth/me and /profile in either order, then a 다음 tap always reaches 거주지역; a saved nickname is kept.");
  assert.deepEqual(errors, []);
  const report = { checked_at: new Date().toISOString(), environment: "Chromium, local Expo web export, deterministic API fixtures; no production writes or actual OAuth", results, errors, native_device_tested: false };
  fs.writeFileSync(process.argv[3] ? path.join(output,"report.json") : path.join(root, "docs/audits/mobile-figma-ui-20261001.json"), JSON.stringify(report, null, 2) + "\n"); console.log(JSON.stringify(report, null, 2));
} finally { await ctx?.close(); await browser.close(); await new Promise(resolve => server.close(resolve)); }
