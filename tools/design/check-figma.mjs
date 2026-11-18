// Figma regression guard for the mobile web export (docs/design/figma-restore-20261026.md).
// (CI runs `export-tokens.mjs --check` first, so figma-tokens.json matches the .fig.)
// 1. The rendered app's shared components carry the Figma values (computed styles at 390px).
// 2. Screens at 390px, and side-by-side images with the Figma frame, go to
//    docs/design/validation/figma-restore/.
// Local server and fixed API fixtures only; no production request, no OAuth, no submission.
// node tools/design/check-figma.mjs [web-export-dir] [output-dir]
import { chromium } from "playwright";
import fs from "node:fs";
import path from "node:path";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import sharp from "sharp";
import { serve, context, settle, programs, MISSING_PROGRAM } from "./fixtures.mjs";
import { figFile } from "./figma-source.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(here, "../..");
const web = path.resolve(process.argv[2] ?? path.join(root, "releases/mobile-figma"));
const output = path.resolve(process.argv[3] ?? path.join(root, "docs/design/validation/figma-restore"));
fs.mkdirSync(output, { recursive: true });

const tokens = JSON.parse(fs.readFileSync(path.join(root, "apps/mobile/constants/figma-tokens.json"), "utf8"));
const c = tokens.components;
const rgb = hex => { const v = [1, 3, 5].map(i => parseInt(hex.slice(i, i + 2), 16)); const a = hex.length > 7 ? Math.round(parseInt(hex.slice(7, 9), 16) / 255 * 1000) / 1000 : 1; return a === 1 ? `rgb(${v.join(", ")})` : `rgba(${v.join(", ")}, ${a})`; };
const sameColor = (actual, hex, label) => {
  const nums = s => s.match(/[\d.]+/g).map(Number);
  const [a, b] = [nums(actual), nums(rgb(hex))];
  if (a.length === 3) a.push(1); if (b.length === 3) b.push(1);
  assert.ok(a.every((x, i) => Math.abs(x - b[i]) <= (i === 3 ? 0.01 : 1)), `${label}: ${actual} is not ${hex}`);
};

// Screens with a Figma frame: route, fixture mode, frame id.
const SCREENS = [
  ["home", "member", "/", "34065:6155", 1544],
  ["explore", "member", "/explore", "34065:2917", 1103],
  ["recommend", "member", "/recommendations", "34065:5283"],
  ["manage", "member", "/manage", "34059:2750", 1053],
  ["vault", "member", "/vault", "34059:3468"],
  ["profile", "member", "/profile", "32026:3562", 986],
  ["notifications", "member", "/notifications", "34065:14563"],
  ["applications", "member", "/applications", "34068:3699"],
  ["detail", "member", `/programs/${programs[0].id}`, "34065:10914", 1600],
  ["login", "guest", "/login", "32002:697"],
  ["consent", "fresh", "/consent", "32002:906"],
  ["onboarding-region", "member", "/onboarding/step1", "32002:1120"],
  ["onboarding-birth", "member", "/onboarding/step2", "34002:3810"],
  ["onboarding-school", "member", "/onboarding/school", "34003:2694"],
  ["onboarding-income", "member", "/onboarding/step3", "34005:2822"],
];
// Features without a frame, built from the Figma components.
const EXTRA = [["agent", "/agent"], ["assistant", "/assistant"], ["assistant-search", "/assistant-search"], ["benefit-assistant", `/benefit-assistant?programId=${programs[0].id}`], ["profile-edit", "/profile-edit"], ["help", "/help"], ["terms", "/terms"], ["account", "/account"]];

const server = await serve(web);
const browser = await chromium.launch({ headless: true, ...(process.env.MAZIMI_CHROMIUM ? { executablePath: process.env.MAZIMI_CHROMIUM } : {}) });
const errors = [], results = [];
async function open(mode, route, height = 844, { agent = true } = {}) {
  const ctx = await context(browser, { signedIn: mode === "member", consented: mode !== "fresh", width: 390, height, errors, agent });
  const page = await ctx.newPage(); page.setDefaultTimeout(15000);
  await page.goto(server.base + route); await settle(page); await page.waitForTimeout(600);
  return { ctx, page };
}
const style = (page, selector, props) => page.locator(selector).first().evaluate((el, props) => { const s = getComputedStyle(el); return Object.fromEntries(props.map(p => [p, s.getPropertyValue(p)])); }, props);

