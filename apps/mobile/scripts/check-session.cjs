// Exercise the shipped auth store and API client with isolated storage and HTTP.
// No real credentials, OAuth providers or network requests are used.
const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const ts = require("typescript");
const { create } = require("zustand");
// lib/apiBaseUrl.ts refuses a release bundle without an API origin.
process.env.EXPO_PUBLIC_API_URL ??= "https://api.fixture.invalid";

function fixture() {
  const memory = new Map(), loaded = new Map();
  let transport = () => { throw new Error("Unexpected HTTP request"); };
  let resets = 0, deviceToken = "ExponentPushToken[fixture-device]";
  const storage = { getItem: key => memory.get(key) ?? null, setItem: (key, value) => memory.set(key, value), removeItem: key => memory.delete(key) };
  function load(relative) {
    const filename = path.resolve(__dirname, "..", relative);
    if (loaded.has(filename)) return loaded.get(filename).exports;
    const module = { exports: {} }; loaded.set(filename, module);
    const source = ts.transpileModule(fs.readFileSync(filename, "utf8"), { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText;
    function dependency(name) {
      if (name === "zustand") return { create };
      if (name === "react-native") return { Platform: { OS: "web" } };
      if (name === "expo-secure-store") return {};
      if (name === "./onboarding") return { useOnboardingStore: { getState: () => ({ reset: () => { resets++; } }) } };
      if (name === "@/lib/tokenStorage") return load("lib/tokenStorage.ts");
      if (name === "../store/auth" || name === "@/store/auth") return load("store/auth.ts");
      if (name === "./api") return load("lib/api.ts");
      if (name === "./apiBaseUrl" || name === "../lib/apiBaseUrl") return load("lib/apiBaseUrl.ts");
      if (name === "./errorCopy") return load("lib/errorCopy.ts");
      if (name === "./displayText") return load("lib/displayText.ts");
      if (name === "../constants/displayLabels") return load("constants/displayLabels.ts");
      if (name === "./districts.generated") return load("constants/districts.generated.ts");
      if (name === "./notifications") return { thisDevicePushToken: async () => deviceToken };
      if (name === "./firstSignup") return load("lib/firstSignup.ts");
      throw new Error(`Unmocked dependency ${name}`);
    }
    new Function("require", "module", "exports", "fetch", "sessionStorage", source)(dependency, module, module.exports, (...args) => transport(...args), storage);
    return module.exports;
  }
  const { useAuthStore: store } = load("store/auth.ts");
  const { api, ApiRequestError, resolveApplyUrl, boundedRead } = load("lib/api.ts");
  const { signOut } = load("lib/session.ts");
  const tokens = load("lib/tokenStorage.ts").tokenStorage;
  store.setState({ token: "expired-fixture", refreshToken: "refresh-fixture", user: { id: "member-a", nickname: "A", image: null }, isLoading: false });
  return { store, retryTimedOutRefresh: load("store/auth.ts").retryTimedOutRefresh, api, ApiRequestError, resolveApplyUrl, boundedRead, signOut, tokens, memory, setFetch: fn => { transport = fn; }, setDeviceToken: token => { deviceToken = token; }, resets: () => resets };
}
const json = (status, body) => new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } });
const deferred = () => { let resolve; const promise = new Promise(done => { resolve = done; }); return { promise, resolve }; };

test("simultaneous 401s share one rotating refresh exchange", async () => {
  const f = fixture(), gate = deferred(); let refreshes = 0, firstRequests = 0;
  f.setFetch(async (url, options) => {
    if (url.endsWith("/auth/refresh")) { refreshes++; await gate.promise; return json(200, { token: "renewed-fixture", refresh_token: "rotated-fixture" }); }
    if (options.headers.Authorization === "Bearer expired-fixture") { firstRequests++; if (firstRequests === 3) gate.resolve(); return json(401, { error: "Expired" }); }
    assert.equal(options.headers.Authorization, "Bearer renewed-fixture"); return json(200, { items: [] });
  });
  await Promise.all([f.api.getSaved(), f.api.getMyApplications(), f.api.getProfile()]);
  assert.equal(refreshes, 1); assert.equal(f.resets(), 0); assert.equal(f.store.getState().user.id, "member-a");
});

for (const status of [403, 404, 503]) test(`a ${status} after refresh retains its status and session`, async () => {
  const f = fixture();
  f.setFetch(async (url, options) => url.endsWith("/auth/refresh") ? json(200, { token: "renewed-fixture", refresh_token: "rotated-fixture" }) : options.headers.Authorization === "Bearer expired-fixture" ? json(401, {}) : json(status, { error: "Fixture failure" }));
  await assert.rejects(f.api.getProfile(), error => error instanceof f.ApiRequestError && error.status === status);
  assert.equal(f.store.getState().user.id, "member-a"); assert.equal(f.resets(), 0);
});

test("temporary refresh outage retains the session and saved conditions", async () => {
  const f = fixture(); f.setFetch(async url => json(url.endsWith("/auth/refresh") ? 503 : 401, {}));
  await assert.rejects(f.api.getProfile());
  assert.equal(f.store.getState().refreshToken, "refresh-fixture"); assert.equal(f.resets(), 0);
});

test("rejected refresh logs out and deletes pending application intent", async () => {
  const f = fixture(); await f.tokens.setItemAsync("mazimi_pending_application", "fixture-intent");
  f.setFetch(async () => json(401, {}));
  await assert.rejects(f.api.getProfile(), error => error.status === 401);
  assert.equal(f.store.getState().user, null); assert.equal(f.memory.get("mazimi_pending_application"), undefined); assert.equal(f.resets(), 1);
});

test("late refresh cannot resurrect a logged-out session", async () => {
  const f = fixture(), gate = deferred();
  f.setFetch(async () => { await gate.promise; return json(200, { token: "late-fixture", refresh_token: "late-refresh-fixture" }); });
  const refresh = f.store.getState().refreshAccessToken();
  await f.store.getState().logout(); gate.resolve();
  assert.equal(await refresh, false); assert.equal(f.store.getState().token, null);
  assert.equal(f.memory.get("mazimi_auth_token"), undefined); assert.equal(f.memory.get("mazimi_refresh_token"), undefined);
});

test("old account 401 cannot refresh or log out a newer account", async () => {
  const f = fixture(), gate = deferred(); let requests = 0;
  f.setFetch(async () => { requests++; await gate.promise; return json(401, {}); });
  const request = f.api.getProfile();
  f.store.setState({ token: "new-account-fixture", refreshToken: "new-account-refresh", user: { id: "member-b", nickname: "B", image: null } });
  gate.resolve(); await assert.rejects(request, error => error.status === 401);
  assert.equal(f.store.getState().user.id, "member-b"); assert.equal(requests, 1); assert.equal(f.resets(), 0);
});

test("startup server outage does not delete stored credentials", async () => {
  const f = fixture(); await f.tokens.setItemAsync("mazimi_auth_token", "stored-fixture"); await f.tokens.setItemAsync("mazimi_refresh_token", "stored-refresh-fixture");
  f.store.setState({ token: null, refreshToken: null, user: null }); f.setFetch(async () => json(503, {}));
  await f.store.getState().restoreSession();
  assert.equal(f.memory.get("mazimi_auth_token"), "stored-fixture"); assert.equal(f.memory.get("mazimi_refresh_token"), "stored-refresh-fixture");
});

test("invalid restored session clears its personal draft and application intent", async () => {
  const f = fixture(); await f.tokens.setItemAsync("mazimi_auth_token", "stored-fixture"); await f.tokens.setItemAsync("mazimi_refresh_token", "stored-refresh-fixture"); await f.tokens.setItemAsync("mazimi_pending_application", "fixture-intent");
  f.store.setState({ token: null, refreshToken: null, user: null }); f.setFetch(async () => json(401, {}));
  await f.store.getState().restoreSession();
  assert.equal(f.memory.get("mazimi_auth_token"), undefined); assert.equal(f.memory.get("mazimi_refresh_token"), undefined); assert.equal(f.memory.get("mazimi_pending_application"), undefined); assert.equal(f.resets(), 1);
});

// Sign-out: the mock records every request; assertions run after signOut so a
// failure inside the bounded, error-swallowing revocation step cannot hide.
const route = url => url.slice(url.indexOf("/api/"));

test("sign-out with an expired access token never exchanges the refresh token", async () => {
  const f = fixture(); const calls = [];
  f.setFetch(async (url, options) => {
    calls.push({ path: route(url), method: options.method, auth: options.headers.Authorization, body: options.body });
    if (url.endsWith("/push/register")) return json(401, { error: "Expired" });
    if (url.endsWith("/auth/logout")) return json(200, { logged_out: true });
    return json(200, { token: "renewed-fixture", refresh_token: "rotated-fixture" });
  });
  await f.signOut();
  assert.deepEqual(calls.map(c => c.path), ["/api/v1/push/register", "/api/v1/auth/logout"], "no /auth/refresh during sign-out");
  assert.equal(calls[0].method, "DELETE"); assert.equal(calls[0].auth, "Bearer expired-fixture");
  assert.deepEqual(JSON.parse(calls[0].body), { token: "ExponentPushToken[fixture-device]" }, "only this device's push token is removed");
  // /auth/logout also drops this device's push row: with an expired access
  // token the DELETE above is refused (review L-4).
  assert.deepEqual(JSON.parse(calls[1].body), { refresh_token: "refresh-fixture", push_token: "ExponentPushToken[fixture-device]" });
  assert.equal(f.store.getState().user, null); assert.equal(f.store.getState().refreshToken, null);
  assert.equal(f.memory.get("mazimi_refresh_token"), undefined); assert.equal(f.resets(), 1);
});

