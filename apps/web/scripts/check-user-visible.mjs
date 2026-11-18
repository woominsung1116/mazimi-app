// User-visible web rules (review 2026-10-21): the preview's partial profile
// save, the destination URL check, KST deadlines and the verified sections.
// Loads the shipped src/lib modules; no network.
// Run: node --test scripts/check-user-visible.mjs (from apps/web).
import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";

const web = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const ts = createRequire(import.meta.url)("typescript");
const requireWeb = createRequire(path.join(web, "package.json"));
/** A loader with its own module cache, so a module can be read with a given env. */
function loader() {
  const loaded = new Map();
  function load(relative) {
    const filename = path.resolve(web, "src", relative.includes("/") ? relative : `lib/${relative}`);
    if (loaded.has(filename)) return loaded.get(filename).exports;
    const record = { exports: {} };
    loaded.set(filename, record);
    const source = ts.transpileModule(fs.readFileSync(filename, "utf8"), { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX } }).outputText;
    const dependency = name => {
      if (name === "./safeUrl") return load("safeUrl.ts");
      if (name === "./profileCodes") return load("profileCodes.ts");
      if (name === "./optionalAnswers" || name === "@/lib/optionalAnswers") return load("optionalAnswers.ts");
      if (name === "react/jsx-runtime" || name === "react") return requireWeb(name);
      throw new Error(`Unmocked dependency ${name}`);
    };
    new Function("require", "module", "exports", source)(dependency, record, record.exports);
    return record.exports;
  }
  return load;
}
const load = loader();
/** Modules read with `env` set (flags are read when a module loads). */
function loadWithEnv(env, relative) {
  const before = Object.fromEntries(Object.keys(env).map(key => [key, process.env[key]]));
  Object.assign(process.env, env);
  try { return loader()(relative); } finally { for (const [key, value] of Object.entries(before)) { if (value === undefined) delete process.env[key]; else process.env[key] = value; } }
}
/** Modules read with every optional question shown. */
const loadWithQuestions = relative => loadWithEnv({ NEXT_PUBLIC_PROFILE_CONDITION_QUESTIONS_ENABLED: "true" }, relative);

const { previewProfileForSave } = load("profileSave.ts");
const { safeDestinationUrl, safeResourceUrl, resourceMetaLine } = load("api.ts");
const { daysLeft, deadlineLabel, effectiveDeadline } = load("deadline.ts");
const { cardStatus, remainingLabels, verifiedSections } = load("verifiedSections.ts");

const saved = {
  user_id: "u", birth_year: 1999, region_code: "busan", city_code: "26410", school_name: "부산대학교",
  school_year: 3, enrollment_status: "enrolled", employment_status: null, major_group: "engineering",
  income_bracket: 4, kosaf_support_bracket: null, housing_type: "monthly_rent", household_size: 2,
  has_disability: false, is_multicultural_family: false, is_low_income_household: false, veteran_family: false,
  preferred_categories: ["scholarship"], school_type: "university", age_band: "19-24", profile_version: 3, updated_at: "",
};
const asked = { region_code: "busan", birth_year: 2000, city_code: "", enrollment_status: "on_leave", employment_status: "", school_name: "부산대학교", income_bracket: null };
const busan = ["26110", "26410", "26500"];

test("이 조건 저장 keeps age_band and a district set elsewhere", () => {
  const body = previewProfileForSave(saved, asked, busan);
  assert.equal(body.age_band, "19-24");
  assert.equal(body.city_code, "26410", "saved 시·군·구 of the same sido is kept");
  assert.equal(body.income_bracket, 4, "an unanswered income keeps the saved bracket");
  assert.equal(body.birth_year, 2000);
  assert.equal(body.enrollment_status, "on_leave");
  assert.equal(body.employment_status, null);
  assert.equal(body.housing_type, "monthly_rent");
});

