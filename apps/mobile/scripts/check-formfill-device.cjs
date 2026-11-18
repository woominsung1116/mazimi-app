// 서류 작성 도우미: the device-QA bugs of 2026-10-31 (B1–B12), each as the
// behaviour a user saw. Runs the shipped TypeScript with pdf-lib; no network.
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const zlib = require('node:zlib');
const Module = require('node:module');
const ts = require('typescript');
require.extensions['.ts'] = (module, filename) => module._compile(ts.transpileModule(fs.readFileSync(filename, 'utf8'), { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, esModuleInterop: true } }).outputText, filename);

// device.ts and vault.ts import native modules: a minimal shim, recording what the share sheet was given.
const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'formfill-device-'));
const local = uri => uri.replace(/^file:\/\//, '');
const platform = { OS: 'ios' };
const share = { result: { action: 'sharedAction' }, calls: [], expo: 0 };
const shims = {
  'react-native': { Platform: platform, Share: { sharedAction: 'sharedAction', dismissedAction: 'dismissedAction', share: async content => { share.calls.push(content); return share.result; } } },
  'expo-crypto': { getRandomBytes: n => new Uint8Array(require('node:crypto').randomBytes(n)), CryptoDigestAlgorithm: { SHA256: 'SHA-256' } },
  'expo-file-system/legacy': {
    EncodingType: { Base64: 'base64' }, cacheDirectory: `file://${dir}/cache/`, documentDirectory: `file://${dir}/docs/`,
    makeDirectoryAsync: async u => fs.mkdirSync(local(u), { recursive: true }),
    writeAsStringAsync: async (u, d) => fs.writeFileSync(local(u), Buffer.from(d, 'base64')),
    deleteAsync: async u => fs.rmSync(local(u), { recursive: true, force: true }),
  },
  'expo-secure-store': {}, 'expo-asset': { Asset: {} },
  'expo-sharing': { isAvailableAsync: async () => true, shareAsync: async () => { share.expo++; } },
  '@react-native-async-storage/async-storage': { __esModule: true, default: {} },
  '@tanstack/react-query': {}, '@/store/auth': {},
};
const resolve = Module._resolveFilename;
Module._resolveFilename = function (request, ...rest) { return shims[request] ? request : resolve.call(this, request, ...rest); };
for (const [name, exports] of Object.entries(shims)) require.cache[name] = { id: name, filename: name, loaded: true, exports };

const root = path.resolve(__dirname, '..');
const F = require('../lib/formFill/fields.ts');
const P = require('../lib/formFill/pdf.ts');
const S = require('../lib/formFill/signature.ts');
const { hitSlopFor } = require('../lib/formFill/touch.ts');
const { applicationFormName } = require('../lib/documentName.ts');
const { PDFDocument, PDFName } = require('pdf-lib');
const FONT = new Uint8Array(fs.readFileSync(path.join(root, 'assets/fonts/NanumGothic-Regular.ttf')));
const TODAY = new Date('2026-10-31T03:00:00Z');
const ctx = { profile: null, remembered: { name: '홍길동' }, today: TODAY };

/** A finger signature: `strokes` strokes of `n` points each, drawn across a 370×160 pad. */
function fingerSignature(strokes, n) {
  return Array.from({ length: strokes }, (_, s) => Array.from({ length: n }, (_, i) => [0.05 + 0.9 * i / (n - 1), 0.3 + 0.1 * s + 0.2 * Math.sin(i / 3)]));
}
const sigField = { id: 's', page: 0, rect: { x: 0.5, y: 0.8, w: 0.2, h: 0.05 }, type: 'signature', label: '신청자 서명', mapping: 'signature', required: true };
function pageOps(doc) {
  const contents = doc.getPage(0).node.Contents();
  const streams = contents.asArray ? contents.asArray().map(r => doc.context.lookup(r)) : [contents];
  return streams.map(s => { const raw = Buffer.from(s.getContents()); return s.dict.get(PDFName.of('Filter')) ? zlib.inflateSync(raw).toString('latin1') : raw.toString('latin1'); }).join('\n');
}
async function blankTemplate(fields, groups) {
  const doc = await PDFDocument.create(); doc.addPage([595, 842]);
  const bytes = await doc.save();
  const hash = require('node:crypto').createHash('sha256').update(bytes).digest('hex');
  return { bytes, template: F.parseTemplate({ version: 1, content_hash: hash, page_count: 1, source: 'admin', fields, ...(groups ? { groups } : {}) }, hash, 'pdf') };
}

test('B1: a 300-point and a 2,000-point signature survive setValue and draw into the PDF', async () => {
  for (const [strokes, n] of [[6, 50], [20, 100]]) {
    const drawn = fingerSignature(strokes, n);
    const state = F.setValue({}, sigField, { kind: 'signature', strokes: drawn, aspect: 370 / 160 });
    const v = state.s.value;
    assert.equal(state.s.status, 'confirmed');
    assert.equal(v.kind, 'signature');
    assert.equal(v.strokes.reduce((k, s) => k + s.length, 0), strokes * n, `${strokes * n} points kept`);
    const { bytes, template } = await blankTemplate([sigField]);
    const out = await P.flattenForm(bytes, template, state, FONT);
    assert.match(pageOps(await PDFDocument.load(out)), /\bl\b/, 'strokes drawn');
  }
  // Text keeps its cap: the cap is a text rule, not a value rule.
  const name = { id: 'n', page: 0, rect: { x: 0.1, y: 0.1, w: 0.3, h: 0.03 }, type: 'text', label: '성명', mapping: 'name', required: true };
  assert.equal(F.setValue({}, name, F.textValue('가'.repeat(900))).n.value.value.length, F.MAX_VALUE_LENGTH);
  // Points past the pad's own limit are dropped, not the whole signature.
  const huge = F.setValue({}, sigField, { kind: 'signature', strokes: fingerSignature(30, 100), aspect: 2 }).s.value;
  assert.equal(huge.strokes.reduce((k, s) => k + s.length, 0), S.MAX_SIGNATURE_POINTS);
  assert.throws(() => F.setValue({}, sigField, F.textValue('{"a":1,"s":[]}')), /signature 칸에 text 값/);
});

test('B2: the pad aspect is stored as drawn and the ink keeps its shape in the box', () => {
  const v = F.setValue({}, sigField, { kind: 'signature', strokes: [[[0, 0.2], [1, 0.8]]], aspect: 2.3125 }).s.value;
  assert.equal(v.aspect, 2.3125);
  // Ink spans the whole pad width and 0.6 of its height: 2.3125 / 0.6 = 3.85 wide per unit high.
  const inkAspect = 2.3125 / 0.6;
  for (const [w, h] of [[73, 22], [200, 20], [40, 60]]) {
    const nums = P.signaturePath(v, w, h).match(/-?\d+(\.\d+)?/g).map(Number);
    const xs = nums.filter((_, i) => i % 2 === 0), ys = nums.filter((_, i) => i % 2 === 1);
    const bw = Math.max(...xs) - Math.min(...xs), bh = Math.max(...ys) - Math.min(...ys);
    assert.ok(Math.abs(bw / bh - inkAspect) / inkAspect < 0.01, `${w}x${h}: drawn ${(bw / bh).toFixed(2)}, ink ${inkAspect.toFixed(2)}`);
    assert.ok(Math.min(...xs) >= 0 && Math.max(...xs) <= w && Math.min(...ys) >= 0 && Math.max(...ys) <= h, 'inside the box');
    assert.ok(Math.abs(bw - w * 0.84) < 0.01 || Math.abs(bh - h * 0.84) < 0.01, 'fills the box on its limiting side');
  }
});

test('B3: dismissing the iOS share sheet is not an export; Android asks', async () => {
  const D = require('../lib/formFill/device.ts');
  const pdf = new Uint8Array([0x25, 0x50, 0x44, 0x46, 0x2d]);
  share.result = { action: 'dismissedAction' };
  assert.equal(await D.shareFilledPdf(pdf, '스터디카페 신청서'), 'dismissed');
  share.result = { action: 'sharedAction', activityType: 'com.apple.DocumentManagerUICore.SaveToFiles' };
  assert.equal(await D.shareFilledPdf(pdf, '스터디카페 신청서'), 'shared');
  assert.match(share.calls.at(-1).url, /스터디카페 신청서 \(작성본\)\.pdf$/, 'the file carries the form name');
  platform.OS = 'android';
  assert.equal(await D.shareFilledPdf(pdf, '스터디카페 신청서'), 'unknown');
  assert.equal(share.expo, 1);
  platform.OS = 'ios';
  assert.deepEqual(fs.readdirSync(path.join(dir, 'cache/form-fill')), [], 'the cache copy is deleted whatever the outcome');
  // The screen runs record / remember / clearSensitive only from completeExport, reached on 'shared' or 내보냈어요.
  const screen = fs.readFileSync(path.join(root, 'app/form-fill.tsx'), 'utf8');
  const exportFn = screen.slice(screen.indexOf('async function exportPdf'), screen.indexOf('async function completeExport'));
  assert.match(exportFn, /if \(outcome === 'shared'\) await completeExport\(\)/);
  assert.doesNotMatch(exportFn, /addRecord|saveRemembered|clearSensitive|kind: 'done'/);
});

test('B6: a box that cannot be drawn is named by id, so the screen can take the user to it', async () => {
  const tiny = { id: 'tiny', page: 0, rect: { x: 0.1, y: 0.1, w: 0.02, h: 0.01 }, type: 'text', label: '비고', mapping: null, required: false };
  const { bytes, template } = await blankTemplate([tiny]);
  await assert.rejects(P.flattenForm(bytes, template, F.setValue({}, tiny, F.textValue('아주 긴 비고 내용입니다')), FONT),
    e => e instanceof P.FieldDrawError && e instanceof P.FieldOverflowError && e.fieldIds[0] === 'tiny');
  const screen = fs.readFileSync(path.join(root, 'app/form-fill.tsx'), 'utf8');
  assert.match(screen, /focus\(fields\.findIndex\(f => f\.id === e\.fieldIds\[0\]\)\)/, '작성 완료 opens the failing box');
});

test('B7: 연도/월/일 boxes of 작성일 take today in KST; confirming one confirms the date', () => {
  const part = (id, p, x) => ({ id, page: 0, rect: { x, y: 0.59, w: 0.05, h: 0.02 }, type: 'text', label: `작성일 ${id}`, mapping: 'application_date', required: false, part: p });
  const fields = [part('y', 'year4', 0.36), part('m', 'month', 0.45), part('d', 'day', 0.5), { ...part('y2', 'year2', 0.36), rect: { x: 0.36, y: 0.8, w: 0.05, h: 0.02 } }];
  const state = F.initialState(fields, { ...ctx, today: new Date('2026-10-31T16:00:00Z') }); // 11-01 01:00 KST
  assert.deepEqual(['y', 'm', 'd', 'y2'].map(id => state[id].value.value), ['2026', '11', '01', '26']);
  const confirmed = F.confirmSuggestion(state, fields[1], fields);
  assert.deepEqual(['y', 'm', 'd'].map(id => confirmed[id].status), ['confirmed', 'confirmed', 'confirmed']);
  assert.equal(confirmed.y2.status, 'suggested', 'a date on another line is a different answer');
});

test('B8: tiny boxes get a 44pt touch area, split halfway between neighbours', () => {
  const agree = { left: 100, top: 300, w: 6, h: 5 }, disagree = { left: 160, top: 300, w: 6, h: 5 }, next = { left: 120, top: 300, w: 6, h: 5 };
  const lone = hitSlopFor(agree, [agree, disagree]);
  assert.deepEqual([lone.left, lone.right, lone.top, lone.bottom], [19, 19, 19.5, 19.5]);
  const close = hitSlopFor(agree, [agree, next]);
  assert.equal(close.right, 7, 'stops halfway to the box 14pt away');
  assert.equal(close.left, 19);
  assert.deepEqual(hitSlopFor({ left: 0, top: 0, w: 60, h: 50 }, []), { top: 0, bottom: 0, left: 0, right: 0 });
});

test('B9: the form is named after the programme, never the file', () => {
  assert.equal(applicationFormName('스터디카페·독서실 이용료 지원'), '스터디카페·독서실 이용료 지원 신청서');
  assert.equal(applicationFormName('청년 월세 지원 신청서'), '청년 월세 지원 신청서');
  assert.equal(applicationFormName('  '), '신청서');
  const screen = fs.readFileSync(path.join(root, 'app/form-fill.tsx'), 'utf8');
  assert.doesNotMatch(screen, /\{r\.resource_label\}|resource\?\.label \?\?|`\$\{program\.title\} \$\{resource\.label\}`/, 'no raw file name on screen or in the export');
  const vaultSave = fs.readFileSync(path.join(root, 'lib/vaultSave.ts'), 'utf8');
  assert.match(vaultSave, /applicationFormName\(form\.programTitle\)\} \(작성본\)/);
  const { shareName } = require('../lib/vault.ts');
  assert.equal(shareName({ name: '스터디카페 신청서 (작성본)', fileUri: 'file:///v/doc_1793434856080_00dde7e5.pdf.enc' }), '스터디카페 신청서 (작성본).pdf');
});

