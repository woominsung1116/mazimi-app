// Copy guard (user rules 2026-11-05: never show copy the user doesn't care about, and
// add none unless strictly necessary). Every sentence the app can draw must be text from
// the .fig (constants/figma-tokens.json `texts`, derived by tools/design/export-tokens.mjs)
// or an entry of scripts/copy-allowlist.json, filed under the reason it is shown. That is
// every sentence written in app/, components/ and lib/ (`sentences`), and every one the
// server writes in crates/api/src and crates/core/src outside their tests (`server`: 비서 answers, cards, errors;
// words it only matches or sends to the model are filed as never drawn). A sentence that
// explains internal behaviour, hedges, or restates another has no reason to be filed
// under; delete it at its source instead. No network.
//   node scripts/check-copy.cjs           run the check
//   node scripts/check-copy.cjs --print   list sentences that are neither, and stale entries
const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const ts = require("typescript");

const root = path.resolve(__dirname, "..");
const ALLOWLIST = path.join(__dirname, "copy-allowlist.json");
const figma = new Set(JSON.parse(fs.readFileSync(path.join(root, "constants/figma-tokens.json"), "utf8")).texts);
const allow = JSON.parse(fs.readFileSync(ALLOWLIST, "utf8"));

// Whole files whose text is a legal document: reviewed as documents, not line by line.
const LEGAL_DOCUMENTS = new Set(["app/privacy-policy.tsx", "app/terms.tsx"]);

const normalise = s => s.replace(/\s+/g, " ").trim();
const HANGUL = /\p{Script=Hangul}/u;
// A sentence, not a label: a sentence ending anywhere in it ("…해요 (1=최저)"), or the
// notice-style hedge "…수 있음/없음". "필요" is a noun; labels such as "선택 안 함" are not sentences.
const SENTENCE = /(?:(?<!필)요|다|까|죠)(?:[\s.!?…)]|$)|수 (?:있|없)음[.)]?$/;

// A word piece ("이에요", "다" in a suffix list) is parsing data, not something drawn.
const isSentence = text => HANGUL.test(text) && SENTENCE.test(text) && (text.includes(" ") || text.length >= 5);

/** Sentences written in one source file: string, template and JSX text, as { text, line }. */
function sentences(rel, source) {
  const file = ts.createSourceFile(rel, source, ts.ScriptTarget.Latest, true, rel.endsWith("x") ? ts.ScriptKind.TSX : ts.ScriptKind.TS);
  const out = [];
  // JSX text read with its <Text> (through nested elements and fragments), not again on its
  // own; text under a {condition ? <>…</> : …} inside a <Text> is read on its own.
  const isText = n => ts.isJsxElement(n) && /Text$/.test(n.openingElement.tagName.getText(file));
  const insideText = node => { for (let p = node.parent; p && (ts.isJsxElement(p) || ts.isJsxFragment(p)); p = p.parent) if (isText(p)) return true; return false; };
  const add = (node, raw) => {
    const text = normalise(raw);
    if (isSentence(text)) out.push({ text, line: file.getLineAndCharacterOfPosition(node.getStart()).line + 1 });
  };
  const visit = node => {
    if (ts.isStringLiteral(node) || ts.isNoSubstitutionTemplateLiteral(node)) add(node, node.text);
    else if (ts.isTemplateExpression(node)) add(node, node.head.text + node.templateSpans.map(span => "${}" + span.literal.text).join(""));
    else if (isText(node)) {
      // A <Text> reads as one sentence, nested <Text> and {expressions} included
      // ("대략 <Text>{n}구간</Text>에 해당해요", "첫 줄{"\n"}둘째 줄").
      const flat = el => el.children.map(c => ts.isJsxText(c) ? c.text
        : ts.isJsxElement(c) || ts.isJsxFragment(c) ? flat(c)
          : ts.isJsxExpression(c) && c.expression && ts.isStringLiteralLike(c.expression) ? c.expression.text
            : ts.isJsxExpression(c) && c.expression ? "${}" : "").join("");
      add(node, flat(node));
    }
    else if (ts.isJsxText(node)) add(node, node.text);
    ts.forEachChild(node, child => { if (!(ts.isJsxText(child) && insideText(child))) visit(child); });
  };
  visit(file);
  return out;
}

function sources() {
  const out = [];
  const walk = dir => {
    for (const entry of fs.readdirSync(path.join(root, dir), { withFileTypes: true })) {
      const rel = path.posix.join(dir, entry.name);
      if (entry.isDirectory()) walk(rel);
      else if (/\.tsx?$/.test(entry.name) && !/\.test\.tsx?$/.test(entry.name) && !LEGAL_DOCUMENTS.has(rel)) out.push(rel);
    }
  };
  // Every source of text a screen draws: screens, components, and the lib modules that
  // return display text (copy modules, errors, form-fill and 비서 helpers).
  ["app", "components", "lib"].forEach(walk);
  return out.map(rel => ({ rel, text: fs.readFileSync(path.join(root, rel), "utf8") }));
}

