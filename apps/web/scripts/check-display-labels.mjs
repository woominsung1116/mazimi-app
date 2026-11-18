// Users never see a raw code (region, program_type, profile status, 신청방법
// code). Loads the shipped src/lib modules and the Rust canonical lists; no
// network. A canonical code added without a label fails here.
// Run: node --test scripts/check-display-labels.mjs (from apps/web).
import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";

const web = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const root = path.resolve(web, "../..");
const ts = createRequire(import.meta.url)("typescript");
const loaded = new Map();
function load(relative) {
  const filename = path.resolve(web, "src/lib", relative);
  if (loaded.has(filename)) return loaded.get(filename).exports;
  const record = { exports: {} };
  loaded.set(filename, record);
  const source = ts.transpileModule(fs.readFileSync(filename, "utf8"), { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText;
  const dependency = name => {
    if (name === "./districts.generated") return load("districts.generated.ts");
    if (name === "./safeUrl") return load("safeUrl.ts");
    throw new Error(`Unmocked dependency ${name}`);
  };
  new Function("require", "module", "exports", source)(dependency, record, record.exports);
  return record.exports;
}

const { regionLabel, regionsLabel, programRegionsLabel, programRegionsChip, applicationMethodText, REGION_UNKNOWN_LABEL, REGION_NAME_ALIASES, PROGRAM_TYPES: CLIENT_PROGRAM_TYPES, PROGRAM_TYPE_FALLBACK, programTypeLabel } = load("displayLabels.ts");
const { DISTRICT_REGION_LABELS } = load("districts.generated.ts");
const { enrollmentLabel, employmentLabel } = load("profileCodes.ts");
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
// Arms of mazimi_core::canonical_region_code: every name the server reads, by code.
const rustRegionNames = (() => {
  const source = read("crates/core/src/lib.rs");
  const start = source.indexOf("pub fn canonical_region_code");
  const body = source.slice(start, source.indexOf("\n}\n", start));
  const names = {};
  for (const arm of body.matchAll(/((?:"[^"]+"\s*\|\s*)*"[^"]+")\s*=>\s*Some\("([a-z]+)"\)/g)) {
    names[arm[2]] = [...arm[1].matchAll(/"([^"]+)"/g)].map(m => m[1]);
  }
  return names;
})();
const korean = value => /^[가-힣][가-힣 ·]*$/.test(value);

test("every canonical region code has a Korean label", () => {
  assert.equal(REGION_CODES.length, 17);
  for (const code of REGION_CODES) {
    const label = regionLabel(code);
    assert.ok(korean(label), `${code} → ${JSON.stringify(label)}`);
    assert.equal(regionLabel(code.toUpperCase()), label, "any case");
    assert.equal(regionLabel(code[0].toUpperCase() + code.slice(1)), label, "Ulsan");
  }
  assert.equal(regionLabel("ulsan"), "울산");
});

test("a region without a label shows nothing, never the raw value", () => {
  for (const raw of ["unknown", "", "  ", "mars", "jeonnam_gwangju", ":남구", null, undefined]) {
    assert.equal(regionLabel(raw), "", String(raw));
  }
  assert.equal(regionsLabel(["unknown"]), "");
  assert.equal(regionsLabel(null), "", "not recorded");
  assert.equal(regionsLabel(undefined), "");
  assert.equal(REGION_UNKNOWN_LABEL, "지역 정보 확인 필요");
});

test("sido:시군구, 전국 and lists read as Korean", () => {
  assert.equal(regionLabel("gangwon:속초시"), "강원 속초시");
  assert.equal(regionLabel("ulsan:남구"), "울산 남구");
  assert.equal(regionLabel("ulsan:31140"), "울산", "a numeric district is dropped, as in Rust region_display");
  assert.equal(regionLabel("ulsan:Nam-gu"), "울산", "a non-Korean district is dropped");
  assert.equal(regionLabel("전국"), "전국");
  assert.equal(regionLabel("nationwide"), "전국");
  assert.equal(regionsLabel(["busan", "Busan", "unknown", "ulsan:남구"]), "부산 · 울산 남구");
  assert.equal(regionsLabel(REGION_CODES), "전국");
  assert.equal(regionsLabel(REGION_CODES.slice(1)).split(" · ").length, 16);
});

test("an empty region list is nationwide; a campus offer shows no region", () => {
  assert.equal(regionsLabel([]), "전국");
  assert.equal(programRegionsLabel({ regions: [], program_type: "benefit" }), "전국");
  assert.equal(programRegionsChip({ regions: [], program_type: "benefit" }), "전국");
  assert.equal(programRegionsLabel({ regions: [], program_type: "campus_benefit" }), "");
  assert.equal(programRegionsChip({ regions: ["busan"], program_type: "campus_benefit" }), "");
  assert.equal(programRegionsLabel({ regions: null, program_type: "benefit" }), "");
  assert.equal(programRegionsChip({ regions: null, program_type: "benefit" }), "");
});

test("a region chip shows the first region and a count", () => {
  const program = regions => ({ regions, program_type: "welfare" });
  assert.equal(programRegionsChip(program(["busan", "ulsan", "daegu", "gyeongnam"])), "부산 외 3");
  assert.equal(programRegionsChip(program(["busan", "Busan"])), "부산");
  assert.equal(programRegionsChip(program(REGION_CODES)), "전국");
  assert.equal(programRegionsChip(program(["unknown"])), "");
});

test("every region value in the live catalogue (2026-10-22) is labelled or hidden", () => {
  const live = ["gyeonggi", "seoul", "jeonnam", "chungnam", "gyeongnam", "gyeongbuk", "jeonbuk", "chungbuk", "gangwon", "incheon", "busan", "gwangju", "ulsan", "daegu", "jeju", "sejong", "daejeon", "unknown"];
  for (const value of live) {
    const label = regionLabel(value);
    assert.ok(label === "" || korean(label), value);
    assert.notEqual(label, value);
  }
});

test("program types match crates/core PROGRAM_TYPES; none falls back", () => {
  assert.ok(PROGRAM_TYPES.length >= 10, "parser lost Rust PROGRAM_TYPES");
  assert.deepEqual(CLIENT_PROGRAM_TYPES, PROGRAM_TYPES);
  for (const { value, label } of PROGRAM_TYPES) {
    assert.equal(programTypeLabel(value), label, value);
    assert.notEqual(programTypeLabel(value), PROGRAM_TYPE_FALLBACK, value);
  }
  assert.equal(programTypeLabel("welfare"), "복지");
  assert.equal(programTypeLabel("youth_policy"), "청년정책");
  assert.equal(programTypeLabel("new_type"), PROGRAM_TYPE_FALLBACK);
});

test("region names match crates/core canonical_region_code", () => {
  assert.deepEqual(Object.keys(rustRegionNames).sort(), [...REGION_CODES].sort());
  for (const [code, names] of Object.entries(rustRegionNames)) {
    const label = DISTRICT_REGION_LABELS[code];
    for (const name of names) assert.equal(regionLabel(name), label, `${name} → ${code}`);
    const aliases = names.filter(name => name !== code && name !== label);
    assert.deepEqual([...REGION_NAME_ALIASES[code]].sort(), aliases.sort(), `${code} aliases`);
  }
  assert.equal(regionLabel("울산광역시"), "울산");
  assert.equal(regionLabel("광주시"), "", "경기 광주시 is not 광주광역시");
});

test("profile labels hide an unrecognised code", () => {
  assert.equal(enrollmentLabel("on_leave"), "휴학");
  assert.equal(employmentLabel("preparing"), "취업 준비");
  assert.equal(enrollmentLabel("mystery_code"), "기타");
  assert.equal(employmentLabel("mystery_code"), "기타");
  assert.equal(employmentLabel(""), "");
  assert.equal(employmentLabel("프리랜서"), "프리랜서", "Korean text is kept");
});

test("보조금24 신청방법 codes read as words", () => {
  assert.equal(applicationMethodText("기타 온라인신청||방문신청||직접입력"), "온라인 신청 · 방문 신청");
  assert.equal(applicationMethodText("정부24온라인신청||방문신청"), "정부24 온라인 신청 · 방문 신청");
  assert.equal(applicationMethodText("방문신청"), "방문 신청");
  assert.equal(applicationMethodText("직접입력"), "");
  assert.equal(applicationMethodText("신청불필요"), "별도 신청 없음");
  assert.equal(applicationMethodText("한국장학재단 누리집에서 신청"), "한국장학재단 누리집에서 신청");
  assert.equal(applicationMethodText(null), "");
});

test("program-type labels have one home: displayLabels.ts", () => {
  assert.doesNotMatch(read("apps/web/src/lib/api.ts"), /export (const PROGRAM_TYPES|function programTypeLabel)/);
  assert.doesNotMatch(read("apps/mobile/lib/api.ts"), /function programTypeLabel/);
});

test("web and mobile share one label module", () => {
  assert.equal(read("apps/web/src/lib/displayLabels.ts"), read("apps/mobile/constants/displayLabels.ts"));
});

test("no screen renders stored regions or alert types raw", () => {
  const screens = ["apps/web/src/app/programs/page.tsx", "apps/web/src/app/programs/[id]/page.tsx", "apps/web/src/app/alerts/page.tsx", "apps/web/src/app/admin/programs/page.tsx"];
  for (const file of screens) {
    const source = read(file);
    assert.doesNotMatch(source, /regions(\?)?\.join\(|regions\[0\]\}/, file);
    assert.doesNotMatch(source, /\{alert\.alert_type\}/, file);
  }
});
