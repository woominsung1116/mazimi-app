// 서류 작성 도우미: mapping heuristics, confirmation gate, sensitive-field
// clearing, integrity refusal, AcroForm import, flattening and the
// non-fillable fallback. Runs the shipped TypeScript with pdf-lib; no network.
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const zlib = require('node:zlib');
const crypto = require('node:crypto');
const ts = require('typescript');
require.extensions['.ts'] = (module, filename) => module._compile(ts.transpileModule(fs.readFileSync(filename, 'utf8'), { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, esModuleInterop: true } }).outputText, filename);

const F = require('../lib/formFill/fields.ts');
// Values are typed (FieldValue): these build them from the strings the cases were written with.
const val = (field, raw) => raw === null || typeof raw === 'object' ? raw : field.type === 'checkbox' ? { kind: 'check', on: raw === 'true' } : { kind: 'text', value: raw };
const setV = (state, field, raw, ...rest) => F.setValue(state, field, val(field, raw), ...rest);
/** The plain value of a box: text, true/false for a checkbox. */
const plain = s => s?.value?.kind === 'text' ? s.value.value : s?.value?.kind === 'check' ? String(s.value.on) : s?.value ?? '';
const P = require('../lib/formFill/pdf.ts');
const { PDFDocument, PDFName, PDFString, degrees } = require('pdf-lib');
const FONT = new Uint8Array(fs.readFileSync(path.join(__dirname, '../assets/fonts/NanumGothic-Regular.ttf')));
const sha = async bytes => crypto.createHash('sha256').update(bytes).digest('hex');
const TODAY = new Date('2026-10-11T16:30:00Z'); // 2026-10-12 01:30 KST
const ctx = (over = {}) => ({ profile: { school_name: '부산대학교', school_year: 3, birth_year: 2003 }, remembered: { name: '홍길동', phone: '010-1234-5678', address: '부산시 금정구', email: 'a@example.org' }, today: TODAY, ...over });
const field = (id, label, extra = {}) => { const mapping = F.classifyLabel(label); return { id, page: 0, rect: { x: 0.1, y: 0.1, w: 0.4, h: 0.04 }, type: F.defaultTypeFor(mapping), label, mapping, required: true, ...extra }; };

test('label mapping table resolves Korean and English form labels', () => {
  const cases = {
    '성 명': 'name', '이름': 'name', '신청인': 'name', 'applicant_name': 'name', '영문 성명': 'name_en',
    '생년월일': 'birth_date', '휴대폰 번호': 'phone', '연락처': 'phone', '전화번호': 'phone', 'Tel': 'phone',
    '주소': 'address', '현 거주지': 'address', '이메일': 'email', 'E-mail': 'email',
    '학교명': 'school', '대학교': 'school', '소속 대학': 'school', '학과': 'major', '전공': 'major', '학년': 'grade', '학번': 'student_id',
    '신청일': 'application_date', '작성일자': 'application_date',
    '주민등록번호': 'rrn', '주민번호': 'rrn', '외국인등록번호': 'rrn',
    '주민등록주소': 'address', '주민등록상 주소': 'address', '주민등록상 거주지': 'address',
    '계좌번호': 'account', '입금 계좌': 'account', '은행명': 'bank', '예금주': 'account_holder',
    '신청인 서명': 'signature', '(인)': 'stamp', '날인': 'stamp',
  };
  for (const [label, kind] of Object.entries(cases)) assert.equal(F.classifyLabel(label), kind, label);
  assert.equal(F.classifyLabel('비고'), null);
  assert.equal(F.classifyLabel('   '), null);
});

test('suggestions come only from profile, opt-in memory or today, never for sensitive or other-person fields', () => {
  const c = ctx();
  assert.deepEqual(F.suggest(field('a', '학교명'), c), { value: '부산대학교', source: 'profile', sourceLabel: '마지미 프로필' });
  assert.equal(F.suggest(field('b', '학년'), c).value, '3');
  assert.equal(F.suggest(field('c', '성명'), c).source, 'remembered');
  assert.equal(F.suggest(field('d', '신청일'), c).value, '2026.10.12', 'KST date, not UTC');
  assert.equal(F.suggest(field('d2', '신청일', { format: 'YYYY년 MM월 DD일' }), c).value, '2026년 10월 12일');
  assert.equal(F.suggest(field('e', '주민등록번호'), c), null);
  assert.equal(F.suggest(field('f', '계좌번호'), c), null);
  assert.equal(F.suggest(field('g', '보호자 연락처'), c), null, 'guardian phone is not the user phone');
  assert.equal(F.suggest(field('h', '부모 성명'), c), null);
  assert.equal(F.suggest(field('i', '생년월일'), c), null, 'birth year alone is not a birth date');
  assert.match(F.hintFor(field('i', '생년월일'), c), /2003년/);
  assert.equal(F.suggest(field('j', '성명'), ctx({ remembered: {} })), null, 'nickname is never used as a legal name');
  assert.equal(F.suggest(field('k', '도장', { type: 'stamp' }), c).value, '홍길동');
  assert.equal(F.suggest(field('l', '서명'), c), null);
});

