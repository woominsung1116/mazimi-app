// User-visible list rules (review 2026-10-21): deadlines in KST, closed saved
// rows, corporate amounts out of sums, the 조건 확인됨 / 확인 필요 sections and
// partial section loads. Loads the shipped lib modules; no network.
const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const ts = require("typescript");
// lib/apiBaseUrl.ts refuses a release bundle without an API origin.
process.env.EXPO_PUBLIC_API_URL ??= "https://api.fixture.invalid";

const loaded = new Map();
function load(relative) {
  const filename = path.resolve(__dirname, "..", relative);
  if (loaded.has(filename)) return loaded.get(filename).exports;
  const module = { exports: {} };
  loaded.set(filename, module);
  const source = ts.transpileModule(fs.readFileSync(filename, "utf8"), { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText;
  const dependency = name => {
    if (name === "./api") return load("lib/api.ts");
    if (name === "./apiBaseUrl") return load("lib/apiBaseUrl.ts");
    if (name === "./errorCopy") return load("lib/errorCopy.ts");
    if (name === "./displayText") return load("lib/displayText.ts");
    if (name === "../constants/displayLabels") return load("constants/displayLabels.ts");
    if (name === "./districts.generated") return load("constants/districts.generated.ts");
    if (name === "../store/auth") return { useAuthStore: { getState: () => ({ token: null, user: null }) } };
    if (name === "./deadline") return load("lib/deadline.ts");
    if (name === "./verifiedSections") return load("lib/verifiedSections.ts");
    if (name === "./documentReadiness") return load("lib/documentReadiness.ts");
    throw new Error(`Unmocked dependency ${name}`);
  };
  new Function("require", "module", "exports", source)(dependency, module, module.exports);
  return module.exports;
}

const { deadlineLabel, daysLeft, effectiveDeadline } = load("lib/deadline.ts");
const { cardStatus, verifiedSections } = load("lib/verifiedSections.ts");
const { loadEach } = load("lib/programBatch.ts");
const kst = (date, time = "00:00") => Date.parse(`${date}T${time}:00+09:00`);

test("a date-only deadline is a KST calendar day", () => {
  // 08:00 KST is still the previous UTC day; the deadline is two KST days away.
  assert.equal(daysLeft("2026-10-23", kst("2026-10-21", "08:00")), 2);
  assert.equal(daysLeft("2026-10-23", kst("2026-10-21", "23:59")), 2);
  assert.equal(daysLeft("2026-10-21", kst("2026-10-21", "08:00")), 0);
  assert.equal(deadlineLabel({ deadline_at: "2026-10-21" }, kst("2026-10-21", "23:00")), "오늘 마감");
  assert.equal(deadlineLabel({ deadline_at: "2026-10-20" }, kst("2026-10-21", "00:30")), "마감");
});

test("the label reads the earliest end date, including benefit_valid_until", () => {
  const now = kst("2026-10-21", "12:00");
  const expiredOffer = { program_status: "always_open", benefit_valid_until: "2026-10-01T14:59:59Z" };
  assert.equal(deadlineLabel(expiredOffer, now), "마감", "an expired offer is not 상시");
  const program = { deadline_at: "2026-11-30T14:59:59Z", application_end_at: "2026-10-25T14:59:59Z", benefit_valid_until: null };
  assert.equal(effectiveDeadline(program), "2026-10-25T14:59:59Z", "minimum, not deadline_at ?? application_end_at");
  assert.equal(deadlineLabel(program, now), "D-4");
  assert.equal(effectiveDeadline({ ...program, effective_deadline: "2026-10-22T14:59:59Z" }), "2026-10-22T14:59:59Z", "the server's value wins");
  assert.equal(deadlineLabel({ program_status: "always_open" }, now), "상시");
  assert.equal(deadlineLabel({}, now), "일정 확인");
});

test("a saved row that left the catalogue says 마감·종료", () => {
  assert.equal(deadlineLabel({ available: false, program_status: "always_open" }), "마감·종료");
  assert.equal(deadlineLabel({ available: true, program_status: "always_open" }), "상시");
});

test("sections and card wording key on verified only while the flag is on", () => {
  const verified = { program_id: "a", verified: true, eligibility_status: "pass", remaining_checks: [] };
  const rulePass = { program_id: "b", verified: false, eligibility_status: "pass", remaining_checks: [{ kind: "window", label: "신청 기간 확인이 필요해요" }] };
  const unknown = { program_id: "c", verified: false, eligibility_status: "unknown", remaining_checks: [{ kind: "other", label: "추가 자격 조건 확인이 필요해요" }, { kind: "other", label: "추가 자격 조건 확인이 필요해요" }] };
  const items = [verified, rulePass, unknown];

  // Off: nothing changes.
  assert.equal(verifiedSections({ verified_section_enabled: false }, items), null);
  assert.equal(verifiedSections({}, items), null);
  assert.deepEqual(items.map(item => cardStatus(item, false)), ["pass", "pass", "unknown"]);

  // On: 조건 확인됨 rests on `verified`, never on eligibility_status.
  const sections = verifiedSections({ verified_section_enabled: true }, items);
  assert.deepEqual(sections.verified.map(item => item.program_id), ["a"]);
  assert.deepEqual(sections.needsCheck.map(item => item.program_id), ["b", "c"]);
  assert.deepEqual(items.map(item => cardStatus(item, true)), ["verified", "unknown", "unknown"]);
});

test("one missing programme does not empty a section", async () => {
  const load = async id => { if (id === "gone") throw new Error("Program not found"); return { id }; };
  assert.deepEqual(await loadEach(["a", "gone", "b"], load), [{ id: "a" }, { id: "b" }]);
  await assert.rejects(loadEach(["gone"], load), /Program not found/);
  assert.deepEqual(await loadEach([], load), []);
});

test("a missing detail page reads as not found or closed", () => {
  const page = fs.readFileSync(path.resolve(__dirname, "..", "app/programs/[id].tsx"), "utf8");
  // The copy lives in lib/errorCopy.ts (NOT_FOUND_PROGRAM), shared with errorCopy()'s 404 line.
  assert.match(page, /title=\{NOT_FOUND_PROGRAM\}/);
  assert.match(fs.readFileSync(path.resolve(__dirname, "..", "lib/errorCopy.ts"), "utf8"), /NOT_FOUND_PROGRAM = "찾을 수 없거나 마감된 혜택이에요"/);
});

// Figma prune (2026-10-31): copy that the .fig has no element for stays out of the app.
// Static notices, exclusion summaries and secondary links; error and empty states are separate.
test("no UI beyond the Figma design: exclusion summary, caveats, repeated notices", () => {
  const root = path.join(__dirname, "..");
  const files = [];
  const walk = dir => { for (const entry of fs.readdirSync(dir, { withFileTypes: true })) { const full = path.join(dir, entry.name); if (entry.isDirectory()) walk(full); else if (/\.tsx?$/.test(entry.name)) files.push(full); } };
  for (const dir of ["app", "components", "lib"]) walk(path.join(root, dir));
  const removed = [
    /제외한 이유/, /개는 .*제외했어요/, /FilterSummaryCard/, /자동 점검 \$?\{?/, /(program|p)\.youth_fit_label/, // card and detail only; 질문으로 찾기 keeps item.youth_fit_label for demoted rows
    /중복 제한을 확인해 주세요/, /내 조건으로 찾은 혜택 중 일부예요/, /신청 이력 보기/, /\{FINAL_CHECK_LINE\}/,
    /주민번호·연락처·계좌는 적지 마세요/, /찾기·조건 확인·준비 정리는 바로 해요\. 저장/, /상황은 탐색 기준이에요/,
    /질문에서 지역·학적·혜택 종류·키워드를 찾아/, /본인 입력을 돕는 체크 목록이에요/, /접수 방식이 다를 수 있어요/,
    /공고에 없는 서류도 직접 추가해/, /앱에서 암호화해 기기에 보관해요\. 서류의 유효기간/,
    /학교 혜택과 대학생 공통 혜택을 모았어요/, /확인된 조건: /, /"공식 혜택"/, /지원 내용과 신청 조건을 확인해 보세요/, /summaryFallback/,
    // Sample data or capabilities the .fig shows that the product does not have (figma-missing.md "skip").
    /340만원/, /아메리카노/, /추천 정확도/, /알림 관리에서 변경/, /"문의하기"/, /세부 조건은 공식 페이지에서 확인/, /연락처·주민번호·계정 비밀정보는 입력하지 마세요/,
  ];
  const hits = [];
  // The card's footer line only; the same words stay in its save notice (notify).
  assert.doesNotMatch(fs.readFileSync(path.join(root, "components/ApplicationResourcesCard.tsx"), "utf8"), /제출은 기관에 직접 해 주세요\.<\/Text>/);
  for (const file of files) { const source = fs.readFileSync(file, "utf8"); for (const pattern of removed) if (pattern.test(source)) hits.push(`${path.relative(root, file)}: ${pattern}`); }
  assert.deepEqual(hits, []);
  for (const route of ["calculator", "region-compare"]) assert.ok(!fs.existsSync(path.join(root, "app", `${route}.tsx`)), `${route} has no entry point and no Figma frame`);
});

// Figma gaps (2026-10-31): restored slots show real data or stay neutral.
test("detail rows: marks from the member's checks and this device's vault, counts only with data", () => {
  const { conditionStatus, documentStatus } = load("lib/detailStatus.ts");
  assert.equal(conditionStatus(undefined), null);
  assert.equal(conditionStatus([]), null);
  const checked = conditionStatus([{ label: "연령", status: "pass", detail: "만 23세" }, { label: "지역", status: "fail", detail: "" }, { label: "소득", status: "unknown", detail: "소득 정보가 없어요" }]);
  assert.deepEqual(checked.rows.map(r => r.mark), ["pass", "fail", "unknown"]);
  assert.equal(checked.count, "1/3 맞음", "never 충족 (claim rule)");
  assert.equal(checked.rows[1].note, null, "an empty detail adds no line");
  const today = new Date("2026-10-31T00:00:00Z");
  const vault = [{ id: "v1", type: "resident_register", name: "주민등록등본", fileUri: "file:///x.enc", mimeType: "application/pdf", issuedAt: "2026-10-20T00:00:00Z", expiresAt: null, addedAt: "2026-10-20T00:00:00Z", note: null, encrypted: true }];
  const docs = documentStatus([{ name: "주민등록등본" }, { name: "재학증명서" }, { name: "채용공고" }], vault, today);
  assert.equal(docs.rows[0].mark, "unknown", "in the vault, but the notice gives no issue window: the date is the user's to check");
  assert.match(docs.rows[0].note, /발급일 기준을 확인/);
  assert.equal(documentStatus([{ name: "주민등록등본(최근 3개월 이내 발급)" }], vault, today).rows[0].mark, "pass", "inside the notice's window");
  // The window may sit in the notice's description line, not the name (M1).
  assert.equal(documentStatus([{ name: "주민등록등본", detail: "최근 3개월 이내 발급분" }], vault, today).rows[0].mark, "pass");
  assert.equal(documentStatus([{ name: "주민등록등본", detail: "최근 5일 이내 발급분" }], vault, today).rows[0].mark, "unknown", "older than the description's window: stale, never 준비");
  assert.equal(docs.rows[1].mark, "fail", "a typed paper not in the vault");
  assert.equal(docs.rows[2].mark, "unknown", "a paper the matcher has no type for stays neutral");
  assert.equal(docs.rows[2].note, null);
  assert.equal(docs.count, "0/3 준비", "only rows with a check mark count (M2)");
  assert.equal(documentStatus([{ name: "주민등록등본", detail: "최근 3개월 이내" }, { name: "채용공고" }], vault, today).count, "1/2 준비");
  assert.equal(documentStatus([{ name: "채용공고" }], [], today).count, null, "no count without a matched row");
});

test("copy helpers: particle, largest stated amount, alert days", () => {
  const copy = load("lib/displayCopy.ts");
  assert.equal(copy.withObjectParticle("청년 월세 한시 특별지원"), "청년 월세 한시 특별지원을");
  assert.equal(copy.withObjectParticle("국가장학금 2유형"), "국가장학금 2유형을");
  assert.equal(copy.withObjectParticle("청년내일채움공제"), "청년내일채움공제를");
  assert.equal(copy.withObjectParticle("(드림옷장)"), "(드림옷장)을(를)");
  assert.equal(copy.withObjectParticle("청년내일채움공제 "), "청년내일채움공제를", "trailing space dropped (L8)");
  const ok = { verified: true };
  // The helper reads verified_section_enabled from the result itself, so a screen cannot pass a wrong flag.
  const on = items => ({ verified_section_enabled: true, items });
  assert.equal(copy.largestStatedAmount(on([])), null);
  assert.equal(copy.largestStatedAmount(undefined), null, "not loaded");
  assert.equal(copy.largestStatedAmount(on([{ ...ok, program_type: "corporate_benefit", benefit_amount_monthly: 900000 }])), null, "a corporate amount is no support money");
  assert.equal(copy.largestStatedAmount(on([{ ...ok, program_type: "policy", benefit_amount_monthly: 200000 }, { ...ok, program_type: "scholarship", benefit_amount_semester: 2000000 }])), "학기 200만원", "compared per year, shown in its own unit");
  assert.equal(copy.largestStatedAmount(on([{ ...ok, program_type: "policy", benefit_amount_monthly: 100000, benefit_amount_semester: 2000000 }])), "학기 200만원", "both amounts of one item compared (L1)");
  // M6: the hero amount rests on 조건 확인됨 rows only; else the hero keeps the count.
  const mixed = [{ ...ok, program_type: "policy", benefit_amount_monthly: 100000 }, { verified: false, eligibility_status: "pass", program_type: "policy", benefit_amount_monthly: 500000 }];
  assert.equal(copy.largestStatedAmount(on(mixed)), "월 10만원", "an unconfirmed row's amount is not offered");
  assert.equal(copy.largestStatedAmount({ verified_section_enabled: false, items: mixed }), null, "no verified section, no amount claim");
  assert.equal(copy.largestStatedAmount({ items: mixed }), null, "an older server without the flag");
  assert.equal(copy.largestStatedAmount(on([mixed[1]])), null);
  const now = Date.parse("2026-10-31T03:00:00Z");
  assert.equal(copy.alertSheetMessage("2026-11-30", now), "2026년 11월 30일 마감\n7일·3일·1일 전에 알림을 보내드릴게요.");
  assert.equal(copy.alertSheetMessage("2026-11-03", now), "2026년 11월 3일 마감\n3일·1일 전에 알림을 보내드릴게요.", "only the days still ahead (D-3 is today, 12:00 KST)");
  const late = Date.parse("2026-10-31T11:30:00Z"); // 20:30 KST: after the worker's last run of the day
  assert.equal(copy.alertSheetMessage("2026-11-03", late), "2026년 11월 3일 마감\n1일 전에 알림을 보내드릴게요.");
  assert.equal(copy.alertSheetMessage("2026-10-31", now), null, "no alert day left");
  assert.equal(copy.alertSheetMessage(null, now), null);
});

test("회원가입을 환영합니다 only after a real first sign-up (login's is_new), once", () => {
  const { markFirstSignup, takeFirstSignup } = load("lib/firstSignup.ts");
  assert.equal(takeFirstSignup(), false, "no mark: an existing member finishing onboarding (홈, 마이페이지, 신청 도우미) sees no welcome");
  markFirstSignup();
  assert.equal(takeFirstSignup(), true);
  assert.equal(takeFirstSignup(), false, "consumed once");
  const read = file => fs.readFileSync(path.join(__dirname, "..", file), "utf8");
  const login = read("app/login.tsx");
  assert.match(login, /if \(data\.is_new\) \{ markFirstSignup\(\);/, "set only from the server's is_new");
  assert.equal((login.match(/markFirstSignup\(\)/g) ?? []).length, 1);
  assert.match(login, /return; \}\n\s*\/\/[^\n]*\n\s*takeFirstSignup\(\);/, "a login that is not new drops a lingering mark");
  assert.match(read("lib/session.ts"), /export async function signOut\(\): Promise<void> \{\n\s*takeFirstSignup\(\);/, "logout drops it too");
  const step3 = read("app/onboarding/step3.tsx");
  assert.match(step3, /const firstSignup = takeFirstSignup\(\);\s*\/\/[^\n]*\n\s*if \(mode === "edit" && router\.canDismiss\(\)\)/, "taken before any early return, so it never lingers");
  assert.match(step3, /if \(firstSignup && typeof next === "string"\) router\.replace\(\{ pathname: "\/onboarding\/welcome"/);
  for (const file of ["app/apply-assistant.tsx", "app/(tabs)/index.tsx", "app/(tabs)/profile.tsx", "app/(tabs)/explore.tsx", "components/RecommendationList.tsx"]) assert.doesNotMatch(read(file), /markFirstSignup|onboarding\/welcome/, file);
});

test("titles show the server's title_display; the raw title only when it is absent (scan by the names program, p, item)", () => {
  const { displayTitle } = load("lib/api.ts");
  assert.equal(displayTitle({ title: "(변경·중지·연장 등) 부산광역시 청년 월세 지원", title_display: "부산광역시 청년 월세 지원" }), "부산광역시 청년 월세 지원");
  assert.equal(displayTitle({ title: "부산 청년 월세", title_display: "  " }), "부산 청년 월세", "blank falls back");
  assert.equal(displayTitle({ title: "부산 청년 월세" }), "부산 청년 월세", "older servers and /my/saved rows");
  // Every screen and component, not a hand-picked list: a programme title rendered from a payload
  // that carries title_display goes through displayTitle. Listed files read payloads the server
  // sends without title_display (or titles that are not programme titles); each says why.
  // Limit: the scan knows programme values by their usual names (program, p, item). A render through
  // another name (row, benefit, …) or a new render inside a listed file is not seen.
  const noTitleDisplay = {
    "app/agent-history.tsx": "conversation titles",
    "app/assistant.tsx": "assistant bundle items carry no title_display",
    "app/assistant-search.tsx": "search items carry no title_display",
    "app/compare.tsx": "comparison guidance carries no title_display",
    "components/AssistantBriefingCard.tsx": "briefing items carry no title_display",
    "app/form-fill.tsx": "the form screen names the programme by its stored title, as its records and vault copies do (lib/documentName programFormName)",
  };
  const root = path.resolve(__dirname, "..");
  const files = dir => fs.readdirSync(path.join(root, dir), { withFileTypes: true }).flatMap(e => e.isDirectory() ? files(path.join(dir, e.name)) : /\.tsx?$/.test(e.name) ? [path.join(dir, e.name)] : []);
  const raw = [];
  for (const file of [...files("app"), ...files("components")]) {
    if (noTitleDisplay[file]) continue;
    fs.readFileSync(path.join(root, file), "utf8").split("\n").forEach((line, i) => {
      // Stored records keep the stored title (vault and form history), they are not a render.
      if (/\b(program|p|item)\??\.title\b/.test(line) && !/program_title: |programTitle: /.test(line)) raw.push(`${file}:${i + 1}`);
    });
  }
  assert.deepEqual(raw, [], "raw programme titles");
  assert.match(fs.readFileSync(path.join(root, "components/RecommendationCard.tsx"), "utf8"), /title_display: data\.title_display/, "recommendation cards carry title_display");
});

test("every path id in lib/api.ts is encoded (ids come from route params and deep links)", () => {
  const source = fs.readFileSync(path.resolve(__dirname, "..", "lib/api.ts"), "utf8");
  const raw = [...source.matchAll(/`\/api\/v1\/[^`]*`/g)].map(m => m[0]).filter(url => /\$\{(?!encodeURIComponent\()[^}]+\}/.test(url.replace(/\?[^`]*/, "")));
  assert.deepEqual(raw, []);
});

test("no unused module keeps its own deadline or income rules (L5)", () => {
  const root = path.resolve(__dirname, "..");
  for (const file of ["components/FilterSheet.tsx", "components/IncomeBracketGuide.tsx", "constants/incomeMedian.ts"]) assert.equal(fs.existsSync(path.join(root, file)), false, file);
  assert.doesNotMatch(fs.readFileSync(path.join(root, "lib/api.ts"), "utf8"), /function programStatusLabel/);
  assert.doesNotMatch(fs.readFileSync(path.join(root, "store/onboarding.ts"), "utf8"), /setStep2/, "the legacy setter has no caller");
});

test("관심 혜택 cards say what the server says: cardStatus of the matching recommendation (L2)", () => {
  const { savedCardStatus } = load("lib/verifiedSections.ts");
  const items = [{ program_id: "a", verified: true, eligibility_status: "pass" }, { program_id: "b", verified: false, eligibility_status: "pass" }];
  assert.equal(savedCardStatus(items, true, "a"), "verified");
  assert.equal(savedCardStatus(items, true, "b"), "unknown");
  assert.equal(savedCardStatus(items, false, "a"), "pass", "flag off: never 조건 확인됨");
  assert.equal(savedCardStatus(items, false, "b"), "pass");
  assert.equal(savedCardStatus(items, true, "missing"), undefined, "no recommendation: the card's own default");
  assert.equal(savedCardStatus(undefined, true, "a"), undefined, "recommendations not loaded");
  assert.match(fs.readFileSync(path.resolve(__dirname, "..", "app/(tabs)/manage.tsx"), "utf8"), /eligibility=\{savedCardStatus\(/);
});

test("form-fill: one name for the form across header, shared file, 작성 기록 and vault copy (stored title)", () => {
  const { programFormName, applicationFormName } = load("lib/documentName.ts");
  const program = { title: "(변경·중지·연장 등) 부산광역시 청년 월세 지원", title_display: "부산광역시 청년 월세 지원" };
  // Records and vault copies store program.title; the header and the shared file must read the same.
  assert.equal(programFormName(program), applicationFormName(program.title));
  const source = fs.readFileSync(path.resolve(__dirname, "..", "app/form-fill.tsx"), "utf8");
  assert.doesNotMatch(source, /applicationFormName\(displayTitle\(/, "no second name for the same form");
});
