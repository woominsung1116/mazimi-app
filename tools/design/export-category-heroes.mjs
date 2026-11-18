// Detail-hero copies of the category 3D images: 640px is 180pt at @3x (programs/[id].tsx draws the hero at 180).
// node tools/design/export-category-heroes.mjs <dir with the 1024px category PNGs>
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import sharp from "sharp";

const SIZE = 640;
const source = process.argv[2];
if (!source) { console.error("usage: export-category-heroes.mjs <png-1024 dir>"); process.exit(1); }
const target = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../../apps/mobile/assets/categories/hero");
for (const name of fs.readdirSync(source).filter(n => n.endsWith(".png"))) {
  await sharp(path.join(source, name)).resize(SIZE, SIZE, { kernel: "lanczos3" }).png({ compressionLevel: 9, adaptiveFiltering: true }).toFile(path.join(target, name));
  console.log(name);
}
