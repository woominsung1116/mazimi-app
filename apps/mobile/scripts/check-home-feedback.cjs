// v1.0.0 real-device feedback, home / explore / detail: one D-day colour rule,
// the 7-day 마감 임박 filter, provider/field tabs, display text, condition
// counts and the guest sign-up dialog. Loads the shipped lib modules; no network.
const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const ts = require("typescript");
process.env.EXPO_PUBLIC_API_URL ??= "https://api.fixture.invalid";

const root = path.resolve(__dirname, "..");
const read = relative => fs.readFileSync(path.join(root, relative), "utf8");
let auth = { token: null, user: null };
const loaded = new Map();
function load(relative) {
  const filename = path.resolve(root, relative);
  if (loaded.has(filename)) return loaded.get(filename).exports;
  const module = { exports: {} };
  loaded.set(filename, module);
  const source = ts.transpileModule(fs.readFileSync(filename, "utf8"), { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText;
  const dependency = name => {
    if (name.endsWith(".png")) return { asset: name };
    if (name === "./api") return load("lib/api.ts");
    if (name === "./fields") return load("constants/fields.ts");
    if (name === "./deadline") return load("lib/deadline.ts");
    if (name === "./verifiedSections") return load("lib/verifiedSections.ts");
    if (name === "./apiBaseUrl") return load("lib/apiBaseUrl.ts");
    // errorCopy has no imports of its own; the real copy keeps api.ts error text honest here.
    if (name === "./errorCopy") return load("lib/errorCopy.ts");
    if (name === "./displayText") return load("lib/displayText.ts");
    if (name === "../constants/displayLabels") return load("constants/displayLabels.ts");
    if (name === "@/constants/districts.generated" || name === "./districts.generated") return load("constants/districts.generated.ts");
    if (name === "../store/auth") return { useAuthStore: { getState: () => auth } };
    throw new Error(`Unmocked dependency ${name}`);
  };
  new Function("require", "module", "exports", source)(dependency, module, module.exports);
  return module.exports;
}

const deadline = load("lib/deadline.ts");
const fields = load("constants/fields.ts");
const categoryStyle = load("constants/categoryStyle.ts");
const api = load("lib/api.ts");
const copy = load("lib/displayCopy.ts");
const regions = load("constants/displayLabels.ts");
const kst = (date, time = "12:00") => Date.parse(`${date}T${time}:00+09:00`);
const NOW = kst("2026-10-23");
const on = date => ({ deadline_at: date });

test("one D-day colour rule: red from D-2 down, everywhere", () => {
  const { deadlineTone, URGENT_DAYS, DEADLINE_TONE_COLORS } = deadline;
  assert.equal(URGENT_DAYS, 2);
  assert.equal(deadlineTone(on("2026-10-26"), NOW), "open", "D-3 is not red");
  assert.equal(deadlineTone(on("2026-10-25"), NOW), "urgent", "D-2 is red");
  assert.equal(deadlineTone(on("2026-10-24"), NOW), "urgent", "D-1 is red");
  assert.equal(deadlineTone(on("2026-10-23"), NOW), "urgent", "오늘 마감 is red");
  assert.equal(deadlineTone(on("2026-10-22"), NOW), "closed");
  assert.equal(deadlineTone({ program_status: "always_open" }, NOW), "none");
  assert.equal(deadlineTone({ available: false, deadline_at: "2026-10-24" }, NOW), "closed");
  assert.notEqual(DEADLINE_TONE_COLORS.urgent.background, DEADLINE_TONE_COLORS.open.background);
  // Same D-3 must never be red in one place and green in another: no screen picks badge colours itself.
  for (const file of ["app/(tabs)/index.tsx", "components/FigmaUI.tsx", "app/programs/[id].tsx", "components/DeadlineBadge.tsx", "app/notifications.tsx"]) {
    const source = read(file);
    assert.doesNotMatch(source, /i === 0 && \{ backgroundColor/, `${file}: first-card-red rule is gone`);
    assert.doesNotMatch(source, /getDDay|dDay <= 3/, `${file}: no local D-day thresholds`);
  }
  assert.match(read("components/DeadlineBadge.tsx"), /deadlineTone\(/);
  assert.match(read("app/(tabs)/index.tsx"), /<DeadlineBadge program=\{p\}/);
  assert.match(read("app/notifications.tsx"), /<DeadlineBadge program=\{item\}/, "alerts list: same rule");
  assert.doesNotMatch(read("app/notifications.tsx"), /due !== "마감" && \{ color/, "no always-green D-day");
});

test("deadline_date (KST) wins over timestamps", () => {
  assert.equal(deadline.effectiveDeadline({ deadline_date: "2026-10-25", effective_deadline: "2026-11-30T14:59:59Z" }), "2026-10-25");
  assert.equal(deadline.deadlineLabel({ deadline_date: "2026-10-25" }, NOW), "D-2");
  assert.equal(deadline.effectiveDeadline({ deadline_date: "bad", effective_deadline: "2026-11-30T14:59:59Z" }), "2026-11-30T14:59:59Z");
});

test("home 마감 임박 keeps only D-0..D-7; 전체 보기 is the deadline-sorted list", () => {
  const { isImminent, IMMINENT_DAYS } = deadline;
  assert.equal(IMMINENT_DAYS, 7);
  assert.equal(isImminent(on("2026-10-23"), NOW), true, "today");
  assert.equal(isImminent(on("2026-10-30"), NOW), true, "D-7");
  assert.equal(isImminent(on("2026-10-31"), NOW), false, "D-8");
  assert.equal(isImminent(on("2026-10-22"), NOW), false, "passed");
  assert.equal(isImminent({ program_status: "closed", deadline_at: "2026-10-24" }, NOW), false);
  assert.equal(isImminent({ program_status: "always_open" }, NOW), false, "rolling rows never count");
  const home = read("app/(tabs)/index.tsx");
  assert.match(home, /IMMINENT_QUERY = \{ status: "open", closing_within: IMMINENT_DAYS, sort: "deadline", per_page: 10 \}/);
  assert.match(home, /\.filter\(p => isImminent\(p\)\)/, "an older server ignores closing_within; the client filters again");
  assert.doesNotMatch(home, /30 \* 86400000/, "the old 30-day window is gone");
  assert.match(home, /params: \{ status: "open", sort: "deadline", category: ALL_TAB \}/, "전체 보기: every open benefit by deadline");
  assert.match(read("app/(tabs)/explore.tsx"), /value === "deadline_asc" \? "deadline"/, "old links still land on the deadline sort");
});

test("category tabs: 전체, provider axis, then field axis; legacy fallback", () => {
  assert.deepEqual(fields.PROVIDER_TABS.map(t => t.label), ["정책", "장학금", "기업혜택"]);
  assert.deepEqual(fields.FIELD_TABS.map(t => t.label), ["주거", "생활", "교육", "복지", "금융", "취업"]);
  assert.deepEqual(fields.tabQuery("provider:scholarship", true), { provider: "scholarship" });
  assert.deepEqual(fields.tabQuery("field:education", true), { field: "education" });
  assert.deepEqual(fields.tabQuery("", true), {});
  assert.deepEqual(fields.tabQuery("provider:corporate", false), { program_type: "corporate_benefit" });
  assert.deepEqual(fields.tabQuery("field:living", false), { program_type: "benefit" });
  assert.equal(fields.tabFromLegacy("scholarship"), "provider:scholarship");
  assert.equal(fields.tabFromLegacy("financial_product"), "field:finance");
  assert.equal(fields.tabFromLegacy("field:housing"), "field:housing");
  assert.equal(fields.tabFromLegacy("nonsense"), "");
  // A corporate scholarship is found under both 장학금 and 기업혜택 (F5.2).
  const corporateScholarship = { program_type: "corporate_benefit", provider_categories: ["scholarship", "corporate"], field_category: "education" };
  assert.equal(fields.matchesTab(corporateScholarship, "provider:scholarship"), true);
  assert.equal(fields.matchesTab(corporateScholarship, "provider:corporate"), true);
  assert.equal(fields.matchesTab(corporateScholarship, "provider:policy"), false);
  assert.equal(fields.matchesTab({ program_type: "housing" }, "field:housing"), true, "older server: program_type stands in");
  // A school's own offer is under the school-scoped 대학 tab only, never 기업혜택; a bank deposit is under 금융.
  const campusOffer = { program_type: "campus_benefit", provider_category: null, provider_categories: [], field_category: "living" };
  assert.equal(fields.matchesTab(campusOffer, "provider:corporate"), false, "server: no provider tab");
  assert.equal(fields.matchesTab(campusOffer, "provider:policy"), false, "server: no provider tab");
  assert.equal(fields.matchesTab({ program_type: "campus_benefit" }, "provider:corporate"), false, "older server: still not 기업혜택");
  assert.equal(fields.matchesTab(campusOffer, fields.CAMPUS_TAB), true);
  const deposit = { program_type: "financial_product", provider_category: null, provider_categories: [], field_category: "finance" };
  assert.equal(fields.matchesTab(deposit, "provider:corporate"), false);
  assert.equal(fields.matchesTab(deposit, "provider:policy"), false, "the server's empty answer is the answer");
  assert.equal(fields.matchesTab(deposit, "field:finance"), true);
  assert.equal(fields.fieldLabelOf({ program_type: "policy", field_category: "welfare", field_label: "복지" }, "정책"), "복지");
  assert.equal(fields.fieldLabelOf({ program_type: "policy" }, "정책"), "정책", "no field known: provider label");
  const tabs = read("components/FigmaUI.tsx");
  const order = ["{tab({ label: \"전체\"", "{providers.map(", "style={ui.tabDivider}", "{fields.map("].map(marker => tabs.indexOf(marker));
  assert.ok(order.every(i => i > 0) && order.join() === [...order].sort((a, b) => a - b).join(), "전체 → providers → divider → fields");
  // Figma restore (docs/design/figma-restore-20261026.md): both axes use the Figma tab 34065:3116; the
  // axes are told apart by the divider and their accessibility labels, no longer by mint chips.
  assert.match(tabs, /providers\.map\(item => tab\(item, `\$\{item\.label\}, 제공 주체`\)\)/);
  assert.match(tabs, /fields\.map\(item => tab\(item, `\$\{item\.label\}, 분야`\)\)/);
});

test("category styles (user decision 2026-10-31): one entry per category code, every file present", () => {
  const { CATEGORY_STYLE, CORPORATE_SUBCATEGORIES, categoryOf } = categoryStyle;
  // 기업혜택 chip codes from the server's list; travel and other have no glyph and fall back to corporate.
  const chips = [...read("../../crates/core/src/corporate_categories.rs").split("pub const CATEGORIES")[1].split("];")[0].matchAll(/^\s+\("([a-z]+)", "[^"]+"\),$/gm)].map(m => m[1]);
  assert.ok(chips.includes("dev") && chips.includes("other"), `parser lost the chip list: ${chips}`);
  assert.deepEqual([...CORPORATE_SUBCATEGORIES].sort(), chips.filter(code => code !== "travel" && code !== "other").sort());
  const codes = [...fields.PROVIDER_TABS.map(t => t.value), ...fields.FIELD_TABS.map(t => t.value), ...CORPORATE_SUBCATEGORIES.map(c => `corporate:${c}`), "campus", "agent", "default"];
  assert.deepEqual(Object.keys(CATEGORY_STYLE).sort(), [...codes].sort(), "no entry missing, none extra");
  for (const code of codes) {
    const entry = CATEGORY_STYLE[code];
    for (const slot of ["image", "hero", "glyph"]) {
      assert.ok(entry[slot]?.asset, `${code}.${slot}`);
      assert.ok(fs.existsSync(path.join(root, "constants", entry[slot].asset)), `${code}.${slot}: ${entry[slot].asset}`);
    }
    for (const colour of ["tile", "accent"]) assert.match(entry[colour], /^#[0-9A-F]{6}$/, `${code}.${colour}`);
  }
  // Real server shapes (the server always fills field_category; company and school offers fall back to living).
  const corp = (extra = {}) => ({ program_type: "corporate_benefit", provider_categories: ["corporate"], field_category: "living", ...extra });
  assert.equal(categoryOf({ program_type: "policy", provider_categories: ["policy"], field_category: "welfare" }), "welfare", "a policy shows its field");
  assert.equal(categoryOf({ program_type: "policy", provider_categories: ["policy"], field_category: "education" }), "education", "education without 장학금 keeps its field");
  assert.equal(categoryOf({ program_type: "scholarship", provider_categories: ["scholarship"], field_category: "education" }), "scholarship", "장학금 beats its 교육 field");
  assert.equal(categoryOf(corp({ corporate_category: "food" })), "corporate:food", "a chip beats the living fallback");
  assert.equal(categoryOf(corp({ corporate_category: "travel" })), "corporate", "no travel glyph: the corporate tag, not living");
  assert.equal(categoryOf(corp({ corporate_category: "other" })), "corporate");
  assert.equal(categoryOf(corp({ corporate_category: null })), "corporate", "chip not stored yet");
  assert.equal(categoryOf(corp()), "corporate", "detail and recommendations of an older server carry no chip");
  assert.equal(categoryOf({ ...corp(), provider_categories: ["corporate", "scholarship"], field_category: "education" }), "scholarship", "a corporate scholarship without a chip is a scholarship");
  assert.equal(categoryOf({ ...corp({ corporate_category: "study" }), provider_categories: ["corporate", "scholarship"] }), "corporate:study", "a corporate chip is the most specific");
  assert.equal(categoryOf({ program_type: "policy", provider_categories: ["policy"], field_category: "welfare", corporate_category: "food" }), "welfare", "a chip only counts on a corporate offer");
  assert.equal(categoryOf({ program_type: "campus_benefit", provider_categories: [], field_category: "living" }), "campus", "a school's own offer is campus, not living");
  assert.equal(categoryOf({ program_type: "financial_product", provider_categories: [], field_category: "finance" }), "finance", "a bank product has no provider tab: its field");
  // Older servers: no category columns, program_type stands in.
  assert.equal(categoryOf({ program_type: "scholarship" }), "scholarship");
  assert.equal(categoryOf({ program_type: "corporate_benefit" }), "corporate");
  assert.equal(categoryOf({ program_type: "campus_benefit" }), "campus");
  assert.equal(categoryOf({ program_type: "housing" }), "housing");
  assert.equal(categoryOf({ program_type: "policy" }), "policy", "no field: the provider");
  // default: rows with no category at all (an application record); a catalogue row always has a field.
  assert.equal(categoryOf({ program_type: "x", provider_categories: [] }), "default");
  // No screen picks a category image or colour itself.
  for (const file of ["components/FigmaUI.tsx", "app/(tabs)/index.tsx", "app/applications.tsx", "app/programs/[id].tsx"]) {
    const source = read(file);
    assert.doesNotMatch(source, /icon-policy|name="policy"|"coin" : "policy"|room\.png|FIELD_IMAGES|FIELD_TILES/, file);
  }
  assert.match(read("app/(tabs)/index.tsx"), /image: CATEGORY_STYLE\.agent\.image/, "비서 banner uses agent.png");
});

test("display text: server summary_display first, raw summary cleaned on the client otherwise", () => {
  assert.equal(api.displaySummary({ summary: "○ 200만원", summary_display: "200만원" }), "200만원");
  assert.equal(api.displaySummary({ summary: "○ 200만원", summary_display: null }), "200만원", "M1: client fallback strips the bullet");
  assert.equal(api.summaryLine({ summary: "○ 장학생 신청서○ 재학증명서" }), "장학생 신청서 · 재학증명서");
  assert.deepEqual(api.summaryLinesOf({ summary: "x", summary_display: "a\nb", summary_lines: [] }), ["a", "b"], "server display beats the client fallback");
  assert.deepEqual(api.summaryLinesOf({ summary: "○ x", summary_display: null, summary_lines: ["서버"] }), ["서버"]);
  const passage = "○ 신규 가입 시 첫 달 무료○ 학생 인증 필요";
  assert.deepEqual(api.summaryLinesOf({ program_type: "corporate_benefit", summary: passage, summary_display: "신규 가입 시 첫 달 무료\n학생 인증 필요", summary_lines: ["a", "b"] }), [passage], "공식 안내 원문 stays verbatim");
  assert.equal(api.displaySummary({ summary: null }), "");
  assert.equal(api.summaryLine({ summary_display: "전문대학생 : 150만원\n종합대학생 : 200만원" }), "전문대학생 : 150만원 · 종합대학생 : 200만원");
  assert.equal(api.formatBenefit({ program_type: "scholarship", benefit_amount_monthly: null, benefit_amount_semester: null, benefit_amount_once: null }), "", "no 혜택 확인 placeholder (F5.5)");
});

test("display copy makes no eligibility claim", () => {
  assert.doesNotMatch(read("lib/displayCopy.ts"), /충족/, "no claim word (claim-copy guard)");
});

test("혜택 요약 sheet and official link wording", () => {
  const program = { title: "t", program_type: "scholarship", summary: "○ 200만원", summary_display: "200만원", summary_lines: ["200만원"], provider_name: "재단", required_documents: [{ id: "1", document_name: "재학증명서", description: null, is_required: true }], required_document_notes: ["첨부파일 참고"], submission_documents: null, benefit_amount_monthly: null, benefit_amount_semester: null, benefit_amount_once: null };
  const sections = copy.summarySections(program, ["연령: 만 19세 이상"], "온라인 신청");
  assert.deepEqual(sections.map(s => s.title), ["정보", "조건", "서류", "신청 방법"]);
  assert.ok(sections[0].lines.includes("200만원"));
  assert.deepEqual(sections[2].lines, ["재학증명서", "첨부파일 참고"], "the sheet's own bullet replaces ※");
  assert.equal(copy.destinationButtonLabel("공식 안내에서 계속"), "공식 안내 페이지로 이동");
  assert.equal(copy.destinationButtonLabel("신청 페이지로 이동"), "신청 페이지로 이동");
  const detail = read("app/programs/[id].tsx");
  assert.match(detail, />혜택 요약</);
  assert.doesNotMatch(detail, /'자세히 보기'/);
});

test("region labels are Korean, 전국 when nationwide (F5.8, constants/displayLabels)", () => {
  assert.equal(regions.regionsLabel(["busan", "ulsan"]), "부산 · 울산");
  const all = ["seoul", "busan", "daegu", "incheon", "gwangju", "daejeon", "ulsan", "sejong", "gyeonggi", "gangwon", "chungbuk", "chungnam", "jeonbuk", "jeonnam", "gyeongbuk", "gyeongnam", "jeju"];
  assert.equal(regions.regionsLabel(all), "전국");
  assert.equal(regions.regionsLabel(["nationwide"]), "전국");
  assert.equal(regions.regionsLabel(["unknown"]), "");
  assert.match(read("app/programs/[id].tsx"), /const regionText = regionLine\(p\)/, "detail region line from programRegionsLabel (전국 for an empty list)");
  assert.doesNotMatch(read("app/programs/[id].tsx"), /p\.regions\.join/);
  assert.equal(regions.applicationMethodText("정부24온라인신청||방문신청||직접입력"), "정부24 온라인 신청 · 방문 신청");
  assert.match(read("app/programs/[id].tsx"), /applicationText = applicationMethodText\(p\.application_method\)/, "no raw 신청방법 codes on detail or in 혜택 요약");
});

test("guests get the sign-up dialog on 찜, 알림 and 직접 추가", () => {
  const detail = read("app/programs/[id].tsx");
  // Only a guest gets the dialog; a member still restoring is never asked to log in.
  assert.match(detail, /member\.run\('save', \(\) => login\.prompt\(GUEST_SAVE_REASON\), !isSaved\)/);
  assert.match(detail, /member\.run\('alert', \(\) => login\.prompt\(GUEST_ALERT_REASON\), !alertOn\)/);
  assert.match(detail, /onGuest=\{\(\) => login\.prompt\(GUEST_DOCUMENT_REASON\)\}/);
  assert.match(detail, /\{login\.dialog\}/, "the dialog is mounted");
  assert.doesNotMatch(detail, /needLogin\(/, "no bare confirm() fallbacks left on this screen");
  assert.match(read("components/RecommendationList.tsx"), /const profile = user \? draft : null;/, "a guest draft is never conditions");
  assert.match(read("app/(tabs)/explore.tsx"), /listLoading = programs\.isLoading \|\| \(!campus && categories === undefined\)/, "no false empty state while the category probe runs");
  const dialog = read("components/LoginPromptDialog.tsx");
  assert.match(dialog, /router\.push\(LOGIN_RETURN_PATH\)/, "CTA opens login and returns here");
  assert.match(dialog, /LOGIN_PROMPT_CTA = "가입·로그인하고 계속하기"/);
});

test("alarm toggle shows a toast with 실행 취소", () => {
  const detail = read("app/programs/[id].tsx");
  assert.match(detail, /const undo = \(\) => alerts\.mutateAsync\(!enabled\);\s*toast\.show\(\{ text: enabled \? '마감 알림을 켰어요' : '마감 알림을 껐어요', undo \}\)/);
  assert.doesNotMatch(detail, /toast\.show\(\{ text: '알림 권한/, "the permission notice never drops the undo");
  assert.match(read("components/UndoToast.tsx"), /실행 취소/);
});

test("guests get one login nudge on home: the hero lock only (product decision, replaces the three locks)", () => {
  const home = read("app/(tabs)/index.tsx");
  const locks = home.match(/<LockedSection locked=\{isGuest\}/g) ?? [];
  assert.equal(locks.length, 1, "only the hero is locked");
  assert.match(home, /\*\/\}\n\s*<Tappable accessibilityRole="button" accessibilityLabel="내 조건 맞춤 추천 보기" onPress=\{openRecommendation\}/, "the 맞춤 추천 entry is never a second lock; it stays for everyone so the layout never shifts (L7)");
  assert.match(home, /member\.run\("reco", [^\n]*\(\) => login\.prompt\(/);
  // F4.2 "reads as tappable": since the Figma restore the entry is the Figma section header with its round chevron (34065:6266).
  assert.match(home, /accessibilityLabel="내 조건 맞춤 추천 보기" onPress=\{openRecommendation\} style=\{s\.recoEntry\}>(?:(?!<\/Tappable>)[\s\S])*<View style=\{ui\.sectionChevron\}>/, "the 맞춤 추천 entry shows the Figma chevron");
});

test("shared pressed feedback: the owned screens use Tappable, not bare Pressable", () => {
  for (const file of ["app/(tabs)/index.tsx", "app/(tabs)/explore.tsx", "app/programs/[id].tsx", "components/RecommendationList.tsx", "components/MyDocumentItems.tsx", "components/BenefitSummarySheet.tsx"]) {
    const source = read(file);
    assert.doesNotMatch(source, /<Pressable(?! style=\{StyleSheet\.absoluteFill\})/, `${file}: bare Pressable`);
  }
  const figma = read("components/FigmaUI.tsx");
  assert.equal((figma.match(/<Pressable/g) ?? []).length, 1, "FigmaUI: only the dropdown backdrop stays a bare Pressable");
  const tappable = read("components/Tappable.tsx");
  assert.match(tappable, /state\.pressed && !busy/, "pressed overlay");
  assert.match(tappable, /<ActivityIndicator/, "spinner while busy");
  assert.match(tappable, /if \(!isPromise\(result\)\) return;\s*setRunning\(true\)/, "an async onPress shows the spinner until it settles");
});

test("members read /recommendations; an older server falls back to the preview", async () => {
  auth = { token: "t", user: { id: "u" } };
  const calls = [];
  global.fetch = async (url, init = {}) => {
    calls.push(`${init.method ?? "GET"} ${new URL(url).pathname}`);
    if (url.includes("/api/v1/recommendations")) return { ok: false, status: 404, statusText: "Not Found", json: async () => ({ error: "not found" }) };
    return { ok: true, status: 200, json: async () => ({ total_available: 1, items: [] }) };
  };
  const result = await api.api.getRecommendations({ birth_year: 2000, region_code: "busan" });
  assert.equal(result.total_available, 1);
  assert.deepEqual(calls, ["GET /api/v1/recommendations", "POST /api/v1/recommend/preview"]);
  global.fetch = async () => ({ ok: false, status: 409, statusText: "Conflict", json: async () => ({ error: "맞춤 추천을 보려면 조건을 먼저 저장해 주세요.", code: "profile_required" }) });
  await assert.rejects(api.api.getRecommendations({ birth_year: 2000, region_code: "busan" }), error => error.status === 409);
  global.fetch = async () => ({ ok: true, status: 204, json: async () => { throw new Error("no body"); } });
  assert.equal(await api.api.deleteDocumentItem("x"), undefined, "204 has no body");
  auth = { token: null, user: null };
});

test("document item names follow the server rule", () => {
  assert.equal(api.validDocumentItemName("통장 사본"), true);
  assert.equal(api.validDocumentItemName("   "), false);
  assert.equal(api.validDocumentItemName("가".repeat(61)), false);
  assert.equal(api.validDocumentItemName("a\u0007b"), false);
  assert.equal(api.validDocumentItemName("a\u0085b"), false, "C1 controls too, like Rust char::is_control");
});

// Same cases as crates/core/src/display_text.rs prod_examples_read_as_expected:
// the client fallback must give the server's result.
test("client display normalizer matches the server's rules (M1)", () => {
  const { normalizeDisplayText } = load("lib/displayText.ts");
  const lines = text => normalizeDisplayText(text).lines;
  assert.equal(read("lib/displayText.ts"), fs.readFileSync(path.join(root, "..", "web/src/lib/displayText.ts"), "utf8"), "web keeps a byte-identical copy");
  assert.deepEqual(lines("○ 산모건강관리사 이용 시, 비용의 본인부담금 90% 지원(50만원 한도내)"), ["산모건강관리사 이용 시, 비용의 본인부담금 90% 지원(50만원 한도내)"]);
  assert.deepEqual(lines("고립·은둔형 청소년 원스톱 지원체계 구축‧운영"), ["고립·은둔형 청소년 원스톱 지원체계 구축‧운영"]);
  assert.deepEqual(lines("고등학생, 대학생 - 성적우수, 저소득, 특정분야별 장학금 지원 \r\n"), ["고등학생, 대학생 - 성적우수, 저소득, 특정분야별 장학금 지원"]);
  assert.deepEqual(lines(" ㅇ (사업 기간) 2004년 ~ 계속사업 \n\n ㅇ (사업 대상) 저소득층, 청년\n   - (시행 방법) 국비지원"), ["(사업 기간) 2004년 ~ 계속사업", "(사업 대상) 저소득층, 청년", "(시행 방법) 국비지원"]);
  assert.deepEqual(lines("전세사기 피해 예방\r\n- (사업기간) 26. 1. 1. ~ 예산 소진 시까지\r\n ▹무주택  ※ 본인, 배우자 모두 무주택"), ["전세사기 피해 예방", "(사업기간) 26. 1. 1. ~ 예산 소진 시까지", "무주택", "본인, 배우자 모두 무주택"]);
  assert.deepEqual(lines("○ 중증장애인들이 일상생활을 영위하는데 필요한 서비스를 적절하게 제공\n    함으로써 기본적인 생활보장\n"), ["중증장애인들이 일상생활을 영위하는데 필요한 서비스를 적절하게 제공 함으로써 기본적인 생활보장"]);
  assert.deepEqual(lines("❍ 취업 시 인센티브를 지원\n❍ 지원금을 홍천사랑카드로 지급"), ["취업 시 인센티브를 지원", "지원금을 홍천사랑카드로 지급"]);
  assert.deepEqual(lines("○ 장학생 신청서○ 재학증명서○ 2026년 1학기 성적증명서※ 자세한 사항은 홈페이지 참고"), ["장학생 신청서", "재학증명서", "2026년 1학기 성적증명서", "자세한 사항은 홈페이지 참고"]);
  assert.deepEqual(lines("지원하기 위함. 금리는 1.5%입니다. 기간은 26. 1. 1.부터예요!"), ["지원하기 위함.", "금리는 1.5%입니다.", "기간은 26. 1. 1.부터예요!"]);
  assert.deepEqual(lines("1. 주민등록초본\n - 2026년 발급분\n2. 소득금액증명원"), ["주민등록초본", "2026년 발급분", "소득금액증명원"]);
  assert.deepEqual(lines("가. 신청서 1부\n나) 서약서\n(3) 동의서\n① 통장 ② 신분증"), ["신청서 1부", "서약서", "동의서", "통장", "신분증"]);
  assert.deepEqual(lines("-10% 할인"), ["-10% 할인"]);
  assert.deepEqual(lines("○ 월 20만원 지원(※ 예산 소진 시 종료)"), ["월 20만원 지원(※ 예산 소진 시 종료)"]);
  assert.deepEqual(lines("• 첫째  ·  둘째"), ["첫째 · 둘째"], "• bullet stripped, inner middle dot kept, whitespace collapsed");
  assert.deepEqual(normalizeDisplayText(null), { display: null, lines: [] });
  assert.deepEqual(normalizeDisplayText("  \n ○ \n"), { display: null, lines: [] });
});

test("필요 서류 without a checklist: the none-required sentence, the notice text, or the fallback", () => {
  const base = { title: "t", program_type: "welfare", summary: null, provider_name: "구청", required_documents: [], required_document_notes: [], benefit_amount_monthly: null, benefit_amount_semester: null, benefit_amount_once: null };
  const none = { ...base, submission_documents: "해당없음", no_documents_required: true };
  assert.deepEqual(copy.documentTextLines(none), [copy.NO_DOCUMENTS_REQUIRED]);
  assert.equal(copy.NO_DOCUMENTS_REQUIRED, "공고상 별도 제출 서류 없음");
  assert.deepEqual(copy.summarySections(none, [], "방문")[2].lines, ["공고상 별도 제출 서류 없음"]);
  assert.deepEqual(copy.documentTextLines({ ...base, submission_documents: "신분증" }), ["신분증"], "the notice text, as written");
  assert.deepEqual(copy.documentTextLines({ ...base, submission_documents: null }), []);
  assert.deepEqual(copy.summarySections({ ...base, submission_documents: null }, [], "방문")[2].lines, ["필요 서류는 공식 안내에서 확인해 주세요."]);
  const detail = read("app/programs/[id].tsx");
  assert.match(detail, /documentTextLines\(p\)\.join\('\\n'\) \|\| '공식 신청처에서 제출 대상 여부와 필요 서류를 확인해 주세요\.'/, "the detail renders the shared lines, else today's fallback");
});

test("혜택 요약 sheet on an older server: no line starts with a bullet of its own (M1)", () => {
  const program = { title: "t", program_type: "scholarship", summary: "○ 지원대상: 대학생○ 지원내용: 200만원", provider_name: "재단", required_documents: [], required_document_notes: ["※ 원본 제출"], submission_documents: "① 신청서 ② 재학증명서", benefit_amount_monthly: null, benefit_amount_semester: null, benefit_amount_once: null };
  const sections = copy.summarySections(program, ["연령: 만 19세 이상"], "- 온라인 신청");
  const all = sections.flatMap(s => s.lines);
  for (const line of all) assert.doesNotMatch(line, /^[○◦•·\-※①-⑳❍ㅇ▪■□◆◇▶▷▹*]/, line);
  assert.deepEqual(sections[0].lines.slice(0, 2), ["지원대상: 대학생", "지원내용: 200만원"]);
  assert.deepEqual(sections[2].lines, ["신청서", "재학증명서", "원본 제출"]);
  assert.deepEqual(sections[3].lines, ["온라인 신청"]);
});

test("recommendation rows never fetch the programme one by one; the card body is the programme's own summary or nothing (M2, 34065:2930)", () => {
  const card = read("components/RecommendationCard.tsx");
  assert.doesNotMatch(card, /getProgram|useQuery/, "no per-row /programs/{id} request in the list");
  assert.match(card, /summary_display: data\.summary_display/, "uses the item's own fields");
  assert.doesNotMatch(card, /data\.(reasons|missing_checks)/, "no sentence built from the match reasons");
  assert.match(read("components/FigmaUI.tsx"), /\{summary \? <Text style=\{ui\.cardSummary\}/);
  const home = read("app/(tabs)/index.tsx");
  assert.match(home, /\.slice\(0, 4\)/, "home reads at most 4");
  assert.match(home, /loadEach\(summaryIds, api\.getProgram\)/, "only items without a summary are read");
  assert.match(home, /programById\.get\(item\.program_id\) \?\? recommendationCardProgram\(item\)/);
});

test("manage, vault and assistant buttons use the shared pressed-state Tappable (F4.3)", () => {
  for (const file of ["app/assistant.tsx", "app/document-vault.tsx"]) assert.match(read(file), /import Tappable from ["']@\/components\/Tappable["']/, file);
  // 관심 혜택 has no button of its own since the Figma prune; its cards are BenefitCard (Tappable).
  for (const file of ["app/(tabs)/manage.tsx", "app/assistant.tsx", "app/document-vault.tsx"]) assert.doesNotMatch(read(file), /TouchableOpacity/, `${file}: no bare TouchableOpacity`);
  assert.doesNotMatch(read("app/(tabs)/manage.tsx"), /<Pressable/);
  assert.doesNotMatch(read("app/assistant.tsx"), /<Pressable/);
  // The vault keeps Pressable only for the sheet's backdrop and its tap-swallowing body.
  assert.equal((read("app/document-vault.tsx").match(/<Pressable/g) ?? []).length, 2);
});

test("비서: no tab of its own (Figma 34065:6364 has five); it opens from a home banner slide, guests see it with one nudge (item 3)", () => {
  const tabs = read("app/(tabs)/_layout.tsx");
  assert.match(tabs, /name="agent" options=\{\{ title: "비서", href: null \}\}/, "the screen stays routable at /agent, without a tab");
  assert.doesNotMatch(tabs, /Ionicons|sparkles/, "only Figma icons in the tab bar");
  const order = [...tabs.matchAll(/Tabs\.Screen name="([a-z]+)"/g)].map(m => m[1]);
  assert.deepEqual(order, ["index", "explore", "agent", "manage", "vault", "profile"]);
  // M4: the last answer seen on this device holds the carousel's shape until the status query settles.
  const memory = new Map();
  const storage = { getItem: async key => memory.get(key) ?? null, setItem: async (key, value) => { memory.set(key, value); } };
  const tab = (() => { const source = ts.transpileModule(fs.readFileSync(path.join(root, "lib/agentTab.ts"), "utf8"), { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText; const module = { exports: {} }; new Function("require", "module", "exports", source)(name => name === "@react-native-async-storage/async-storage" ? { default: storage, __esModule: true } : {}, module, module.exports); return module.exports; })();
  assert.equal(tab.agentTabHiddenFor("guest", false, false), false, "guests always see it");
  assert.equal(tab.agentTabHiddenFor("pending", undefined, false), true, "restoring: the last answer (off) hides it");
  assert.equal(tab.agentTabHiddenFor("member", undefined, false), true, "status in flight: last answer");
  assert.equal(tab.agentTabHiddenFor("member", true, false), false, "the server's answer wins");
  assert.equal(tab.agentTabHiddenFor("member", undefined, null), false, "nothing known: shown");
  const entry = read("lib/useAgentEntry.ts");
  assert.match(entry, /agentTabHiddenFor\(memberState, enabledNow, agent\.isError \? null : lastEnabled\)/, "a failed status check shows the entry (L4)");
  assert.match(entry, /if \(user && typeof enabledNow === "boolean"\) \{ setLastEnabled\(enabledNow\); void writeAgentEnabled\(enabledNow\); \}/, "only a member's answer is stored");
  const home = read("app/(tabs)/index.tsx");
  assert.match(home, /\.\.\.\(agentHidden \? \[\] : \[\{ key: "agent"[^\n]*onPress: \(\) => router\.push\("\/agent"\)/, "the 비서 banner slide follows the same rule");
  const agent = read("app/(tabs)/agent.tsx");
  assert.match(agent, /!user \? <NonMemberState title="로그인하고 비서에게 맡겨 보세요"/, "guests: one nudge, restoring members: loading");
});

test("저장한 혜택, 다음 할 일 stays reachable for every member, whatever the 비서 flag or conversation (R-H2)", () => {
  const home = read("app/(tabs)/index.tsx");
  const slide = home.match(/\.\.\.\(user \? \[\{ key: "briefing"[^\n]*\] : \[\]\)/);
  assert.ok(slide, "a member-only briefing slide that does not read agentHidden");
  assert.doesNotMatch(slide[0], /agentHidden/);
  assert.match(slide[0], /onPress: \(\) => router\.push\("\/briefing"\)/);
  const briefing = read("app/briefing.tsx");
  assert.match(briefing, /user \? <ScrollView[^\n]*<AssistantBriefingCard \/>/, "the page renders the card for members");
  assert.doesNotMatch(briefing, /agent|entries/i, "the page never depends on the 비서 or its conversation");
  assert.match(read("app/_layout.tsx"), /<Stack\.Screen name="briefing"/);
  const agent = read("app/(tabs)/agent.tsx");
  assert.match(agent, /!status\.data\?\.enabled \? <ScrollView[^\n]*<AssistantBriefingCard \/>/, "비서 off: the card is still on the screen");
});

test("추천 혜택 목록 header is the Figma count row only, and the verified filter never fakes an empty list (R-M1, R-M3)", () => {
  const list = read("components/RecommendationList.tsx");
  const header = list.slice(list.indexOf("const header ="), list.indexOf("return <><FlatList"));
  // 34065:5283 has no exclusion summary, caveat or 내 조건 수정 link above the count row.
  assert.doesNotMatch(header, /FilterSummaryCard|제외|내 조건 수정|수령액|중복 제한/);
  assert.match(header, /\{sectionEnabled && <>\s*<Tappable accessibilityRole="checkbox"/, "the checkbox only while the server sorts verified rows");
  assert.match(list, /const onlyVerified = verifiedOnly && sectionEnabled;/);
  assert.match(list, /ListEmptyComponent=\{onlyVerified && ranked\.length \? <EmptyState title="조건 확인됨 혜택이 아직 없어요"[^>]*action="전체 보기" onAction=\{\(\) => setVerifiedOnly\(false\)\}/, "a filtered-empty list says so and offers the full list");
});

test("홈 banner carousel keeps the frame's four slides; 질문으로 혜택 찾기 stays reachable from /assistant", () => {
  const home = read("app/(tabs)/index.tsx");
  const slides = home.slice(home.indexOf("const slides = ["), home.indexOf("].slice(0, 4);") + 14);
  assert.match(slides, /\]\.slice\(0, 4\);$/, "four dots (34065:6259)");
  const order = [...slides.matchAll(/key: "(\w+)"/g)].map(m => m[1]);
  assert.deepEqual(order, ["vault", "briefing", "agent", "assistant", "search"], "priority order: the briefing and 비서 come before the search slide");
  assert.match(read("app/assistant.tsx"), /router\.push\('\/assistant-search'\)/, "the search screen keeps its own entry");
});