test("이 조건 저장 never sends a group flag no screen asks (the server keeps what a save omits)", () => {
  const body = previewProfileForSave(saved, asked, busan);
  for (const flag of ["has_disability", "is_multicultural_family", "is_low_income_household", "veteran_family"]) {
    assert.ok(!(flag in body), flag);
  }
});

test("a district picked on the preview wins; one of another sido is dropped", () => {
  assert.equal(previewProfileForSave(saved, { ...asked, city_code: "26500" }, busan).city_code, "26500");
  const moved = previewProfileForSave(saved, { ...asked, region_code: "seoul" }, ["11110", "11140"]);
  assert.equal(moved.city_code, null);
  assert.equal(moved.region_code, "seoul");
  const fresh = previewProfileForSave(null, { ...asked, income_bracket: 2 }, busan);
  assert.equal(fresh.age_band, undefined);
  assert.equal(fresh.city_code, null);
  assert.equal(fresh.income_bracket, 2);
});

test("이 조건 저장: undefined = the choice does not speak to the field (keep stored), null = cleared", () => {
  const { PREVIEW_STATUS_CHOICES } = load("profileSave.ts");
  const member = { ...saved, school_name: "부산대학교", enrollment_status: "enrolled", employment_status: "employed" };
  const choice = id => PREVIEW_STATUS_CHOICES.find(c => c.id === id);
  // Every row states both sides; none leaves a side to an empty-string guess.
  for (const c of PREVIEW_STATUS_CHOICES) for (const side of ["enrollment", "employment"]) assert.ok(side in c && c[side] !== "", `${c.id}.${side}`);
  const answer = (id, school_name = undefined) => ({ ...asked, enrollment_status: choice(id).enrollment, employment_status: choice(id).employment, school_name });
  // 재학, 학교명 blank: 취업 상태 and the school are not spoken to.
  const enrolled = previewProfileForSave(member, answer("enrolled"), busan);
  assert.equal(enrolled.enrollment_status, "enrolled");
  assert.equal(enrolled.employment_status, "employed");
  assert.equal(enrolled.school_name, "부산대학교");
  // 취업 준비 says nothing about enrolment: a final-year student can be preparing for work.
  const preparing = previewProfileForSave(member, answer("preparing"), busan);
  assert.equal(preparing.employment_status, "preparing");
  assert.equal(preparing.enrollment_status, "enrolled");
  assert.equal(preparing.school_name, "부산대학교");
  // A typed school replaces; an explicit null clears; nothing stored and nothing said stays null.
  assert.equal(previewProfileForSave(member, answer("enrolled", "동아대학교"), busan).school_name, "동아대학교");
  assert.equal(previewProfileForSave(member, { ...answer("graduated"), employment_status: null }, busan).employment_status, null);
  const none = previewProfileForSave(null, answer("preparing"), busan);
  assert.equal(none.enrollment_status, null);
  assert.equal(none.school_name, null);
});

const withAnswers = { ...saved, homeless_household: true, marital_status: "married", newlywed: true,
  children_count: 1, income_median_band: "le100" };
const skipped = { homeless_household: null, marital_status: null, newlywed: null, children_count: null, income_median_band: null };
const OPTIONAL_KEYS = ["homeless_household", "marital_status", "newlywed", "children_count", "income_median_band"];

