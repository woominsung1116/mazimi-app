// Mock-only browser replay of the detail page's 신청 진행 단계 and 신청 방법.
// node scripts/check-apply-steps-web.mjs EXPORT_DIR
// Fixtures: three production programmes (보조금24 기장군 청년 면접수당, 온통청년 부산 전세피해
// 주거안정지원금, 복지로 부산 난임시술비) with the server's own derive() output
// (crates/api services/application_procedure.rs). SHOTS_DIR saves screenshots;
// SHOTS_ONLY=1 only takes them (to capture an older build for comparison).
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { serveExport, launchBrowser } from './web-check-kit.mjs';

const here = path.dirname(fileURLToPath(import.meta.url));
const fixtures = JSON.parse(fs.readFileSync(path.join(here, 'apply-steps-fixture.json'), 'utf8'));
const GIJANG = '23509378-7c00-4f2b-bdc9-7e5717187be3', LEASE = '9d65f110-3ab4-47a4-9d25-fca4d2342adb', FERTILITY = '33670fe4-2b6f-4ac1-879f-7f90850a86ae';
const shots = process.env.SHOTS_DIR, only = process.env.SHOTS_ONLY === '1';
const stage = (states) => ['발견', '자격 확인', '서류 준비', '신청'].map((label, i) => ({ stage: ['discover', 'eligibility', 'documents', 'apply'][i], label, state: states[i] }));
const consent = { required: { termsAgreed: true, privacyAgreed: true, agreedAt: '2026-10-07', termsVersion: '2026-03-21', privacyVersion: '2026-09-26' }, healthInsurance: { agreed: false } };