test('예금주 is never prefilled: the account may be a guardian\'s', () => {
  const holder = field('account_holder', '예금주');
  assert.equal(F.isSensitive(holder), true);
  assert.equal(F.suggest(holder, ctx()), null);
  assert.equal(F.suggest({ ...holder, type: 'stamp' }, ctx()), null);
  assert.equal(F.initialState([holder], ctx())[holder.id].status, 'empty');
  assert.equal(F.isSensitive({ ...holder, mapping: 'name' }), true, 'the label keeps it sensitive');
  // A name, not a number: normal keyboard, no hidden-digit input, no masking.
  assert.equal(F.isNumericSecret(holder), false);
  assert.equal(F.isNumericSecret(field('account', '계좌번호')), true);
  assert.equal(F.isNumericSecret(field('rrn', '주민등록번호')), true);
});
test('every suggested value blocks completion until the user confirms it', () => {
  const fields = [field('name', '성명'), field('school', '학교명'), field('rrn', '주민등록번호'), field('memo', '비고', { required: false })];
  let state = F.initialState(fields, ctx());
  assert.equal(state.name.status, 'suggested'); assert.equal(state.rrn.status, 'empty');
  let p = F.progress(fields, state);
  assert.equal(p.unconfirmed, 2); assert.equal(p.requiredRemaining, 3); assert.equal(p.canFinish, false);
  state = F.confirmSuggestion(state, fields[0]);
  state = setV(state, fields[1], '부산대학교 양산캠퍼스');
  assert.equal(state.school.status, 'confirmed');
  p = F.progress(fields, state);
  assert.equal(p.unconfirmed, 0); assert.equal(p.requiredRemaining, 1); assert.equal(p.canFinish, false);
  assert.equal(F.nextPending(fields, state, -1), 2, 'navigation goes to the next unfinished field');
  state = F.markNotApplicable(state, fields[2]);
  p = F.progress(fields, state);
  assert.equal(p.canFinish, true, 'optional empty fields do not block, 해당 없음 counts as handled');
  assert.equal(setV(state, fields[0], '   \u0007').name.status, 'empty', 'control characters and blanks are not a value');
  assert.equal(setV(state, fields[0], '').name.status, 'empty');
});

test('sensitive values are cleared after export and never remembered', () => {
  const fields = [field('name', '성명'), field('phone', '연락처'), field('rrn', '주민등록번호'), field('acct', '계좌번호'), field('gphone', '보호자 연락처')];
  let state = {};
  for (const [f, v] of fields.map((f, i) => [f, ['김마지', '010-0000-1111', '030101-4123456', '110-123-456789', '010-9999-8888'][i]])) state = setV(state, f, v);
  const cleared = F.clearSensitive(fields, state);
  assert.deepEqual(cleared.rrn, { value: null, status: 'empty' });
  assert.deepEqual(cleared.acct, { value: null, status: 'empty' });
  assert.equal(plain(cleared.name), '김마지');
  assert.deepEqual(F.rememberable(fields, state), { name: '김마지', phone: '010-0000-1111' });
  assert.deepEqual(F.sanitizeRemembered({ name: '홍길동', rrn: '030101-4123456', address: '030101-4123456', phone: 7, email: 'x'.repeat(201) }), { name: '홍길동' });
  assert.equal(F.looksSensitive('110123456789'), true);
  assert.equal(F.looksSensitive('+82 10-1234-5678'), false);
});

test('template integrity: hash mismatch, size mismatch and missing hash are refused', async () => {
  const bytes = new TextEncoder().encode('%PDF-1.7 fixture');
  const hash = await sha(bytes);
  await P.verifyTemplateBytes(bytes, { content_hash: hash, byte_size: bytes.length }, sha);
  await assert.rejects(P.verifyTemplateBytes(bytes, { content_hash: 'a'.repeat(64), byte_size: bytes.length }, sha), e => e.problem === 'hash_mismatch');
  await assert.rejects(P.verifyTemplateBytes(bytes, { content_hash: hash, byte_size: bytes.length + 1 }, sha), e => e.problem === 'size_mismatch');
  await assert.rejects(P.verifyTemplateBytes(bytes, { content_hash: null, byte_size: null }, sha), e => e.problem === 'missing_hash');
  await assert.rejects(P.verifyTemplateBytes(new Uint8Array(0), { content_hash: hash, byte_size: null }, sha), e => e.problem === 'size');
  const random = crypto.randomBytes(1001);
  assert.deepEqual(Buffer.from(P.base64ToBytes(P.bytesToBase64(random))), random);
  assert.equal(P.bytesToBase64(random), random.toString('base64'));
});