test("default flags: only 소득 구간 is asked, with neutral copy; a save sends only income_median_band", () => {
  const flags = load("optionalAnswers.ts");
  assert.equal(flags.PROFILE_CONDITION_QUESTIONS_ENABLED, false);
  assert.equal(flags.INCOME_QUESTION_ENABLED, true);
  const { renderToStaticMarkup } = requireWeb("react-dom/server");
  const { createElement } = requireWeb("react");
  const OptionalConditions = load("components/OptionalConditions.tsx").default;
  const html = renderToStaticMarkup(createElement(OptionalConditions, { value: skipped, onChange() {} }));
  assert.ok(html.includes("선택 질문이에요. 답하면 소득 조건이 있는 혜택을 더 정확히 걸러드려요."));
  assert.ok(html.includes("소득 구간 (기준 중위소득 %)"));
  for (const hidden of ["무주택 여부", "혼인·자녀", "법률상 혼인"]) assert.ok(!html.includes(hidden), hidden);
  const household = OPTIONAL_KEYS.filter(key => key !== "income_median_band");
  const changed = previewProfileForSave(withAnswers, { ...asked, optional: { ...skipped, marital_status: "unmarried", income_median_band: "le60", homeless_household: false } }, busan);
  assert.equal(changed.income_median_band, "le60");
  for (const key of household) assert.equal(key in changed, false, key);
  assert.equal(previewProfileForSave(withAnswers, { ...asked, optional: skipped }, busan).income_median_band, "le100", "a skip keeps the saved band");
  for (const key of OPTIONAL_KEYS) assert.equal(key in previewProfileForSave(withAnswers, asked, busan), false, `no optional step: ${key}`);
});

test("NEXT_PUBLIC_INCOME_QUESTION_ENABLED=false: nothing renders and a save sends no optional key", () => {
  const env = { NEXT_PUBLIC_INCOME_QUESTION_ENABLED: "false" };
  const { renderToStaticMarkup } = requireWeb("react-dom/server");
  const { createElement } = requireWeb("react");
  const OptionalConditions = loadWithEnv(env, "components/OptionalConditions.tsx").default;
  assert.equal(renderToStaticMarkup(createElement(OptionalConditions, { value: skipped, onChange() {} })), "");
  const save = loadWithEnv(env, "profileSave.ts").previewProfileForSave;
  for (const optional of [undefined, skipped, { ...skipped, marital_status: "unmarried", income_median_band: "le60", homeless_household: true }]) {
    const body = save(withAnswers, { ...asked, optional }, busan);
    for (const key of OPTIONAL_KEYS) assert.equal(key in body, false, key);
  }
});

test("questions shown: they render with neutral copy and a skip keeps the saved answer", () => {
  const { renderToStaticMarkup } = requireWeb("react-dom/server");
  const { createElement } = requireWeb("react");
  const OptionalConditions = loadWithQuestions("components/OptionalConditions.tsx").default;
  const html = renderToStaticMarkup(createElement(OptionalConditions, { value: skipped, onChange() {} }));
  for (const text of ["선택 질문이에요. 답하지 않아도 돼요.", "무주택 여부", "혼인·자녀", "소득 구간 (기준 중위소득 %)", "법률상 혼인"]) assert.ok(html.includes(text), text);
  assert.ok(!html.includes("혜택이 늘어"), "no promise about more benefits");
  const save = loadWithQuestions("profileSave.ts").previewProfileForSave;
  const kept = save(withAnswers, { ...asked, optional: skipped }, busan);
  assert.equal(kept.homeless_household, true);
  assert.equal(kept.marital_status, "married");
  assert.equal(kept.newlywed, true);
  assert.equal(kept.income_median_band, "le100");
  const changed = save(withAnswers, { ...asked, optional: { ...skipped, marital_status: "unmarried", income_median_band: "unknown" } }, busan);
  assert.equal(changed.marital_status, "unmarried");
  assert.equal(changed.newlywed, null, "신혼 goes with 기혼");
  assert.equal(changed.income_median_band, "unknown");
  assert.equal("homeless_household" in save(withAnswers, asked, busan), false, "no optional step: no keys");
});

test("a destination is opened only on a public HTTPS host of its declared origin", () => {
  const at = (url, origin = new URL(url).origin) => safeDestinationUrl({ url, origin, platform: "", kind: "information", evidence: "official_url", label: "", explanation: "" });
  assert.equal(at("https://www.gov.kr/portal/service"), "https://www.gov.kr/portal/service");
  for (const url of ["https://intranet.local/apply", "https://apply.internal/x", "https://router.home.arpa/", "https://www.gov.kr./a", "https://[::1]/a", "https://10.0.0.1/a", "http://www.gov.kr/a", "https://u:p@www.gov.kr/a", "https://www.gov.kr:8443/a"]) {
    assert.equal(at(url), null, url);
  }
  assert.equal(at("https://www.gov.kr/a", "https://evil.example"), null, "origin mismatch");
  assert.equal(safeDestinationUrl(null), null);
});

