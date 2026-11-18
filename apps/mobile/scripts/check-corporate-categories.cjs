// 기업혜택 browses by category chips, not one chip per company. Runs the shipped
// chip logic (lib/corporateChips.ts) and API client with a stubbed fetch, and
// reads the Rust chip table; no network. The browser flow is in
// tools/design/check-ui.mjs.
const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const ts = require("typescript");
process.env.EXPO_PUBLIC_API_URL ??= "https://api.fixture.invalid";

const mobile = path.resolve(__dirname, "..");
const root = path.resolve(mobile, "../..");
const read = relative => fs.readFileSync(path.resolve(root, relative), "utf8");
const loaded = new Map();
function load(relative) {
  const filename = path.resolve(mobile, relative);
  if (loaded.has(filename)) return loaded.get(filename).exports;
  const module = { exports: {} };
  loaded.set(filename, module);
  const source = ts.transpileModule(fs.readFileSync(filename, "utf8"), { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText;
  const dependency = name => {
    if (name === "./apiBaseUrl") return load("lib/apiBaseUrl.ts");
    if (name === "./errorCopy") return load("lib/errorCopy.ts");
    if (name === "./displayText") return load("lib/displayText.ts");
    if (name === "../constants/displayLabels") return load("constants/displayLabels.ts");
    if (name === "../constants/districts.generated" || name === "./districts.generated") return load("constants/districts.generated.ts");
    if (name === "../store/auth") return { useAuthStore: { getState: () => ({ token: null, user: null }) } };
    throw new Error(`Unmocked dependency ${name}`);
  };
  new Function("require", "module", "exports", source)(dependency, module, module.exports);
  return module.exports;
}

// mazimi_core::corporate_categories::CATEGORIES, the server's chip codes and labels.
const rust = read("crates/core/src/corporate_categories.rs");
const start = rust.indexOf("pub const CATEGORIES:");
const CATEGORIES = [...rust.slice(start, rust.indexOf("];", start)).matchAll(/\("([a-z]+)", "([^"]+)"\)/g)].map(m => ({ code: m[1], label: m[2] }));
const { corporateChips, chipQuery, keptSelection, countFilters } = load("lib/corporateChips.ts");
const counts = { items: [{ code: "dev", label: "개발·소프트웨어", count: 2 }, { code: "design", label: "디자인·창작", count: 1 }], total: 3 };

test("the server defines a small chip set, labelled in Korean, ending in 기타", () => {
  assert.ok(CATEGORIES.length >= 6 && CATEGORIES.length <= 9, `${CATEGORIES.length} chips`);
  assert.equal(CATEGORIES.at(-1).label, "기타");
  for (const { label } of CATEGORIES) assert.match(label, /^[가-힣][가-힣·]*$/);
});

test("chips: 전체 then the server's chips, none without the category axis", () => {
  assert.deepEqual(corporateChips(counts, true).map(c => `${c.label}:${c.count}`), ["전체:0", "개발·소프트웨어:2", "디자인·창작:1"]);
  // Older server (probe failed), probe still running, no counts yet, nothing to choose: no chip row.
  assert.deepEqual(corporateChips(counts, false), []);
  assert.deepEqual(corporateChips(counts, undefined), []);
  assert.deepEqual(corporateChips(undefined, true), []);
  assert.deepEqual(corporateChips({ items: [], total: 0 }, true), []);
  // One category: its chip would list exactly what 전체 lists.
  assert.deepEqual(corporateChips({ items: [counts.items[0]], total: 2 }, true), []);
  // Counts of other filters while new ones load: chips stay, numbers hidden.
  assert.deepEqual(corporateChips(counts, true, false).map(c => c.count), [0, 0, 0]);
});

test("the chip is sent only on 기업혜택 of a server that filters by it", () => {
  assert.equal(chipQuery("design", true, true), "design");
  assert.equal(chipQuery("design", false, true), undefined, "other tabs");
  assert.equal(chipQuery("design", true, false), undefined, "older server");
  assert.equal(chipQuery("design", true, undefined), undefined, "probe pending");
  assert.equal(chipQuery("", true, true), undefined, "전체");
});

test("a chip the fresh counts no longer show is dropped", () => {
  assert.equal(keptSelection("design", counts), "design");
  assert.equal(keptSelection("food", counts), "");
  assert.equal(keptSelection("food", undefined), "food", "kept while counts load");
  assert.equal(keptSelection("", counts), "");
});

test("counts share the list's search, status and region, nothing else", () => {
  assert.deepEqual(countFilters({ q: "스타벅스", status: "open", region: "busan", sort: "deadline", provider: "corporate", corporate_category: "food", per_page: 20 }), { q: "스타벅스", status: "open", region: "busan" });
  assert.deepEqual(countFilters({ status: "open", income_band: "51-75", sort: "deadline" }), { status: "open", income_band: "51-75" }, "소득 구간 narrows the counts like the list");
  assert.deepEqual(countFilters({ q: undefined, status: "", region: undefined }), {});
});

test("the client reads the counts with those filters and sends the chip as corporate_category", async () => {
  const calls = [];
  global.fetch = async url => { calls.push(new URL(String(url))); return new Response(JSON.stringify({ items: [], total: 0, page: 1, per_page: 20, has_next: false }), { status: 200, headers: { "Content-Type": "application/json" } }); };
  const { api } = load("lib/api.ts");
  assert.equal(typeof api.getBrands, "undefined", "the brand chip list is gone");
  // No client path builds a per-company view: the list client takes no brand filter.
  assert.doesNotMatch(read("apps/mobile/lib/api.ts"), /brand_id|\/api\/v1\/brands/);
  await api.getCorporateCategories();
  assert.equal(calls.at(-1).pathname, "/api/v1/programs/corporate-categories");
  assert.equal(calls.at(-1).search, "");
  await api.getCorporateCategories(countFilters({ q: "어도비", status: "open", region: "busan" }));
  assert.deepEqual(Object.fromEntries(calls.at(-1).searchParams), { q: "어도비", status: "open", region: "busan" });
  await api.getPrograms({ provider: "corporate", corporate_category: "design", q: "어도비" });
  assert.equal(calls.at(-1).searchParams.get("corporate_category"), "design");
  assert.equal(calls.at(-1).searchParams.get("q"), "어도비");
});

test("explore draws the chips with the existing filter pill and no company chips", () => {
  // Wiring and styles are source facts; the behaviour is in the tests above and check-ui.
  const explore = read("apps/mobile/app/(tabs)/explore.tsx");
  assert.doesNotMatch(explore, /getBrands|brand_id|brandId|모든 기업/);
  for (const name of ["corporateChips(", "chipQuery(", "keptSelection(", "countFilters("]) assert.ok(explore.includes(name), name);
  const row = explore.split("\n").find(line => line.includes("contentContainerStyle={s.chips}"));
  assert.ok(row, "chip row");
  for (const style of ["s.filter,", "s.filterActive", "s.filterValue,", "s.filterValueActive"]) assert.ok(row.includes(style), style);
  assert.doesNotMatch(row, /#[0-9a-f]{3,8}|fontSize|color:/i, "no style values of its own");
  assert.match(explore, /chips: \{ paddingHorizontal: 16, paddingVertical: 8, gap: 8 \},/, "the brand row's spacing");
  for (const { label } of CATEGORIES.filter(c => c.label !== "기타")) assert.ok(!explore.includes(label), `${label} comes from the server`);
  assert.match(explore, /placeholder="혜택명이나 기업명으로 검색"/);
});
