// User-facing copy from the 2026-10-23 QA: server errors become Korean
// generic copy (never error.message), the explore loading state is not the
// empty state, and unknown routes get a Korean not-found screen.
const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const ts = require("typescript");

const root = path.resolve(__dirname, "..");
const read = relative => fs.readFileSync(path.join(root, relative), "utf8");
function load(relative) {
  const source = ts.transpileModule(read(relative), { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText;
  const module = { exports: {} };
  new Function("require", "module", "exports", source)(name => { throw new Error(`Unmocked dependency ${name}`); }, module, module.exports);
  return module.exports;
}
const httpError = (status, message) => Object.assign(new Error(message), { status });

test("server errors map to Korean generic copy and never echo the server message", () => {
  const { errorCopy, NOT_FOUND_PROGRAM } = load("lib/errorCopy.ts");
  const cases = [
    [httpError(503, "Service unavailable"), "잠시 후 다시 시도해 주세요."],
    [httpError(500, "relation \"programs\" does not exist"), "잠시 후 다시 시도해 주세요."],
    [httpError(502, "Bad Gateway"), "잠시 후 다시 시도해 주세요."],
    [httpError(404, "Profile not found"), NOT_FOUND_PROGRAM],
    [httpError(429, "Too many requests"), "요청이 많아요. 잠시 후 다시 시도해 주세요."],
    [httpError(401, "Unauthorized: session expired"), "로그인이 필요해요. 다시 로그인해 주세요."],
    [httpError(403, "Forbidden"), "이 기능을 쓸 수 있는 권한이 없어요."],
    [httpError(400, "invalid uuid"), "요청을 처리하지 못했어요. 다시 시도해 주세요."],
    [new TypeError("Failed to fetch"), "연결을 확인한 뒤 다시 시도해 주세요."],
    [null, "연결을 확인한 뒤 다시 시도해 주세요."],
  ];
  for (const [error, copy] of cases) {
    const shown = errorCopy(error);
    assert.equal(shown, copy, String(error?.message));
    if (error?.message) assert.ok(!shown.includes(error.message), "server text leaked");
    assert.match(shown, /[가-힣]/);
  }
  assert.equal(NOT_FOUND_PROGRAM, "찾을 수 없거나 마감된 혜택이에요");
  assert.equal(errorCopy(httpError(404, "x"), { notFound: "없는 신청이에요" }), "없는 신청이에요");
});

test("the programme detail never renders error.message", () => {
  const detail = read("app/programs/[id].tsx");
  assert.doesNotMatch(detail, /error\?\.message|e\.message/);
  assert.match(detail, /errorCopy\(/);
  assert.match(detail, /NOT_FOUND_PROGRAM/, "404 title shares one copy constant");
});

test("explore shows a skeleton while loading, not the empty-result copy or —건", () => {
  const explore = read("app/(tabs)/explore.tsx");
  assert.match(explore, /ProgramListSkeleton/);
  // listLoading also covers the wait for the category probe (the query is not enabled yet).
  assert.match(explore, /!items\.length && (?:programs\.isLoading|listLoading) && <ProgramListSkeleton/);
  assert.match(explore, /!items\.length && !(?:programs\.isLoading|listLoading) && <EmptyState/);
  assert.match(explore, /(?:programs\.isLoading|listLoading) \? "확인 중"/);
});

test("unknown routes get a Korean not-found screen with a home link and no sitemap", () => {
  const file = "app/+not-found.tsx";
  assert.ok(fs.existsSync(path.join(root, file)), file);
  const page = read(file);
  assert.match(page, /페이지를 찾을 수 없어요/);
  assert.match(page, /홈으로/);
  assert.doesNotMatch(page, /_sitemap|sitemap/i);
  // Without app/_sitemap.tsx Expo Router serves its route list at /_sitemap.
  assert.match(read("app/_sitemap.tsx"), /export \{ default \} from "\.\/\+not-found"/);
});

test("apiErrorText keeps sanitized Korean API copy and maps everything else", () => {
  const { apiErrorText, GENERIC_NOT_FOUND } = load("lib/errorCopy.ts");
  assert.equal(apiErrorText(httpError(409, "이미 신청 준비를 시작했어요.")), "이미 신청 준비를 시작했어요.");
  assert.equal(apiErrorText(httpError(503, "Service unavailable")), "잠시 후 다시 시도해 주세요.");
  assert.equal(apiErrorText(new TypeError("Network request failed")), "연결을 확인한 뒤 다시 시도해 주세요.");
  assert.equal(apiErrorText(httpError(404, "Profile not found")), GENERIC_NOT_FOUND);
  assert.equal(apiErrorText(null), "연결을 확인한 뒤 다시 시도해 주세요.");
});

test("screens that showed error.message use apiErrorText", () => {
  const sites = { "app/apply-assistant.tsx": 2, "app/compare.tsx": 1, "app/assistant.tsx": 1, "components/ApplicationJourneyCard.tsx": 1 };
  for (const [file, count] of Object.entries(sites)) {
    const source = read(file);
    assert.doesNotMatch(source, /(query|plan|save)\.error\??\.message|setError\(e\.message\)/, file);
    assert.ok((source.match(/apiErrorText\(/g) ?? []).length >= count, file);
  }
});
