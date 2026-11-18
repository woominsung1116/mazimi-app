// Category tile and accent colours, derived from each category's 3D image (user decision 2026-10-31).
//   sample = alpha-weighted mean RGB of the image's hued pixels: HSV saturation >= MIN_SATURATION,
//            so cream, white and grey highlights do not wash the hue out (fully transparent pixels weigh 0)
//   tile   = white mixed with the sample at TILE_MIX
//   accent = the sample's hue at saturation >= ACCENT_SATURATION, its HSL lightness lowered in 0.5% steps
//            until it reaches WCAG AA 4.5:1 on the tile (badge text is 11px, so the normal-text bar applies)
// node tools/design/category-colors.mjs           prints the table
// node tools/design/category-colors.mjs --check   fails when apps/mobile/constants/categoryStyle.ts differs (CI runs it)
// The entries and the image each samples are read from that table, so the two cannot list different codes.
import fs from "node:fs";
import path from "node:path";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import sharp from "sharp";

const here = path.dirname(fileURLToPath(import.meta.url));
const mobile = path.resolve(here, "../../apps/mobile");
export const TILE_MIX = 0.14;
export const MIN_SATURATION = 0.2;
export const ACCENT_SATURATION = 0.4;
export const AA = 4.5;

// The table is the source: every entry's code and the `image` it requires (the sampled file).
const TABLE = path.join(mobile, "constants/categoryStyle.ts");
const ENTRY = /\n  "?([a-z:]+)"?: \{ image: require\("\.\.\/([^"]+)"\)[^\n]*\}/g;
const entries = () => [...fs.readFileSync(TABLE, "utf8").matchAll(ENTRY)].map(m => ({ code: m[1], image: m[2], line: m[0] }));

const hex = rgb => "#" + rgb.map(v => Math.round(v).toString(16).padStart(2, "0")).join("").toUpperCase();
const lin = c => { c /= 255; return c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4; };
const luminance = ([r, g, b]) => 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b);
export const contrast = (a, b) => { const [x, y] = [luminance(a), luminance(b)].sort((p, q) => q - p); return (x + 0.05) / (y + 0.05); };
const parse = h => [1, 3, 5].map(i => parseInt(h.slice(i, i + 2), 16));

const toHsl = ([r, g, b]) => {
  [r, g, b] = [r / 255, g / 255, b / 255];
  const max = Math.max(r, g, b), min = Math.min(r, g, b), l = (max + min) / 2, d = max - min;
  if (!d) return [0, 0, l];
  const s = d / (1 - Math.abs(2 * l - 1));
  const h = max === r ? ((g - b) / d) % 6 : max === g ? (b - r) / d + 2 : (r - g) / d + 4;
  return [(h * 60 + 360) % 360, s, l];
};
const fromHsl = ([h, s, l]) => {
  const c = (1 - Math.abs(2 * l - 1)) * s, x = c * (1 - Math.abs((h / 60) % 2 - 1)), m = l - c / 2;
  const [r, g, b] = h < 60 ? [c, x, 0] : h < 120 ? [x, c, 0] : h < 180 ? [0, c, x] : h < 240 ? [0, x, c] : h < 300 ? [x, 0, c] : [c, 0, x];
  return [r + m, g + m, b + m].map(v => v * 255);
};

async function sample(file) {
  const { data, info } = await sharp(file).ensureAlpha().raw().toBuffer({ resolveWithObject: true });
  let r = 0, g = 0, b = 0, w = 0;
  for (let i = 0; i < data.length; i += info.channels) {
    const a = data[i + 3] / 255, max = Math.max(data[i], data[i + 1], data[i + 2]), min = Math.min(data[i], data[i + 1], data[i + 2]);
    if (!a || !max || (max - min) / max < MIN_SATURATION) continue;
    r += data[i] * a; g += data[i + 1] * a; b += data[i + 2] * a; w += a;
  }
  assert.ok(w > 0, `${file}: no hued pixels`);
  return [r / w, g / w, b / w];
}

export async function derive() {
  const out = {};
  for (const { code, image } of entries()) {
    const mean = await sample(path.join(mobile, image));
    const tileRgb = parse(hex(mean.map(c => 255 + (c - 255) * TILE_MIX)));
    const [h, sat, light] = toHsl(mean);
    let accent;
    for (let l = light; l >= 0; l -= 0.005) {
      accent = parse(hex(fromHsl([h, Math.max(sat, ACCENT_SATURATION), l])));
      if (contrast(accent, tileRgb) >= AA) break;
    }
    out[code] = { sample: hex(mean), tile: hex(tileRgb), accent: hex(accent), contrast: Math.floor(contrast(accent, tileRgb) * 100) / 100 };
  }
  return out;
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const table = await derive();
  if (process.argv.includes("--check")) {
    const lines = Object.fromEntries(entries().map(e => [e.code, e.line]));
    assert.ok(Object.keys(table).length >= 12, `parser lost table entries: ${Object.keys(table)}`);
    for (const [code, v] of Object.entries(table)) {
      assert.match(lines[code], new RegExp(`tile: "${v.tile}"`), `${code} tile should be ${v.tile}`);
      assert.match(lines[code], new RegExp(`accent: "${v.accent}"`), `${code} accent should be ${v.accent}`);
      assert.ok(v.contrast >= AA, `${code} accent on tile ${v.contrast}:1`);
    }
    console.log(`category colours match the images (${Object.keys(table).length} entries)`);
  } else {
    console.table(table);
  }
}