test('B10: the done screen names only the sensitive values the form had; self-filled vault cards have no issuer line', () => {
  const name = { id: 'n', page: 0, rect: { x: 0.1, y: 0.1, w: 0.3, h: 0.03 }, type: 'text', label: '성명', mapping: 'name', required: true };
  const acct = { ...name, id: 'a', label: '계좌번호', mapping: 'account' };
  const holder = { ...name, id: 'h', label: '예금주', mapping: 'account_holder' };
  let s = F.setValue({}, name, F.textValue('홍길동'));
  assert.deepEqual(F.sensitiveFilledLabels([name], s), []);
  s = F.setValue(F.setValue(s, acct, F.textValue('110-123-456789')), holder, F.textValue('홍길동'));
  assert.deepEqual(F.sensitiveFilledLabels([name, acct, holder], s), ['계좌번호', '예금주']);
  const screen = fs.readFileSync(path.join(root, 'app/form-fill.tsx'), 'utf8');
  assert.doesNotMatch(screen, /주민등록번호·계좌번호는 지웠어요|주민등록번호·계좌번호 같은 민감 정보/);
  const vault = fs.readFileSync(path.join(root, 'app/document-vault.tsx'), 'utf8');
  assert.match(vault, /doc\.source\?\.kind!=="filled_form"&&<Text style=\{ui\.muted\}>발급처:/);
});