const exportDir = path.resolve(process.argv[2]);
const { origin, close } = await serveExport(exportDir);
let browser;
const checks = [];
try {
  browser = await launchBrowser();
  /** One browser context: a guest, or a member whose progress endpoint answers `states`. */
  async function open(id, { member = false, states = null } = {}) {
    const context = await browser.newContext({ viewport: { width: 390, height: 2600 } });
    await context.addInitScript(([signedIn, consentValue]) => {
      localStorage.setItem('mazimi_consent_v1', JSON.stringify(consentValue));
      if (signedIn) sessionStorage.setItem('mazimi_auth_token', 'fixture-only-token');
    }, [member, consent]);
    const progressCalls = [];
    await context.route('**/*', async route => {
      const request = route.request(), url = new URL(request.url());
      if (url.pathname.startsWith('/api/v1/')) {
        let body = { items: [], total: 0 }, status = 200;
        const detail = url.pathname.match(/^\/api\/v1\/programs\/([0-9a-f-]{36})(\/progress)?$/);
        if (url.pathname.endsWith('/auth/me')) { if (member) body = { id: 'fixture-user', role: 'user' }; else { status = 401; body = { error: 'unauthorized' }; } }
        else if (url.pathname.endsWith('/profile')) body = { profile: { birth_year: 2001, region_code: 'busan' } };
        else if (detail?.[2]) { progressCalls.push(request.headers().authorization ?? ''); body = { stages: stage(states) }; }
        else if (detail) body = fixtures[detail[1]];
        await route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });
        return;
      }
      // The static export has one page for the dynamic route: programs/[id].html.
      if (url.origin === origin && /^\/programs\/[0-9a-f-]{36}$/.test(url.pathname)) { await route.fulfill({ contentType: 'text/html', body: fs.readFileSync(path.join(exportDir, 'programs', '[id].html')) }); return; }
      if (url.origin === origin) { await route.continue(); return; }
      await route.abort();
    });
    const page = await context.newPage();
    const errors = [];
    page.on('pageerror', e => errors.push(e.message));
    await page.goto(`${origin}/programs/${id}`);
    await page.getByText('신청 진행 단계', { exact: true }).waitFor();
    await page.getByText('신청 방법', { exact: true }).waitFor();
    await page.waitForTimeout(300);
    return { context, page, errors, progressCalls };
  }
  async function shoot(page, name) {
    if (!shots) return;
    fs.mkdirSync(shots, { recursive: true });
    await page.screenshot({ path: path.join(shots, `${name}.png`) });
  }
  const label = (page, text) => page.getByLabel(text, { exact: true });

  // 1. 기장군, guest: the four Figma stages, 발견 current; 신청 방법 lines; no bare "온라인 신청".
  {
    const { context, page, errors, progressCalls } = await open(GIJANG);
    await shoot(page, 'gijang-guest');
    if (!only) {
      assert.equal(await label(page, '1단계 발견, 현재 단계').count(), 1);
      assert.equal(await label(page, '4단계 신청, 예정').count(), 1);
      assert.equal(progressCalls.length, 0, 'a guest never asks for member progress');
      await page.getByText('전화 문의 : 기장군청 일자리경제과 051-709-4395', { exact: true }).waitFor();
      await page.getByText('신청 기간 : 2026.02.02 ~ 2026.12.10', { exact: true }).waitFor();
      // The named site is not the vetted destination (the 신청하기 link): named, not linked.
      await page.getByText('온라인 신청 : gijang.go.kr', { exact: true }).waitFor();
      assert.equal(await page.getByRole('link', { name: /gijang/ }).count(), 0);
      assert.equal(await page.getByText('온라인 신청', { exact: true }).count(), 0, 'the bare method line is replaced');
      for (const invented of ['심사', '지급']) assert.equal(await page.getByText(invented, { exact: true }).count(), 0, `no ${invented} step without data`);
      assert.deepEqual(errors, []);
      checks.push('guest_stages', 'gijang_lines', 'bare_method_line_removed', 'no_invented_steps');
    }
    await context.close();
  }
  // 2. 기장군, member preparing documents.
  {
    const { context, page, errors, progressCalls } = await open(GIJANG, { member: true, states: ['done', 'done', 'current', 'upcoming'] });
    await shoot(page, 'gijang-preparing');
    if (!only) {
      await label(page, '3단계 서류 준비, 현재 단계').waitFor();
      assert.equal(await label(page, '2단계 자격 확인, 완료').count(), 1);
      assert.ok(progressCalls.every(h => h === 'Bearer fixture-only-token') && progressCalls.length > 0);
      assert.deepEqual(errors, []);
      checks.push('member_preparing_stages');
    }
    await context.close();
  }
  // 3. 기장군, member who applied (report or accepted connector submission).
  {
    const { context, page, errors } = await open(GIJANG, { member: true, states: ['done', 'done', 'done', 'done'] });
    await shoot(page, 'gijang-applied');
    if (!only) {
      await label(page, '4단계 신청, 완료').waitFor();
      assert.equal(await page.getByLabel(/현재 단계$/).count(), 0);
      assert.deepEqual(errors, []);
      checks.push('member_applied_stages');
    }
    await context.close();
  }
  // 4. 부산 전세피해: the notice's numbered procedure.
  {
    const { context, page, errors } = await open(LEASE);
    await shoot(page, 'lease-guest');
    if (!only) {
      for (const [n, title] of [[1, '신청대상자 안내 문자'], [5, '계좌 입금']]) {
        await page.getByText(`${n}.`, { exact: true }).first().waitFor();
        await page.getByText(title, { exact: true }).waitFor();
      }
      assert.equal(await page.getByText('6.', { exact: true }).count(), 1);
      assert.deepEqual(errors, []);
      checks.push('numbered_procedure');
    }
    await context.close();
  }
  // 5. 부산 난임시술비 (복지로): department and channels, no steps.
  {
    const { context, page, errors } = await open(FERTILITY);
    await shoot(page, 'fertility-guest');
    if (!only) {
      await page.getByText('부산광역시 시민건강국 건강정책과', { exact: true }).last().waitFor();
      await page.getByRole('link', { name: '온라인 신청 : 복지로 열기' }).waitFor();
      assert.equal(await page.getByText('1.', { exact: true }).count(), 0);
      assert.deepEqual(errors, []);
      checks.push('channel_lines_without_steps');
    }
    await context.close();
  }
  console.log(JSON.stringify({ status: only ? 'screenshots' : 'passed', checks, production_writes: 0 }));
} finally {
  await browser?.close();
  await close();
}
