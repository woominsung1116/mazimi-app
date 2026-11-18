import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { createRequire } from "node:module";
import assert from "node:assert/strict";
import sharp from "sharp";
import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const mobile = path.join(root, "apps/mobile");
const require = createRequire(path.join(mobile, "package.json"));
const config = JSON.parse(fs.readFileSync(path.join(mobile, "app.json"))).expo;
const plugin = config.plugins.find(p => Array.isArray(p) && p[0] === "expo-splash-screen")[1];
assert.equal(plugin.backgroundColor, "#34BB84");
assert.equal(config.splash.backgroundColor, plugin.backgroundColor);
const brand = JSON.parse(fs.readFileSync(path.join(mobile, "assets/brand/source.json")));
assert.equal(config.android.adaptiveIcon.backgroundColor, brand.background);
for (const [name, expected] of Object.entries(brand.output_sha256)) {
  assert.equal(createHash("sha256").update(fs.readFileSync(path.join(mobile, `assets/${name}.png`))).digest("hex"), expected, `Supplied app icon must be preserved: ${name}`);
}
assert.equal(plugin.dark.backgroundColor, plugin.backgroundColor);
assert.equal(plugin.imageWidth, 163);
const scratch = fs.mkdtempSync(path.join(os.tmpdir(), "mazimi-native-launch-"));
const expoConfig = path.dirname(require.resolve("@expo/prebuild-config/package.json"));
const { setIconAsync } = require(path.join(expoConfig, "build/plugins/icons/withAndroidIcons.js"));
await setIconAsync(scratch, {
  icon: path.join(mobile, config.android.adaptiveIcon.foregroundImage),
  backgroundColor: brand.background,
  backgroundImage: null,
  monochromeImage: null,
  isAdaptive: true,
});
const launcherHashes = new Map();
for (const density of ["mdpi", "hdpi", "xhdpi", "xxhdpi", "xxxhdpi"]) {
  for (const kind of ["ic_launcher", "ic_launcher_round", "ic_launcher_foreground"]) {
    const file = path.join(scratch, "android/app/src/main/res", `mipmap-${density}`, `${kind}.webp`);
    const { data, info } = await sharp(file).ensureAlpha().raw().toBuffer({ resolveWithObject: true });
    launcherHashes.set(createHash("sha256").update(data).digest("hex"), { kind, density, width: info.width, height: info.height });
  }
}
const { setSplashImageDrawablesForThemeAsync } = require(path.join(expoConfig, "build/plugins/unversioned/expo-splash-screen/withAndroidSplashImages.js"));
const densityKeys = ["mdpi", "hdpi", "xhdpi", "xxhdpi", "xxxhdpi"];
const imageConfig = { ...plugin, ...Object.fromEntries(densityKeys.map(key => [key, path.join(mobile, plugin.image)])) };
for (const mode of ["light", "dark"]) await setSplashImageDrawablesForThemeAsync(imageConfig, mode, scratch, plugin.imageWidth);
const images = [];
const pixelHashes = new Map();
for (const mode of ["light", "dark"]) for (const density of densityKeys) {
  const dir = `drawable-${mode === "dark" ? "night-" : ""}${density}`;
  const file = path.join(scratch, "android/app/src/main/res", dir, "splashscreen_logo.png");
  const { data, info } = await sharp(file).ensureAlpha().raw().toBuffer({ resolveWithObject: true });
  assert.deepEqual([...data.subarray(0, 4)], [52, 187, 132, 255]);
  let white = 0, oldBlue = 0;
  for (let offset = 0; offset < data.length; offset += 4) {
    const [r, g, b, a] = data.subarray(offset, offset + 4);
    if (r > 240 && g > 240 && b > 240 && a > 240) white++;
    if (b > r + 50 && b > g + 20 && a > 240) oldBlue++;
  }
  assert(white > 100, "Original white wordmark must be visible");
  assert.equal(oldBlue, 0, "Old blue launch artwork must not return");
  const hash = createHash("sha256").update(data).digest("hex");
  pixelHashes.set(hash, { density, width: info.width, height: info.height });
  images.push({ mode, density, width: info.width, height: info.height, oldBluePixels: oldBlue, whitePixels: white });
}
for (const name of ["icon", "adaptive-icon", "favicon", "splash"]) {
  const meta = await sharp(path.join(mobile, `assets/${name}.png`)).metadata();
  if (["icon", "adaptive-icon"].includes(name)) assert.equal(meta.width, 1024);
  assert.equal(meta.format, "png");
}
const report = { checked_at: new Date().toISOString(), method: "SDK 54 installed Expo native Android splash generator; raw pixel checks for all five density buckets in light and dark mode", splash: plugin, images, native_device_tested: false };
report.launcher = { original_sha256: brand.original_sha256, generatedResources: [...launcherHashes.values()] };
if (process.env.MAZIMI_APK) {
  const apk = path.resolve(process.env.MAZIMI_APK);
  const entries = execFileSync("unzip", ["-Z1", apk], { encoding: "utf8" }).trim().split("\n");
  const matched = new Map();
  const matchedLauncher = new Map();
  for (const entry of entries.filter(name => name.startsWith("res/") && /\.(png|webp)$/.test(name))) {
    const bytes = execFileSync("unzip", ["-p", apk, entry], { maxBuffer: 16 * 1024 * 1024 });
    const meta = await sharp(bytes).metadata();
    if (![...images, ...launcherHashes.values()].some(i => i.width === meta.width && i.height === meta.height)) continue;
    const pixels = await sharp(bytes).ensureAlpha().raw().toBuffer();
    const hash = createHash("sha256").update(pixels).digest("hex");
    const expected = pixelHashes.get(hash);
    if (expected) matched.set(expected.density, { ...expected, entry });
    const expectedLauncher = launcherHashes.get(hash);
    if (expectedLauncher) matchedLauncher.set(hash, { ...expectedLauncher, entry });
  }
  assert.equal(matched.size, densityKeys.length, "The built APK must contain all five original green/white splash pixel maps");
  assert.equal(matchedLauncher.size, launcherHashes.size, "The built APK must contain every generated house launcher icon");
  report.apk = { file: path.basename(apk), sha256: createHash("sha256").update(fs.readFileSync(apk)).digest("hex"), matchingDensityResources: [...matched.values()], matchingLauncherResources: [...matchedLauncher.values()] };
}
fs.writeFileSync(path.join(root, "docs/audits/mobile-native-assets-20261001.json"), JSON.stringify(report, null, 2) + "\n");
console.log(JSON.stringify({ ok: true, generated: images.length, background: plugin.backgroundColor, imageWidth: plugin.imageWidth, output: scratch }, null, 2));