test('B11: an optional page never blocks 작성 완료; its suggestion stays a suggestion', async () => {
  const req = { id: 'r', page: 0, rect: { x: 0.1, y: 0.1, w: 0.3, h: 0.03 }, type: 'text', label: '신청자 성명', mapping: 'name', required: true };
  const opt = { ...req, id: 'o', label: '미진학 확인서 신고인 성명', required: false, rect: { x: 0.1, y: 0.5, w: 0.3, h: 0.03 } };
  let s = F.initialState([req, opt], ctx);
  assert.equal(F.progress([req, opt], s).canFinish, false, 'the required suggestion still needs a confirm');
  s = F.confirmSuggestion(s, req);
  const p = F.progress([req, opt], s);
  assert.equal(p.canFinish, true);
  assert.equal(p.unconfirmed, 1, 'still shown as 확인 필요');
  const { bytes, template } = await blankTemplate([req, opt]);
  const ops = pageOps(await PDFDocument.load(await P.flattenForm(bytes, template, s, FONT)));
  assert.equal((ops.match(/Tj/g) ?? []).length, 1, 'only the confirmed value is drawn');
});

test('B12: a single line sits on the vertical centre of a tall row', async () => {
  const row = { id: 'n', page: 0, rect: { x: 0.2, y: 0.3, w: 0.5, h: 28 / 842 }, type: 'text', label: '성명', mapping: 'name', required: true };
  const { bytes, template } = await blankTemplate([row]);
  const ops = pageOps(await PDFDocument.load(await P.flattenForm(bytes, template, F.setValue({}, row, F.textValue('홍길동')), FONT)));
  const [, size] = ops.match(/\/\S+ ([\d.]+) Tf/);
  const tm = ops.match(/([-\d.]+) ([-\d.]+) Tm/);
  const baseline = Number(tm[2]);
  // NanumGothic Hangul ink runs from -0.10 to 0.83 em: its middle is 0.36 em above the baseline.
  const inkMiddle = baseline + 0.36 * Number(size);
  const boxMiddle = 842 * (1 - row.rect.y - row.rect.h / 2);
  assert.ok(Math.abs(inkMiddle - boxMiddle) < 0.5, `ink middle ${inkMiddle.toFixed(2)} vs box middle ${boxMiddle.toFixed(2)}`);
});

test('review: the summary shows what the PDF holds, and the busy guard covers 내보냈어요 and 수정하기', () => {
  const screen = fs.readFileSync(path.join(root, 'app/form-fill.tsx'), 'utf8');
  const review = screen.slice(screen.indexOf("if (phase.kind === 'review')"), screen.indexOf("// phase.kind === 'fill'"));
  assert.match(review, /s\?\.status === 'suggested' \? '—' : summaryOf\(f, s, '—'\)/, 'an unconfirmed suggestion is not listed as a value');
  assert.match(review, /disabled=\{busy\} onPress=\{\(\) => \{ dropFilledBytes\(\);/, '수정하기 waits for an export in progress');
  assert.match(screen, /async function confirmExport\(\) \{\n    if \(busy\) return;\n    setBusy\(true\);/);
});

test('B7: the app asks the server for date parts', () => {
  const api = fs.readFileSync(path.join(root, 'lib/api.ts'), 'utf8');
  const fn = api.slice(api.indexOf('getFormTemplate:'), api.indexOf('getUniversities:'));
  assert.match(fn, /'X-Mazimi-Client-Caps': 'form-date-parts-v1'/);
});
