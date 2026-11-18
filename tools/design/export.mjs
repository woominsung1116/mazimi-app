// Reproduce the shipped brand assets from the supplied .fig, without redrawing them.
// npm ci --prefix tools/design && npm run export --prefix tools/design
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { createHash } from "node:crypto";
import sharp from "sharp";
import { iconSvg, loadFigma } from "./figma-source.mjs";
let rawFigma;
const raw = async () => (rawFigma ??= await loadFigma());

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const mobile = path.join(root, "apps/mobile");
const source = fs.readdirSync(mobile).find(name => name.endsWith(".fig"));
if (!source) throw new Error("The original Figma file is missing");
// Pinned refig 0.0.7 exposes its Kiwi reader in this bundled module.
const dist = path.join(path.dirname(fileURLToPath(import.meta.resolve("@grida/refig"))));
const bundle = fs.readdirSync(dist).find(name => /^lib-.*\.mjs$/.test(name));
const { u: io } = await import(path.join(dist, bundle));
const bytes = fs.readFileSync(path.join(mobile, source));
const parsed = io.kiwi.parseFile(bytes);
const nodes = new Map();
function index(node) { nodes.set(node.id, node); node.children?.forEach(index); }
parsed.pages.forEach(page => page.rootNodes.forEach(index));
const target = path.join(mobile, "assets/figma");
fs.mkdirSync(path.join(target, "vectors"), { recursive: true });
const hex = color => "#" + [color.r, color.g, color.b].map(v => Math.round(v * 255).toString(16).padStart(2, "0")).join("");

// These exported marks consist only of Figma fill paths. Preserve their original
// local transforms, winding rules and per-path opacity (including duotone icons).
function vector(node, rootNode = false, tint) {
  if (node.visible === false) return "";
  const t = node.relativeTransform ?? [[1, 0, 0], [0, 1, 0]];
  const matrix = rootNode ? "" : ` transform="matrix(${t[0][0]} ${t[1][0]} ${t[0][1]} ${t[1][1]} ${t[0][2]} ${t[1][2]})"`;
  let shapes = "";
  for (const fill of node.fills ?? []) {
    if (fill.visible === false || fill.type !== "SOLID") continue;
    for (const geometry of node.fillGeometry ?? []) {
      shapes += `<path d="${geometry.path}" fill="${tint ?? hex(fill.color)}" fill-opacity="${(fill.opacity ?? 1) * (fill.color.a ?? 1)}" fill-rule="${geometry.windingRule === "EVENODD" ? "evenodd" : "nonzero"}"/>`;
    }
  }
  return `<g${matrix} opacity="${node.opacity ?? 1}">${shapes}${(node.children ?? []).map(child => vector(child, false, tint)).join("")}</g>`;
}
const marks = {
  "logo": "34065:6395", "logo-white": "34002:3563",
  "icon-home": "34065:6368", "icon-search": "34065:6372",
  "icon-bookmark": "34065:6376", "icon-archive": "34065:6380",
  "icon-user": "34065:6384", "icon-bell": "34065:6405",
  "icon-arrow": "32026:3612", "icon-back": "34005:2865",
  "icon-check-circle": "32002:1010", "icon-minus-circle": "34065:6317",
  "icon-info": "34065:6181", "icon-close": "34059:3449",
  "icon-policy": "34065:6292", "icon-coin": "34065:6325",
  "icon-kakao": "32002:799",
  "icon-search-mono": "34060:4256", "icon-x-circle": "34002:3856",
  "icon-check": "34060:4233", "icon-link": "34060:4204", "icon-minus": "34065:11060",
  "icon-pin": "34065:11146", "icon-repeat": "34065:3676", "icon-document": "34068:6094",
  "icon-eye-off": "34002:3762", "icon-bookmark-mono": "34065:11461", "icon-setting": "34065:14380",
};
for (const [name, id] of Object.entries(marks)) {
  const node = nodes.get(id);
  if (!node) throw new Error(`Missing Figma node ${id}`);
  const { x: width, y: height } = node.size;
  let svg = `<svg xmlns="http://www.w3.org/2000/svg" width="${width}" height="${height}" viewBox="0 0 ${width} ${height}">${vector(node, true, name.startsWith("icon-") ? "#000000" : undefined)}</svg>`;
  // refig's tree drops the geometry of X_VECTOR icons; draw those from the symbol's raw vector network.
  if (!svg.includes("<path")) svg = iconSvg(await raw(), id);
  if (!svg.includes("<path")) throw new Error(`${name} (${id}) exports empty`);
  fs.writeFileSync(path.join(target, "vectors", `${name}.svg`), svg);
  await sharp(Buffer.from(svg), { density: 288 }).png().toFile(path.join(target, `${name}.png`));
}
if (process.argv.includes("--marks-only")) { console.log(`Exported ${Object.keys(marks).length} original marks.`); process.exit(0); }
const illustrations = {
  "student-enrolled": ["34003:2747", "ef7efe8ffcc34fc8d313b0b5c3f0f16ea213567a"],
  "student-leave": ["34003:2753", "1d74d83a9e7c4b8e5e837f5f68403527bf0c94ff"],
  "student-graduated": ["34003:2767", "ff355e1e752e442be524ba38e216b139362949a8"],
  "student-none": ["34003:2775", "447d1981997b9a627825e92db500270533deaffb"],
  "gift": ["34065:6184", "7689ec461874a9f54cfbd465eef84c144f0c57f6"],
  "scholarship": ["34065:6202", "ff355e1e752e442be524ba38e216b139362949a8"],
  "benefit": ["34065:6214", "7b354c34d76d19eeee31bc65821d79eeb94485d6"],
  // 서류 검색 empty state 34061:3121 draws this image in Luminosity blend: greyscale.
  "search-empty": ["34061:3121", "daa78a3847cc25f2bbb667bb9a45c541faa4c075", { greyscale: true }],
  // 회원가입을 환영합니다 (28023:1751) illustration.
  "welcome": ["28023:1829", "80d984af49a9461ca92af208952c5e582776b1ec"],
};
for (const [name, [, hash, options]] of Object.entries(illustrations)) {
  const data = parsed.zip_files[`images/${hash}`];
  if (!data) throw new Error(`Missing Figma image ${hash}`);
  const image = sharp(data).resize(496, 496, { fit: "inside", withoutEnlargement: true });
  await (options?.greyscale ? image.greyscale() : image).png().toFile(path.join(target, `${name}.png`));
}

const white = fs.readFileSync(path.join(target, "vectors/logo-white.svg"), "utf8");
// Android 12 applies an icon mask to native launch artwork. The full wordmark
// fits inside its safe circle; the React loading screen adds the tagline.
await sharp(Buffer.from(white), { density: 288 }).png().toFile(path.join(mobile, "assets/splash.png"));
await import("./export-app-icon.mjs");
const previous = JSON.parse(fs.readFileSync(path.join(target, "source.json"), "utf8"));
fs.writeFileSync(path.join(target, "source.json"), JSON.stringify({ ...previous, images: { ...previous.images, ...Object.fromEntries(Object.entries(illustrations).map(([name, [, hash]]) => [name, hash])) }, source_sha256: createHash("sha256").update(bytes).digest("hex"), vector_nodes: marks, illustration_nodes: illustrations, native_background: "#34BB84", login_background: "#30AC7A", export_command: "npm ci --prefix tools/design && npm run export --prefix tools/design" }, null, 2) + "\n");
console.log(`Exported ${Object.keys(marks).length} original marks, ${Object.keys(illustrations).length} illustrations and native launch assets.`);
