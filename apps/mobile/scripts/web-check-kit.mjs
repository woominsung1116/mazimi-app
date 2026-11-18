// Shared harness for the check-*-web.mjs browser checks (run by run-checks.cjs --web).
// Serves an Expo web export from disk and drives it with Playwright's Chromium.
//
// Readiness: the export is pre-rendered (app.json web.output "static"), so #root holds a
// splash before React hydrates and is not a ready signal. A check is ready when the
// document has loaded (gotoApp / reloadApp wait for the load event, so document.body
// exists) and the screen shows its fixture text (waitText). Nothing here sleeps.
import http from 'node:http';
import { readFile, stat } from 'node:fs/promises';
import { resolve, join, extname, sep } from 'node:path';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const { chromium } = require('../../../tools/design/node_modules/playwright');

const MIME = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.png': 'image/png', '.svg': 'image/svg+xml', '.ttf': 'font/ttf', '.woff2': 'font/woff2' };

/** Serves EXPORT_DIR on 127.0.0.1; a route without an extension gets its .html page. */
export async function serveExport(dir) {
  const root = resolve(dir);
  const server = http.createServer(async (req, res) => {
    try {
      let file = resolve(root, '.' + decodeURIComponent(new URL(req.url, 'http://localhost').pathname));
      if (file !== root && !file.startsWith(root + sep)) { res.writeHead(403).end(); return; }
      if (file === root) file = join(file, 'index.html');
      else if (!(await stat(file).catch(() => null))?.isFile()) file += '.html';
      res.setHeader('Content-Type', MIME[extname(file)] ?? 'application/octet-stream');
      res.end(await readFile(file));
    } catch { res.writeHead(404).end(); }
  });
  await new Promise(done => server.listen(0, '127.0.0.1', done));
  return { origin: `http://127.0.0.1:${server.address().port}`, close: () => new Promise(done => server.close(done)) };
}

/** Headless Chromium: CHROME_BIN when set, otherwise Playwright's own (CI installs it in tools/design). */
export function launchBrowser() {
  return chromium.launch({ headless: true, executablePath: process.env.CHROME_BIN || undefined });
}

export async function gotoApp(page, url) {
  await page.goto(url, { waitUntil: 'load' });
}

export async function reloadApp(page) {
  await page.reload({ waitUntil: 'load' });
}

/** Waits until the rendered text of the page contains `text`. */
export async function waitText(page, text, timeout = 15_000) {
  try {
    await page.waitForFunction(t => document.body.innerText.includes(t), text, { timeout });
  } catch (error) {
    const current = await page.evaluate(() => document.body?.innerText ?? '').catch(() => '');
    throw new Error(`Missing UI text: ${text}; current: ${current.slice(0, 500)}`, { cause: error });
  }
}

/** Clicks the element with `role` whose rendered text is exactly `text`. */
export async function clickText(page, text, role = 'button') {
  const exact = new RegExp(`^\\s*${text.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}\\s*$`);
  await page.locator(`[role="${role}"]`).filter({ hasText: exact }).first().click();
}

/** Clicks the element with `role` whose accessible label is exactly `label`. */
export async function clickLabel(page, label, role) {
  await page.locator(`[role="${role}"][aria-label=${JSON.stringify(label)}]`).click();
}