try {
  // Shared components on 탐색 (34065:2917).
  {
    const { ctx, page } = await open("member", "/explore");
    assert.equal(await page.getByText("앱에 문제가 발생했어요", { exact: true }).count(), 0);
    const card = await style(page, `[aria-label="${programs[0].title} 상세 보기"]`, ["border-top-left-radius", "border-top-width", "border-top-color", "background-color", "box-shadow"]);
    assert.equal(card["border-top-left-radius"], `${c.card.radius}px`, "card radius");
    assert.equal(card["border-top-width"], "1px", "card stroke");
    sameColor(card["border-top-color"], c.card.stroke, "card stroke colour");
    sameColor(card["background-color"], c.card.fill, "card fill");
    assert.match(card["box-shadow"], /0px 8px 12px 2px/, "card shadow 34065:2930");
    const tab = await style(page, '[role="tab"][aria-label="전체"]', ["border-bottom-width", "border-bottom-color", "height"]);
    assert.equal(tab["border-bottom-width"], `${c.tabSelected.strokeSides.bottom}px`, "selected tab underline");
    sameColor(tab["border-bottom-color"], c.tabSelected.stroke, "selected tab colour");
    assert.equal(tab.height, `${c.tabSelected.height}px`, "tab height");
    const label = await page.locator('[role="tab"][aria-label="전체"] div[dir="auto"]').first().evaluate(el => { const s = getComputedStyle(el); return { size: s.fontSize, height: s.lineHeight, color: s.color }; });
    assert.equal(label.size, `${c.tabSelectedText.fontSize}px`); assert.equal(label.height, `${c.tabSelectedText.lineHeight}px`); sameColor(label.color, c.tabSelectedText.color, "selected tab text");
    const pill = await style(page, '[aria-label="대상 필터"]', ["height", "border-top-left-radius", "border-top-color"]);
    assert.equal(pill.height, `${c.filterPill.height}px`); assert.equal(pill["border-top-left-radius"], `${c.filterPill.radius}px`); sameColor(pill["border-top-color"], c.filterPill.stroke, "filter pill stroke");
    const header = await page.getByText("탐색", { exact: true }).first().evaluate(el => { const s = getComputedStyle(el); let p = el.parentElement; while (p && getComputedStyle(p).height !== "56px") p = p.parentElement; return { size: s.fontSize, weightOk: true, bar: p ? getComputedStyle(p).backgroundColor : null }; });
    assert.equal(header.size, `${c.headerTitleText.fontSize}px`, "header title size"); assert.ok(header.bar, "a 56px header bar"); sameColor(header.bar, c.headerTitle.fill, "header fill");
    results.push("탐색: card 34065:2930, selected tab 34065:3116, filter pill 34065:3283 and header 34065:3106 render with their Figma values.");
    await ctx.close();
  }
  // Tab bar and home sections (34065:6155).
  {
    const { ctx, page } = await open("member", "/");
    const bar = await page.getByText("마이페이지", { exact: true }).last().evaluate(el => { const label = getComputedStyle(el); let p = el.parentElement; while (p && !/^5\d|6\dpx$/.test(getComputedStyle(p).height) ) p = p.parentElement; let q = el; while (q && getComputedStyle(q).borderTopWidth !== "1px") q = q.parentElement; return { size: label.fontSize, height: label.lineHeight, bg: q && getComputedStyle(q).backgroundColor, line: q && getComputedStyle(q).borderTopColor, radius: q && getComputedStyle(q).borderTopLeftRadius }; });
    assert.equal(bar.size, `${c.tabLabelInactive.fontSize}px`, "tab label size"); assert.equal(bar.height, `${c.tabLabelInactive.lineHeight}px`, "tab label line height");
    sameColor(bar.bg, c.tabBar.fill, "tab bar fill"); sameColor(bar.line, c.tabBar.stroke, "tab bar line"); assert.equal(bar.radius, `${c.tabBar.radius[0]}px`, "tab bar corners");
    const hero = await style(page, '[aria-label="맞춤 혜택 확인"]', ["border-top-left-radius", "background-color"]);
    assert.equal(hero["border-top-left-radius"], `${c.hero.radius}px`); sameColor(hero["background-color"], c.hero.fill, "hero fill 34065:6169");
    const radii = await page.locator(`[aria-label="${programs[0].title} 상세 보기"]`).evaluateAll(els => els.map(el => getComputedStyle(el).borderTopLeftRadius));
    assert.ok(radii.includes(`${c.deadlineTile.radius}px`), `마감 임박 tile 34065:6201 radius: ${radii}`);
    assert.ok(radii.includes(`${c.cardHome.radius}px`), `home list card 34065:6288 radius: ${radii}`);
    results.push("홈: tab bar 34065:6364 (11/16 labels, White 98%, GrayF1 line, 12px corners) and hero 34065:6169 match.");
    await ctx.close();
  }
  // Tab bar has the frame's five items; the home banner carousel has its page dots.
  {
    const { ctx, page } = await open("member", "/");
    const tabs = await page.getByRole("tab").or(page.getByRole("link")).filter({ hasText: /^(홈|탐색|비서|관심 혜택|서류 보관함|마이페이지)$/ }).allInnerTexts();
    assert.deepEqual([...new Set(tabs.map(t => t.trim()))], ["홈", "탐색", "관심 혜택", "서류 보관함", "마이페이지"], "tab bar 34065:6364");
    results.push("탭 바: 홈, 탐색, 관심 혜택, 서류 보관함, 마이페이지 only.");
    await ctx.close();
  }
  // 홈 banner carousel 34065:6235: always the frame's four slides (dots 34065:6259), for each 비서 state.
  {
    const SLIDES = { vault: "서류를 미리 준비하고 혜택을 놓치지 마세요", briefing: "저장한 혜택, 다음 할 일", agent: "마지미 비서", assistant: "복학·독립·취업 준비, 지금 필요한 혜택부터", search: "질문으로 혜택 찾기" };
    const expected = { "member, 비서 on": ["vault", "briefing", "agent", "assistant"], "member, 비서 off": ["vault", "briefing", "assistant", "search"], guest: ["vault", "agent", "assistant", "search"] };
    for (const [state, keys] of Object.entries(expected)) {
      const { ctx, page } = await open(state === "guest" ? "guest" : "member", "/", 844, { agent: state !== "member, 비서 off" });
      await page.waitForTimeout(400);
      const shown = []; for (const [key, label] of Object.entries(SLIDES)) if (await page.getByRole("button", { name: label, exact: true }).count()) shown.push(key);
      assert.deepEqual(shown, keys, `carousel slides, ${state}`);
      await ctx.close();
    }
    // 비서 off: /agent still gives the member their 다음 할 일.
    const { ctx, page } = await open("member", "/agent", 844, { agent: false });
    await page.getByText("AI 비서는 준비 중이에요", { exact: true }).waitFor(); await page.getByText("저장한 혜택, 다음 할 일", { exact: true }).waitFor();
    await ctx.close();
    results.push("홈 carousel: four slides for a member (비서 on or off) and a guest; with 비서 off, /agent still shows 다음 할 일.");
  }
  // 상세 stepper 34065:10997: a member's done step DeepGray, current Primary, the rest GrayE1 (the frame draws all three).
  {
    const { ctx, page } = await open("member", `/programs/${programs[0].id}`, 1600);
    await page.getByLabel("2단계 자격 확인, 현재 단계", { exact: true }).waitFor();
    const circles = await page.getByText(/^[1-4]$/).evaluateAll(els => els.map(el => getComputedStyle(el.parentElement).backgroundColor));
    assert.equal(circles.length, 4, "four stepper circles");
    sameColor(circles[0], "#8894A1", "done step"); sameColor(circles[1], c.deadlineBadge.fill, "current step"); for (const later of circles.slice(2, 4)) sameColor(later, "#E1E1E1", "future step");
    results.push("상세: the member's stepper draws done DeepGray, current Primary and later GrayE1, as 34065:10997.");
    await ctx.close();
  }
  // Onboarding bottom bar and button (34002:3810).
  {
    const { ctx, page } = await open("member", "/onboarding/step1");
    const next = await style(page, '[role="button"]:has-text("다음")', ["height", "border-top-left-radius", "background-color"]);
    assert.equal(next.height, `${c.buttonPrimary.height}px`); assert.equal(next["border-top-left-radius"], `${c.buttonPrimary.radius}px`); sameColor(next["background-color"], c.buttonDisabled.fill, "disabled 다음 34002:3961");
    const field = await style(page, '[aria-label="거주지역 선택"]', ["min-height", "border-top-left-radius", "background-color"]);
    assert.equal(field["min-height"], `${c.input.height}px`); assert.equal(field["border-top-left-radius"], `${c.input.radius}px`); sameColor(field["background-color"], c.input.fill, "input fill 34002:3818");
    results.push("온보딩: input 34002:3818 and disabled button 34002:3961 match.");
    await ctx.close();
  }

  // 상세: the back arrow must not be white on white, on a 404 and on the light category hero
  // (the room photo it was white for was replaced by the category tile, user decision 2026-10-31).
  for (const [label, route] of [["상세 404", `/programs/${MISSING_PROGRAM}`], ["상세 hero", `/programs/${programs[0].id}`]]) {
    const { ctx, page } = await open("member", route);
    const back = page.getByRole("button", { name: "뒤로 가기" }).first();
    await back.waitFor();
    const { data, info } = await sharp(await back.screenshot()).raw().toBuffer({ resolveWithObject: true });
    let dark = 0;
    for (let i = 0; i < data.length; i += info.channels) if (Math.min(data[i], data[i + 1], data[i + 2]) < 200) dark++;
    assert.ok(dark > 10, `${label}: the back arrow is invisible (${dark} non-white pixels)`);
    results.push(`${label}: the back arrow draws in #15D688, not white on a light background.`);
    await ctx.close();
  }

  // 마이페이지 rows keep a 44px target inside the Figma spacing (32026:3609).
  {
    const { ctx, page } = await open("member", "/profile");
    for (const name of ["자주 묻는 질문", "이용약관", "개인정보처리방침"]) {
      const box = await page.getByRole("button", { name, exact: true }).first().boundingBox();
      assert.ok(box && box.height >= 44, `마이페이지 ${name}: ${box?.height}px target`);
    }
    const toggle = await page.getByRole("switch", { name: "마감 임박 알림" }).first().boundingBox();
    assert.ok(toggle && Math.round(toggle.width) === 36 && Math.round(toggle.height) === 20, `Figma toggle 34065:4967 is 36x20: ${toggle?.width}x${toggle?.height}`);
    results.push("마이페이지: settings rows are 44px targets; alert toggles are the 36x20 Figma toggle.");
    await ctx.close();
  }

  // Screens and side-by-side comparisons.
  const refig = path.join(path.dirname(fileURLToPath(import.meta.resolve("@grida/refig"))), "cli.mjs");
  const fig = path.join(root, "apps/mobile", figFile());
  const fonts = fs.mkdtempSync(path.join(output, ".fonts-"));
  for (const f of fs.readdirSync(path.join(root, "apps/mobile/assets/fonts")).filter(n => n.startsWith("Pretendard"))) fs.copyFileSync(path.join(root, "apps/mobile/assets/fonts", f), path.join(fonts, f));
  try {
    for (const [name, mode, route, frame, height = 844] of SCREENS) {
      const { ctx, page } = await open(mode, route, height);
      assert.equal(await page.getByText("앱에 문제가 발생했어요", { exact: true }).count(), 0, `${name} crashed`);
      assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth + 1), false, `${name} overflows 390px`);
      const appShot = path.join(output, `${name}.png`);
      await page.screenshot({ path: appShot });
      await ctx.close();
      const figmaShot = path.join(fonts, `${name}-figma.png`);
      execFileSync(process.execPath, [refig, fig, "--node", frame, "--out", figmaShot, "--fonts", fonts, "--width", "375", "--height", "2400", "--skip-default-fonts"], { stdio: "ignore" });
      const [a, b] = await Promise.all([sharp(figmaShot).resize({ width: 390 }).toBuffer({ resolveWithObject: true }), sharp(appShot).toBuffer({ resolveWithObject: true })]);
      const h = Math.max(a.info.height, b.info.height);
      await sharp({ create: { width: 390 * 2 + 24, height: h, channels: 3, background: "#C9CCCE" } }).composite([{ input: a.data, left: 0, top: 0 }, { input: b.data, left: 414, top: 0 }]).png().toFile(path.join(output, `compare-${name}.png`));
    }
    for (const [name, route] of EXTRA) {
      const { ctx, page } = await open("member", route);
      assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth + 1), false, `${name} overflows 390px`);
      await page.screenshot({ path: path.join(output, `extra-${name}.png`) });
      await ctx.close();
    }
  } finally { fs.rmSync(fonts, { recursive: true, force: true }); }
  results.push(`${SCREENS.length} Figma screens (with compare-*.png next to the frame) and ${EXTRA.length} frameless screens render at 390px without overflow.`);
  assert.deepEqual(errors, [], "page errors");
  const report = { checked_at: new Date().toISOString(), source_sha256: tokens.source_sha256, environment: "Chromium 390px, local Expo web export, fixed API fixtures; no production requests", results, errors };
  fs.writeFileSync(path.join(output, "report.json"), JSON.stringify(report, null, 2) + "\n");
  console.log(JSON.stringify(report, null, 2));
} finally { await browser.close(); await server.close(); }
