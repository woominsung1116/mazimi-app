// Gate (a): fill a template with obviously fake values through the app's own code
// (apps/mobile/lib/formFill: parseTemplate -> analyzePdf -> chooseMode -> flattenForm)
// and report what the app refused. Never uses real data.
//
//   node scripts/forms/auto/fill.cjs <template.json> <blank.pdf> <official sha256> <format> <out.pdf> <report.json> [groups.json]
//
// With groups.json (groups.py), a single-choice group gets one tick (its first option), as the
// app will allow once templates carry groups; multi groups keep every box ticked.
//
// Report: overflow (labels whose realistic fake value did not fit; the app would refuse a
// real value of that length too), sensitive (rrn/account/signature boxes that the app would
// prefill or that are not typed as sensitive), fields with the value written to each.
const { createRequire } = require('node:module');
const fs = require('node:fs'), path = require('node:path'), crypto = require('node:crypto');
const APP = path.resolve(__dirname, '../../../apps/mobile') + '/';
const req = createRequire(APP + 'package.json');
const ts = req('typescript');
require.extensions['.ts'] = (m, f) => m._compile(ts.transpileModule(fs.readFileSync(f, 'utf8'), { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, esModuleInterop: true } }).outputText, f);
const Module = require('node:module');
const origResolve = Module._resolveFilename;
Module._resolveFilename = function (r, parent, ...rest) { try { return origResolve.call(this, r, parent, ...rest); } catch (e) { return req.resolve(r); } };
const F = require(APP + 'lib/formFill/fields.ts');
const P = require(APP + 'lib/formFill/pdf.ts');
const FONT = new Uint8Array(fs.readFileSync(APP + 'assets/fonts/NanumGothic-Regular.ttf'));

const SIG = JSON.stringify({ a: 3, s: [[[0.05, 0.7], [0.3, 0.2], [0.5, 0.8], [0.7, 0.2], [0.95, 0.6]]] });
// Realistic lengths, fake content: a box that cannot hold these cannot hold real data either.
const BY_KIND = {
  name: '가나다', name_en: 'HONG GILDONG', phone: '010-0000-0000', email: 'test@example.invalid',
  address: '가상시 테스트구 시험로 123, 101동 1001호', birth_date: '2000.01.01', application_date: '2026.10.16',
  school: '테스트대학교', major: '테스트학과', grade: '3', student_id: '20260000',
  rrn: '000000-0000000', account: '000-000000-00-000', bank: '테스트은행', account_holder: '가나다',
};
// Short numeric blanks named by the printed unit or label.
const NUMERIC = [[/연도$/, '2026'], [/ 월$|월$/, '10'], [/ 일$|일$/, '16'], [/나이|만\s*\(?\s*\)?\s*세|세\)?$/, '25'], [/우편번호/, '00000'],
  [/학년$/, '3'], [/학기$/, '1'], [/평점|점수|학점/, '4.0'], [/금액|원$/, '100000'], [/개월/, '6'], [/인원|명$/, '3'], [/\(\d+\)$/, '테스트']];
const SENSITIVE_LABEL = /주민(등록)?번호|외국인등록번호|계좌/;
const SIGN_LABEL = /서명/;

// One part of a value split by printed separators ('주민등록번호 (1)', '휴대폰 (2)'): that part's length.
const PARTS = { rrn: ['000000', '0000000'], phone: ['010', '0000', '0000'], account: ['000', '000000', '00', '000'], birth_date: ['2000', '01', '01'] };
// One part of a date: the value the app writes there for 2026-10-16.
const PART = { year2: '26', year4: '2026', month: '10', day: '16' };
function fakeValue(f) {
  if (f.type === 'checkbox') return 'true';
  if (f.part && PART[f.part]) return PART[f.part];
  const part = /\((\d)\)$/.exec(f.label);
  const partKind = f.mapping || F.classifyLabel(f.label);
  if (part && PARTS[partKind]) return PARTS[partKind][Math.min(+part[1], PARTS[partKind].length) - 1];
  if (f.type === 'signature') return SIG;
  if (f.type === 'stamp') return '가나';
  if (f.mapping && BY_KIND[f.mapping] && !(f.mapping === 'name' && / \(\d+\)$/.test(f.label))) return BY_KIND[f.mapping];
  const kind = F.classifyLabel(f.label);
  if (kind && ['rrn', 'account', 'phone', 'birth_date', 'email'].includes(kind) && !/\(\d\)$/.test(f.label)) return BY_KIND[kind];
  for (const [re, v] of NUMERIC) if (re.test(f.label)) return v;
  return f.type === 'multiline' ? '테스트 내용입니다. 가상 값으로 채운 확인용 문장입니다.' : '테스트';
}