test("a resource link is shown only inside its review window", () => {
  const resource = { id: "r", kind: "template", url: "https://www.gov.kr/form.hwp", source_url: "https://www.gov.kr/notice", label: "신청서", access: "direct", file_format: "hwp", byte_size: 1, content_hash: null, verified_at: "2026-10-01T00:00:00Z", valid_until: "2026-10-30T00:00:00Z" };
  assert.equal(safeResourceUrl(resource, Date.parse("2026-10-21T00:00:00Z")), "https://www.gov.kr/form.hwp");
  assert.equal(safeResourceUrl(resource, Date.parse("2026-10-31T00:00:00Z")), null);
  assert.equal(safeResourceUrl({ ...resource, url: "https://files.internal/form.hwp" }, Date.parse("2026-10-21T00:00:00Z")), null);
});

test("deadlines use the earliest end date in KST days", () => {
  const kst = (date, time) => Date.parse(`${date}T${time}:00+09:00`);
  assert.equal(daysLeft("2026-10-23", kst("2026-10-21", "08:00")), 2);
  const program = { deadline_at: null, application_end_at: "2026-11-30T14:59:59Z", benefit_valid_until: "2026-10-25T14:59:59Z" };
  assert.equal(effectiveDeadline(program), "2026-10-25T14:59:59Z", "benefit_valid_until counts");
  assert.equal(deadlineLabel({ available: false }), "마감·종료");
});

test("sections key on verified only while the server flag is on", () => {
  const items = [{ program_id: "a", verified: true, eligibility_status: "pass" }, { program_id: "b", verified: false, eligibility_status: "pass", remaining_checks: [{ kind: "window", label: "신청 기간 확인이 필요해요" }] }];
  assert.equal(verifiedSections({ verified_section_enabled: false }, items), null);
  const on = verifiedSections({ verified_section_enabled: true }, items);
  assert.deepEqual([on.verified.length, on.needsCheck.length], [1, 1]);
  assert.equal(cardStatus(items[1], true), "unknown");
  assert.deepEqual(remainingLabels(items[1], true), ["신청 기간 확인이 필요해요"]);
});

test("a template's line has no empty format part and no verification date", () => {
  const resource = { file_format: "hwp", verified_at: "2026-10-06T16:00:00Z" };
  assert.equal(resourceMetaLine(resource), "HWP · 원본 양식");
  assert.equal(resourceMetaLine({ ...resource, file_format: null }), "원본 양식");
  assert.equal(resourceMetaLine({ ...resource, file_format: "" }), "원본 양식");
  const detail = fs.readFileSync(path.join(web, "src/app/programs/[id]/page.tsx"), "utf8");
  assert.doesNotMatch(detail, /확인 중이에요|다시 확인하고 있어요|로그인 후 제공될 수도/, "no pipeline status on the detail");
  assert.match(detail, /resources\.notice \|\| resources\.templates\.length > 0\) && <OfficialResources/);
  assert.match(detail, /최종 조건은 공식 공고에서 확인해 주세요/, "one quiet line with the official link");
});

const source = relative => fs.readFileSync(path.resolve(web, "src", relative), "utf8");

test("a missing detail page reads as not found or closed", () => {
  assert.match(source("app/programs/[id]/page.tsx"), /찾을 수 없거나 마감된 혜택이에요/);
});

test("candidate approval asks no amounts: it publishes a corporate offer, whose amounts are dropped", () => {
  const form = source("app/admin/sources/CandidateApprovalForm.tsx");
  assert.doesNotMatch(form, /benefit_amount_/);
  assert.doesNotMatch(form, /혜택액|지원금|금액/);
});