test("sign-out revokes the refresh token that a concurrent request rotated meanwhile", async () => {
  const f = fixture(); const revoked = [];
  f.setFetch(async (url, options) => {
    if (url.endsWith("/push/register")) {
      // Another request's refresh exchange completes while the push token is being removed.
      f.store.setState({ token: "renewed-fixture", refreshToken: "rotated-fixture" });
      return json(200, {});
    }
    if (url.endsWith("/auth/logout")) { revoked.push(JSON.parse(options.body).refresh_token); return json(200, { logged_out: true }); }
    throw new Error(`Unexpected request ${route(url)}`);
  });
  await f.signOut();
  assert.deepEqual(revoked, ["rotated-fixture"], "the latest refresh token is the one revoked");
  assert.equal(f.store.getState().token, null); assert.equal(f.store.getState().user, null);
});

test("a server that never answers the push revocation does not block local sign-out", async () => {
  const f = fixture(); const calls = [];
  f.setFetch((url, options) => {
    calls.push(route(url));
    if (url.endsWith("/push/register")) return new Promise(() => {});
    assert.equal(options.method, "POST"); return Promise.resolve(json(200, { logged_out: true }));
  });
  await f.signOut();
  assert.deepEqual(calls, ["/api/v1/push/register", "/api/v1/auth/logout"], "the refresh token is still revoked after the bounded wait");
  assert.equal(f.store.getState().user, null); assert.equal(f.memory.get("mazimi_auth_token"), undefined);
});

test("sign-out without a device push token skips the push revocation and still revokes the refresh token", async () => {
  const f = fixture(); const calls = []; f.setDeviceToken(null);
  f.setFetch(async (url, options) => { calls.push({ path: route(url), method: options.method }); return json(200, { logged_out: true }); });
  await f.signOut();
  assert.deepEqual(calls, [{ path: "/api/v1/auth/logout", method: "POST" }]);
  assert.equal(f.store.getState().user, null);
});

test("a missing application is a new application, independent of server error wording", async () => {
  const f = fixture(); f.setFetch(async () => json(404, { error: "Application not found" }));
  assert.equal(await f.api.getApplicationStatus("11111111-1111-4111-8111-111111111111"), null);
  assert.equal(f.resets(), 0);
});

test("assistant question is a scoped read with explicit AI consent and no user ID", async () => {
  const f = fixture(); let called = 0;
  f.setFetch(async (url, options) => {
    called++;
    assert.ok(url.endsWith('/programs/test-program/assistant'));
    assert.equal(options.method, 'POST');
    assert.ok(options.signal instanceof AbortSignal);
    assert.deepEqual(JSON.parse(options.body), { question: '필요한 서류는?', allow_ai_processing: false });
    return json(200, { mode: 'rules', alert_enabled: false });
  });
  const response = await f.api.askBenefitAssistant('test-program', '필요한 서류는?', false);
  assert.equal(response.mode, 'rules'); assert.equal(called, 1);
});

test("assistant overview and briefing are GET requests and do not initiate generation", async () => {
  const f = fixture(); const calls = [];
  f.setFetch(async (url, options) => { calls.push(url); assert.equal(options.method, undefined); assert.equal(options.body, undefined); return json(200, {}); });
  await f.api.getBenefitGuidance('test-program'); await f.api.getAssistantBriefing();
  assert.equal(calls.length, 2);
  assert.ok(calls[0].endsWith('/programs/test-program/assistant'));
  assert.ok(calls[1].endsWith('/assistant/briefing'));
});

test("scenario discovery and comparison send only bounded selectors, never profile mutations", async () => {
  const f = fixture(); const calls = [];
  f.setFetch(async (url, options) => { calls.push({ url, options }); return json(200, {}); });
  const controller = new AbortController();
  await f.api.getAssistantBundle('independent_living', controller.signal);
  await f.api.compareBenefits(['a', 'b'], controller.signal);
  assert.ok(calls[0].url.endsWith('/assistant/bundles?scenario=independent_living'));
  assert.equal(calls[0].options.method, undefined);
  assert.equal(calls[0].options.body, undefined);
  assert.equal(calls[0].options.signal, controller.signal);
  assert.ok(calls[1].url.endsWith('/assistant/compare'));
  assert.equal(calls[1].options.method, 'POST');
  assert.equal(calls[1].options.signal, controller.signal);
  assert.deepEqual(JSON.parse(calls[1].options.body), { program_ids: ['a', 'b'] });
  assert.equal(calls.length, 2);
});

test("apply links open only the re-validated server destination, never the raw official URL", () => {
  const { resolveApplyUrl } = fixture();
  const at = (url, origin = new URL(url).origin) => ({ application_policy: { destination: { url, origin } } });
  assert.equal(resolveApplyUrl(at("https://www.gov.kr/apply?id=1")), "https://www.gov.kr/apply?id=1");
  assert.equal(resolveApplyUrl(at("https://www.gov.kr:443/apply")), "https://www.gov.kr/apply");
  assert.equal(resolveApplyUrl({ official_url: "https://www.gov.kr/", application_policy: { destination: null } }), null);
  assert.equal(resolveApplyUrl({ official_url: "https://www.gov.kr/" }), null);
  for (const url of [
    "http://www.gov.kr/", "https://user:pw@www.gov.kr/", "https://www.gov.kr:8443/", "https://127.0.0.1/",
    "https://[::1]/", "https://localhost/", "https://intranet/", "https://admin.localhost/", "https://printer.local/",
    "javascript:alert(1)",
  ]) assert.equal(resolveApplyUrl(at(url, "https://www.gov.kr")), null, url);
  assert.equal(resolveApplyUrl(at("https://evil.example.com/", "https://www.gov.kr")), null);
});

