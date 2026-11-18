// Derive apps/mobile/constants/figma-tokens.json from the .fig. `--check` fails when the
// committed file no longer matches the .fig, so the tokens cannot be edited by hand.
// npm run tokens --prefix tools/design  |  npm run check:tokens --prefix tools/design
import fs from "node:fs";
import path from "node:path";
import { flatten, gradientColors, fontWeight, hex, lineHeightPx, loadFigma, mobile, paintColor, summary } from "./figma-source.mjs";
import { COMPONENTS } from "./figma-components.mjs";

const figma = await loadFigma();
const page = [...figma.nodes.values()].find(node => node.type === "CANVAS" && node.name === "UI");

const palette = new Set(), sizes = new Set(), lineHeights = new Set(), weights = new Set(), styles = new Set(), radii = new Set();
const symbols = new Set();
// Every text the design shows, for the app's copy check (scripts/check-copy.cjs).
const texts = new Set();
const addText = characters => { const s = characters?.replace(/\s+/g, " ").trim(); if (s) texts.add(s); };
const withAlpha = (c, opacity) => {
  if (opacity >= 0.995) return c;
  const a = (c.length > 7 ? parseInt(c.slice(7, 9), 16) / 255 : 1) * opacity;
  return c.slice(0, 7) + Math.round(a * 255).toString(16).padStart(2, "0").toUpperCase();
};
// `opacity` is the product of the ancestors' layer opacity (a 60% tab item, the sheet overlay).
const addPaints = (paints, opacity) => {
  const colors = (paints ?? []).map(p => paintColor(figma, p)).filter(Boolean);
  colors.forEach(c => { palette.add(c); palette.add(withAlpha(c, opacity)); });
  if (colors.length > 1) palette.add(flatten(colors));
  for (const p of paints ?? []) gradientColors(figma, p).forEach(c => palette.add(c));
  return colors;
};
const visit = (node, opacity, fromSymbol) => {
  if (node.visible === false) return;
  const own = opacity * (node.opacity ?? 1);
  if (own <= 0) return;
  addPaints(node.fillPaints, own); addPaints(node.strokePaints, own);
  for (const effect of node.effects ?? []) if (effect.visible !== false && effect.color) palette.add(hex(effect.color));
  if (node.type === "TEXT") addText(node.textData?.characters);
  if (node.type === "TEXT" && !fromSymbol) {
    const w = fontWeight(node.fontName?.style), lh = lineHeightPx(node);
    sizes.add(node.fontSize); weights.add(w); if (lh) lineHeights.add(lh);
    styles.add(`${node.fontSize}/${w}/${lh}`);
    for (const o of node.textData?.styleOverrideTable ?? []) addPaints(o.fillPaints, own);
  }
  if (!fromSymbol) {
    if (node.rectangleCornerRadiiIndependent) for (const k of ["rectangleTopLeftCornerRadius", "rectangleTopRightCornerRadius", "rectangleBottomRightCornerRadius", "rectangleBottomLeftCornerRadius"]) radii.add(node[k] ?? 0);
    else if (node.cornerRadius) radii.add(node.cornerRadius);
  }
  if (node.type === "INSTANCE") {
    const symbol = node.symbolData?.symbolID;
    if (symbol) symbols.add(`${symbol.sessionID}:${symbol.localID}`);
    for (const o of node.symbolData?.symbolOverrides ?? []) { addPaints(o.fillPaints, own); addPaints(o.strokePaints, own); addText(o.textData?.characters); }
  }
  for (const child of figma.children.get(node.id) ?? []) visit(child, own, fromSymbol);
};
visit(page, 1, false);
// Icons and other instances draw their symbol's own paints unless an override replaces them.
for (const sid of symbols) { const symbol = figma.nodes.get(sid); if (symbol) visit(symbol, 1, true); }

const local = [...figma.variables.values()].filter(v => figma.nodes.get(v.set)?.name === "Color");
const sort = set => [...set].sort((a, b) => (typeof a === "number" ? a - b : a < b ? -1 : 1));
const components = {};
for (const [key, nodeId] of Object.entries(COMPONENTS)) {
  const node = figma.nodes.get(nodeId);
  if (!node) throw new Error(`Missing Figma node ${nodeId} (${key})`);
  components[key] = summary(figma, node);
}
const tokens = {
  source: figma.file,
  source_sha256: figma.sha256,
  generated_by: "tools/design/export-tokens.mjs",
  variables: Object.fromEntries(local.map(v => [v.name, paintColor(figma, { type: "SOLID", color: v.color, opacity: 1 })]).sort(([a], [b]) => (a < b ? -1 : 1))),
  palette: sort(palette),
  font_sizes: sort(sizes),
  font_weights: sort(weights),
  line_heights: sort(lineHeights),
  text_styles: sort(styles),
  radii: sort(radii),
  components,
  texts: sort(texts),
};
const target = path.join(mobile, "constants/figma-tokens.json");
const text = JSON.stringify(tokens, null, 2) + "\n";
if (process.argv.includes("--check")) {
  const current = fs.existsSync(target) ? fs.readFileSync(target, "utf8") : "";
  if (current !== text) { console.error("constants/figma-tokens.json does not match the .fig. Run: npm run tokens --prefix tools/design"); process.exit(1); }
  console.log(`figma-tokens.json matches ${figma.file} (${tokens.palette.length} colours, ${Object.keys(components).length} components).`);
} else {
  fs.writeFileSync(target, text);
  console.log(`Wrote ${path.relative(process.cwd(), target)}: ${tokens.palette.length} colours, ${tokens.text_styles.length} text styles, ${Object.keys(components).length} components.`);
}