test('server template validation rejects a changed form or out-of-page boxes', () => {
  const hash = 'b'.repeat(64);
  const ok = { version: 1, content_hash: hash, page_count: 2, source: 'admin', fields: [{ id: 'f1', page: 1, rect: { x: 0.1, y: 0.2, w: 0.3, h: 0.05 }, type: 'text', label: '성명', mapping: 'name', required: true }] };
  assert.equal(F.parseTemplate(ok, hash, 'pdf').fields[0].page, 1);
  assert.throws(() => F.parseTemplate(ok, 'c'.repeat(64), 'pdf'), /버전 불일치/);
  assert.throws(() => F.parseTemplate({ ...ok, fields: [{ ...ok.fields[0], page: 2 }] }, hash, 'pdf'), /쪽 번호/);
  assert.throws(() => F.parseTemplate({ ...ok, fields: [{ ...ok.fields[0], rect: { x: 0.9, y: 0, w: 0.2, h: 0.1 } }] }, hash, 'pdf'), /위치 범위/);
  assert.throws(() => F.parseTemplate({ ...ok, fields: [{ ...ok.fields[0], mapping: 'password' }] }, hash, 'pdf'), /연결 정보/);
  assert.throws(() => F.parseTemplate({ ...ok, fields: [ok.fields[0], ok.fields[0]] }, hash, 'pdf'), /항목 ID/);
});

async function acroFixture() {
  const doc = await PDFDocument.create();
  const page = doc.addPage([600, 800]);
  const form = doc.getForm();
  const add = (name, tu, y) => { const f = form.createTextField(name); f.addToPage(page, { x: 100, y, width: 200, height: 20 }); if (tu) f.acroField.dict.set(PDFName.of('TU'), PDFString.of(tu)); return f; };
  add('txtName', null, 700).enableRequired();
  const korean = add('field2', null, 650); korean.acroField.dict.set(PDFName.of('TU'), require('pdf-lib').PDFHexString.fromText('학교명'));
  add('jumin', null, 600);
  add('apply_date', null, 550);
  form.createCheckBox('agree').addToPage(page, { x: 100, y: 500, width: 12, height: 12 });
  return doc.save();
}

test('AcroForm fields import as a starting template with page fractions and mappings', async () => {
  const bytes = await acroFixture();
  const hash = await sha(bytes);
  const analysis = await P.analyzePdf(bytes, hash);
  assert.equal(analysis.ok, true); assert.equal(analysis.pageCount, 1);
  assert.ok(Math.abs(analysis.aspects[0] - 0.75) < 1e-9);
  const t = analysis.acroform;
  assert.equal(t.source, 'acroform'); assert.equal(t.content_hash, hash);
  const byName = Object.fromEntries(t.fields.map(f => [f.acro_name, f]));
  assert.equal(byName.field2.label, '학교명'); assert.equal(byName.field2.mapping, 'school');
  assert.equal(byName.jumin.mapping, 'rrn'); assert.equal(F.isSensitive(byName.jumin), true);
  assert.equal(byName.apply_date.type, 'date');
  assert.equal(byName.agree.type, 'checkbox');
  assert.equal(byName.txtName.required, true);
  // pdf-lib grows the widget by half its 1pt border; allow that.
  const expected = { x: 100 / 600, y: 80 / 800, w: 200 / 600, h: 20 / 800 };
  for (const k of Object.keys(expected)) assert.ok(Math.abs(byName.txtName.rect[k] - expected[k]) < 0.002, `${k}=${byName.txtName.rect[k]}`);
  assert.deepEqual(t.fields.map(f => f.acro_name), ['txtName', 'field2', 'jumin', 'apply_date', 'agree'], 'reading order');
  assert.equal(P.chooseMode('pdf', analysis, null).mode, 'overlay');
});

function pageText(doc) {
  const contents = doc.getPage(0).node.Contents();
  const streams = contents.asArray ? contents.asArray().map(r => doc.context.lookup(r)) : [contents];
  return streams.map(s => { const raw = Buffer.from(s.getContents()); return s.dict.get(PDFName.of('Filter')) ? zlib.inflateSync(raw).toString('latin1') : raw.toString('latin1'); }).join('\n');
}

test('flattening draws only confirmed values in Korean and removes interactive fields', async () => {
  const bytes = await acroFixture();
  const { acroform: t } = await P.analyzePdf(bytes, await sha(bytes));
  const byName = Object.fromEntries(t.fields.map(f => [f.acro_name, f]));
  let state = F.initialState(t.fields, ctx());
  assert.equal(state[byName.field2.id].status, 'suggested');
  state = setV(state, byName.txtName, '홍길동');
  state = setV(state, byName.jumin, '030101-4123456');
  state = setV(state, byName.agree, 'true');
  const out = await P.flattenForm(bytes, t, state, FONT);
  const doc = await PDFDocument.load(out);
  assert.equal(doc.catalog.getAcroForm(), undefined, 'no AcroForm left');
  assert.equal(doc.getPage(0).node.Annots().size(), 0, 'no widgets left');
  const ops = pageText(doc);
  assert.equal((ops.match(/Tj/g) ?? []).length, 2, 'name and RRN drawn; the unconfirmed school suggestion is not');
  assert.ok(/ l\b/.test(ops) || / l\n/.test(ops), 'check mark drawn');
  const fonts = [...doc.context.enumerateIndirectObjects()].filter(([, o]) => o.get && o.get(PDFName.of('Type')) === PDFName.of('Font'));
  assert.ok(fonts.some(([, o]) => String(o.get(PDFName.of('BaseFont'))).includes('NanumGothic')), 'Korean font embedded');
  assert.ok(out.length < bytes.length + 1_200_000, 'full font embedded once, compressed');
});

