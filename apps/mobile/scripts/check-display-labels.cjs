// Users never see a raw code (region, program_type, profile status or field,
// 신청방법 code). Loads the shipped modules and the Rust canonical lists; no
// network. A canonical code added without a label fails here.
const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const ts = require("typescript");
// lib/apiBaseUrl.ts refuses a release bundle without an API origin.
process.env.EXPO_PUBLIC_API_URL ??= "https://api.fixture.invalid";

const mobile = path.resolve(__dirname, "..");
const root = path.resolve(mobile, "../..");
const loaded = new Map();
function load(relative) {
  const filename = path.resolve(mobile, relative);
  if (loaded.has(filename)) return loaded.get(filename).exports;
  const module = { exports: {} };
  loaded.set(filename, module);
  const source = ts.transpileModule(fs.readFileSync(filename, "utf8"), { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText;
  const dependency = name => {
    if (name === "./districts.generated") return load("constants/districts.generated.ts");
    if (name === "./apiBaseUrl") return load("lib/apiBaseUrl.ts");
    if (name === "./errorCopy") return load("lib/errorCopy.ts");
    if (name === "./displayText") return load("lib/displayText.ts");
    if (name === "../constants/displayLabels") return load("constants/displayLabels.ts");
    if (name === "../store/auth") return { useAuthStore: { getState: () => ({ token: null, user: null }) } };
    throw new Error(`Unmocked dependency ${name}`);
  };
  new Function("require", "module", "exports", source)(dependency, module, module.exports);
  return module.exports;
}

const { regionLabel, regionsLabel, programRegionsLabel, applicationMethodText, PROGRAM_TYPE_FALLBACK } = load("constants/displayLabels.ts");
const { regionLabel: pickerRegionLabel, REGIONS } = load("constants/regions.ts");
const { programTypeLabel } = load("lib/api.ts");
const { enrollmentLabel, employmentLabel } = load("lib/profileCodes.ts");
const { profileFieldList, profileFieldLabels } = load("lib/assistantTypes.ts");
const read = relative => fs.readFileSync(path.resolve(root, relative), "utf8");
const rustList = (file, name) => {
  const source = read(file);
  const start = source.indexOf(`pub const ${name}:`);
  assert.ok(start >= 0, `${name} in ${file}`);
  return source.slice(start, source.indexOf("];", start));
};
// mazimi_core::agent::REGION_CODES and mazimi_core::PROGRAM_TYPES keys.
const REGION_CODES = [...rustList("crates/core/src/agent.rs", "REGION_CODES").matchAll(/"([a-z]+)"/g)].map(m => m[1]);
const PROGRAM_TYPES = [...rustList("crates/core/src/lib.rs", "PROGRAM_TYPES").matchAll(/\("([a-z_]+)", "([^"]+)"\)/g)].map(m => ({ value: m[1], label: m[2] }));
const korean = value => /^[가-힣][가-힣 ·]*$/.test(value);

test("every canonical region code has a Korean label, in both tables", () => {
  assert.equal(REGION_CODES.length, 17);
  assert.deepEqual(REGIONS.map(r => r.code).sort(), [...REGION_CODES].sort());
  for (const code of REGION_CODES) {
    assert.ok(korean(regionLabel(code)), code);
    assert.equal(regionLabel(code), pickerRegionLabel(code), "one label per code");
    assert.equal(regionLabel(code.toUpperCase()), regionLabel(code));
  }
  assert.equal(regionLabel("Ulsan"), "울산");
});

test("the detail page's region line never shows a code", () => {
  // apps/mobile/app/programs/[id].tsx: 지역: regionsLabel(...) or REGION_UNKNOWN_LABEL.
  assert.equal(regionsLabel(["ulsan"]), "울산");
  assert.equal(regionsLabel(["gangwon:속초시", "unknown"]), "강원 속초시");
  assert.equal(regionsLabel(["unknown"]), "");
  assert.equal(regionsLabel([]), "전국", "an empty list is confirmed nationwide");
  assert.equal(programRegionsLabel({ regions: [], program_type: "campus_benefit" }), "", "school-scoped");
  assert.equal(regionLabel("울산광역시"), "울산");
  assert.equal(regionsLabel(REGION_CODES), "전국");
  assert.equal(pickerRegionLabel("mars"), "", "pickers drop an unknown value too");
  const page = read("apps/mobile/app/programs/[id].tsx");
  assert.doesNotMatch(page, /regions(\?)?\.join\(/);
  assert.match(page, /REGION_UNKNOWN_LABEL/);
  assert.match(page, /programRegionsLabel\(p\)/, "nationwide reads 전국, not 공식 공고에서 확인");
});

test("program types never fall back to the key", () => {
  assert.ok(PROGRAM_TYPES.length >= 10, "parser lost Rust PROGRAM_TYPES");
  for (const { value, label } of PROGRAM_TYPES) {
    assert.equal(programTypeLabel(value), label, value);
    assert.notEqual(programTypeLabel(value), PROGRAM_TYPE_FALLBACK, value);
  }
  assert.equal(programTypeLabel("welfare"), "복지", "same as web and Rust");
  assert.equal(programTypeLabel("new_type"), PROGRAM_TYPE_FALLBACK);
});

test("profile values and fields are labelled", () => {
  assert.equal(enrollmentLabel("research_student"), "수료 후 연구생");
  assert.equal(employmentLabel("self_employed"), "자영업");
  assert.equal(enrollmentLabel("mystery_code"), "기타");
  assert.equal(employmentLabel("mystery_code"), "기타");
  assert.equal(employmentLabel(""), "");
  assert.equal(profileFieldList(["birth_year", "region_code"]), "출생연도 · 거주 지역");
  assert.equal(profileFieldList(["education_stage", "major_category", "major_group", "city_code"]), "학력 · 전공 계열 · 거주 시·군·구");
  assert.equal(profileFieldList(["new_field", "other_field"]), "추가 프로필 조건");
  // Every rule field of mazimi_core::rule_engine::FIELDS (the one vocabulary
  // `field_label` reads) can be a missing field.
  const fields = [...rustList("crates/core/src/rule_engine.rs", "FIELDS").matchAll(/\bfield\(\s*"([a-z_]+)"/g)].map(m => m[1]);
  assert.ok(fields.length >= 20, "parser lost the rule_engine::FIELDS rows");
  // Fields no profile answers (rule_engine::NO_PROFILE_ANSWER: a source-coded
  // restriction with no question) read as the generic fallback until the
  // client gives them a label.
  const unanswered = [...rustList("crates/core/src/rule_engine.rs", "NO_PROFILE_ANSWER").matchAll(/"([a-z_]+)"/g)].map(m => m[1]);
  assert.ok(unanswered.length >= 1, "parser lost rule_engine::NO_PROFILE_ANSWER");
  for (const field of fields.filter(f => !unanswered.includes(f))) assert.ok(profileFieldLabels[field], `profileFieldLabels lacks ${field}`);
  for (const field of unanswered) assert.equal(profileFieldList([field]), profileFieldLabels[field] ?? "추가 프로필 조건");
});

test("보조금24 신청방법 codes read as words", () => {
  assert.equal(applicationMethodText("기타 온라인신청||방문신청||직접입력"), "온라인 신청 · 방문 신청");
  assert.equal(applicationMethodText("직접입력"), "");
  assert.equal(applicationMethodText("공고문 참조"), "공고문 참조");
});

test("web and mobile share one label module", () => {
  assert.equal(read("apps/web/src/lib/displayLabels.ts"), read("apps/mobile/constants/displayLabels.ts"));
});