function sensitiveProblems(t) {
  const ctx = { profile: { school_name: '테스트대학교', school_year: 3, birth_year: 2000 }, remembered: { name: '가나다', phone: '010-0000-0000', address: '가상시 테스트로 1', email: 'test@example.invalid' }, today: new Date('2026-10-16T00:00:00+09:00') };
  const out = [];
  for (const f of t.fields) {
    const sensitiveLabel = SENSITIVE_LABEL.test(f.label.replace(/\s+/g, ''));
    const signLabel = SIGN_LABEL.test(f.label) && !/서명\s*필수/.test(f.label);
    if (sensitiveLabel && f.type !== 'checkbox' && !F.isSensitive(f)) out.push(`${f.id} '${f.label}': 주민번호/계좌 칸인데 민감 칸으로 표시되지 않음 (mapping ${f.mapping})`);
    if (signLabel && f.type !== 'signature' && f.type !== 'checkbox' && f.type !== 'stamp') out.push(`${f.id} '${f.label}': 서명 칸인데 type ${f.type}`);
    if ((sensitiveLabel || f.type === 'signature' || F.isSensitive(f)) && F.suggest(f, ctx) !== null) out.push(`${f.id} '${f.label}': 앱이 값을 미리 채움`);
  }
  return out;
}

(async () => {
  const [tplPath, pdfPath, officialHash, format, out, reportPath, groupsPath] = process.argv.slice(2);
  const groups = groupsPath && fs.existsSync(groupsPath) ? JSON.parse(fs.readFileSync(groupsPath, 'utf8')).groups : [];
  const raw = JSON.parse(fs.readFileSync(tplPath, 'utf8'));
  const t = F.parseTemplate(raw, officialHash, format);
  const bytes = new Uint8Array(fs.readFileSync(pdfPath));
  if (crypto.createHash('sha256').update(bytes).digest('hex') !== t.content_hash) throw new Error('pdf hash != template content_hash');
  const a = await P.analyzePdf(bytes, t.content_hash);
  if (!a.ok) throw new Error('analyze: ' + a.reason);
  // The app picks overlay for a PDF, and for HWP/HWPX once the derived PDF is used.
  const mode = P.chooseMode('pdf', a, t);
  if (mode.mode !== 'overlay') throw new Error('mode ' + JSON.stringify(mode));
  const state = {}, values = {};
  for (const f of t.fields) { values[f.id] = fakeValue(f); state[f.id] = { value: values[f.id], status: 'confirmed' }; }
  for (const g of groups) if (g.mode === 'single') for (const id of g.members.slice(1)) if (state[id]) { values[id] = 'false'; state[id] = { value: 'false', status: 'confirmed' }; }
  const overflow = [];
  let pdf;
  for (let i = 0; i < 5 && !pdf; i++) {
    try { pdf = await P.flattenForm(bytes, t, state, FONT); }
    catch (e) {
      if (!(e instanceof P.FieldOverflowError)) throw e;
      // Record, then shrink only so the rest of the page can still be rendered and checked.
      for (const f of t.fields) if (e.labels.includes(f.label) && state[f.id].value !== '1') {
        if (!overflow.includes(f.label)) overflow.push(f.label);
        state[f.id] = { value: state[f.id].value.length > 1 ? '가' : '1', status: 'confirmed' };
        values[f.id] = state[f.id].value;
      }
    }
  }
  if (!pdf) throw new Error('could not render even with one-character values');
  fs.writeFileSync(out, pdf);
  const groupOf = {};
  for (const g of groups) for (const id of g.members) groupOf[id] = g;
  const report = { fields: t.fields.map(f => ({ id: f.id, page: f.page, rect: f.rect, type: f.type, label: f.label, mapping: f.mapping, required: f.required, value: f.type === 'signature' ? '(가짜 서명)' : values[f.id], group: groupOf[f.id] ? { id: groupOf[f.id].id, mode: groupOf[f.id].mode, review_flag: groupOf[f.id].review_flag } : null })), groups, overflow, sensitive: sensitiveProblems(t), page_count: t.page_count };
  fs.writeFileSync(reportPath, JSON.stringify(report, null, 1));
  console.log(`${path.basename(out)}: ${t.fields.length} fields, overflow ${overflow.length}, sensitive problems ${report.sensitive.length}`);
})().catch(e => { console.error('FAIL', e.message); process.exit(1); });
