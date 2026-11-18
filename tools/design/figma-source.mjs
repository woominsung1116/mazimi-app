// Reads apps/mobile/마지미 (공유용).fig at the Kiwi level, the authority for design values.
// refig's REST-style conversion defaults a missing per-side stroke to the stroke weight
// (a 2px underline becomes a box) and drops auto-layout, so this reads the raw node
// changes it decodes instead. Variable-bound paints resolve to the variable's value,
// as Figma itself renders them.
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { createHash } from "node:crypto";
import { fileURLToPath, pathToFileURL } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
export const root = path.resolve(here, "../..");
export const mobile = path.join(root, "apps/mobile");
export const FIG_FILE = "마지미 (공유용).fig";

// Pinned refig 0.0.7 keeps readFigFile private inside its bundle; expose it on a temporary copy.
async function kiwi() {
  const dist = path.dirname(fileURLToPath(import.meta.resolve("@grida/refig")));
  const bundle = fs.readdirSync(dist).find(name => /^lib-.*\.mjs$/.test(name));
  const anchor = "_kiwi.parseFile = parseFile;";
  let source = fs.readFileSync(path.join(dist, bundle), "utf8");
  if (!source.includes(anchor) || !source.includes("function readFigFile(")) throw new Error("refig bundle changed: readFigFile is no longer where figma-source.mjs expects it");
  source = source.replace(anchor, `${anchor} _kiwi.readFigFile = readFigFile;`).replace(/from "(@grida\/canvas-wasm)"/, (_, name) => `from ${JSON.stringify(import.meta.resolve(name))}`);
  const copy = path.join(fs.mkdtempSync(path.join(os.tmpdir(), "mazimi-fig-")), "refig.mjs");
  fs.writeFileSync(copy, source);
  try { return (await import(pathToFileURL(copy).href)).u.kiwi; } finally { fs.rmSync(path.dirname(copy), { recursive: true, force: true }); }
}

const id = guid => `${guid.sessionID}:${guid.localID}`;

/** The one design source; a second .fig would make the tokens depend on directory order. */
export function figFile() {
  const figs = fs.readdirSync(mobile).filter(name => name.endsWith(".fig"));
  if (figs.length !== 1 || figs[0].normalize("NFC") !== FIG_FILE.normalize("NFC")) throw new Error(`apps/mobile must hold exactly one .fig, ${FIG_FILE} (found: ${figs.join(", ") || "none"})`);
  return figs[0];
}

export async function loadFigma() {
  const file = figFile();
  const bytes = fs.readFileSync(path.join(mobile, file));
  const fig = (await kiwi()).readFigFile(bytes);
  const nodes = new Map(), children = new Map();
  for (const change of fig.message.nodeChanges) {
    const node = { ...change, id: id(change.guid), parent: change.parentIndex ? id(change.parentIndex.guid) : null, position: change.parentIndex?.position ?? "" };
    nodes.set(node.id, node);
    if (!children.has(node.parent)) children.set(node.parent, []);
    children.get(node.parent).push(node);
  }
  for (const list of children.values()) list.sort((a, b) => (a.position < b.position ? -1 : a.position > b.position ? 1 : 0));
  const variables = new Map();
  for (const node of nodes.values()) {
    if (node.type !== "VARIABLE") continue;
    const value = node.variableDataValues?.entries?.[0]?.variableData?.value?.colorValue;
    if (value) variables.set(node.id, { name: node.name, set: node.variableSetID?.guid ? id(node.variableSetID.guid) : null, color: value });
  }
  return { file, sha256: createHash("sha256").update(bytes).digest("hex"), nodes, children, variables, blobs: fig.message.blobs ?? [] };
}

export function hex(color, opacity = 1) {
  const alpha = (color.a ?? 1) * opacity;
  const rgb = "#" + ["r", "g", "b"].map(k => Math.round(color[k] * 255).toString(16).padStart(2, "0")).join("").toUpperCase();
  return alpha >= 0.995 ? rgb : rgb + Math.round(alpha * 255).toString(16).padStart(2, "0").toUpperCase();
}

/** Resolved solid paint colour, or null for gradients, images and hidden paints. */
export function paintColor(figma, paint) {
  if (!paint || paint.visible === false || paint.type !== "SOLID") return null;
  const alias = paint.colorVar?.value?.alias?.guid;
  const bound = alias && figma.variables.get(id(alias));
  // Figma snapshots a bound variable's alpha into paint.opacity. An override can carry the
  // variable with opacity left at 1; then the variable's own alpha is the one painted.
  if (bound) { const opacity = paint.opacity ?? 1; return hex({ ...bound.color, a: 1 }, opacity < 1 ? opacity : bound.color.a ?? 1); }
  return hex(paint.color, paint.opacity ?? 1);
}

/** Colours of a gradient paint's stops (resolved like solid paints). */
export function gradientColors(figma, paint) {
  if (!paint || paint.visible === false || !String(paint.type).startsWith("GRADIENT")) return [];
  return (paint.stops ?? []).map(stop => {
    const alias = stop.colorVar?.value?.alias?.guid;
    const bound = alias && figma.variables.get(id(alias));
    return hex(bound ? { ...bound.color, a: stop.color.a ?? 1 } : stop.color, paint.opacity ?? 1);
  });
}

