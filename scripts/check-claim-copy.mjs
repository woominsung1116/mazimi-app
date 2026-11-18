// Copy guard: the app must not tell users they qualify for a benefit.
// The API's eligibility_status "pass" is not a verified signal, so no screen
// and no server string may turn it into an eligibility claim. Every match of
// a claim phrase must sit inside an allow-listed snippet that gives a reason.
// Run: node --test scripts/check-claim-copy.mjs (from the repo root or an app).
import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const CLAIM = /충족|받을 수 있어요|조건 일치|해당돼요|자격이 돼요|대상이에요|일치해요|받을 수 있습니다|자격이 있/g;

const CLIENT_SOURCES = [
  "apps/mobile/app", "apps/mobile/components", "apps/mobile/lib", "apps/mobile/store",
  "apps/mobile/constants", "apps/web/src",
];
// Server strings reach users as reasons, summaries and check details.
const SERVER_SOURCES = ["crates/api/src/services", "crates/core/src/rule_engine.rs", "crates/core/src/condition_review"];

// file + a snippet that must contain the whole match. Line numbers drift; snippets do not.
const CLIENT_ALLOW = [
  { file: "apps/mobile/app/privacy-policy.tsx", text: "조건 충족 여부와 금액·기간은 모델이 아니라", reason: "Explains how results are computed; not a claim about the user." },
  { file: "apps/mobile/app/(tabs)/profile.tsx", text: "푸시 알림을 받을 수 있어요", reason: "About receiving push notifications, not benefits." },
];
// Server lines a claim pattern may match without being drawn ({ file, text,
// reason }). Empty: the answer validator no longer holds a phrase blocklist.
const SERVER_ALLOW = [];

function* walk(target, ext) {
  if (fs.statSync(target).isFile()) { yield target; return; }
  for (const entry of fs.readdirSync(target, { withFileTypes: true })) {
    const full = path.join(target, entry.name);
    if (entry.isDirectory()) { if (entry.name !== "node_modules") yield* walk(full, ext); }
    else if (ext.test(entry.name)) yield full;
  }
}

const rel = file => path.relative(root, file).split(path.sep).join("/");

/** Every claim match in `text`, with its column. */
function matches(file, line, text, from = 0, to = text.length) {
  return [...text.slice(from, to).matchAll(CLAIM)].map(m => ({ file, line, text, start: from + m.index, end: from + m.index + m[0].length }));
}

/** Client sources: every match on every line. */
function clientHits() {
  return CLIENT_SOURCES.flatMap(dir => [...walk(path.join(root, dir), /\.(tsx?|jsx?|mjs|cjs)$/)].flatMap(file =>
    fs.readFileSync(file, "utf8").split("\n").flatMap((text, i) => matches(rel(file), i + 1, text))));
}

/** Rust sources: matches inside string literals of non-test, non-comment code.
 * The `#[cfg(test)] mod` at the end of a file and `//` comment lines are skipped.
 * Literals are read per line, so a claim split across a `\` continuation is missed. */
function serverHits() {
  return SERVER_SOURCES.flatMap(dir => [...walk(path.join(root, dir), /\.rs$/)].flatMap(file => {
    const lines = fs.readFileSync(file, "utf8").split("\n");
    const testMod = lines.findIndex((l, i) => l.trim() === "#[cfg(test)]" && /^\s*mod\s/.test(lines[i + 1] ?? ""));
    return lines.slice(0, testMod < 0 ? lines.length : testMod).flatMap((text, i) => {
      if (text.trim().startsWith("//")) return [];
      return [...text.matchAll(/"(?:[^"\\]|\\.)*"/g)].flatMap(lit => matches(rel(file), i + 1, text, lit.index, lit.index + lit[0].length));
    });
  }));
}

/** True when an occurrence of the entry's snippet on the same line covers the whole match. */
function covered(hit, entry) {
  if (entry.file !== hit.file) return false;
  for (let at = hit.text.indexOf(entry.text); at >= 0; at = hit.text.indexOf(entry.text, at + 1)) {
    if (at <= hit.start && hit.end <= at + entry.text.length) return true;
  }
  return false;
}

const show = h => `${h.file}:${h.line}:${h.start + 1}: …${h.text.slice(Math.max(0, h.start - 30), h.end + 30).trim()}…`;

for (const [name, hits, allow] of [["client", clientHits, CLIENT_ALLOW], ["server", serverHits, SERVER_ALLOW]]) {
  test(`no eligibility claim copy in ${name} sources outside the allow-list`, () => {
    const unexpected = hits().filter(h => !allow.some(a => covered(h, a)));
    assert.deepEqual(unexpected.map(show), [], "Claim copy found. Reword it (pass is not verified) or allow-list the exact snippet with a reason.");
  });
  test(`every ${name} allow-list entry still covers a match and has a reason`, () => {
    const found = hits();
    const stale = allow.filter(a => !found.some(h => covered(h, a)));
    assert.deepEqual(stale.map(a => `${a.file}: ${a.text}`), [], "Remove stale allow-list entries.");
    for (const a of allow) assert.ok(a.reason.length > 0, `${a.file} needs a reason`);
  });
}

test("an allow-listed snippet does not excuse another claim on the same line", () => {
  const entry = { file: "x.tsx", text: "푸시 알림을 받을 수 있어요" };
  const line = 'Alert.alert("푸시 알림을 받을 수 있어요"); <Text>자격 충족</Text>';
  const hits = matches("x.tsx", 1, line);
  assert.equal(hits.length, 2);
  assert.deepEqual(hits.map(h => covered(h, entry)), [true, false]);
});