// Grounded explanations: pure copy helpers, loaded without React Native.
function loadGrounding() {
  const filename = path.resolve(__dirname, "..", "lib/grounding.ts");
  const source = ts.transpileModule(fs.readFileSync(filename, "utf8"), { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText;
  const module = { exports: {} };
  new Function("require", "module", "exports", source)(name => { throw new Error(`Unmocked dependency ${name}`); }, module, module.exports);
  return module.exports;
}

test("no evidence UI: users never see source labels, 공고 데이터, 지역 코드 or 확인일 (product decision)", () => {
  const read = file => fs.readFileSync(path.resolve(__dirname, "..", file), "utf8");
  assert.equal(fs.existsSync(path.resolve(__dirname, "..", "components/EvidenceList.tsx")), false, "EvidenceList is gone");
  const grounding = loadGrounding();
  for (const name of ["evidenceCaption", "evidenceDate", "evidenceFor"]) assert.equal(grounding[name], undefined, name);
  const walk = dir => fs.readdirSync(path.resolve(__dirname, "..", dir), { withFileTypes: true }).flatMap(entry => entry.isDirectory() ? walk(path.join(dir, entry.name)) : /\.tsx?$/.test(entry.name) ? [path.join(dir, entry.name)] : []);
  const offenders = [];
  for (const file of [...walk("app"), ...walk("components")]) {
    const source = read(file);
    // Classes, not single strings: evidence objects, source notes and labels,
    // collection / check / review times, and pipeline status or origins.
    for (const pattern of [
      /EvidenceList/, /\.evidence\b/, /\bsource_label\b/, /source\.note/, /공고 데이터/, /지역 ?코드/,
      /원문 확인일/, /확인일 \$\{/, /운영자 대조/, /자료 수집/, /수집 시각/, /공식 출처/, /공고 원문:/, /공식 페이지 원문/,
      /\b(last_synced_at|captured_at|approved_at|checked_at|source_checked_at|verified_at)\b/, /checkedAt/, /10분 안에 확인/,
      /template_hash\.slice/, /policy(\?)?\.reason/, /\.origin\}/,
    ]) if (pattern.test(source)) offenders.push(`${file}: ${pattern}`);
  }
  assert.deepEqual(offenders, []);
  // What stays: the honest status words. The repeated "최종 조건은 공식 공고에서" line is gone
  // from the detail, its sheet and the benefit assistant (Figma 34065:10914 has no such line).
  const detail = read("app/programs/[id].tsx");
  assert.match(detail, /확인 필요/);
  for (const file of ["app/programs/[id].tsx", "components/BenefitSummarySheet.tsx", "app/benefit-assistant.tsx"]) assert.doesNotMatch(read(file), /FINAL_CHECK_LINE/, file);
  assert.match(read("lib/displayCopy.ts"), /FINAL_CHECK_LINE = "최종 조건은 공식 공고에서 확인해 주세요"/);
  assert.doesNotMatch(detail, /application_policy\?\.reason/, "no pipeline status (공식 신청 주소를 확인하고 있어요)");
  assert.match(read("app/apply-assistant.tsx"), /\?\.explanation \|\| FINAL_CHECK_LINE/, "apply-assistant falls back to the quiet line, not policy.reason");
  assert.match(read("app/benefit-assistant.tsx"), /\{sourceUrl \? <View style=\{s\.card\}><Pressable accessibilityRole="link"/, "the official link only with a link that passes the re-check");
  const card = read("components/ApplicationResourcesCard.tsx");
  assert.match(card, /!resources\.notice && !resources\.templates\.length\)\) return null/, "the card shows only when there is something to open");
  assert.doesNotMatch(card, /확인 중|다시 확인하고 있어요|조회 중/);
});

test("grounded follow-up sends only the last topic, and search sends only the question", async () => {
  const f = fixture(); const calls = [];
  f.setFetch(async (url, options) => { calls.push({ url, options }); return json(200, { mode: 'rules' }); });
  await f.api.askBenefitAssistant('test-program', '그럼 휴학생은?', false, 'documents');
  await f.api.searchAssistant('부산 사는 휴학생 장학금');
  assert.deepEqual(JSON.parse(calls[0].options.body), { question: '그럼 휴학생은?', allow_ai_processing: false, previous_topic: 'documents' });
  assert.ok(calls[1].url.endsWith('/assistant/search'));
  assert.equal(calls[1].options.method, 'POST');
  assert.deepEqual(JSON.parse(calls[1].options.body), { question: '부산 사는 휴학생 장학금' });
});

test("quoted passages highlight by code point and drafts never become final", () => {
  const { highlightSegments, followUpSuggestions, verdictBadge, parseAnswerStream, finalSummary, streamErrorMessage } = loadGrounding();
  assert.deepEqual(highlightSegments("😀 신분증, 주민등록 등본", [{ start: 2, end: 5 }]).map(s => [s.text, s.marked]),
    [["😀 ", false], ["신분증", true], [", 주민등록 등본", false]]);
  assert.deepEqual(highlightSegments("abc", [{ start: 2, end: 9 }, { start: -1, end: 1 }]), [{ text: "abc", marked: false }], "out-of-range spans are ignored");
  assert.deepEqual(followUpSuggestions("documents"), ["어떻게 신청해?", "언제까지 신청해?"]);
  assert.equal(followUpSuggestions(undefined).length, 3);
  assert.deepEqual(verdictBadge({ check: "enrollment", status: "pass", registered: true, summary: "", asked_value: "휴학", asked_status: "fail" }), { label: "‘휴학’ 등록 조건에 없음", tone: "fail" });
  assert.deepEqual(verdictBadge({ check: "region", status: "unknown", registered: false, summary: "" }), { label: "등록된 조건 없음 · 확인 필요", tone: "unknown" });
  // "pass" is not verified eligibility, so it never reads as the user's result.
  assert.deepEqual(verdictBadge({ check: "region", status: "pass", registered: true, summary: "" }), { label: "일부 조건 맞음", tone: "pass" });
  const first = parseAnswerStream('{"type":"start"}\n{"type":"delta","text":"신분"}\nnot json\n{"type":"de');
  assert.deepEqual(first.events, [{ type: "start" }, { type: "delta", text: "신분" }]);
  assert.equal(first.rest, '{"type":"de');
  const final = parseAnswerStream(first.rest + 'lta","text":"증"}\n{"type":"final","valid":false,"text":"월 50만원","violation":"new_number"}\n');
  assert.equal(final.events.length, 2);
  assert.deepEqual(finalSummary(final.events[1]), { text: "근거 문장을 확인해 주세요", grounded: false });
  assert.deepEqual(finalSummary({ type: "final", valid: true, text: "신분증이 필요해요." }), { text: "신분증이 필요해요.", grounded: true });
  assert.equal(streamErrorMessage(429, "busy"), "지금은 요약이 붐벼요. 근거 문장을 먼저 확인해 주세요.");
  assert.equal(streamErrorMessage(503), "지금은 AI 요약을 쓸 수 없어요. 근거 문장을 확인해 주세요.");
});

// AI benefit agent: pure helpers (lib/agent.ts) and request shapes.
function loadAgent() {
  const filename = path.resolve(__dirname, "..", "lib/agent.ts");
  const source = ts.transpileModule(fs.readFileSync(filename, "utf8"), { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText;
  const module = { exports: {} };
  new Function("require", "module", "exports", source)(name => { throw new Error(`Unmocked dependency ${name}`); }, module, module.exports);
  return module.exports;
}
const PROGRAM = "11111111-1111-4111-8111-111111111111";
const RESOURCE = "22222222-2222-4222-8222-222222222222";

test("agent requests carry no user id; approval sends only the card's hash", async () => {
  const f = fixture(); const calls = [];
  f.setFetch(async (url, options) => { calls.push({ url, options }); return json(200, { run_id: "r", status: "received", existing: false }); });
  await f.api.sendAgentGoal("conv-1", "마감 정리해줘", "agent-key-00000001", PROGRAM, []);
  await f.api.approveAgentConfirmation("c-1", "a".repeat(64), "agent-key-00000002");
  await f.api.rejectAgentConfirmation("c-1");
  await f.api.deleteAgentConversation("conv-1");
  assert.ok(calls[0].url.endsWith("/assistant/conversations/conv-1/messages"));
  assert.equal(calls[0].options.headers["Idempotency-Key"], "agent-key-00000001");
  assert.deepEqual(JSON.parse(calls[0].options.body), { goal: "마감 정리해줘", program_id: PROGRAM });
  assert.ok(calls[1].url.endsWith("/assistant/confirmations/c-1/approve"));
  assert.deepEqual(JSON.parse(calls[1].options.body), { payload_hash: "a".repeat(64) });
  assert.equal(calls[2].options.method, "POST"); assert.deepEqual(JSON.parse(calls[2].options.body), {});
  assert.equal(calls[3].options.method, "DELETE");
  for (const call of calls) assert.ok(!String(call.options.body ?? "").includes("user_id"));
});

test("agent NDJSON parsing keeps partial lines and drops foreign ones", () => {
  const { parseAgentEvents } = loadAgent();
  const parsed = parseAgentEvents('{"type":"status","status":"planning"}\nnot json\n{"type":"evil","x":1}\n{"type":"tool","seq":0');
  assert.deepEqual(parsed.events, [{ type: "status", status: "planning" }]);
  assert.equal(parsed.rest, '{"type":"tool","seq":0');
});

test("agent timeline keys steps by plan index and call seq, and done replaces it with the stored run", () => {
  const { reduceTimeline, initialTimeline, isResting } = loadAgent();
  let t = reduceTimeline(initialTimeline, { type: "plan", planner: "fast_path", goal_kind: "enable_alerts", steps: [
    { index: 0, tool: "check_eligibility", label: "내 조건 확인", permission: "read" },
    { index: 1, tool: "set_alert", label: "알림 설정 제안", permission: "confirm_write" }] });
  assert.deepEqual(t.steps.map(s => [s.key, s.status, s.write]), [["plan-0", "pending", false], ["plan-1", "pending", true]]);
  t = reduceTimeline(t, { type: "tool", seq: 0, index: 0, total: 2, tool: "check_eligibility", label: "내 조건 확인", status: "running" });
  t = reduceTimeline(t, { type: "tool", seq: 0, index: 0, total: 2, tool: "check_eligibility", label: "내 조건 확인", status: "ok" });
  t = reduceTimeline(t, { type: "tool", seq: 1, index: 1, total: 2, tool: "set_alert", label: "알림 설정 제안", status: "awaiting_confirmation" });
  t = reduceTimeline(t, { type: "tool", seq: 2, index: 1, total: 2, tool: "set_alert", label: "알림 설정 제안", status: "skipped" });
  assert.deepEqual(t.steps.map(s => [s.key, s.status]), [["call-0", "ok"], ["call-1", "awaiting_confirmation"], ["call-2", "skipped"]]);
  assert.equal(new Set(t.steps.map(s => s.key)).size, t.steps.length, "stable unique keys");
  t = reduceTimeline(t, { type: "awaiting_confirmation", count: 1 });
  assert.equal(t.status, "awaiting_confirmation"); assert.ok(isResting(t.status));
  t = reduceTimeline(t, { type: "backgrounded", run_id: "r" });
  assert.equal(t.backgrounded, true);
  const run = { status: "completed", planner: "fast_path", confirmations: [], steps: [{ seq: 0, tool: "deadlines", label: "마감 일정 정리", permission: "read", status: "ok" }] };
  t = reduceTimeline(t, { type: "done", status: "completed", run });
  assert.deepEqual(t.steps.map(s => s.key), ["call-0"]); assert.equal(t.status, "completed");
});

test("agent confirmation cards decide only while pending and unexpired", () => {
  const { canDecide, confirmationStatusLabel, approvalBody } = loadAgent();
  const now = Date.parse("2026-10-12T00:00:00Z");
  const card = { id: "c", payload_hash: "b".repeat(64), status: "pending", expires_at: "2026-10-12T00:05:00Z" };
  assert.equal(canDecide(card, now), true);
  assert.equal(canDecide({ ...card, expires_at: "2026-10-11T23:59:00Z" }, now), false);
  assert.equal(canDecide({ ...card, status: "executed" }, now), false);
  assert.equal(confirmationStatusLabel({ ...card, status: "stale" }, now), "그 사이 상태가 바뀌어 실행하지 않았어요");
  assert.deepEqual(approvalBody(card), { payload_hash: "b".repeat(64) });
});

// Capture 2026-11-04 #08: per-tool notes in the app repeated the server's card.
// The notes are mapped from the card's own data, the same for every tool; the
// facts' single statement is checked over the real answer and card on the
// server (agent_tools_cases a_form_card_turn_says_each_fact_once...).
test("agent confirmation card notes come only from the card's data, the same for every tool", () => {
  const { confirmationNotes, confirmationStatusLabel } = loadAgent();
  const now = Date.parse("2026-10-12T00:00:00Z");
  const card = { id: "c", payload_hash: "b".repeat(64), status: "pending", expires_at: "2026-10-12T00:05:00Z", reversible: true, program_id: PROGRAM, title: "t", action: "a", before: "b", after: "c" };
  const tools = ["open_form_fill", "open_official_application", "profile_patch", "add_to_calendar", "set_alert", "record_application_status"];
  for (const tool of tools) {
    assert.deepEqual(confirmationNotes({ ...card, tool }, true), [], `${tool}: no app-side text`);
    assert.deepEqual(confirmationNotes({ ...card, tool, note: "서버 문장" }, true), ["서버 문장"], `${tool}: the server's note`);
    assert.deepEqual(confirmationNotes({ ...card, tool, note: "서버 문장" }, false), [], `${tool}: a decided card drops the note`);
    assert.deepEqual(confirmationNotes({ ...card, tool, reversible: false }, false), ["이 기록은 나중에 되돌리기 어려워요."]);
  }
  assert.deepEqual(confirmationNotes({ ...card, tool: "set_alert", note: null }, true), [], "an older card without a note");
  // A decidable card has no state line: the answer says nothing changes before 승인.
  assert.equal(confirmationStatusLabel({ ...card, tool: "set_alert" }, now), null);
  assert.equal(confirmationStatusLabel({ ...card, tool: "set_alert", expires_at: "2026-10-11T00:00:00Z" }, now), "확인 시간이 지났어요");
});

test("agent actions route only to fixed app screens with valid ids, never URLs", () => {
  const { actionRoute, goalChips, newIdempotencyKey } = loadAgent();
  assert.equal(actionRoute({ type: "open_program", label: "", payload: { program_id: PROGRAM } }), `/programs/${PROGRAM}`);
  assert.equal(actionRoute({ type: "prepare_application", label: "", payload: { program_id: PROGRAM } }), `/application/${PROGRAM}`);
  assert.equal(actionRoute({ type: "open_form_fill", label: "", payload: { program_id: PROGRAM, resource_id: RESOURCE } }), `/form-fill?programId=${PROGRAM}&resourceId=${RESOURCE}`);
  assert.equal(actionRoute({ type: "open_program", label: "", payload: { program_id: "https://evil.example" } }), null);
  assert.equal(actionRoute({ type: "open_url", label: "", payload: { url: "https://evil.example" } }), null);
  const chips = goalChips([
    { type: "goal", label: "마감 정리", payload: { goal: "마감 임박한 것 정리해줘", goal_kind: "deadline_digest" } },
    { type: "goal", label: "이 혜택 준비", payload: { goal: "이 혜택 신청 준비 도와줘", goal_kind: "prepare_program" } },
    { type: "pick_program", label: "장학금", payload: { goal: "이 혜택 신청 준비 도와줘", program_id: PROGRAM } },
    { type: "pick_program", label: "bad", payload: { goal: "x", program_id: "not-an-id" } },
    { type: "confirm", label: "승인", payload: { confirmation_id: "c" } },
  ], false);
  assert.deepEqual(chips.map(c => c.key), ["goal:deadline_digest", `pick:${PROGRAM}`]);
  // Round 12: "신청 상태 기록" is a route chip to the status picker; the
  // plain prepare action stays the result card's button, and a bad id is no chip.
  const routes = goalChips([
    { type: "prepare_application", label: "신청 상태 기록", payload: { program_id: PROGRAM, focus: "status" } },
    { type: "prepare_application", label: "신청 준비 화면 열기", payload: { program_id: PROGRAM } },
    { type: "prepare_application", label: "x", payload: { program_id: "bad", focus: "status" } },
  ], true);
  assert.deepEqual(routes.map(c => [c.label, c.route]), [["신청 상태 기록", `/application/${PROGRAM}`]]);
  // Understanding step 0: "체크리스트" routes to the checklist screen, where
  // a task is checked only by the user's own tap.
  const checklist = goalChips([
    { type: "prepare_application", label: "체크리스트", payload: { program_id: PROGRAM, focus: "checklist" } },
    { type: "prepare_application", label: "x", payload: { program_id: "bad", focus: "checklist" } },
  ], true);
  assert.deepEqual(checklist.map(c => [c.label, c.route]), [["체크리스트", `/apply-assistant?programId=${PROGRAM}`]]);
  assert.deepEqual(goalChips([{ type: "edit_profile", label: "프로필 수정", payload: {} }], false).map(c => c.route), ["/profile-edit"]);
  const key = newIdempotencyKey();
  assert.match(key, /^[A-Za-z0-9_-]{8,128}$/);
  assert.notEqual(key, newIdempotencyKey());
});

test("journey buttons ask for each handoff by name, about the programme on screen", () => {
  const { JOURNEY_GOALS, goalChips } = loadAgent();
  for (const goal of [JOURNEY_GOALS.form, JOURNEY_GOALS.official]) {
    assert.ok(goal.startsWith("이 혜택 "), goal);
    // A button goal is dropped with no programme on screen, kept with one.
    const chip = [{ type: "goal", label: "x", payload: { goal } }];
    assert.equal(goalChips(chip, false).length, 0);
    assert.equal(goalChips(chip, true).length, 1);
  }
  assert.match(JOURNEY_GOALS.official, /공식 신청 페이지 열어줘$/);
});

test("agent result readers ignore malformed tool data", () => {
  const { benefitRows, checklistRows, verdictOf, formLinks } = loadAgent();
  const search = { tool: "search_benefits", data: { results: [{ program_id: PROGRAM, title: "장학금", eligibility_status: "pass", deadline: "2026-11-01" }, { program_id: "x", title: "bad" }, null, "str"] } };
  assert.deepEqual(benefitRows(search).map(r => r.programId), [PROGRAM]);
  assert.deepEqual(checklistRows({ tool: "get_preparation", data: { tasks: [{ id: "information", title: "신청 정보 준비", done: true }, { id: 3 }] } }), [{ id: "information", title: "신청 정보 준비", done: true }]);
  assert.equal(verdictOf({ tool: "check_eligibility", data: { status: "pass" } }), "pass");
  assert.equal(verdictOf({ tool: "check_eligibility", data: { status: "approved" } }), null);
  assert.deepEqual(formLinks({ tool: "open_form_fill", data: { forms: [{ resource_id: RESOURCE, label: "신청서" }, { resource_id: "../etc" }] } }), [{ resourceId: RESOURCE, label: "신청서" }]);
});

test("agent polling backs off and outlasts the server run timeout", () => {
  const { pollDelaysMs, pollBudgetMs } = loadAgent();
  const delays = pollDelaysMs();
  assert.equal(delays[0], 2000);
  assert.ok(delays.every(d => d <= 10_000));
  assert.ok(delays.reduce((a, b) => a + b, 0) >= 300_000, "covers the 300 s run timeout");
  assert.ok(delays.length < 60, "bounded number of requests");
  // The budget follows the server's run timeout (bounded like the server's setting).
  const sum = ms => pollDelaysMs(ms).reduce((a, b) => a + b, 0);
  for (const secs of [30, 300, 600]) assert.ok(sum(pollBudgetMs(secs)) >= secs * 1000, `covers ${secs} s`);
  assert.equal(pollBudgetMs(undefined), pollBudgetMs(300));
  // The server bounds its setting; the client trusts it and falls back to 300 s.
  assert.equal(pollBudgetMs(900), 930_000);
  assert.equal(pollBudgetMs(Number.NaN), pollBudgetMs(300));
  assert.equal(pollBudgetMs(0), pollBudgetMs(300));
});

test("agent polling fetches once more after the last wait", async () => {
  const { pollUntilResting } = loadAgent();
  const sleep = async () => {};
  // Finishes during the final wait: the last fetch sees it.
  let fetches = 0;
  const finishLate = await pollUntilResting({
    delays: [1, 1, 1], resting: () => false, aborted: () => false, sleep,
    fetchStatus: async () => (++fetches > 3 ? "completed" : "planning"),
  });
  assert.equal(finishLate, "rested");
  assert.equal(fetches, 4);
  const never = await pollUntilResting({
    delays: [1, 1], resting: () => false, aborted: () => false, sleep, fetchStatus: async () => "planning",
  });
  assert.equal(never, "still_running");
  const known = await pollUntilResting({
    delays: [1], resting: () => true, aborted: () => false, sleep, fetchStatus: async () => { throw new Error("no fetch"); },
  });
  assert.equal(known, "rested");
  const aborted = await pollUntilResting({
    delays: [1], resting: () => false, aborted: () => true, sleep, fetchStatus: async () => "planning",
  });
  assert.equal(aborted, "aborted");
});

test("agent shows 생각 중 at once, a model wait note, and a timeout note", () => {
  const { reduceTimeline, initialTimeline, runStatusLabels, progressNote } = loadAgent();
  assert.equal(runStatusLabels[initialTimeline.status], "생각 중", "before the first event");
  assert.equal(progressNote(initialTimeline), null);
  let t = reduceTimeline(initialTimeline, { type: "status", status: "planning" });
  assert.equal(runStatusLabels[t.status], "생각 중");
  t = reduceTimeline(t, { type: "planning", planner: "model" });
  assert.match(progressNote(t), /1분/);
  // The 요청 취소 button sits beside the note; the note does not say it again.
  assert.doesNotMatch(progressNote(t), /취소/);
  t = reduceTimeline(t, { type: "backgrounded", run_id: "r" });
  assert.match(progressNote(t), /알림으로/);
  assert.match(progressNote({ ...t, status: "timed_out" }), /멈췄어요/);
  assert.equal(progressNote({ ...t, status: "completed" }), null);
});

// Loads the shipped district constants, onboarding store and condition saver
// with an in-memory AsyncStorage and a recording profile API.
function loadConditions() {
  const loaded = new Map(), saves = [], memory = new Map();
  const storage = { getItem: async key => memory.get(key) ?? null, setItem: async (key, value) => { memory.set(key, value); }, removeItem: async key => { memory.delete(key); } };
  const api = { getProfile: async () => ({ profile: { region_code: "busan", city_code: "26500" } }), saveProfile: async body => { saves.push(body); return { profile: body }; } };
  const files = { "@/constants/regions": "constants/regions.ts", "./districts.generated": "constants/districts.generated.ts", "@/store/onboarding": "store/onboarding.ts", "./profileCodes": "lib/profileCodes.ts", "@/lib/profileCodes": "lib/profileCodes.ts", "@/lib/optionalAnswers": "lib/optionalAnswers.ts", "./optionalAnswers": "lib/optionalAnswers.ts", "./birthDate": "lib/birthDate.ts", "./writeEffects": "lib/writeEffects.ts" };
  function load(relative) {
    const filename = path.resolve(__dirname, "..", relative);
    if (loaded.has(filename)) return loaded.get(filename).exports;
    const module = { exports: {} }; loaded.set(filename, module);
    const source = ts.transpileModule(fs.readFileSync(filename, "utf8"), { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText;
    new Function("require", "module", "exports", source)(name => {
      if (files[name]) return load(files[name]);
      if (name === "zustand") return { create };
      if (name === "zustand/middleware") return require("zustand/middleware");
      if (name === "@react-native-async-storage/async-storage") return { default: storage, __esModule: true };
      if (name === "./api") return { api, ApiRequestError: class extends Error {} };
      if (name === "@/store/auth") return { useAuthStore: { getState: () => ({ user: { id: "member-a" } }) } };
      throw new Error(`Unmocked dependency ${name}`);
    }, module, module.exports);
    return module.exports;
  }
  return { regions: load("constants/regions.ts"), store: load("store/onboarding.ts").useOnboardingStore, conditions: load("lib/conditions.ts"), saves };
}

function withEnv(env, fn) {
  const before = Object.fromEntries(Object.keys(env).map(key => [key, process.env[key]]));
  Object.assign(process.env, env);
  try { return fn(); } finally { for (const [key, value] of Object.entries(before)) { if (value === undefined) delete process.env[key]; else process.env[key] = value; } }
}
const withQuestions = fn => withEnv({ EXPO_PUBLIC_PROFILE_CONDITION_QUESTIONS_ENABLED: "true" }, fn);
const OPTIONAL_KEYS = ["homeless_household", "marital_status", "newlywed", "children_count", "income_median_band"];
const skippedAnswers = { homeless_household: null, marital_status: null, newlywed: null, children_count: null, income_median_band: null };
const conditionDraft = () => ({ nickname: "", region: "busan", cityCode: "", birthYear: new Date().getFullYear() - 25, enrollmentStatus: "", schoolName: "", employmentStatus: "", incomeBracket: null });
const queryClient = { setQueryData() {}, invalidateQueries: async () => {} };

// The question component with every native dependency stubbed: only the
// flag decides whether it renders at all.
function loadOptionalConditions() {
  const filename = path.resolve(__dirname, "..", "components/OptionalConditions.tsx");
  const source = ts.transpileModule(fs.readFileSync(filename, "utf8"), { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX } }).outputText;
  const optional = { exports: {} };
  const optionalSource = ts.transpileModule(fs.readFileSync(path.resolve(__dirname, "..", "lib/optionalAnswers.ts"), "utf8"), { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText;
  new Function("require", "module", "exports", optionalSource)(() => ({}), optional, optional.exports);
  const module = { exports: {} };
  new Function("require", "module", "exports", source)(name => {
    if (name === "@/lib/optionalAnswers") return optional.exports;
    if (name === "react-native") return { StyleSheet: { create: x => x }, Linking: {}, Pressable: "Pressable", View: "View" };
    if (name === "@/constants/theme") return { colors: {} };
    if (name === "./OnboardingFrame") return { onboardingStyle: {} };
    if (name === "react/jsx-runtime") return { jsx: (type, props) => ({ type, props }), jsxs: (type, props) => ({ type, props }) };
    return {};
  }, module, module.exports);
  return module.exports.default;
}

test("default flags: only 소득 구간 is asked, with neutral copy; saves send only income_median_band", async () => {
  const rendered = loadOptionalConditions()({ value: skippedAnswers, onChange() {} });
  assert.notEqual(rendered, null, "the income question is on by default");
  const copy = JSON.stringify(rendered);
  assert.ok(copy.includes("선택 질문이에요. 답하면 소득 조건이 있는 혜택을 더 정확히 걸러드려요."));
  assert.ok(copy.includes("소득 구간 (기준 중위소득 %)"));
  for (const hidden of ["무주택 여부", "혼인·자녀", "자녀 수"]) assert.ok(!copy.includes(hidden), `${hidden} stays hidden`);
  assert.ok(!copy.includes("혜택이 늘어"));
  const { conditions, saves } = loadConditions();
  const answered = { ...skippedAnswers, homeless_household: true, marital_status: "married", newlywed: true, children_count: 2, income_median_band: "le60" };
  for (const extra of [{ optional: answered }, { optional: answered, replaceOptional: true }]) {
    await conditions.saveConditions(queryClient, "member-a", { ...conditionDraft(), ...extra });
    assert.equal(saves.at(-1).income_median_band, "le60");
    for (const key of OPTIONAL_KEYS.filter(k => k !== "income_median_band")) assert.equal(key in saves.at(-1), false, key);
  }
  await conditions.saveConditions(queryClient, "member-a", conditionDraft());
  for (const key of OPTIONAL_KEYS) assert.equal(key in saves.at(-1), false, `no optional step: ${key} left out`);
});

test("EXPO_PUBLIC_INCOME_QUESTION_ENABLED=false hides 소득 구간 too: nothing renders and no optional key is sent", async () => {
  // Modules read the flags when they load, so load inside withEnv and run outside it.
  const { Optional, conditions, saves } = withEnv({ EXPO_PUBLIC_INCOME_QUESTION_ENABLED: "false" }, () => ({ Optional: loadOptionalConditions(), ...loadConditions() }));
  assert.equal(Optional({ value: skippedAnswers, onChange() {} }), null);
  const answered = { ...skippedAnswers, marital_status: "married", newlywed: true, income_median_band: "le60" };
  for (const extra of [{}, { optional: answered }, { optional: answered, replaceOptional: true }]) {
    await conditions.saveConditions(queryClient, "member-a", { ...conditionDraft(), ...extra });
    for (const key of OPTIONAL_KEYS) assert.equal(key in saves.at(-1), false, key);
  }
});

test("optional questions shown: onboarding keeps saved ones on skip, the edit screen replaces them", async () => {
  const rendered = withQuestions(() => loadOptionalConditions()({ value: skippedAnswers, onChange() {} }));
  assert.notEqual(rendered, null);
  const copy = JSON.stringify(rendered);
  assert.ok(copy.includes("선택 질문이에요. 답하지 않아도 돼요."));
  assert.ok(!copy.includes("혜택이 늘어"));
  const { conditions, saves } = withQuestions(() => loadConditions());
  await conditions.saveConditions(queryClient, "member-a", { ...conditionDraft(), optional: { ...skippedAnswers, marital_status: "married", newlywed: true, income_median_band: "le60" } });
  assert.equal(saves.at(-1).marital_status, "married");
  assert.equal(saves.at(-1).newlywed, true);
  assert.equal(saves.at(-1).income_median_band, "le60");
  assert.equal(saves.at(-1).homeless_household, null);
  await conditions.saveConditions(queryClient, "member-a", { ...conditionDraft(), optional: { ...skippedAnswers, marital_status: "unmarried" }, replaceOptional: true });
  assert.equal(saves.at(-1).marital_status, "unmarried");
  await conditions.saveConditions(queryClient, "member-a", conditionDraft());
  assert.equal("income_median_band" in saves.at(-1), false, "no optional step: keys left out, the server keeps them");
});

test("district options are codes the server reads, grouped by sido", () => {
  const { regions } = loadConditions();
  assert.deepEqual(regions.districtsOf("광주").map(([code]) => code.slice(0, 2)), Array(5).fill("29"));
  assert.ok(regions.districtsOf("jeonnam").every(([code]) => code.startsWith("46")));
  assert.ok(regions.districtsOf("gangwon").every(([code]) => code.startsWith("51")));
  assert.equal(regions.districtName("gyeonggi", "41110"), "수원시");
  assert.equal(regions.districtName("gyeonggi", "41117"), "수원시 영통구");
  assert.equal(regions.districtName("busan", "41117"), "", "another sido's code is not a district of this one");
  assert.deepEqual(regions.districtsOf("unknown"), []);
});

test("changing the sido clears the 시·군·구; choosing the same sido keeps it", () => {
  const { store } = loadConditions();
  store.getState().setRegion("부산");
  store.getState().setCityCode("26500");
  store.getState().setRegion("busan");
  assert.equal(store.getState().cityCode, "26500");
  store.getState().setRegion("daegu");
  assert.equal(store.getState().cityCode, "");
});

test("saved conditions send city_code only when it lies in the chosen sido", async () => {
  const { conditions, saves, store } = loadConditions();
  const draft = { nickname: "", region: "busan", cityCode: "26500", birthYear: 2000, enrollmentStatus: "", schoolName: "", employmentStatus: "", incomeBracket: null };
  const client = { setQueryData() {}, invalidateQueries: async () => {} };
  assert.equal(await conditions.saveConditions(client, "member-a", draft), true);
  assert.equal(saves.at(-1).city_code, "26500");
  assert.equal(store.getState().cityCode, "26500");
  await conditions.saveConditions(client, "member-a", { ...draft, cityCode: "" });
  assert.equal(saves.at(-1).city_code, null, "clearing the picker clears the saved district");
  await conditions.saveConditions(client, "member-a", { ...draft, region: "daegu" });
  assert.equal(saves.at(-1).city_code, null, "a 부산 district is never sent with 대구");
});

test("세종 has no 시·군·구 to choose; its one code is sent automatically", async () => {
  const { conditions, regions, saves } = loadConditions();
  assert.equal(regions.hasDistrictChoice("세종"), false);
  assert.equal(regions.hasDistrictChoice("busan"), true);
  assert.equal(regions.cityCodeFor("sejong", ""), "36110");
  assert.equal(regions.cityCodeFor("busan", "26500"), "26500");
  assert.equal(regions.cityCodeFor("busan", "41117"), "");
  const client = { setQueryData() {}, invalidateQueries: async () => {} };
  await conditions.saveConditions(client, "member-a", { nickname: "", region: "sejong", cityCode: "", birthYear: 2000, enrollmentStatus: "", schoolName: "", employmentStatus: "", incomeBracket: null });
  assert.equal(saves.at(-1).city_code, "36110");
});

test("the 시·군·구 options are announced as one radio group", () => {
  const source = fs.readFileSync(path.resolve(__dirname, "../components/DistrictPicker.tsx"), "utf8");
  assert.match(source, /accessibilityRole="radiogroup"/);
});

test("a member's saved profile sends unasked flags as unknown, never as false", () => {
  const filename = path.resolve(__dirname, "..", "lib/memberProfile.ts");
  const source = ts.transpileModule(fs.readFileSync(filename, "utf8"), { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText;
  const module = { exports: {} };
  new Function("require", "module", "exports", source)(name => { throw new Error(`Unmocked dependency ${name}`); }, module, module.exports);
  const { memberPreviewProfile } = module.exports;
  // The server stores an unanswered flag as false; no screen asks for these.
  const saved = { birth_year: 2002, region_code: "gwangju", enrollment_status: "graduated", has_disability: false, is_low_income_household: false, is_multicultural_family: false, veteran_family: false };
  const sent = memberPreviewProfile(saved, "gwangju");
  for (const flag of ["has_disability", "is_low_income_household", "is_multicultural_family", "veteran_family"]) assert.equal(sent[flag], null, flag);
  assert.equal(sent.birth_year, 2002); assert.equal(sent.region_code, "gwangju"); assert.equal(sent.enrollment_status, "graduated");
  // An explicit true is kept.
  assert.equal(memberPreviewProfile({ ...saved, has_disability: true }, "gwangju").has_disability, true);
  assert.equal(memberPreviewProfile({ ...saved, birth_year: null }, "gwangju"), null);
  assert.equal(memberPreviewProfile(saved, undefined), null);
});

// QA 2026-10-23 M1: an API outage at launch must not turn a member into a guest.
async function launchFixture() {
  const f = fixture();
  f.store.setState({ token: null, refreshToken: null, user: null, isLoading: true });
  await f.tokens.setItemAsync("mazimi_auth_token", "stored-access");
  await f.tokens.setItemAsync("mazimi_refresh_token", "stored-refresh");
  return f;
}

for (const outage of ["503", "network"]) test(`a ${outage} outage at launch keeps the tokens and reports a connection problem`, async () => {
  const f = await launchFixture();
  f.setFetch(async () => { if (outage === "network") throw new TypeError("Failed to fetch"); return json(503, { error: "Service unavailable" }); });
  await f.store.getState().restoreSession();
  const s = f.store.getState();
  assert.equal(s.user, null); assert.equal(s.isLoading, false);
  assert.equal(s.sessionError, true, "the screen can show 연결에 문제가 있어요 instead of the guest view");
  assert.equal(f.memory.get("mazimi_auth_token"), "stored-access"); assert.equal(f.memory.get("mazimi_refresh_token"), "stored-refresh");
  assert.equal(f.resets(), 0);
});

test("retrying after the outage restores the member without a new login", async () => {
  const f = await launchFixture();
  f.setFetch(async () => json(503, {}));
  await f.store.getState().restoreSession();
  f.setFetch(async (url, options) => { assert.match(url, /\/auth\/me$/); assert.equal(options.headers.Authorization, "Bearer stored-access"); return json(200, { id: "member-a", nickname: "A", image: null }); });
  await f.store.getState().restoreSession();
  const s = f.store.getState();
  assert.equal(s.user.id, "member-a"); assert.equal(s.token, "stored-access"); assert.equal(s.sessionError, false);
});

test("a rejected session and a device without tokens are not connection problems", async () => {
  let f = await launchFixture();
  f.setFetch(async () => json(401, {}));
  await f.store.getState().restoreSession();
  assert.equal(f.store.getState().user, null); assert.equal(f.store.getState().sessionError, false);
  assert.equal(f.memory.get("mazimi_auth_token"), undefined);

  f = fixture(); f.store.setState({ token: null, refreshToken: null, user: null, isLoading: true });
  await f.store.getState().restoreSession();
  assert.equal(f.store.getState().sessionError, false); assert.equal(f.store.getState().isLoading, false);
});

test("a 404 from /auth/me (account gone) ends the session instead of a retry loop (M2)", async () => {
  const f = await launchFixture();
  const calls = [];
  f.setFetch(async url => { calls.push(new URL(url).pathname); return json(404, { error: "User not found" }); });
  await f.store.getState().restoreSession();
  const s = f.store.getState();
  assert.equal(s.user, null); assert.equal(s.sessionError, false, "a guest now, not a connection problem");
  assert.equal(s.isLoading, false);
  assert.equal(f.memory.get("mazimi_auth_token"), undefined); assert.equal(f.memory.get("mazimi_refresh_token"), undefined);
  assert.deepEqual(calls, ["/api/v1/auth/me"], "no refresh for a deleted account");
  const problem = fs.readFileSync(path.resolve(__dirname, "..", "components/SessionProblem.tsx"), "utf8");
  assert.match(problem, /SWITCH_ACCOUNT_LABEL = "다른 계정으로 로그인"/);
  assert.match(problem, /const switchAccount = async \(\) => \{ await signOut\(\); router\.replace\("\/login"\); \}/, "the retry state has a way out");
});

test("a proxy or route 404 on /auth/me is an outage, not a deleted account (L5)", async () => {
  for (const body of [{}, { error: "Not Found" }, "<html>404</html>"]) {
    const f = await launchFixture();
    f.setFetch(async () => typeof body === "string" ? new Response(body, { status: 404 }) : json(404, body));
    await f.store.getState().restoreSession();
    assert.equal(f.store.getState().sessionError, true, JSON.stringify(body));
    assert.equal(f.memory.get("mazimi_auth_token"), "stored-access", "tokens kept");
  }
});

test("a logout while restoreSession runs wins: a late /auth/me never revives the ended session (N-M1)", async () => {
  for (const late of ["ok", "outage"]) {
    const f = await launchFixture();
    const gate = deferred();
    f.setFetch(async () => { await gate.promise; return late === "ok" ? json(200, { id: "member-a", nickname: "A", image: null }) : json(503, {}); });
    const restoring = f.store.getState().restoreSession();
    await new Promise(resolve => setImmediate(resolve));
    await f.store.getState().logout();     // 다른 계정으로 로그인 → signOut → logout
    gate.resolve();
    await restoring;
    const s = f.store.getState();
    assert.equal(s.user, null, `${late}: no user comes back`);
    assert.equal(s.token, null);
    assert.equal(s.sessionError, false, `${late}: a guest, not a connection problem`);
    assert.equal(s.isLoading, false);
  }
});

test("profile and manage show the connection problem, not the guest view, and other screens get a retry banner", () => {
  const read = file => fs.readFileSync(path.resolve(__dirname, "..", file), "utf8");
  // NonMemberState: retry state on an outage, loading while restoring, a login nudge only for a guest.
  assert.match(read("app/(tabs)/profile.tsx"), /memberState !== "guest" \? <NonMemberState \/>/);
  assert.match(read("app/(tabs)/manage.tsx"), /!items\.length && !user && <NonMemberState/);
  const nonMember = read("components/NonMemberState.tsx");
  assert.match(nonMember, /state === "pending-error"\) return <SessionErrorState \/>/);
  assert.match(nonMember, /state === "pending"\) return <EmptyState loading/);
  assert.match(read("components/SessionProblem.tsx"), /연결에 문제가 있어요/);
  assert.match(read("components/SessionProblem.tsx"), /다시 시도/);
  assert.match(read("app/_layout.tsx"), /<SessionErrorBanner \/>/);
  // L1: no banner on top of a screen's own SessionErrorState, and none over the home intro.
  const layout = read("app/_layout.tsx");
  const set = layout.match(/const NO_SESSION_BANNER = new Set\((\[[^\]]*\])\)/)[1];
  for (const path of ["/", "/profile", "/manage", "/profile-edit", "/login", "/consent"]) assert.ok(set.includes(`"${path}"`), path);
  assert.match(layout, /!pathname\.startsWith\("\/onboarding"\)/);
  // L5: every route that renders NonMemberState (and so SessionErrorState) is in the set, so the retry never shows twice.
  const walk = dir => fs.readdirSync(path.resolve(__dirname, "..", dir), { withFileTypes: true }).flatMap(entry => entry.isDirectory() ? walk(path.join(dir, entry.name)) : /\.tsx$/.test(entry.name) ? [path.join(dir, entry.name)] : []);
  const routes = walk("app").filter(file => /<NonMemberState/.test(read(file))).map(file => "/" + file.replace(/^app\//, "").replace(/\(tabs\)\//, "").replace(/\.tsx$/, "").replace(/^index$/, ""));
  assert.ok(routes.length >= 13, routes.join(","));
  for (const route of routes) assert.ok(set.includes(`"${route}"`), `${route} renders SessionErrorState and must not get the floating banner too`);
  assert.match(layout, /sessionProblem && showsSessionBanner\(pathname\) && <SessionErrorBanner \/>/);
  assert.match(read("app/(tabs)/index.tsx"), /\{sessionProblem && <SessionErrorBanner inline \/>\}/, "home shows it in its own flow");
});

test("API errors carry Korean copy: English or internal server text never reaches the screen", async () => {
  const f = fixture(); f.store.setState({ token: null, refreshToken: null, user: null });
  for (const [status, body, copy] of [
    [503, { error: "Service unavailable" }, "잠시 후 다시 시도해 주세요."],
    [404, { error: "Profile not found" }, "요청한 정보를 찾을 수 없어요."],
    [502, "<html>Bad Gateway</html>", "잠시 후 다시 시도해 주세요."],
    [409, { error: "이미 신청 준비를 시작했어요." }, "이미 신청 준비를 시작했어요."],
  ]) {
    f.setFetch(async () => typeof body === "string" ? new Response(body, { status }) : json(status, body));
    await assert.rejects(f.api.getProgram("p1"), error => { assert.equal(error.status, status); assert.equal(error.message, copy); return true; });
  }
});

test("fetch failures and 401 rejections reach the screen as Korean copy", async () => {
  let f = fixture(); f.store.setState({ token: null, refreshToken: null, user: null });
  for (const message of ["Network request failed", "Failed to fetch"]) {
    f.setFetch(async () => { throw new TypeError(message); });
    await assert.rejects(f.api.getProgram("p1"), error => error instanceof f.ApiRequestError && error.status === 0 && error.message === "연결을 확인한 뒤 다시 시도해 주세요.");
  }
  f = fixture(); f.setFetch(async () => json(401, {}));
  await assert.rejects(f.api.getProfile(), error => error.status === 401 && error.message === "로그인이 필요해요. 다시 로그인해 주세요.");
  const source = fs.readFileSync(path.resolve(__dirname, "../lib/api.ts"), "utf8");
  assert.doesNotMatch(source, /new ApiRequestError\("Unauthorized/);
});

test("a server without GET /recommendations is asked once, then the preview directly (M3)", async () => {
  const f = fixture(); const calls = [];
  f.store.setState({ token: "fresh-fixture" });
  f.setFetch(async url => { calls.push(new URL(url).pathname); return url.includes("/api/v1/recommendations") ? json(404, { error: "Not Found" }) : json(200, { items: [], total_available: 0, estimated_monthly: 0, estimated_semester: 0 }); });
  const profile = { birth_year: 2001, region_code: "busan" };
  await f.api.getRecommendations(profile);
  await f.api.getRecommendations(profile);
  await f.api.getRecommendations(profile, "all");
  assert.deepEqual(calls, ["/api/v1/recommendations", "/api/v1/recommend/preview", "/api/v1/recommend/preview", "/api/v1/recommend/preview"]);
});

test("a server with GET /recommendations keeps using it; other errors are not remembered (M3)", async () => {
  const f = fixture(); const calls = []; let fail = true;
  f.store.setState({ token: "fresh-fixture" });
  f.setFetch(async url => { calls.push(new URL(url).pathname); if (fail) { fail = false; return json(503, { error: "down" }); } return json(200, { items: [], total_available: 0, estimated_monthly: 0, estimated_semester: 0 }); });
  await assert.rejects(f.api.getRecommendations({ birth_year: 2001, region_code: "busan" }));
  await f.api.getRecommendations({ birth_year: 2001, region_code: "busan" });
  assert.deepEqual(calls, ["/api/v1/recommendations", "/api/v1/recommendations"]);
});

test("직접 추가 stays hidden until the document-items route is known to exist (M4)", async () => {
  const f = fixture(); const seen = [];
  for (const [status, expected] of [[404, false], [405, false], [401, true], [200, true]]) {
    f.setFetch(async (url, options = {}) => { seen.push(options.headers?.Authorization); return json(status, {}); });
    assert.equal(await f.api.hasDocumentItemsRoute(), expected, `HTTP ${status}`);
  }
  assert.deepEqual(seen, [undefined, undefined, undefined, undefined], "the probe sends no token, so guests can ask too");
  f.setFetch(async () => { throw new TypeError("Failed to fetch"); });
  await assert.rejects(f.api.hasDocumentItemsRoute(), "a network error is unknown, not 'missing'");
  const items = fs.readFileSync(path.resolve(__dirname, "../components/MyDocumentItems.tsx"), "utf8");
  assert.match(items, /if \(supported !== true\) return null;/, "hidden for guests and members until the probe says yes");
  assert.match(items, /enabled: !!user && supported === true/);
  assert.match(fs.readFileSync(path.resolve(__dirname, "../lib/catalogue.ts"), "utf8"), /queryKey: \["document-items-capability"\][^\n]*staleTime: Infinity/, "probed once per session");
  assert.match(fs.readFileSync(path.resolve(__dirname, "../app/document-vault.tsx"), "utf8"), /\{documentItems===true&&<View/, "the vault checklist card hides too");
});

test("a 401 from the preview (anonymous preview off) never refreshes or logs a member out", async () => {
  const f = fixture(); const calls = [];
  f.store.setState({ token: "fresh-fixture" });
  f.setFetch(async url => { calls.push(new URL(url).pathname); return json(401, { error: "로그인하고 조건을 저장하면 맞춤 추천을 볼 수 있어요.", code: "login_required" }); });
  await assert.rejects(f.api.getRecommendPreview({ birth_year: 2001, region_code: "busan" }), error => error.status === 401);
  assert.deepEqual(calls, ["/api/v1/recommend/preview"], "no refresh exchange");
  assert.equal(f.store.getState().user.id, "member-a");
  assert.equal(f.store.getState().token, "fresh-fixture");
  assert.equal(f.resets(), 0);
});

test("iOS: expo-calendar gets both usage strings, so its module never hits EXFatal at launch", () => {
  const app = JSON.parse(fs.readFileSync(path.resolve(__dirname, "..", "app.json"), "utf8"));
  const entry = app.expo.plugins.find(plugin => Array.isArray(plugin) && plugin[0] === "expo-calendar");
  assert.ok(entry, "expo-calendar plugin configured");
  const options = entry[1];
  // ExpoCalendar's OnCreate checks the reminders permission; without
  // NSRemindersFullAccessUsageDescription it calls EXFatal, which in Release
  // takes down the module proxy ("Cannot find native module ExpoLinking").
  for (const key of ["calendarPermission", "remindersPermission"]) {
    assert.equal(typeof options[key], "string", `${key} must be a usage string, not false`);
    assert.ok(/[가-힣]/.test(options[key]) && options[key].trim().length >= 10, `${key}: Korean usage copy`);
  }
  // The app never reads or writes reminders, so the copy says so instead of describing a feature (App Review 5.1.1).
  assert.equal(options.remindersPermission, "마지미는 미리 알림을 읽거나 쓰지 않아요. 캘린더 기능에 필요한 시스템 항목이에요.");
  const plugin = fs.readFileSync(require.resolve("expo-calendar/plugin/build/withCalendar.js", { paths: [path.resolve(__dirname, "..")] }), "utf8");
  assert.match(plugin, /NSRemindersFullAccessUsageDescription: remindersPermission/, "the plugin writes the key prebuild needs");
  // The app never asks for reminders, so the string is a declaration only; no prompt is shown.
  const walk = dir => fs.readdirSync(path.resolve(__dirname, "..", dir), { withFileTypes: true }).flatMap(entry => entry.isDirectory() ? walk(path.join(dir, entry.name)) : /\.tsx?$/.test(entry.name) ? [path.join(dir, entry.name)] : []);
  for (const file of [...walk("app"), ...walk("components"), ...walk("lib")]) assert.doesNotMatch(fs.readFileSync(path.resolve(__dirname, "..", file), "utf8"), /requestRemindersPermissionsAsync|EntityTypes\.REMINDER/, file);
});

test("the vault's add sheet sizes to its content on iOS, capped by maxHeight (no flex: 1 inside a height-less sheet)", () => {
  const vault = fs.readFileSync(path.resolve(__dirname, "..", "app/document-vault.tsx"), "utf8");
  const block = name => { const start = vault.indexOf(`  ${name}: {`); assert.ok(start > 0, name); return vault.slice(start, vault.indexOf("},", start)); };
  // The sheet has only a maxHeight; flex: 1 inside it resolves to 0 on iOS and the sheet collapses to its handle.
  assert.match(vault, /modalStyles\.sheet,\s*\{ maxHeight: sheetMaxHeight \}/);
  for (const name of ["sheetInner", "fieldsScroll"]) {
    const style = block(name);
    assert.doesNotMatch(style, /\bflex: 1\b/, `${name}: no flex: 1`);
    assert.match(style, /flexGrow: 0/, name);
    assert.match(style, /flexShrink: 1/, name);
  }
});

test("iOS: the config plugin raises every Pods target to the app's 15.1 deployment target (Xcode 27 rejects iOS 9-13 pods)", () => {
  const app = JSON.parse(fs.readFileSync(path.resolve(__dirname, "..", "app.json"), "utf8"));
  const entry = app.expo.plugins.find(plugin => Array.isArray(plugin) && plugin[0] === "./plugins/withPodsDeploymentTarget");
  assert.ok(entry, "plugin registered in app.json");
  assert.equal(entry[1].deploymentTarget, "15.1");
  // The plugin requires @expo/config-plugins directly, so it is a pinned devDependency (not a hoisting accident),
  // at the version the lock resolves for expo itself.
  const pkg = JSON.parse(fs.readFileSync(path.resolve(__dirname, "..", "package.json"), "utf8"));
  const lock = JSON.parse(fs.readFileSync(path.resolve(__dirname, "..", "package-lock.json"), "utf8"));
  assert.equal(pkg.devDependencies["@expo/config-plugins"], lock.packages["node_modules/@expo/config-plugins"].version);
  assert.match(fs.readFileSync(path.resolve(__dirname, "..", "plugins/withPodsDeploymentTarget.js"), "utf8"), /resource-bundle targets/, "the comment says why RN's own hook is not enough");
  const plugin = require(path.resolve(__dirname, "..", "plugins/withPodsDeploymentTarget.js"));
  const snippet = plugin.podfileSnippet("15.1");
  assert.match(snippet, /installer\.pods_project\.targets\.each/);
  assert.match(snippet, /Gem::Version\.new\(current\) < Gem::Version\.new\('15\.1'\)/, "only raises, never lowers");
  assert.match(snippet, /build_settings\['IPHONEOS_DEPLOYMENT_TARGET'\] = '15\.1'/);
  // Applied to the Podfile Expo generates: inserted into post_install, idempotent.
  const { mergeContents } = require(require.resolve("@expo/config-plugins/build/utils/generateCode", { paths: [path.resolve(__dirname, "..")] }));
  const podfile = "target 'app' do\n  post_install do |installer|\n    react_native_post_install(installer)\n  end\nend\n";
  const once = mergeContents({ tag: "mazimi-pods-deployment-target", src: podfile, newSrc: snippet, anchor: /post_install do \|installer\|/, offset: 1, comment: "#" });
  assert.ok(once.didMerge);
  assert.match(once.contents, /post_install do \|installer\|\n# @generated begin mazimi-pods-deployment-target[\s\S]*react_native_post_install/);
  const twice = mergeContents({ tag: "mazimi-pods-deployment-target", src: once.contents, newSrc: snippet, anchor: /post_install do \|installer\|/, offset: 1, comment: "#" });
  assert.equal(twice.contents, once.contents, "a second prebuild changes nothing");
});

test("reads a kept tap waits on are bounded: a hung request fails as a dropped connection (M2)", async () => {
  const f = fixture();
  const hung = signal => new Promise((resolve, reject) => signal.addEventListener("abort", () => { const e = new Error("aborted"); e.name = "AbortError"; reject(e); }));
  const started = Date.now();
  await assert.rejects(f.boundedRead(hung, 30), error => error instanceof f.ApiRequestError && error.status === 0 && /연결/.test(error.message));
  assert.ok(Date.now() - started < 1000, "aborted at the bound");
  await assert.rejects(f.boundedRead(async () => { throw new f.ApiRequestError("x", 503); }, 30), error => error.status === 503, "other failures pass through");
  assert.deepEqual(await f.boundedRead(async () => ({ items: [] }), 30), { items: [] });
  // The real reads go through it with request()'s signal.
  const source = fs.readFileSync(path.resolve(__dirname, "../lib/api.ts"), "utf8");
  for (const name of ["getSaved", "getAlertPreference", "getProfile"]) assert.match(source, new RegExp(`${name}: [^\\n]*(\\n\\s*)?boundedRead\\(signal => request[^\\n]*\\{ signal \\}`), name);
  assert.match(source, /READ_TIMEOUT_MS = 15_000/);
});

test("a hung refresh exchange times out as an outage and never logs out (L-1)", async (t) => {
  const f = fixture();
  await f.tokens.setItemAsync("mazimi_refresh_token", "refresh-fixture");
  f.setFetch((url, options) => new Promise((resolve, reject) => {
    assert.match(url, /\/auth\/refresh$/);
    options.signal.addEventListener("abort", () => { const e = new Error("aborted"); e.name = "AbortError"; reject(e); });
  }));
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const refreshing = f.store.getState().refreshAccessToken();
  t.mock.timers.tick(15_000);
  await assert.rejects(refreshing, /인증 서버에 연결하지 못했어요/, "thrown as an outage, not false");
  t.mock.timers.reset();
  const s = f.store.getState();
  assert.equal(s.user.id, "member-a", "still signed in");
  assert.equal(s.refreshToken, "refresh-fixture");
  assert.equal(f.memory.get("mazimi_refresh_token"), "refresh-fixture");
  assert.equal(f.resets(), 0);
  assert.match(fs.readFileSync(path.resolve(__dirname, "../store/auth.ts"), "utf8"), /REFRESH_TIMEOUT_MS = 15_000/);
});

test("a refresh that timed out under 60 s ago is retried on the next foreground, once (L-4)", async (t) => {
  const f = fixture();
  await f.tokens.setItemAsync("mazimi_refresh_token", "refresh-fixture");
  let calls = 0;
  let hang = true;
  f.setFetch((url, options) => {
    assert.match(url, /\/auth\/refresh$/);
    calls++;
    if (hang) return new Promise((resolve, reject) => options.signal.addEventListener("abort", () => { const e = new Error("aborted"); e.name = "AbortError"; reject(e); }));
    return Promise.resolve({ ok: true, status: 200, json: async () => ({ token: "fresh-access", refresh_token: "refresh-next" }) });
  });
  assert.equal(f.retryTimedOutRefresh(), null, "nothing timed out yet: no retry");
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const refreshing = f.store.getState().refreshAccessToken();
  t.mock.timers.tick(15_000);
  await assert.rejects(refreshing, /인증 서버에 연결하지 못했어요/);
  t.mock.timers.reset();
  const timedOutAt = Date.now();
  assert.equal(f.retryTimedOutRefresh(timedOutAt + 60_000), null, "past the server's 60 s grace window: no retry");
  hang = false;
  const retried = f.retryTimedOutRefresh(timedOutAt + 59_000);
  assert.ok(retried, "inside the window: retried");
  assert.equal(await retried, true);
  assert.equal(calls, 2);
  assert.equal(f.store.getState().refreshToken, "refresh-next", "the chain continued with the rotated token");
  assert.equal(f.retryTimedOutRefresh(timedOutAt + 59_500), null, "the retry is spent: one per timeout");
  // A retry that fails again resolves false instead of throwing into the AppState handler.
  hang = true;
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const second = f.store.getState().refreshAccessToken();
  t.mock.timers.tick(15_000);
  await assert.rejects(second);
  const again = f.retryTimedOutRefresh();
  t.mock.timers.tick(15_000);
  assert.equal(await again, false);
  t.mock.timers.reset();
  assert.equal(f.resets(), 0, "never logged out");
  const layout = fs.readFileSync(path.resolve(__dirname, "../app/_layout.tsx"), "utf8");
  assert.match(layout, /if \(s\.user\) void retryTimedOutRefresh\(\)/, "the foreground handler calls it");
});

function loadAgentVault() {
  const load = relative => {
    const filename = path.resolve(__dirname, "..", relative);
    const source = ts.transpileModule(fs.readFileSync(filename, "utf8"), { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText;
    const module = { exports: {} };
    new Function("require", "module", "exports", source)(name => {
      if (name === "./documentReadiness") return load("lib/documentReadiness.ts");
      throw new Error(`Unmocked dependency ${name}`);
    }, module, module.exports);
    return module.exports;
  };
  return load("lib/agentVault.ts");
}

test("the agent card matches documents with the app's one matcher and never calls a doubtful paper ready", () => {
  const { vaultRows } = loadAgentVault();
  const today = new Date("2026-10-23T00:00:00Z");
  const doc = (name, extra = {}) => ({ name, status: "missing", detail: "", qualified: false, window_days: null, issue_condition: false, ...extra });
  const result = { tool: "match_vault_documents", data: { documents: [
    doc("주민등록등본(최근 1개월 이내 발급)", { window_days: 31, issue_condition: true }),
    doc("부모 소득금액증명원", { status: "needs_check", qualified: true }),
    doc("재학증명서"),
    doc("통장사본", { status: "have" }),
    doc("가족관계증명서"),
    doc("졸업증명서"),
    doc("주민등록초본(신청월 발급분)", { issue_condition: true }),
    doc("건강보험료 납부확인서"),
  ] } };
  const file = "file:///vault/x.enc";
  const vault = [
    { id: "1", type: "resident_register", name: "등본", fileUri: file, issuedAt: "2026-07-01", expiresAt: null },
    { id: "2", type: "income_proof", name: "소득금액증명원", fileUri: file, issuedAt: "2026-10-01", expiresAt: null },
    { id: "3", type: "enrollment_cert", name: "재학증명서", fileUri: file, issuedAt: "2026-10-01", expiresAt: null },
    { id: "4", type: "family_relation", name: "가족관계증명서", fileUri: file, issuedAt: "2025-01-01", expiresAt: "2026-01-01" },
    // An entry without a file is not a paper in hand.
    { id: "5", type: "graduation_cert", name: "졸업증명서", fileUri: null, issuedAt: "2026-10-01", expiresAt: null },
    // An old issue date and no window: the date must be checked.
    { id: "6", type: "health_insurance", name: "건강보험료 납부확인서", fileUri: file, issuedAt: "2025-03-01", expiresAt: null },
  ];
  const rows = Object.fromEntries(vaultRows(result, vault, today).map(r => [r.name, r.status]));
  assert.equal(rows["주민등록등본(최근 1개월 이내 발급)"], "check", "issued outside the window");
  assert.equal(rows["부모 소득금액증명원"], "check", "someone else's paper");
  assert.equal(rows["재학증명서"], "check", "in the vault, no window stated: check the date");
  assert.equal(rows["통장사본"], "ready", "a plain paper marked done on the checklist");
  assert.equal(rows["가족관계증명서"], "check", "expired in the vault");
  assert.equal(rows["졸업증명서"], "missing", "an entry with no file");
  assert.equal(rows["주민등록초본(신청월 발급분)"], "check", "the vault's 주민등록 copy may be it; an undated issue condition is never ready");
  assert.equal(rows["건강보험료 납부확인서"], "check", "an old issue date");
  // A dated copy inside a stated window is ready.
  const fresh = { tool: "match_vault_documents", data: { documents: [doc("재학증명서(최근 3개월 이내 발급)", { window_days: 93, issue_condition: true })] } };
  assert.equal(vaultRows(fresh, vault, today)[0].status, "ready");
  // Re-review 3 M4: a parent's paper in the vault is not the applicant's.
  const parentVault = [{ id: "7", type: "income_proof", name: "부모 소득금액증명원", fileUri: file, issuedAt: "2026-10-13", expiresAt: null }];
  const own = { tool: "match_vault_documents", data: { documents: [doc("소득금액증명원(최근 3개월 이내 발급)", { window_days: 93, issue_condition: true })] } };
  assert.equal(vaultRows(own, parentVault, today)[0].status, "check");
  // …nor the reverse.
  const ownVault = [{ id: "8", type: "income_proof", name: "소득금액증명원", fileUri: file, issuedAt: "2026-10-13", expiresAt: null }];
  const parents = { tool: "match_vault_documents", data: { documents: [doc("부모 소득금액증명원", { qualified: true })] } };
  assert.equal(vaultRows(parents, ownVault, today)[0].status, "check");
  // An issue condition with no digit window is never proven by a date.
  const undated = { tool: "match_vault_documents", data: { documents: [doc("재학증명서(신청월 발급분)", { issue_condition: true })] } };
  assert.equal(vaultRows(undated, vault, today)[0].status, "check");
});

test("agent goals declare the journey handoffs this build opens", async () => {
  const f = fixture(); const calls = [];
  f.setFetch(async (url, options) => { calls.push({ url, options }); return json(200, { run_id: "r", status: "received", existing: false }); });
  await f.api.sendAgentGoal("conv-1", "이 혜택 신청 준비해줘", "agent-key-00000003", PROGRAM, []);
  assert.equal(calls[0].options.headers["X-Mazimi-Client-Caps"], "journey-v1");
});
