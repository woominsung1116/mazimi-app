// Figma is the design source (docs/design/figma-restore-20261026.md). Every colour, font
// size, weight and line height the app writes must come from 마지미 (공유용).fig, and the
// shared tokens in constants/theme.ts must equal the frames they reproduce.
// constants/figma-tokens.json is derived from the .fig; `npm run check:tokens --prefix
// tools/design` fails when it no longer matches. No network.
const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const ts = require("typescript");

const root = path.resolve(__dirname, "..");
const tokens = JSON.parse(fs.readFileSync(path.join(root, "constants/figma-tokens.json"), "utf8"));
const palette = new Set(tokens.palette);
const c = tokens.components;

function loadTheme() {
  const source = ts.transpileModule(fs.readFileSync(path.join(root, "constants/theme.ts"), "utf8"), { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText;
  const module = { exports: {} };
  new Function("require", "module", "exports", source)(() => ({}), module, module.exports);
  return module.exports;
}
const { fig } = loadTheme();

// Source files that draw UI. Generated data and the legal text bodies carry no styles of their own.
const SKIP = new Set(["constants/districts.generated.ts", "constants/universities.json", "constants/figma-tokens.json"]);
function sources() {
  const out = [];
  const walk = dir => {
    for (const entry of fs.readdirSync(path.join(root, dir), { withFileTypes: true })) {
      const rel = path.posix.join(dir, entry.name);
      if (entry.isDirectory()) walk(rel);
      else if (/\.(tsx?|jsx?)$/.test(entry.name) && !SKIP.has(rel)) out.push(rel);
    }
  };
  ["app", "components", "constants", "lib", "store"].forEach(walk);
  return out;
}
const files = sources().map(rel => ({ rel, text: fs.readFileSync(path.join(root, rel), "utf8").replace(/\/\/[^\n]*|\/\*[\s\S]*?\*\//g, comment => comment.replace(/[^\n]/g, "")) }));
const lineOf = (text, index) => text.slice(0, index).split("\n").length;

function normalise(literal) {
  let m = literal.match(/^#([0-9a-f]{3,8})$/i);
  if (m) {
    let h = m[1].toUpperCase();
    if (h.length === 3 || h.length === 4) h = [...h].map(x => x + x).join("");
    if (h.length === 8 && h.endsWith("FF")) h = h.slice(0, 6);
    return "#" + h;
  }
  m = literal.match(/^rgba?\(\s*(\d+)\s*,\s*(\d+)\s*,\s*(\d+)\s*(?:,\s*([\d.]+)\s*)?\)$/i);
  if (m) {
    const hex = [m[1], m[2], m[3]].map(v => Number(v).toString(16).padStart(2, "0")).join("").toUpperCase();
    const a = m[4] == null ? 1 : Number(m[4]);
    return "#" + hex + (a >= 0.995 ? "" : Math.round(a * 255).toString(16).padStart(2, "0").toUpperCase());
  }
  return null;
}

// The form-fill preview draws what lib/formFill/pdf.ts writes into the official form
// (INK, STAMP_RED): document content, not app UI, so it keeps the PDF's colours.
// The preview draws a checked box as ✓, the check mark pdf.ts (drawCheck) strokes into the form.
const DOCUMENT_INK = { "components/formFill/Signature.tsx": ["#0D0D1A", "#D11F1F"], "components/formFill/PageView.tsx": ["#0D0D1A", "✓"] };

// Category tiles and accents are not .fig colours: the user's 2026-10-31 decision derives them from the
// category icons (tools/design/category-colors.mjs, whose --check pins them to the images). Only the
// tile/accent slots of constants/categoryStyle.ts are exempt, and the test below holds them to AA.
const CATEGORY_STYLE_FILE = "constants/categoryStyle.ts";
const categoryColours = text => [...text.matchAll(/\b(tile|accent): "(#[0-9A-F]{6})"/g)].map(m => ({ slot: m[1], hex: m[2], index: m.index }));

test("every colour literal in the app is a .fig colour", () => {
  const bad = [];
  for (const { rel, text } of files) {
    const ink = new Set([...(DOCUMENT_INK[rel] ?? []), ...(rel === CATEGORY_STYLE_FILE ? categoryColours(text).map(c => normalise(c.hex)) : [])]);
    for (const m of text.matchAll(/["'`]([^"'`\n]*)["'`]/g)) {
      for (const lit of m[1].matchAll(/#[0-9a-fA-F]{3,8}\b|rgba?\([^)]*\)/g)) {
        const value = normalise(lit[0]);
        if (value && !palette.has(value) && !ink.has(value)) bad.push(`${rel}:${lineOf(text, m.index)} ${lit[0]}`);
      }
    }
  }
  assert.deepEqual(bad, [], "colours outside the .fig palette (use fig.color in constants/theme.ts)");
});

test("category colours (user decision 2026-10-31): very light tiles, accents readable as 11px badge text (AA 4.5:1)", () => {
  const text = files.find(f => f.rel === CATEGORY_STYLE_FILE).text;
  const lin = c => { c /= 255; return c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4; };
  const lum = hex => [1, 3, 5].map(i => lin(parseInt(hex.slice(i, i + 2), 16))).reduce((sum, c, i) => sum + c * [0.2126, 0.7152, 0.0722][i], 0);
  const lines = text.split("\n").filter(line => /\b(tile|accent): "#/.test(line));
  assert.ok(lines.length >= 12, "every entry carries tile and accent");
  for (const line of lines) {
    const [tile, accent] = ["tile", "accent"].map(slot => line.match(new RegExp(`\\b${slot}: "(#[0-9A-F]{6})"`))?.[1]);
    assert.ok(tile && accent, line.trim().slice(0, 40));
    const ratio = (lum(tile) + 0.05) / (lum(accent) + 0.05);
    assert.ok(ratio >= 4.5, `${line.trim().slice(0, 30)}: accent on tile ${ratio.toFixed(2)}:1`);
    assert.ok(lum(tile) > 0.8, `${line.trim().slice(0, 30)}: tile ${tile} is not a light tint`);
  }
});

// Each guard is a scanner over { rel, text } so the same code checks the app and its own probes:
// every probe is a drift the guard must catch (a mutation test that runs with the guard).
const probe = (text, rel = "components/Probe.tsx") => [{ rel, text }];
const mustCatch = (scan, probes) => { for (const text of probes) assert.notDeepEqual(scan(probe(text)), [], `guard misses: ${text}`); };

const sizes = new Set(tokens.font_sizes), weights = new Set(tokens.font_weights.map(String)), heights = new Set(tokens.line_heights);
function scanTextValues(list) {
  const bad = [];
  for (const { rel, text } of list) {
    for (const m of text.matchAll(/fontSize:\s*(\d+(?:\.\d+)?)\b/g)) if (!sizes.has(Number(m[1]))) bad.push(`${rel}:${lineOf(text, m.index)} fontSize ${m[1]}`);
    for (const m of text.matchAll(/lineHeight:\s*(\d+(?:\.\d+)?)\b/g)) if (!heights.has(Number(m[1]))) bad.push(`${rel}:${lineOf(text, m.index)} lineHeight ${m[1]}`);
    for (const m of text.matchAll(/fontWeight:\s*["']?(\d+)["']?/g)) if (!weights.has(m[1])) bad.push(`${rel}:${lineOf(text, m.index)} fontWeight ${m[1]}`);
  }
  return bad;
}
test("font sizes, weights and line heights are .fig text values", () => {
  assert.deepEqual(scanTextValues(files), [], "text values outside the .fig (use fig.text in constants/theme.ts)");
  mustCatch(scanTextValues, ["{ fontSize: 15, lineHeight: 20 }", "{ fontSize: 13, lineHeight: 21 }", '{ fontWeight: "800" }', "{ fontWeight: 800 }"]);
});

const styles = new Set(tokens.text_styles);
const themeTypography = (() => { const m = {}; const source = ts.transpileModule(fs.readFileSync(path.join(root, "constants/theme.ts"), "utf8"), { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText; const module = { exports: {} }; new Function("require", "module", "exports", source)(() => ({}), module, module.exports); return module.exports.typography; })();
// typography.fontSize.md / typography.fontWeight.bold are written out so token-written styles are checked too.
const resolveTokens = block => block
  .replace(/typography\.fontSize(?:\.(\w+)|\[["'](\w+)["']\])/g, (_, a, b) => String(themeTypography.fontSize[a ?? b]))
  .replace(/typography\.fontWeight\.(\w+)/g, (_, a) => `"${themeTypography.fontWeight[a]}"`);
function scanTextStyles(list) {
  const bad = [];
  for (const { rel, text } of list) {
    for (const m of text.matchAll(/\{[^{}]*\}/g)) {
      if (!/\bfontSize\s*:/.test(m[0])) continue;
      // Every Figma text has a line height; without one (or with undefined) the platform's "normal" draws.
      // This holds whatever the size is written as: a number, a token or a variable.
      const height = m[0].match(/\blineHeight\s*:\s*([^,}\s]+)/);
      if (!height || /^(undefined|null)$/.test(height[1])) { bad.push(`${rel}:${lineOf(text, m.index)} fontSize without a lineHeight`); continue; }
      const block = resolveTokens(m[0]);
      const size = block.match(/\bfontSize:\s*(\d+)\b/), number = height[1].match(/^(\d+)$/);
      if (!size || !number) continue;
      // A spread style may carry the weight; only a block that sets none of its own is read as 400.
      if (!/fontWeight/.test(block) && /\.\.\./.test(block)) {
        if (![...styles].some(key => key.startsWith(`${size[1]}/`) && key.endsWith(`/${number[1]}`))) bad.push(`${rel}:${lineOf(text, m.index)} ${size[1]}/?/${number[1]}`);
        continue;
      }
      const weight = block.match(/fontWeight:\s*["']?(\d+)["']?/)?.[1] ?? "400";
      const key = `${size[1]}/${weight}/${number[1]}`;
      if (!styles.has(key)) bad.push(`${rel}:${lineOf(text, m.index)} ${key}`);
    }
  }
  return bad;
}
test("every text style has a line height, and literal size/weight/height together are a .fig text style", () => {
  assert.deepEqual(scanTextStyles(files), [], "size/weight/line-height combinations outside the .fig (use fig.text)");
  mustCatch(scanTextStyles, ["{ fontSize: 13 }", "{ fontSize: typography.fontSize.xs }", "{ fontSize: size, color }", "{ fontSize: 13, lineHeight: undefined }", "{ fontSize: typography.fontSize.sm, fontWeight: typography.fontWeight.bold, lineHeight: 20 }", "{ ...T.body13, fontSize: 16, lineHeight: 20 }", '{ fontSize: 13, fontWeight: "700", lineHeight: 20 }', "{ fontSize: 13, fontWeight: 700, lineHeight: 20 }"]);
});

// The UI page strokes at 0.5, 1, 2 and 4 px only, all solid.
const strokeWeights = new Set([0, 0.5, 1, 2, 4]);
// The expression a width is set to, up to the comma or brace that ends it.
const expressionAt = (text, index) => { let depth = 0, i = index; for (; i < text.length; i++) { const ch = text[i]; if ("([{".includes(ch)) depth++; else if (")]}".includes(ch)) { if (!depth) break; depth--; } else if (ch === "," && !depth) break; } return text.slice(index, i).trim(); };
// A number, a component token, or a condition choosing between .fig weights; no arithmetic and no bare variable.
const strokeOk = value => /^(K|fig\.component)\.[\w.]+$/.test(value)
  || (!/Math\.|[*/+]|\s-\s|hairline/.test(value) && /\d/.test(value) && [...value.matchAll(/(?<![\w.])(\d+(?:\.\d+)?)(?![\w.])/g)].every(n => strokeWeights.has(Number(n[1])))
    && (/^\d+(\.\d+)?$/.test(value) || [...value.matchAll(/\?/g)].length === [...value.matchAll(/(?<![\w.])\d+(?:\.\d+)?(?![\w.])/g)].length - 1));
// The stamp preview scales the PDF stamp (lib/formFill/pdf.ts): document content, like its ink.
const DOCUMENT_GEOMETRY = new Set(["lib/formFill/pdf.ts", "components/formFill/Signature.tsx"]);
function scanStrokes(list) {
  const bad = [];
  for (const { rel, text } of list.filter(f => !DOCUMENT_GEOMETRY.has(f.rel))) {
    if (/hairlineWidth/.test(text)) bad.push(`${rel}: StyleSheet.hairlineWidth (the .fig's thinnest stroke is 0.5)`);
    for (const m of text.matchAll(/border(?:Top|Right|Bottom|Left|Start|End)?Width:\s*/g)) {
      const value = expressionAt(text, m.index + m[0].length);
      if (!strokeOk(value)) bad.push(`${rel}:${lineOf(text, m.index)} ${m[0]}${value.slice(0, 40)}`);
    }
    for (const m of text.matchAll(/["'](dashed|dotted)["']/g)) bad.push(`${rel}:${lineOf(text, m.index)} ${m[0]} stroke`);
  }
  return bad;
}
test("stroke widths are .fig stroke weights, never hairline or dashed", () => {
  assert.deepEqual(scanStrokes(files), [], "strokes outside the .fig");
  mustCatch(scanStrokes, ["{ borderWidth: 1.5 }", "{ borderLeftWidth: 3 }", "{ borderWidth: StyleSheet.hairlineWidth }", "{ borderWidth: W }", "{ borderTopWidth: active ? 3 : 1 }", "{ borderWidth: on ? (a ? 2 : 1.5) : 0 }", "{ borderWidth: Math.max(1, size * 0.04) }", "{ borderWidth: w * 2 }", '{ borderStyle: "dashed" }', 'const dash = "dashed"; const s = { borderStyle: dash };']);
});

const radii = new Set(tokens.radii.map(r => Math.round(r * 2) / 2));
function scanRadii(list) {
  const bad = [];
  for (const { rel, text } of list.filter(f => /^(app|components)\//.test(f.rel))) {
    for (const m of text.matchAll(/\{[^{}]*\}/g)) {
      for (const r of m[0].matchAll(/border(?:Top|Bottom)?(?:Left|Right)?Radius:\s*(\d+(?:\.\d+)?)\b/g)) {
        const value = Number(r[1]);
        if (radii.has(value)) continue;
        // A circle or pill: half its own width or height.
        const box = ["width", "height", "minHeight"].map(key => m[0].match(new RegExp(`\\b${key}:\\s*(\\d+(?:\\.\\d+)?)\\b`))?.[1]).filter(Boolean).map(Number);
        if (box.some(side => Math.abs(side / 2 - value) < 0.01)) continue;
        bad.push(`${rel}:${lineOf(text, m.index)} ${r[0]}`);
      }
    }
  }
  return bad;
}
test("corner radii are .fig radii or a circle", () => {
  assert.deepEqual(scanRadii(files), [], "radii outside the .fig (use fig.component radii)");
  mustCatch(scanRadii, ["{ borderRadius: 7 }", "{ width: 30, height: 30, borderRadius: 9 }", "{ borderTopLeftRadius: 5 }"]);
});

// Copy may use → for "becomes"; everything else in these ranges is an icon stand-in:
// ‹›«», arrows, technical symbols, geometric shapes, dingbats, and pictographic emoji.
const glyph = /[×•‹›«»←↑↓-⇿⌀-⏿■-◿☀-➿⬀-⯿]|(?![©®™])\p{Extended_Pictographic}/u;
const iconLibrary = /from\s+["'](@expo\/vector-icons|react-native-vector-icons|@react-native-vector-icons|lucide-react(-native)?|react-native-heroicons|phosphor-react-native|react-icons|@tabler\/icons[\w-]*|react-native-feather|@fortawesome\/[\w-]+)[^"']*["']|require\(["'](@expo\/vector-icons|react-native-vector-icons)/;
function scanIcons(list) {
  const bad = [];
  for (const { rel, text } of list.filter(f => /^(app|components)\//.test(f.rel) && !DOCUMENT_INK[f.rel]?.includes("✓"))) {
    const lib = text.match(iconLibrary); if (lib) bad.push(`${rel}: imports ${lib[1] ?? lib[3]}`);
    // "+ 서류 추가": a typed plus standing in for the rotated close icon.
    for (const m of text.matchAll(/["'`>]([^"'`<>\n]*)["'`<]/g)) if (glyph.test(m[1]) || /^\s*\+\s+\p{Script=Hangul}/u.test(m[1])) bad.push(`${rel}:${lineOf(text, m.index)} ${m[1].trim().slice(0, 40)}`);
  }
  return bad;
}
test("icons are Figma icons: no icon library, no glyph or emoji standing in for one", () => {
  assert.deepEqual(scanIcons(files), [], "use FigmaIcon (components/FigmaUI.tsx), LinkLabel or CheckMark");
  mustCatch(scanIcons, ['import { Ionicons } from "@expo/vector-icons";', 'import Icon from "react-native-vector-icons/Feather";', 'import { Plus } from "lucide-react-native";', "<Text>›</Text>", "<Text>✓ 완료</Text>", '"정렬 ▾"', "<Text>더 보기 »</Text>", "<Text>⚙</Text>", "<Text>🎉</Text>", "<Text>↗</Text>", "<Text>+ 서류 추가</Text>", '"+ 직접 추가"', "<Text>×</Text>", "<Text style={s.dot}>•</Text>", '"• 목적: 보관"']);
});

// Size, weight and line height of a style block, read in order (later entries win), with theme spreads
// and tokens (T./fig.text./typography.*) written out. null where a value cannot be known statically.
const themeText = fig.text;
function tokenValue(expr, key) {
  const m = expr.match(/^(?:T|fig\.text|text)\.(\w+)\.(fontSize|fontWeight|lineHeight)$/); if (m) return themeText[m[1]]?.[m[2]] ?? null;
  const t = expr.match(/^typography\.(fontSize|fontWeight)(?:\.(\w+)|\[["'](\w+)["']\])$/); if (t) return themeTypography[t[1]][t[2] ?? t[3]] ?? null;
  if (/^\d+(\.\d+)?$/.test(expr)) return Number(expr);
  const q = expr.match(/^["'](\d+)["']$/); if (q) return q[1];
  return key ? null : null;
}
function resolveStyle(block) {
  const out = { fontSize: undefined, fontWeight: undefined, lineHeight: undefined };
  for (const m of block.matchAll(/\.\.\.\s*([\w.]+)|\b(fontSize|fontWeight|lineHeight)\s*:\s*([^,}\n]+)/g)) {
    if (m[1]) {
      const spread = m[1].match(/^(?:T|fig\.text|text)\.(\w+)$/) ? themeText[m[1].split(".").pop()] : m[1].match(/^typography\.styles\.(\w+)$/) ? themeTypography.styles[m[1].split(".").pop()] : undefined;
      if (spread) for (const key of Object.keys(out)) { if (spread[key] !== undefined) out[key] = spread[key]; }
      else for (const key of Object.keys(out)) out[key] = null; // an unknown spread may set anything
    } else out[m[2]] = tokenValue(m[3].trim(), m[2]);
  }
  return out;
}
const comboOk = ({ fontSize, fontWeight, lineHeight }) => fontSize == null || lineHeight == null || fontWeight === null || styles.has(`${fontSize}/${fontWeight ?? "400"}/${lineHeight}`);

// Styles composed in one style={[a, b]} array: a size from one block and a weight from another (R1).
function scanComposed(list) {
  const bad = [];
  for (const { rel, text } of list.filter(f => /^(app|components)\//.test(f.rel))) {
    const blocks = new Map();
    for (const sheet of text.matchAll(/const (\w+) = StyleSheet\.create\(\{/g)) {
      let i = sheet.index + sheet[0].length, depth = 1; const start = i;
      for (; i < text.length && depth; i++) { if (text[i] === "{") depth++; else if (text[i] === "}") depth--; }
      for (const entry of text.slice(start, i - 1).matchAll(/(\w+)\s*:\s*\{([^{}]*)\}/g)) blocks.set(`${sheet[1]}.${entry[1]}`, entry[2]);
    }
    for (const m of text.matchAll(/style=\{\[([^\]]*)\]\}/g)) {
      const parts = [...m[1].matchAll(/\b(\w+\.\w+)\b(?!\s*\()/g)].map(x => blocks.get(x[1])).filter(Boolean);
      if (parts.length < 2) continue;
      const merged = resolveStyle(parts.join(",\n"));
      if (merged.fontSize != null && merged.lineHeight != null && merged.fontWeight !== null && !comboOk(merged)) bad.push(`${rel}:${lineOf(text, m.index)} ${merged.fontSize}/${merged.fontWeight ?? "400"}/${merged.lineHeight} composed`);
    }
  }
  return bad;
}
test("text styles composed from several blocks are still .fig text styles", () => {
  assert.deepEqual(scanComposed(files), [], "a composed style outside the .fig");
  mustCatch(scanComposed, [
    'const styles = StyleSheet.create({ label: { fontSize: 13, lineHeight: 20 }, selected: { fontWeight: "700" } });\n<Text style={[styles.label, on && styles.selected]} />',
    'const s = StyleSheet.create({ a: { ...T.body13 }, b: { fontWeight: 700 } });\n<Text style={[s.a, s.b]} />',
  ]);
});

// Values written through a variable: a size, line height or radius must be a number, a theme token
// (or a local alias of one), or for a radius half of a size (a circle). Props passed through are checked at the call.
const TOKEN_ROOTS = /^(fig|T|K|C|typography|borderRadius|spacing|layout|theme|text)\b/;
const PASSTHROUGH = { "components/Skeleton.tsx": ["radius = borderRadius.sm", "radius"], "components/Tappable.tsx": ["radius"] };
function scanVariables(list) {
  const bad = [];
  for (const { rel, text } of list.filter(f => /^(app|components)\//.test(f.rel) && !DOCUMENT_GEOMETRY.has(f.rel) && f.rel !== "components/formFill/PageView.tsx")) {
    const aliases = new Set([...text.matchAll(/const (\w+) = (?:fig|K|C|T|typography)\b[\w.]*;/g)].map(m => m[1]));
    for (const m of text.matchAll(/\b(fontSize|lineHeight|border(?:Top|Bottom)?(?:Left|Right)?Radius)\s*:\s*/g)) {
      const value = expressionAt(text, m.index + m[0].length);
      if (/^\d+(\.\d+)?$/.test(value) || /^["'`]/.test(value)) continue;
      const root = value.match(/^(\w+)/)?.[1];
      if (TOKEN_ROOTS.test(value) || (root && aliases.has(root) && /^\w+(\.\w+)+$/.test(value))) continue;
      if (/Radius/.test(m[1]) && /^[\w.]+\s*\/\s*2$/.test(value)) continue;
      if (PASSTHROUGH[rel]?.includes(value)) continue;
      bad.push(`${rel}:${lineOf(text, m.index)} ${m[1]}: ${value.slice(0, 40)}`);
    }
    for (const m of text.matchAll(/\b(?:borderRadius|radius)=\{(\d+(?:\.\d+)?)\}/g)) {
      // A circle: half of the same element's width or height.
      const element = text.slice(text.lastIndexOf("<", m.index), text.indexOf(">", m.index));
      const sides = [...element.matchAll(/\b(?:width|height)=\{(\d+(?:\.\d+)?)\}/g)].map(x => Number(x[1]));
      if (!radii.has(Number(m[1])) && !sides.some(side => side / 2 === Number(m[1]))) bad.push(`${rel}:${lineOf(text, m.index)} radius prop ${m[1]}`);
    }
  }
  return bad;
}
test("sizes, line heights and radii set through variables are still .fig values", () => {
  assert.deepEqual(scanVariables(files), [], "a variable outside the .fig tokens");
  mustCatch(scanVariables, ["{ fontSize: size }", "{ lineHeight: font * 1.2 }", "{ borderRadius: r }", "{ fontSize: Math.min(12, h) }", "<SkeletonBlock borderRadius={7} />", "<SkeletonBlock width={30} height={30} borderRadius={9} />"]);
});

test("theme colours are .fig colours and named variables keep their Figma values", () => {
  for (const [name, value] of Object.entries(fig.color)) assert.ok(palette.has(value), `fig.color.${name} ${value}`);
  const v = tokens.variables;
  const named = { primary: "Colors/Primeary color", primary2: "Colors/Primeary color 2", secondary: "Colors/Secondary color", secondary4: "Colors/Secondary 4%", secondary8: "Colors/Secondary 8%", secondary32: "Colors/Secondary 32%", black: "Colors/Black", navy: "Colors/Color/Navy", blue: "Colors/Color/Blue", skyblue: "Colors/Color/Skyblue", orange: "Colors/Color/Orange", red: "Colors/Color/Red", deepGray: "Colors/DeepGray", gray66: "Colors/Gray/Gray66", gray99: "Colors/Gray/Gray99", grayC1: "Colors/Gray/GrayC1", grayC8: "Colors/Gray/GrayC8", grayE1: "Colors/Gray/GrayE1", grayE8: "Colors/Gray/GrayE8", grayF1: "Colors/Gray/GrayF1", grayF7: "Colors/Gray/GrayF7", white: "Colors/White", input2: "Colors/Input filed  2%", input8: "Colors/Input filed  8%", inputError: "Colors/Input filed -Error" };
  for (const [key, variable] of Object.entries(named)) assert.equal(fig.color[key], v[variable], `fig.color.${key} = ${variable}`);
  const frame = { title: c.cardTitle.color, heading: c.homeTitle.color, white92: c.headerLogo.fill, white96: c.bottomBar.fill, white98: c.tabBar.fill, filterLine: c.filterBar.stroke, unread: c.alertRowUnread.fill, disabled: c.buttonDisabled.fill, loginBackground: c.loginBackground.fill, kakao: c.loginKakao.fill, footnote: c.alertFootnote.color, tileScholarship: c.deadlineTile.fill, whiteLine4: c.loginGuestArrow.fill, whiteLine16: c.loginGuestArrow.stroke };
  for (const [key, value] of Object.entries(frame)) assert.equal(fig.color[key], value, `fig.color.${key}`);
});

const style = node => ({ fontSize: node.fontSize, fontWeight: String(node.fontWeight), lineHeight: node.lineHeight });
test("theme text styles equal the frames they name", () => {
  const pairs = { label9: c.inputLabel, caption10: c.heroNote, badge10: c.deadlineBadgeText, body11: c.tabLabelActive, badge11: c.typeBadgeText, meta12: c.alertTime, body12: c.cardMeta, medium12: c.listToggleText, semibold12: c.statusLabel, bold12: c.listCount, body13: c.cardSummary, medium13: c.loginCaption, semibold13: c.tabSelectedText, bold14: c.buttonPrimaryText, body16: c.sheetOptionText, medium16: c.sheetOptionSelectedText, bold16: c.cardTitle, bold18: c.sectionTitle, bold20: c.stepTitle, bold24: c.heroAmount };
  for (const [key, node] of Object.entries(pairs)) assert.deepEqual(fig.text[key], style(node), `fig.text.${key} = ${node.id}`);
  for (const [key, value] of Object.entries(fig.text)) assert.ok(tokens.text_styles.includes(`${value.fontSize}/${value.fontWeight}/${value.lineHeight}`), `fig.text.${key} is a .fig text style`);
});

test("shared component tokens equal their Figma nodes", () => {
  const k = fig.component;
  assert.equal(k.header.height, c.headerLogo.height); assert.equal(k.header.paddingHorizontal, c.headerLogo.padding[1]); assert.equal(k.header.backgroundColor, c.headerLogo.fill);
  assert.equal(k.headerLine.logo, c.headerLogo.stroke); assert.equal(k.headerLine.back, c.headerBack.stroke);
  assert.deepEqual(c.headerLogo.strokeSides, { top: 0, right: 0, bottom: 1, left: 0 });
  assert.equal(k.tabBar.height, c.tabBar.height); assert.equal(k.tabBar.paddingVertical, c.tabBar.padding[0]); assert.equal(k.tabBar.backgroundColor, c.tabBar.fill); assert.equal(k.tabBar.borderTopColor, c.tabBar.stroke);
  assert.equal(k.tabBar.radius, c.tabBar.radius[0]); assert.equal(k.tabBar.item, c.tabItem.height); assert.equal(k.tabBar.iconBox, c.tabIconBox.width); assert.equal(k.tabBar.inactiveOpacity, c.tabItemInactive.opacity);
  assert.equal(k.tabs.height, c.tabsRow.height); assert.equal(k.tabs.paddingHorizontal, c.tabSelected.padding[1]); assert.equal(k.tabs.paddingVertical, c.tabSelected.padding[0]); assert.equal(k.tabs.underline, c.tabSelected.strokeSides.bottom); assert.equal(k.tabs.lineColor, c.tabsRow.stroke);
  assert.equal(k.card.radius, c.card.radius); assert.equal(k.card.radiusHome, c.cardHome.radius); assert.equal(k.card.padding, c.card.padding[0]); assert.equal(k.card.paddingBottom, c.card.padding[2]); assert.equal(k.card.gap, c.card.gap); assert.equal(k.card.borderColor, c.card.stroke);
  assert.equal(k.cardIcon.size, c.cardIcon.width); assert.equal(k.cardIcon.radius, c.cardIcon.radius);
  assert.equal(k.badge.radius, c.typeBadge.radius); assert.equal(k.badge.paddingHorizontal, c.typeBadge.padding[1]); assert.equal(k.badge.paddingVertical, c.typeBadge.padding[0]);
  assert.equal(k.filterPill.height, c.filterPill.height); assert.equal(k.filterPill.radius, c.filterPill.radius); assert.equal(k.filterPill.paddingHorizontal, c.filterPill.padding[1]);
  assert.equal(k.input.height, c.input.height); assert.equal(k.input.radius, c.input.radius); assert.equal(k.input.paddingHorizontal, c.input.padding[1]); assert.equal(k.input.paddingVertical, c.input.padding[0]);
  assert.equal(k.infoBox.radius, c.infoBox.radius); assert.equal(k.infoBox.paddingLeft, c.infoBox.padding[3]); assert.equal(k.infoBox.paddingRight, c.infoBox.padding[1]);
  assert.equal(k.button.height, c.buttonPrimary.height); assert.equal(k.button.radius, c.buttonPrimary.radius); assert.equal(k.button.paddingHorizontal, c.buttonPrimary.padding[1]);
  assert.equal(k.bottomBar.height, c.bottomBar.height); assert.equal(k.bottomBar.radius, c.bottomBar.radius[0]);
  assert.equal(k.sheet.radius, c.sheet.radius[0]); assert.equal(k.sheet.handleWidth, c.sheetHandle.width); assert.equal(k.sheet.handleHeight, c.sheetHandle.height);
  assert.equal(k.banner.radius, c.banner.radius); assert.equal(k.banner.padding, c.banner.padding[0]); assert.equal(k.hero.radius, c.hero.radius);
  assert.equal(k.tile.width, c.deadlineTile.width); assert.equal(k.tile.height, c.deadlineTile.height); assert.equal(k.tile.radius, c.deadlineTile.radius);
  assert.equal(k.choice.radius, c.choiceSelected.radius); assert.equal(k.choice.art, c.choiceArt.width);
  assert.equal(k.alertRow.avatar, c.alertAvatar.width);
  const cssShadow = s => `${s.x}px ${s.y}px ${s.blur}px ${s.spread}px ${s.color}`;
  assert.equal(fig.shadow.card, cssShadow(c.card.shadow)); assert.equal(fig.shadow.bar, cssShadow(c.bottomBar.shadow)); assert.equal(fig.shadow.badge, cssShadow(c.bannerBadge.shadow));
});

// Captures 2026-11-04 #04/#05: the detail top bar kept Figma's White 92% without its background
// blur, so 자격 조건 수정 and 0/5 준비 read through the back arrow and the title once scrolled.
test("a header laid over scrolling content is opaque", () => {
  const bg = fig.component.floatingHeader.backgroundColor;
  assert.ok(palette.has(bg), `${bg} is a .fig colour`);
  assert.match(bg, /^#[0-9A-F]{6}(FF)?$/i, "no alpha: the app draws no background blur");
  const detail = fs.readFileSync(path.join(root, "app/programs/[id].tsx"), "utf8");
  const solid = detail.match(/topBarSolid:\s*\{([^}]*)\}/);
  assert.ok(solid, "the detail top bar has a scrolled style");
  assert.match(solid[1], /backgroundColor:\s*K\.floatingHeader\.backgroundColor\b/, "the detail top bar over the sheet uses fig.component.floatingHeader");
});