// The server writes the 비서 answers, cards and errors the app draws (crates/api). Its
// string literals outside `#[cfg(test)] mod` are read the same way: a `format!`
// placeholder reads as ${}, a `\` line continuation joins the lines.
// crates/core holds the rules whose text the API returns (application methods, procedure
// steps, rule reasons).
const SERVER_ROOTS = ["crates/api/src", "crates/core/src"].map(dir => path.resolve(root, "../..", dir));
function serverSources() {
  const walk = dir => fs.readdirSync(dir, { withFileTypes: true }).flatMap(e => e.isDirectory() ? walk(path.join(dir, e.name)) : e.name.endsWith(".rs") ? [path.join(dir, e.name)] : []);
  // A *tests.rs file is a test module of its parent (`#[cfg(test)] #[path] mod tests`).
  return SERVER_ROOTS.flatMap(walk).filter(file => !/tests\.rs$/.test(file)).map(file => ({ rel: path.relative(path.resolve(root, "../.."), file).split(path.sep).join("/"), text: fs.readFileSync(file, "utf8") }));
}
function rustSentences(rel, source) {
  const lines = source.split("\n");
  const testMod = lines.findIndex((l, i) => l.trim() === "#[cfg(test)]" && /^\s*mod\s/.test(lines[i + 1] ?? ""));
  // A char literal ('"', '\'') would pair with the next string's quote; it holds no text.
  const code = (testMod < 0 ? lines : lines.slice(0, testMod)).map(l => (l.trim().startsWith("//") ? "" : l)).join("\n").replace(/'(?:\\.|[^'\\\n])'/g, "' '");
  const out = [];
  for (const m of code.matchAll(/r#"([\s\S]*?)"#|"((?:[^"\\]|\\[\s\S])*)"/g)) {
    const text = normalise((m[1] ?? m[2]).replace(/\\\n\s*/g, "").replace(/\\n/g, " ").replace(/\{[^{}]*\}/g, "${}"));
    if (isSentence(text)) out.push({ text, line: code.slice(0, m.index).split("\n").length });
  }
  return out;
}

const surfaces = [
  { name: "app", key: "sentences", files: sources(), read: sentences, probeRel: "components/Probe.tsx" },
  { name: "server", key: "server", files: serverSources(), read: rustSentences, probeRel: "crates/api/src/probe.rs" },
];
for (const s of surfaces) {
  s.listed = new Map();
  for (const [reason, entries] of Object.entries(allow[s.key])) {
    assert.ok(allow.reasons[reason], `copy-allowlist.json: reason "${reason}" is not defined in "reasons"`);
    for (const text of entries) { assert.ok(!s.listed.has(text), `${s.key}: listed twice: ${text}`); s.listed.set(text, reason); }
  }
  s.scan = list => list.flatMap(({ rel, text }) => s.read(rel, text).filter(x => !figma.has(x.text) && !s.listed.has(x.text)).map(x => `${rel}:${x.line} ${x.text}`));
  s.written = () => new Set(s.files.flatMap(({ rel, text }) => s.read(rel, text).map(x => x.text)));
  s.probe = text => s.scan([{ rel: s.probeRel, text }]);
}
const [app, server] = surfaces;

if (process.argv.includes("--print")) {
  for (const s of surfaces) {
    for (const line of s.scan(s.files)) console.log(line);
    const now = s.written();
    for (const text of s.listed.keys()) if (!now.has(text)) console.log(`stale (${s.key}): ${text}`);
  }
} else {
  test("every sentence the app draws is .fig text or filed under a reason in copy-allowlist.json", () => {
    assert.deepEqual(app.scan(app.files), [], "new copy: delete it, or file it under a reason in scripts/copy-allowlist.json `sentences`");
    for (const noise of [
      "<Text>초안은 이 기기에서만 만들고 서버로 보내지 않아요.</Text>",
      'const note = "자격·중복 수혜는 공식 공고 확인이 필요해요";',
      "const hint = `최대 ${px}px 너비로 최적화됩니다`;",
      "<Text>대략 {n}구간에 해당해요</Text>",
      "<Text>대략 <Text style={b}>{n}구간</Text>에 해당해요 (1=최저 ~ 10=최고)</Text>",
      "<Text>실제와 다를 수 있음</Text>",
      "<Text><>정확한 판정이 아니에요</></Text>",
      "<Text>{ok ? <>정확한 판정이 아니에요</> : null}</Text>",
    ]) assert.notDeepEqual(app.probe(noise), [], `guard misses: ${noise}`);
    for (const label of ["<Text>관심 혜택</Text>", "<Text>다음</Text>", "<Text>선택 안 함</Text>", "<Text>가구원수 (본인 포함)</Text>", 'const SUFFIXES = ["이에요", "예요"];'])
      assert.deepEqual(app.probe(label), [], `not a sentence: ${label}`);
  });

  test("every sentence the server writes is filed under a reason in copy-allowlist.json", () => {
    assert.deepEqual(server.scan(server.files), [], "new server copy: delete it, or file it under a reason in scripts/copy-allowlist.json `server`");
    for (const noise of [
      'lines.push("결과는 모델이 아니라 규칙으로 계산했어요.".into());',
      'format!("‘{title}’은 참고용이며 실제와 다를 수 있음")',
      'format!("{n}개를 찾았어요. \\\n             위에서부터 보여 드려요.")',
      "let quote = '\"'; let note = \"결과는 모델이 아니라 규칙으로 계산했어요.\";",
    ]) assert.notDeepEqual(server.probe(noise), [], `guard misses: ${noise}`);
    assert.deepEqual(server.probe('#[cfg(test)]\nmod tests {\n    const X: &str = "테스트에서만 쓰는 문장이에요.";\n}'), [], "test code is not drawn");
  });

  test("every allow-listed sentence is still written somewhere (the list stays true)", () => {
    for (const s of surfaces) {
      const now = s.written();
      assert.deepEqual([...s.listed.keys()].filter(text => !now.has(text)), [], `stale entries: remove them from scripts/copy-allowlist.json \`${s.key}\``);
      assert.deepEqual([...s.listed.keys()].filter(text => figma.has(text)), [], `${s.key}: .fig text needs no entry`);
    }
  });
}