test('signature, stamp and date overlay on a flat PDF; overflow is refused', async () => {
  const doc = await PDFDocument.create(); doc.addPage([595, 842]); const bytes = await doc.save();
  const hash = await sha(bytes);
  const analysis = await P.analyzePdf(bytes, hash);
  assert.equal(analysis.acroform, null);
  assert.equal(P.chooseMode('pdf', analysis, null).mode, 'guided', 'flat PDF without a template → copy sheet');
  const template = F.parseTemplate({ version: 1, content_hash: hash, page_count: 1, source: 'admin', fields: [
    { id: 'sig', page: 0, rect: { x: 0.6, y: 0.8, w: 0.2, h: 0.05 }, type: 'signature', label: '서명', mapping: 'signature', required: true },
    { id: 'seal', page: 0, rect: { x: 0.82, y: 0.8, w: 0.06, h: 0.05 }, type: 'stamp', label: '(인)', mapping: 'stamp', required: false },
    { id: 'date', page: 0, rect: { x: 0.1, y: 0.8, w: 0.3, h: 0.03 }, type: 'date', label: '신청일', mapping: 'application_date', required: true, format: 'YYYY. MM. DD.' },
    { id: 'tiny', page: 0, rect: { x: 0.1, y: 0.1, w: 0.02, h: 0.01 }, type: 'text', label: '비고', mapping: null, required: false },
  ] }, hash, 'pdf');
  const mode = P.chooseMode('pdf', analysis, template);
  assert.equal(mode.mode, 'overlay'); assert.equal(mode.template, template);
  let state = F.initialState(template.fields, ctx());
  assert.equal(plain(state.date), '2026. 10. 12.');
  state = F.confirmSuggestion(state, template.fields[2]);
  state = F.confirmSuggestion(state, template.fields[1]);
  state = setV(state, template.fields[0], { kind: 'signature', strokes: [[[0.1, 0.5], [0.5, 0.2], [0.9, 0.6]], [[0.4, 0.4]]], aspect: 3 });
  const out = await P.flattenForm(bytes, template, state, FONT);
  const ops = pageText(await PDFDocument.load(out));
  assert.equal((ops.match(/Tj/g) ?? []).length, 1 + 3, 'date + three stamp characters');
  assert.match(ops, / c\b|\bm\b/, 'signature path drawn');
  await assert.rejects(P.flattenForm(bytes, template, setV(state, template.fields[3], '아주 긴 비고 내용입니다'), FONT), e => e instanceof P.FieldOverflowError && e.labels[0] === '비고');
  assert.throws(() => setV(state, template.fields[0], '서명'), /signature 칸에 text 값/, 'a signature box never takes text');
  assert.deepEqual(P.stampLayout('홍길동', 10).map(c => c.ch), ['홍', '길', '동']);
  assert.equal(P.stampLayout('남궁민수', 10)[0].dx > 0, true, '4-character seal starts at the right column');
});

test('non-fillable formats and unsupported PDFs fall back to the guided copy sheet', async () => {
  assert.equal(P.chooseMode('hwp', null, null).mode, 'guided');
  assert.match(P.chooseMode('hwp', null, null).reason, /한글\(HWP\)/);
  assert.equal(P.chooseMode('hwpx', null, null).mode, 'guided');
  assert.equal(P.chooseMode('pdf', null, null).mode, 'guided', 'web / no renderer');
  const rotated = await PDFDocument.create(); rotated.addPage([595, 842]).setRotation(degrees(90));
  const rb = await rotated.save();
  const ra = await P.analyzePdf(rb, await sha(rb));
  assert.deepEqual(ra, { ok: false, reason: 'rotated' });
  assert.match(P.chooseMode('pdf', ra, null).reason, /회전/);
  assert.deepEqual(await P.analyzePdf(new TextEncoder().encode('%PDF-1.4 garbage'), 'a'.repeat(64)), { ok: false, reason: 'invalid' });
  const rows = F.guidedRows(ctx());
  const rrn = rows.find(r => r.kind === 'rrn');
  assert.equal(rrn.value, null); assert.equal(rrn.sensitive, true);
  assert.equal(rows.find(r => r.kind === 'school').value, '부산대학교');
  assert.equal(rows.find(r => r.kind === 'application_date').value, '2026.10.12');
  const mismatched = { version: 1, content_hash: 'a'.repeat(64), page_count: 3, source: 'admin', fields: [] };
  const flat = await PDFDocument.create(); flat.addPage(); const fb = await flat.save();
  assert.equal(P.chooseMode('pdf', await P.analyzePdf(fb, 'a'.repeat(64)), mismatched).mode, 'guided', 'template for a different page count is ignored');
});