/** Solid fills stacked bottom-up and flattened over white, the colour Figma shows for a multi-fill node. */
export function flatten(colors) {
  let [r, g, b] = [255, 255, 255];
  for (const c of colors) {
    const v = [1, 3, 5].map(i => parseInt(c.slice(i, i + 2), 16));
    const a = c.length > 7 ? parseInt(c.slice(7, 9), 16) / 255 : 1;
    [r, g, b] = [r, g, b].map((x, i) => x * (1 - a) + v[i] * a);
  }
  return "#" + [r, g, b].map(x => Math.round(x).toString(16).padStart(2, "0")).join("").toUpperCase();
}

export function descendants(figma, start) {
  const out = [];
  const walk = node => { out.push(node); for (const child of figma.children.get(node.id) ?? []) walk(child); };
  walk(start);
  return out;
}

const WEIGHTS = { Thin: 100, ExtraLight: 200, Light: 300, Regular: 400, Medium: 500, SemiBold: 600, Bold: 700, ExtraBold: 800, Black: 900 };
export const fontWeight = style => WEIGHTS[String(style ?? "Regular").replace(/\s/g, "")] ?? 400;
export const lineHeightPx = node => node.lineHeight?.units === "PIXELS" ? node.lineHeight.value : node.lineHeight?.units === "PERCENT" ? Math.round(node.fontSize * node.lineHeight.value / 100) : null;

/** The visible values of one node, in the units React Native styles use. */
export function summary(figma, node) {
  const out = { id: node.id, name: node.name, type: node.type, width: round(node.size?.x), height: round(node.size?.y) };
  const fills = (node.fillPaints ?? []).map(p => paintColor(figma, p)).filter(Boolean);
  if (node.type === "TEXT") {
    Object.assign(out, { fontSize: node.fontSize, fontWeight: fontWeight(node.fontName?.style), lineHeight: lineHeightPx(node), color: fills[0] ?? null });
    return out;
  }
  if (fills.length) out.fill = fills.length === 1 ? fills[0] : flatten(fills);
  const strokes = (node.strokePaints ?? []).map(p => paintColor(figma, p)).filter(Boolean);
  if (strokes.length) {
    out.stroke = strokes[0];
    out.strokeSides = node.borderStrokeWeightsIndependent
      ? { top: node.borderTopWeight ?? 0, right: node.borderRightWeight ?? 0, bottom: node.borderBottomWeight ?? 0, left: node.borderLeftWeight ?? 0 }
      : { top: node.strokeWeight ?? 1, right: node.strokeWeight ?? 1, bottom: node.strokeWeight ?? 1, left: node.strokeWeight ?? 1 };
  }
  if (node.rectangleCornerRadiiIndependent) out.radius = [node.rectangleTopLeftCornerRadius ?? 0, node.rectangleTopRightCornerRadius ?? 0, node.rectangleBottomRightCornerRadius ?? 0, node.rectangleBottomLeftCornerRadius ?? 0];
  else if (node.cornerRadius) out.radius = node.cornerRadius;
  if (node.opacity != null && node.opacity < 1) out.opacity = round(node.opacity);
  if (node.stackMode && node.stackMode !== "NONE") {
    const v = node.stackVerticalPadding ?? 0, h = node.stackHorizontalPadding ?? 0;
    Object.assign(out, { direction: node.stackMode === "HORIZONTAL" ? "row" : "column", gap: round(node.stackSpacing ?? 0), padding: [v, node.stackPaddingRight ?? h, node.stackPaddingBottom ?? v, h].map(round) });
  }
  const shadows = (node.effects ?? []).filter(e => e.visible !== false && e.type === "DROP_SHADOW").map(e => ({ color: hex(e.color), x: round(e.offset?.x ?? 0), y: round(e.offset?.y ?? 0), blur: round(e.radius ?? 0), spread: round(e.spread ?? 0) }));
  if (shadows.length) out.shadow = shadows[0];
  return out;
}

const round = x => (x == null ? x : Math.round(x * 100) / 100);

// Kiwi path blobs: [cmd u8][f32 LE…]: 0 Z, 1 M x y, 2 L x y, 3 Q cx cy x y, 4 C c1 c2 x y.
function blobPath(figma, index) {
  const bytes = blobBytes(figma, index);
  if (!bytes) return "";
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const out = []; let o = 0;
  const f = () => { const v = view.getFloat32(o, true); o += 4; return Math.round(v * 1000) / 1000; };
  const arity = { 1: ["M", 2], 2: ["L", 2], 3: ["Q", 4], 4: ["C", 6] };
  while (o < bytes.length) {
    const cmd = bytes[o++];
    if (cmd === 0) { out.push("Z"); continue; }
    const [letter, n] = arity[cmd] ?? [];
    if (!letter || o + n * 4 > bytes.length) return "";
    out.push(letter + Array.from({ length: n }, f).join(" "));
  }
  return out.join(" ");
}