// QA 2026-10-23 H3: the detail CTA was fixed to bottom-0 with no z-index, under
// BottomNav (fixed bottom-0 z-40). Browser evidence: qa/fixes/web-cta.mjs.
test("the detail page's official-site CTA sits above the bottom nav", () => {
  const read = file => fs.readFileSync(path.resolve(web, file), "utf8");
  const nav = read("src/components/BottomNav.tsx");
  const detail = read("src/app/programs/[id]/page.tsx");
  const css = read("src/app/globals.css");
  const navZ = Number(nav.match(/fixed bottom-0[^"]*\bz-(\d+)/)?.[1]);
  const cta = detail.match(/<div className="(fixed [^"]*)" style=\{\{ bottom: "var\(--bottom-nav-height\)" \}\}>/);
  assert.ok(cta, "CTA is offset by --bottom-nav-height");
  assert.doesNotMatch(cta[1], /\bbottom-0\b/);
  assert.ok(Number(cta[1].match(/\bz-(\d+)/)?.[1]) > navZ, "CTA z-index above the nav");
  assert.match(css, /--bottom-nav-height:\s*calc\(57px \+ env\(safe-area-inset-bottom, 0px\)\)/);
  assert.match(css, /\.safe-area-pb\s*\{/, "the nav's safe-area class exists");
  assert.match(nav, /min-h-\[56px\]/, "nav item height matches --bottom-nav-height");
});

test("web onboarding asks for a nickname and 이 조건 저장 sends it (F2.6 parity)", () => {
  const { validNickname, NICKNAME_MAX } = load("nickname.ts");
  assert.equal(NICKNAME_MAX, 30);
  for (const bad of ["", "   ", "가".repeat(31)]) assert.equal(validNickname(bad), false, JSON.stringify(bad));
  for (const good of ["마지미", " 마지미 ", "가".repeat(30)]) assert.equal(validNickname(good), true, good);
  assert.equal(previewProfileForSave(saved, { ...asked, nickname: " 마지미 " }, busan).nickname, "마지미");
  assert.equal(previewProfileForSave(saved, { ...asked, nickname: "" }, busan).nickname, null, "asked but blank: null, so the server can require one");
  assert.equal("nickname" in previewProfileForSave(saved, asked, busan), false, "not asked: no key");
  const step1 = fs.readFileSync(path.join(web, "src/app/onboarding/page.tsx"), "utf8");
  assert.match(step1, /const canProceed = nicknameOk && /, "다음 waits for a valid nickname");
  assert.match(step1, /setStep1\(region, birthYear, nickname\)/);
  // L2: a signed-in member starts from the saved nickname; typing first wins.
  assert.match(step1, /session\.status !== "authenticated"\) return;/);
  assert.match(step1, /api\.getProfile\(\)\.then\(r => r\.profile\?\.nickname/);
  assert.match(step1, /setNickname\(current => current \|\| /);
  assert.match(fs.readFileSync(path.join(web, "src/app/preview/page.tsx"), "utf8"), /\{\n\s*nickname,\n\s*region_code: region,/, "the save passes the onboarding nickname");
});

test("web detail cleans the summary like the app when the server sends no summary_display (M1 parity)", () => {
  const read = relative => fs.readFileSync(path.resolve(web, "..", "..", relative), "utf8");
  assert.equal(read("apps/web/src/lib/displayText.ts"), read("apps/mobile/lib/displayText.ts"), "one normalizer, two byte-identical copies");
  const { normalizeDisplayText, displayLines } = load("displayText.ts");
  assert.deepEqual(normalizeDisplayText("○ 장학생 신청서○ 재학증명서※ 자세한 사항은 홈페이지 참고").lines, ["장학생 신청서", "재학증명서", "자세한 사항은 홈페이지 참고"]);
  assert.deepEqual(displayLines(null, null, "• 월 20만원  지원"), ["월 20만원 지원"]);
  assert.deepEqual(displayLines(["서버"], "a", "○ b"), ["서버"]);
  assert.deepEqual(displayLines([], "a\nb", "○ c"), ["a", "b"]);
  const detail = fs.readFileSync(path.join(web, "src/app/programs/[id]/page.tsx"), "utf8");
  assert.match(detail, /displayLines\(program\.summary_lines, program\.summary_display, program\.summary\)/);
  assert.match(detail, /isCorporate \? \(program\.summary\?\.trim\(\) \? \[program\.summary\.trim\(\)\] : \[\]\) : displayLines/, "a corporate passage (공식 안내 원문) stays verbatim");
});

test("web 찜 sends the intent (idempotent PUT) and re-reads the server after a failure (W-H1)", () => {
  const read = relative => fs.readFileSync(path.resolve(web, "..", "..", relative), "utf8");
  assert.equal(read("apps/web/src/lib/bookmarkSync.ts"), read("apps/mobile/lib/bookmarkSync.ts"), "one intent rule, two byte-identical copies");
  const button = fs.readFileSync(path.join(web, "src/components/BookmarkButton.tsx"), "utf8");
  assert.match(button, /setBookmarkToIntent\(\s*\{ put: \(value\) => api\.setBookmark\(programId, value\)/);
  assert.doesNotMatch(button, /setBookmarked\(previous\)/, "a failure never assumes the server did nothing");
  assert.match(button, /setBookmarked\(await isSaved\(\)\)/, "a failure shows the server's state");
  assert.match(fs.readFileSync(path.join(web, "src/lib/api.ts"), "utf8"), /method: "PUT",\s*headers: \{ "Content-Type": "application\/json" \},\s*body: JSON\.stringify\(\{ bookmarked \}\)/);
});

test("titles show the server's title_display; the raw title only when it is absent", () => {
  const { displayTitle } = load("api.ts");
  assert.equal(displayTitle({ title: "(변경·중지·연장 등) 부산광역시 청년 월세 지원", title_display: "부산광역시 청년 월세 지원" }), "부산광역시 청년 월세 지원");
  assert.equal(displayTitle({ title: "부산 청년 월세", title_display: " " }), "부산 청년 월세", "blank falls back");
  assert.equal(displayTitle({ title: "부산 청년 월세" }), "부산 청년 월세", "older servers and /my/saved rows");
  for (const file of ["app/programs/page.tsx", "app/programs/[id]/page.tsx", "app/preview/page.tsx", "app/my/saved/page.tsx", "app/dashboard/page.tsx"])
    assert.doesNotMatch(fs.readFileSync(path.join(web, "src", file), "utf8"), /\b(program|item)\.title\b/, `${file} renders the raw title`);
});

test("web 알림 마감 date is the deadline's KST day, in the page's numeric format", () => {
  const { formatDeadlineDate } = load("deadline.ts");
  // 24:00 KST on 11-30 is stored as 15:00Z; the D-day counts to 11-30, so must the date.
  assert.equal(formatDeadlineDate("2026-11-30T15:00:00Z", "numeric"), "2026. 11. 30.");
  assert.equal(formatDeadlineDate("2026-11-30", "numeric"), "2026. 11. 30.");
  assert.equal(formatDeadlineDate("2026-11-30T15:00:00Z"), "2026년 11월 30일", "the long form is unchanged");
  const page = fs.readFileSync(path.join(web, "src/app/alerts/page.tsx"), "utf8");
  assert.doesNotMatch(page, /toLocale(Date)?String/, "deadline text goes through lib/deadline");
  assert.match(page, /effectiveDeadline\(alert\)/, "the earliest end date, application_end_at included");
  assert.match(page, /formatDeadlineDate\(end, "numeric"\)/);
});

test("no unused module keeps its own deadline or alert rules (L5)", () => {
  assert.equal(fs.existsSync(path.join(web, "src/components/AlertCard.tsx")), false);
  assert.doesNotMatch(fs.readFileSync(path.join(web, "src/lib/api.ts"), "utf8"), /function programStatusLabel/);
});