test('dates parse and format across form styles', () => {
  assert.deepEqual(F.parseDate('2003.02.28'), { y: 2003, m: 2, d: 28 });
  assert.deepEqual(F.parseDate('030228', TODAY), { y: 2003, m: 2, d: 28 });
  assert.deepEqual(F.parseDate('990101', TODAY), { y: 1999, m: 1, d: 1 });
  assert.equal(F.parseDate('2003.02.30'), null);
  assert.equal(F.formatDate({ y: 2026, m: 1, d: 5 }, 'YYMMDD'), '260105');
  assert.equal(F.formatDate({ y: 2026, m: 1, d: 5 }, 'YYYY-MM-DD'), '2026-01-05');
  assert.throws(() => F.formatDate({ y: 2026, m: 2, d: 29 }));
});

test('review fixes: radio exclusivity, explicit unchecked, label-or-mapping sensitivity, typed suggestions', async () => {
  const doc = await PDFDocument.create(); const page = doc.addPage([600, 800]); const form = doc.getForm();
  const radio = form.createRadioGroup('gender'); radio.addOptionToPage('남', page, { x: 100, y: 700, width: 12, height: 12 }); radio.addOptionToPage('여', page, { x: 150, y: 700, width: 12, height: 12 }); radio.enableRequired();
  const bytes = await doc.save();
  const { acroform: t } = await P.analyzePdf(bytes, await sha(bytes));
  assert.equal(t.fields.length, 2); assert.ok(t.fields.every(f => f.type === 'checkbox' && !f.required && f.acro_name === 'gender'), 'radio options are optional checkboxes sharing a group');
  let s = setV({}, t.fields[0], 'true', t.fields);
  s = setV(s, t.fields[1], 'true', t.fields);
  assert.deepEqual([plain(s[t.fields[0].id]), plain(s[t.fields[1].id])], ['false', 'true'], 'only one radio option stays checked');
  const box = field('agree', '동의', { type: 'checkbox', mapping: null, required: true });
  const unchecked = setV({}, box, '');
  assert.deepEqual(unchecked.agree, { value: { kind: 'check', on: false }, status: 'confirmed' }, 'explicit unchecked is an answer');
  assert.equal(F.nextPending([box, field('memo', '비고', { required: false })], unchecked, -1), -1, 'answered and optional-empty fields are not pending');
  const mislinked = field('acct', '입금 계좌번호', { mapping: 'address' });
  assert.equal(F.isSensitive(mislinked), true, 'label keeps a mis-mapped account box sensitive');
  assert.equal(F.suggest(mislinked, ctx()), null);
  assert.deepEqual(F.rememberable([mislinked], setV({}, mislinked, '부산시 해운대구')), {});
  assert.equal(F.suggest(field('d', '생년월일', { type: 'date', mapping: 'name' }), ctx()), null, 'a date box never receives a name');
  assert.equal(F.suggest(field('k', '학교 직인', { type: 'stamp', mapping: 'school' }), ctx()), null, 'a seal only takes a seal name');
  const hash = 'b'.repeat(64);
  const base = { version: 1, content_hash: hash, page_count: 1, source: 'admin', fields: [{ id: 'f', page: 0, rect: { x: 0, y: 0, w: 0.1, h: 0.1 }, type: 'text', label: 'x', mapping: null, required: false }] };
  assert.throws(() => F.parseTemplate({ ...base, fields: [{ ...base.fields[0], label: '성명\u0007' }] }, hash, 'pdf'), /항목 이름/);
  assert.equal(F.parseTemplate({ ...base, fields: [{ ...base.fields[0], label: '😀'.repeat(80) }] }, hash, 'pdf').fields[0].label.length, 160, '80 code points like the server');
});

