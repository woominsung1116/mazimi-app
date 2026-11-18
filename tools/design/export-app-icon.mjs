// Builds the app icon (A3 in output/app-icon-options.html): the white 마지미
// wordmark on a brand-green gradient that darkens one step towards the bottom.
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { createHash } from "node:crypto";
import sharp from "sharp";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const assets = path.join(root, "apps/mobile/assets");
const logo = path.join(assets, "figma/vectors/logo-white.svg");
// theme.ts primary and onPrimaryFixedVariant
const top = "#34BB84";
const bottom = "#239969";
const background = top;

const gradient = (size) => Buffer.from(`<svg xmlns="http://www.w3.org/2000/svg" width="${size}" height="${size}">
  <defs><linearGradient id="g" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="${top}"/><stop offset="1" stop-color="${bottom}"/></linearGradient></defs>
  <rect width="${size}" height="${size}" fill="url(#g)"/></svg>`);

// Wordmark centred on the gradient, logo width as a share of the side
async function square(size, logoShare) {
  const mark = await sharp(logo, { density: 600 }).resize({ width: Math.round(size * logoShare) }).png().toBuffer();
  const { width, height } = await sharp(mark).metadata();
  return sharp(gradient(size))
    .composite([{ input: mark, left: Math.round((size - width) / 2), top: Math.round((size - height) / 2) }])
    .flatten({ background }).removeAlpha().png();
}

await (await square(1024, 0.78)).toFile(path.join(assets, "icon.png"));
await (await square(1024, 0.78)).toFile(path.join(assets, "brand/app-icon-original.png"));
// Android shows the middle 72 of 108 dp, so 52% of the layer reads as about 78% on the home screen
await (await square(1024, 0.52)).toFile(path.join(assets, "adaptive-icon.png"));
await (await square(64, 0.82)).toFile(path.join(assets, "favicon.png"));

const hashes = {};
for (const name of ["icon", "adaptive-icon", "favicon"]) {
  hashes[name] = createHash("sha256").update(fs.readFileSync(path.join(assets, `${name}.png`))).digest("hex");
}
fs.writeFileSync(path.join(assets, "brand/source.json"), JSON.stringify({
  source: "마지미 wordmark (assets/figma/vectors/logo-white.svg) on a #34BB84 to #239969 gradient, option A3, 2026-10-23",
  original_file: "app-icon-original.png",
  original_sha256: createHash("sha256").update(fs.readFileSync(path.join(assets, "brand/app-icon-original.png"))).digest("hex"),
  background, gradient: [top, bottom], logo_share: { icon: 0.78, adaptive: 0.52, favicon: 0.82 }, canvas_size: 1024,
  output_sha256: hashes,
  export_command: "node tools/design/export-app-icon.mjs",
  splash: "Separate original Figma wordmark; not replaced by the app icon"
}, null, 2) + "\n");
console.log("Exported the A3 wordmark app icon for iOS, Android and web.");
