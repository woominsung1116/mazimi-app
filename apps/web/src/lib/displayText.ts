// Client copy of the server's display normalizer (crates/core/src/display_text.rs,
// v1 feedback F4.5/F5.6). It runs only when the server sent no summary_display /
// summary_lines (older servers), so a summary never shows "○ …○ …" or a bullet
// next to the app's own bullet. It adds no words and drops no content: it only
// removes leading list symbols and enumerators and collapses whitespace.
// No imports, so the node test harnesses can load it as-is. Kept byte-identical
// at apps/mobile/lib/displayText.ts and apps/web/src/lib/displayText.ts; both
// clients' checks assert the copies match.

export type DisplayText = { display: string | null; lines: string[] };

/** Symbols removed at the start of an item. */
const LEADING = new Set([
  "○", "●", "◦", "•", "❍", "◯", "◎", "▪", "■", "□", "◆", "◇", "◈", "▣", "▶", "►", "▷", "▸", "▹",
  "‣", "⁃", "❖", "-", "–", "—", "·", "∙", "※", "*", "★", "☆", "✓", "✔", "➢", "→", "⇒", "ㅇ",
]);
/** Symbols that start a new item even in the middle of a line. Word-joining marks (·, -, ‧) are not among them. */
const INLINE = new Set(["○", "●", "◦", "❍", "◯", "▪", "■", "□", "◆", "◇", "◈", "▣", "▶", "►", "▷", "▸", "▹", "❖", "※"]);
/** Hangul syllables used as enumerators (가., 나)). */
const HANGUL_ENUM = new Set(["가", "나", "다", "라", "마", "바", "사", "아", "자", "차", "카", "타", "파", "하"]);

const isCircledNumber = (c: string | undefined) => !!c && c >= "①" && c <= "⑳";
const isHangulSyllable = (c: string | undefined) => !!c && c >= "가" && c <= "힣";
const isDigit = (c: string | undefined) => !!c && c >= "0" && c <= "9";
const isSpace = (c: string | undefined) => !!c && /\s/.test(c);

/** Characters of a leading enumerator (1. , 1) , (1) , 가. , 가) ) including the space after it. A date ("26. 1. 1.") is not one. */
function enumeratorLen(chars: string[]): number | null {
  let digits = 0;
  while (isDigit(chars[digits])) digits++;
  if (digits >= 1 && digits <= 2 && (chars[digits] === "." || chars[digits] === ")") && isSpace(chars[digits + 1])) {
    const rest = chars.slice(digits + 1).join("").trimStart();
    let next = 0;
    while (isDigit(rest[next])) next++;
    const date = next > 0 && rest[next] === ".";
    return date ? null : digits + 2;
  }
  if (chars[0] === "(") {
    let inner = 0;
    while (isDigit(chars[1 + inner])) inner++;
    if (inner >= 1 && inner <= 2 && chars[1 + inner] === ")" && isSpace(chars[2 + inner])) return 3 + inner;
  }
  if (HANGUL_ENUM.has(chars[0]) && (chars[1] === "." || chars[1] === ")") && isSpace(chars[2])) return 3;
  return null;
}

/** One leading list symbol? "-" and "*" count only when no digit follows ("-10%" is a number). */
function hasLeadingSymbol(chars: string[]): boolean {
  if (!LEADING.has(chars[0])) return false;
  return !((chars[0] === "-" || chars[0] === "*" || chars[0] === "–") && isDigit(chars[1]));
}

function startsWithMarker(text: string): boolean {
  const chars = [...text.trimStart()];
  if (!chars.length) return false;
  return hasLeadingSymbol(chars) || isCircledNumber(chars[0]) || enumeratorLen(chars) !== null;
}

/** Remove every leading symbol and enumerator, then collapse whitespace. */
function clean(item: string): string {
  let chars = [...item.trimStart()];
  for (;;) {
    let cut: number | null = null;
    if (hasLeadingSymbol(chars) || isCircledNumber(chars[0])) cut = 1;
    else cut = enumeratorLen(chars);
    if (cut === null) break;
    chars = [...chars.slice(cut).join("").trimStart()];
  }
  return chars.join("").split(/\s+/).filter(Boolean).join(" ");
}

/** A line split at inline bullets: each piece keeps its bullet. "(※ 참고)" stays quoted. */
function pieces(line: string): string[] {
  const chars = [...line];
  const out: string[] = [];
  let start = 0;
  chars.forEach((c, i) => {
    const before = chars.slice(start, i).join("").trimEnd();
    if (i > start && (INLINE.has(c) || isCircledNumber(c)) && before.trim() !== "" && !/[([<]$/.test(before)) {
      out.push(chars.slice(start, i).join(""));
      start = i;
    }
  });
  out.push(chars.slice(start).join(""));
  return out;
}

/** Sentences of a single paragraph: after "." following a Hangul syllable, or "!" / "?", when whitespace follows. */
function sentences(text: string): string[] {
  const chars = [...text];
  const out: string[] = [];
  let current = "";
  chars.forEach((c, i) => {
    current += c;
    const ends = c === "." ? i > 0 && isHangulSyllable(chars[i - 1]) : c === "!" || c === "?";
    if (ends && isSpace(chars[i + 1])) { out.push(current); current = ""; }
  });
  out.push(current);
  return out.map(s => s.trim()).filter(Boolean);
}

/** Clean `text` for display. Same rules and results as the server's normalize(). */
export function normalizeDisplayText(text: string | null | undefined): DisplayText {
  if (text == null) return { display: null, lines: [] };
  const raw = text.replace(/\r\n?/g, "\n").split("\n").flatMap(pieces);
  const marked = raw.some(startsWithMarker);
  const items: string[] = [];
  for (const piece of raw) {
    if (!piece.trim()) continue;
    if (items.length && marked && !startsWithMarker(piece)) items[items.length - 1] += ` ${piece}`;
    else items.push(piece);
  }
  let lines = items.map(clean).filter(Boolean);
  if (!marked && lines.length === 1) lines = sentences(lines[0]);
  return { display: lines.length ? lines.join("\n") : null, lines };
}

/** Items of a cleaned text: the server's lines, else its display text split, else the raw text cleaned here. */
export function displayLines(serverLines: string[] | null | undefined, serverDisplay: string | null | undefined, raw: string | null | undefined): string[] {
  if (serverLines?.length) return serverLines;
  if (serverDisplay?.trim()) return serverDisplay.split("\n").map(line => line.trim()).filter(Boolean);
  return normalizeDisplayText(raw).lines;
}