test('review fixes: undrawable values fail loudly, agency-printed values survive, small boxes and long text', async () => {
  const doc = await PDFDocument.create(); const page = doc.addPage([595, 842]); const form = doc.getForm();
  const agency = form.createTextField('agency'); agency.addToPage(page, { x: 50, y: 780, width: 200, height: 20 }); agency.setText('Busan City'); agency.enableReadOnly();
  const bytes = await doc.save(); const hash = await sha(bytes);
  const template = F.parseTemplate({ version: 1, content_hash: hash, page_count: 1, source: 'admin', fields: [
    { id: 'seal', page: 0, rect: { x: 0.8, y: 0.8, w: 0.06, h: 0.05 }, type: 'stamp', label: '(인)', mapping: null, required: false },
    { id: 'sig', page: 0, rect: { x: 0.5, y: 0.8, w: 0.2, h: 0.05 }, type: 'signature', label: '서명', mapping: null, required: false },
    { id: 'tiny', page: 0, rect: { x: 0.1, y: 0.5, w: 0.05, h: 0.0053 }, type: 'text', label: '체크칸', mapping: null, required: false },
    { id: 'essay', page: 0, rect: { x: 0.1, y: 0.1, w: 0.8, h: 0.3 }, type: 'multiline', label: '자기소개', mapping: null, required: false },
  ] }, hash, 'pdf');
  const set = (st, id, v) => setV(st, template.fields.find(f => f.id === id), v);
  await assert.rejects(P.flattenForm(bytes, template, set({}, 'seal', '홍 길 동 님'), FONT), e => e instanceof P.FieldRenderError && e.labels[0] === '(인)');
  // A value of the wrong kind (a stale or hand-built state) is refused, never skipped.
  await assert.rejects(P.flattenForm(bytes, template, { sig: { value: { kind: 'text', value: '{"broken":1}' }, status: 'confirmed' } }, FONT), e => e instanceof P.FieldRenderError && e.fieldIds[0] === 'sig');
  let state = set({}, 'tiny', 'V');
  state = set(state, 'essay', '청년 정책을 통해 지역에서 일하며 배우고 싶습니다. '.repeat(18).slice(0, 500));
  const started = Date.now();
  const out = await P.flattenForm(bytes, template, state, FONT);
  assert.ok(Date.now() - started < 5000, 'long multi-line layout stays fast');
  const ops = pageText(await PDFDocument.load(out));
  assert.match(ops, /Do/, 'agency-printed field appearance baked into the page');
});

test('HWP/HWPX: a derived template binds the converted PDF to the official file hash', () => {
  const source = 'a'.repeat(64), pdf = 'b'.repeat(64);
  const ok = { version: 1, content_hash: pdf, page_count: 1, source: 'admin', derived: { source_sha256: source, converter: 'rhwp 0.8.6 export-pdf' },
    fields: [{ id: 'f1', page: 0, rect: { x: 0.1, y: 0.2, w: 0.3, h: 0.05 }, type: 'text', label: '성명', mapping: 'name', required: true }] };
  const t = F.parseTemplate(ok, source, 'hwp');
  assert.equal(t.content_hash, pdf);
  assert.deepEqual(t.derived, ok.derived);
  assert.equal(F.parseTemplate({ ...ok, derived: undefined, content_hash: source }, source, 'pdf').derived, undefined, 'plain PDF templates are unchanged');
  // The official file changed: the derived boxes no longer apply.
  assert.throws(() => F.parseTemplate(ok, 'c'.repeat(64), 'hwp'), /버전 불일치/);
  // The converted PDF hash is not the official hash, and is never accepted in its place.
  assert.throws(() => F.parseTemplate(ok, pdf, 'hwp'), /버전 불일치/);
  assert.throws(() => F.parseTemplate({ ...ok, content_hash: source }, source, 'hwp'), /버전 불일치/);
  assert.throws(() => F.parseTemplate({ ...ok, derived: { ...ok.derived, pdf_url: 'https://evil.example/x.pdf' } }, source, 'hwp'), /변환 출처/);
  assert.throws(() => F.parseTemplate({ ...ok, derived: { ...ok.derived, converter: '변환기' } }, source, 'hwp'), /변환 출처/);
  assert.throws(() => F.parseTemplate({ ...ok, derived: { ...ok.derived, converter: '' } }, source, 'hwp'), /변환 출처/);

  // Boxes drawn on Mazimi's PDF never reach an agency PDF resource with the same official hash.
  assert.equal(F.parseTemplate(ok, source, 'hwp').content_hash, pdf);
  assert.equal(F.parseTemplate(ok, source, 'hwpx').content_hash, pdf);
  assert.throws(() => F.parseTemplate(ok, source, 'pdf'), /변환 출처/);
  assert.throws(() => F.parseTemplate(ok, source, null), /변환 출처/);
  assert.equal(F.parseTemplate({ ...ok, derived: undefined, content_hash: source }, source, 'pdf').content_hash, source);
  const derivedUrl = 'https://mazimi.kr/api/v1/programs/p/application-resources/r/form-pdf';
  const hwp = { url: 'https://agency.example/f.hwp', file_format: 'hwp', content_hash: source, byte_size: 41472 };
  assert.deepEqual(P.templateSource(hwp, t, derivedUrl), { url: derivedUrl, content_hash: pdf, byte_size: null }, 'download Mazimi\'s PDF, check its own hash');
  assert.deepEqual(P.templateSource({ ...hwp, file_format: 'hwpx' }, t, derivedUrl).content_hash, pdf);
  assert.equal(P.templateSource(hwp, null, derivedUrl), null, 'no boxes → copy sheet, never an unbound PDF');
  assert.equal(P.templateSource({ ...hwp, content_hash: 'c'.repeat(64) }, t, derivedUrl), null, 'official file changed');
  assert.equal(P.templateSource({ ...hwp, file_format: 'docx' }, t, derivedUrl), null);
  const official = { url: 'https://agency.example/f.pdf', file_format: 'pdf', content_hash: source, byte_size: 10 };
  assert.deepEqual(P.templateSource(official, t, derivedUrl), { url: official.url, content_hash: source, byte_size: 10 }, 'a PDF resource always uses the agency file');
});