const blobBytes = (figma, index) => { const raw = figma.blobs[index]?.bytes; return raw ? (raw instanceof Uint8Array ? raw : new Uint8Array(Object.values(raw))) : null; };

/** A vector network blob's filled regions as SVG paths in the node's own size (refig's binary layout). */
function networkPaths(figma, node) {
  const bytes = blobBytes(figma, node.vectorData?.vectorNetworkBlob);
  if (!bytes) return [];
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const u32 = o => view.getUint32(o, true), f32 = o => view.getFloat32(o, true);
  const [vc, sc, rc] = [u32(0), u32(4), u32(8)];
  let o = 12;
  const v = []; for (let i = 0; i < vc; i++, o += 12) v.push([f32(o + 4), f32(o + 8)]);
  const seg = []; for (let i = 0; i < sc; i++, o += 28) seg.push({ a: u32(o + 4), ad: [f32(o + 8), f32(o + 12)], b: u32(o + 16), bd: [f32(o + 20), f32(o + 24)] });
  const norm = node.vectorData.normalizedSize ?? node.size;
  const k = [node.size.x / norm.x, node.size.y / norm.y];
  const pt = ([x, y]) => `${Math.round(x * k[0] * 1000) / 1000} ${Math.round(y * k[1] * 1000) / 1000}`;
  const out = [];
  for (let r = 0; r < rc; r++) {
    const rule = u32(o) & 1 ? "nonzero" : "evenodd"; const loops = u32(o + 4); o += 8;
    let d = "";
    for (let l = 0; l < loops; l++) {
      const n = u32(o); o += 4;
      let current = null;
      for (let i = 0; i < n; i++, o += 4) {
        let s = seg[u32(o)];
        if (current !== null && s.a !== current && s.b === current) s = { a: s.b, ad: s.bd, b: s.a, bd: s.ad };
        const A = v[s.a], B = v[s.b];
        if (current === null) d += `M${pt(A)} `;
        d += s.ad[0] || s.ad[1] || s.bd[0] || s.bd[1] ? `C${pt([A[0] + s.ad[0], A[1] + s.ad[1]])} ${pt([B[0] + s.bd[0], B[1] + s.bd[1]])} ${pt(B)} ` : `L${pt(B)} `;
        current = s.b;
      }
      d += "Z ";
    }
    out.push({ d: d.trim(), rule });
  }
  return out;
}

/**
 * An icon instance drawn from its symbol's own vector geometry, in black (the app tints it).
 * refig's converted tree drops the geometry of the X_VECTOR nodes some icons are built from.
 */
export function iconSvg(figma, instanceId) {
  const instance = figma.nodes.get(instanceId);
  const symbolId = instance?.symbolData?.symbolID;
  const symbol = symbolId && figma.nodes.get(id(symbolId));
  if (!symbol) throw new Error(`${instanceId} is not an icon instance`);
  const hidden = new Set((instance.symbolData.symbolOverrides ?? []).filter(o => o.visible === false).map(o => o.guidPath?.guids?.map(id).at(-1)));
  const draw = (node, isRoot) => {
    if (node.visible === false || hidden.has(node.id)) return "";
    const t = node.transform ?? { m00: 1, m01: 0, m02: 0, m10: 0, m11: 1, m12: 0 };
    const matrix = isRoot ? "" : ` transform="matrix(${t.m00} ${t.m10} ${t.m01} ${t.m11} ${t.m02} ${t.m12})"`;
    let shapes = "";
    const fills = (node.fillPaints ?? []).filter(p => p.visible !== false && p.type === "SOLID");
    for (const paint of fills) {
      const alias = paint.colorVar?.value?.alias?.guid; const bound = alias && figma.variables.get(id(alias));
      const alpha = (paint.opacity ?? 1) < 1 ? paint.opacity : bound ? bound.color.a ?? 1 : paint.color?.a ?? 1;
      const geometry = node.fillGeometry?.length ? node.fillGeometry.map(g => ({ d: blobPath(figma, g.commandsBlob), rule: g.windingRule === "ODD" || g.windingRule === "EVENODD" ? "evenodd" : "nonzero" })) : networkPaths(figma, node);
      for (const g of geometry) if (g.d) shapes += `<path d="${g.d}" fill="#000000" fill-opacity="${Math.round(alpha * 1000) / 1000}" fill-rule="${g.rule}"/>`;
    }
    return `<g${matrix} opacity="${isRoot ? 1 : node.opacity ?? 1}">${shapes}${(figma.children.get(node.id) ?? []).map(c => draw(c, false)).join("")}</g>`;
  };
  const body = draw(symbol, true);
  if (!body.includes("<path")) throw new Error(`${instanceId}: the icon has no drawable geometry`);
  const w = instance.size.x, h = instance.size.y;
  return `<svg xmlns="http://www.w3.org/2000/svg" width="${w}" height="${h}" viewBox="0 0 ${symbol.size.x} ${symbol.size.y}">${body}</svg>`;
}