// Checkbox groups (crates/core form_templates.rs `Template.groups`): same
// validation as the server, single = one answer, multi = independent ticks.
const groupedTemplate = () => {
  const hash = 'b'.repeat(64);
  const box = (id, label, group, page = 0, x = 0.1) => ({ id, page, rect: { x, y: 0.5, w: 0.02, h: 0.02 }, type: 'checkbox', label, mapping: null, required: false, group });
  return {
    hash,
    t: { version: 1, content_hash: hash, page_count: 2, source: 'admin',
      fields: [
        { id: 'name', page: 0, rect: { x: 0.1, y: 0.2, w: 0.3, h: 0.04 }, type: 'text', label: '성명', mapping: 'name', required: true },
        box('a', '구분: 대학생', 'g1', 0, 0.1), box('b', '구분: 졸업생', 'g1', 0, 0.2),
        box('c', '관심: 주거', 'g2', 0, 0.3), box('d', '관심: 일자리', 'g2', 0, 0.4),
      ],
      groups: [{ id: 'g1', mode: 'single', label: '구분' }, { id: 'g2', mode: 'multi', label: '관심 분야' }] },
  };
};

test('checkbox groups parse with the server rules and invalid groups are refused', () => {
  const { hash, t } = groupedTemplate();
  const parsed = F.parseTemplate(t, hash, 'pdf');
  assert.deepEqual(parsed.groups, t.groups);
  assert.deepEqual(parsed.fields.map(f => f.group ?? null), [null, 'g1', 'g1', 'g2', 'g2']);
  // No groups: no key, exactly as before.
  const plain = F.parseTemplate({ ...t, groups: undefined, fields: t.fields.map(({ group, ...f }) => f) }, hash, 'pdf');
  assert.ok(!('groups' in plain) && plain.fields.every(f => !('group' in f)));
  const bad = mutate => { const { t: c } = groupedTemplate(); mutate(c); return () => F.parseTemplate(c, hash, 'pdf'); };
  assert.throws(bad(c => { c.fields[1].group = 'g9'; }), /선택 묶음/, 'unknown group');
  assert.throws(bad(c => { delete c.fields[2].group; }), /선택 묶음/, 'one option');
  assert.throws(bad(c => { c.fields[2].page = 1; }), /선택 묶음/, 'two pages');
  assert.throws(bad(c => { c.fields[0].group = 'g1'; }), /선택 묶음/, 'not a checkbox');
  assert.throws(bad(c => { c.groups[0].mode = 'radio'; }), /선택 묶음/, 'mode');
  assert.throws(bad(c => { c.groups[1].id = 'g1'; }), /선택 묶음/, 'duplicate id');
  assert.throws(bad(c => { c.groups[0].id = 'G1'; c.fields[1].group = 'G1'; c.fields[2].group = 'G1'; }), /선택 묶음/, 'id pattern');
  assert.throws(bad(c => { c.groups[0].label = ' '; }), /선택 묶음/, 'label');
  for (const invisible of ['\uFEFF', '구\u200B분', '\u202E구분']) assert.throws(bad(c => { c.groups[0].label = invisible; }), /선택 묶음/, 'invisible');
  assert.throws(bad(c => { c.groups.push({ id: 'g3', mode: 'multi', label: '빈 묶음' }); }), /선택 묶음/, 'empty group');
  assert.throws(bad(c => { c.groups = Array.from({ length: 101 }, (_, n) => ({ id: `g${n}`, mode: 'multi', label: '묶음' })); }), /선택 묶음/, 'too many');
  assert.throws(bad(c => { c.groups[0].default = 'a'; }), /선택 묶음/, 'unknown key');
});

test('a single group keeps one answer, a multi group keeps every tick, acro_name still works', async () => {
  const { hash, t } = groupedTemplate();
  const parsed = F.parseTemplate(t, hash, 'pdf');
  const [, a, b, c, d] = parsed.fields;
  let s = setV({}, a, 'true', parsed.fields, parsed.groups);
  s = setV(s, b, 'true', parsed.fields, parsed.groups);
  assert.deepEqual([plain(s.a), plain(s.b)], ['false', 'true'], 'checking B unchecks A');
  assert.equal(s.a.status, 'confirmed');
  s = setV(s, b, 'false', parsed.fields, parsed.groups);
  assert.deepEqual([plain(s.a), plain(s.b)], ['false', 'false'], 'unchecking does not check another');
  s = setV(s, c, 'true', parsed.fields, parsed.groups);
  s = setV(s, d, 'true', parsed.fields, parsed.groups);
  assert.deepEqual([plain(s.c), plain(s.d)], ['true', 'true'], 'multi stays independent');
  // Radio options imported from a PDF share acro_name: still one answer.
  const radio = [
    { id: 'r1', page: 0, rect: { x: 0.1, y: 0.1, w: 0.02, h: 0.02 }, type: 'checkbox', label: '성별 · 남', mapping: null, required: false, acro_name: 'gender' },
    { id: 'r2', page: 0, rect: { x: 0.2, y: 0.1, w: 0.02, h: 0.02 }, type: 'checkbox', label: '성별 · 여', mapping: null, required: false, acro_name: 'gender' },
  ];
  let r = setV({}, radio[0], 'true', radio);
  r = setV(r, radio[1], 'true', radio);
  assert.deepEqual([plain(r.r1), plain(r.r2)], ['false', 'true']);
  // The flattened PDF draws only the checked boxes.
  const doc = await PDFDocument.create(); doc.addPage([600, 800]); doc.addPage([600, 800]);
  const bytes = await doc.save();
  const flat = await P.flattenForm(bytes, { ...parsed, content_hash: await sha(bytes) }, { ...s, name: { value: { kind: 'text', value: '홍길동' }, status: 'confirmed' } }, FONT);
  const counts = await Promise.all([false, true].map(async checkB => {
    const state = setV({ ...s, name: { value: { kind: 'text', value: '홍길동' }, status: 'confirmed' } }, b, checkB ? 'true' : 'false', parsed.fields, parsed.groups);
    const out = await P.flattenForm(bytes, { ...parsed, content_hash: await sha(bytes) }, state, FONT);
    return out.length;
  }));
  assert.ok(flat.length > 0 && counts[1] > counts[0], 'a checked box adds ink, an unchecked one adds none');
});

test('PDF radio groups import as single groups', async () => {
  const doc = await PDFDocument.create(); const page = doc.addPage([600, 800]); const form = doc.getForm();
  const radio = form.createRadioGroup('gender'); radio.addOptionToPage('남', page, { x: 100, y: 700, width: 12, height: 12 }); radio.addOptionToPage('여', page, { x: 150, y: 700, width: 12, height: 12 });
  form.createCheckBox('agree').addToPage(page, { x: 100, y: 500, width: 12, height: 12 });
  const bytes = await doc.save();
  const { acroform: t } = await P.analyzePdf(bytes, await sha(bytes));
  assert.equal(t.groups.length, 1);
  assert.equal(t.groups[0].mode, 'single');
  const members = t.fields.filter(f => f.group === t.groups[0].id);
  assert.equal(members.length, 2);
  assert.ok(t.fields.find(f => f.acro_name === 'agree').group === undefined);
  // The imported template passes the same validation the server applies.
  const digest = await sha(bytes);
  assert.doesNotThrow(() => F.parseTemplate(t, digest, 'pdf'));
});

test('a two-digit year part prints 26 after a printed 20, never 2026', () => {
  const hash = 'a'.repeat(64);
  const raw = { version: 1, content_hash: hash, page_count: 1, source: 'admin', fields: [
    { id: 'y', page: 0, rect: { x: 0.1, y: 0.1, w: 0.04, h: 0.02 }, type: 'text', label: '작성일 연도', mapping: 'application_date', required: false, part: 'year2' },
    { id: 'm', page: 0, rect: { x: 0.2, y: 0.1, w: 0.04, h: 0.02 }, type: 'text', label: '작성일 월', mapping: 'application_date', required: false, part: 'month' },
  ] };
  const t = F.parseTemplate(raw, hash, 'pdf');
  assert.equal(t.fields[0].part, 'year2');
  const [y, m] = t.fields;
  // Today's date splits into the part a box asks for.
  assert.equal(F.suggest(y, ctx()).value, '26');
  assert.equal(F.suggest(m, ctx()).value, '10');
  // Whatever the user types, a year2 box keeps two digits.
  for (const [typed, kept] of [['2026', '26'], ['26', '26'], ['2026년', '26'], ['  26 ', '26']]) assert.equal(setV({}, y, typed).y.value.value, kept, typed);
  assert.equal(setV({}, m, '3').m.value.value, '03');
  // Only text boxes take a part, and only the known ones.
  for (const bad of [{ part: 'year3' }, { part: 'year2', type: 'checkbox' }]) {
    assert.throws(() => F.parseTemplate({ ...raw, fields: [{ ...raw.fields[0], ...bad }] }, hash, 'pdf'), /날짜 부분/);
  }
  // Templates without parts are unchanged.
  assert.equal(F.parseTemplate({ ...raw, fields: [{ ...raw.fields[0], part: undefined }] }, hash, 'pdf').fields[0].part, undefined);
});
