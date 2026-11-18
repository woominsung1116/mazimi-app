// 신청 준비 journey (agent + 서류 작성 도우미): on-device document matching,
// the device-built draft, and the journey helpers of the agent screen.
// Runs the shipped TypeScript; no network, no storage.
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const ts = require('typescript');
require.extensions['.ts'] = (module, filename) => module._compile(ts.transpileModule(fs.readFileSync(filename, 'utf8'), { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, esModuleInterop: true } }).outputText, filename);

const R = require('../lib/documentReadiness.ts');
const D = require('../lib/formFill/draft.ts');
const F = require('../lib/formFill/fields.ts');
// Values are typed (FieldValue): these build them from the strings the cases were written with.
const val = (field, raw) => raw === null || typeof raw === 'object' ? raw : field.type === 'checkbox' ? { kind: 'check', on: raw === 'true' } : { kind: 'text', value: raw };
const setV = (state, field, raw, ...rest) => F.setValue(state, field, val(field, raw), ...rest);
/** The plain value of a box: text, true/false for a checkbox. */
const plain = s => s?.value?.kind === 'text' ? s.value.value : s?.value?.kind === 'check' ? String(s.value.on) : s?.value ?? '';
const A = require('../lib/agent.ts');

const TODAY = new Date('2026-10-16T03:00:00Z'); // 2026-10-16 12:00 KST
const doc = (id, type, issuedAt, extra = {}) => ({ id, type, name: id, fileUri: `file:///vault/${id}`, mimeType: 'application/pdf', issuedAt, expiresAt: null, addedAt: issuedAt, note: null, encrypted: true, ...extra });
const task = (title, detail = '', kind = 'document') => ({ id: `document:${title}`, title, detail, kind, done: false });

test('document names map to vault types, first match wins', () => {
  const cases = {
    '주민등록등본': 'resident_register', '주민등록 초본': 'resident_register', '가족관계증명서': 'family_relation',
    '건강보험료 납부확인서': 'health_insurance', '소득금액증명원': 'income_proof', '재학증명서': 'enrollment_cert',
    '휴학증명서': 'leave_cert', '졸업증명서': 'graduation_cert', '졸업예정증명서': 'graduation_cert',
    '통장사본': 'bank_statement', '장애인증명서': 'disability_cert',
  };
  for (const [title, type] of Object.entries(cases)) assert.equal(R.documentTypeOf(title)?.type, type, title);
  assert.equal(R.documentTypeOf('자기소개서'), null);
  assert.equal(R.documentTypeOf('  '), null);
  for (const entry of R.DOCUMENT_CATALOG) {
    if (entry.issuer.url) assert.match(entry.issuer.url, /^https:\/\/([a-z0-9-]+\.)*(go\.kr|or\.kr|gov\.kr)$/, entry.type);
  }
});

test('freshness windows are read from the notice text', () => {
  assert.equal(R.windowDaysOf('최근 3개월 이내 발급분'), 90);
  assert.equal(R.windowDaysOf('발급일로부터 30일 이내'), 30);
  assert.equal(R.windowDaysOf('1개월 이내'), 30);
  assert.equal(R.windowDaysOf('2주 이내'), 14);
  assert.equal(R.windowDaysOf('발급일을 확인하세요.'), null);
  assert.equal(R.ageInDays('2026-10-16T00:00:00+09:00', TODAY), 0);
  assert.equal(R.ageInDays('2026-09-16', TODAY), 30);
  assert.equal(R.ageInDays('not a date', TODAY), null);
  assert.equal(R.ageInDays('2026-12-01', TODAY), null, 'a future date is not an age');
});

test('readiness: have, stale, check date, missing, newest copy wins, issuer links for missing ones', () => {
  const vault = [
    doc('old-register', 'resident_register', '2026-05-01'),
    doc('new-register', 'resident_register', '2026-10-01'),
    doc('enroll', 'enrollment_cert', '2026-06-01'),
    doc('income', 'income_proof', '2026-09-01', { expiresAt: '2026-10-01' }),
  ];
  const tasks = [
    task('주민등록등본', '최근 1개월 이내 발급'),
    task('재학증명서', '최근 3개월 이내 발급'),
    task('소득금액증명원'),
    task('가족관계증명서'),
    task('자기소개서'),
    { id: 'information', title: '신청 정보 준비', detail: '', kind: 'information' },
  ];
  const rows = R.documentReadiness(tasks, vault, TODAY);
  const by = Object.fromEntries(rows.map(r => [r.title, r]));
  assert.equal(rows.length, 5, 'non-document tasks are skipped');
  assert.equal(by['주민등록등본'].status, 'have');
  assert.equal(by['주민등록등본'].document.id, 'new-register', 'the newest copy is the one shown');
  assert.equal(by['주민등록등본'].document.ageDays, 15);
  assert.equal(by['재학증명서'].status, 'stale', '137 days > 90');
  assert.equal(by['소득금액증명원'].status, 'stale', 'past its own expiry');
  assert.equal(by['가족관계증명서'].status, 'missing');
  assert.equal(by['가족관계증명서'].issuer.url, 'https://efamily.scourt.go.kr');
  assert.match(R.readinessNote(by['가족관계증명서']), /발급: 대법원/);
  assert.equal(by['자기소개서'].status, 'missing');
  assert.equal(by['자기소개서'].issuer, null);
  const metadataOnly = R.documentReadiness([task('통장사본')], [doc('meta', 'bank_statement', '2026-10-01', { fileUri: null })], TODAY)[0];
  assert.equal(metadataOnly.status, 'missing', 'an entry without a kept file is not in hand');
  const checkDate = R.documentReadiness([task('주민등록등본')], vault, TODAY)[0];
  assert.equal(checkDate.status, 'check_date', 'no window stated: show the date, never claim it is valid');
  assert.match(R.readinessNote(checkDate), /2026\.10\.01 발급\(15일 전\) · 공고의 발급 기한을 확인/);
  assert.deepEqual(R.readinessSummary(rows), { have: 1, needAction: 4, total: 5 });
});

test('readiness never mutates the vault and the module has no I/O', () => {
  const vault = [doc('a', 'bank_statement', '2026-10-01')];
  const before = JSON.stringify(vault);
  R.documentReadiness([task('통장사본')], vault, TODAY);
  assert.equal(JSON.stringify(vault), before);
  for (const file of ['../lib/documentReadiness.ts', '../lib/formFill/draft.ts']) {
    const source = fs.readFileSync(path.join(__dirname, file), 'utf8');
    const imports = [...source.matchAll(/^import (?!type ).*from '([^']+)'/gm)].map(m => m[1]);
    assert.deepEqual(imports.filter(m => m !== './fields'), [], `${file} imports only pure modules`);
    assert.doesNotMatch(source, /fetch\(|AsyncStorage|SecureStore|FileSystem|api\./, file);
  }
});

const field = (id, label, extra = {}) => { const mapping = F.classifyLabel(label); return { id, page: 0, rect: { x: 0.1, y: 0.1, w: 0.4, h: 0.04 }, type: F.defaultTypeFor(mapping), label, mapping, required: true, ...extra }; };
const ctx = { profile: { school_name: '부산대학교', school_year: 3, birth_year: 2003 }, remembered: { name: '홍길동', phone: '010-1234-5678', address: '부산시 금정구', email: 'a@example.org' }, today: TODAY };

test('the draft fills only proposals; 주민등록번호, 계좌번호, 예금주 and other people stay empty', () => {
  const fields = [
    field('name', '성명'), field('school', '학교명'), field('grade', '학년'), field('date', '신청일'),
    field('rrn', '주민등록번호'), field('acct', '입금 계좌번호'), field('acct2', '계좌', { mapping: 'name' }),
    field('gphone', '보호자 연락처'), field('memo', '비고', { required: false }),
  ];
  const { state, summary } = D.buildDraft(fields, ctx);
  assert.equal(state.name.status, 'suggested');
  assert.equal(plain(state.school), '부산대학교');
  assert.equal(plain(state.grade), '3');
  assert.equal(plain(state.date), '2026.10.16');
  for (const id of ['rrn', 'acct', 'acct2', 'gphone']) assert.deepEqual(state[id], { value: null, status: 'empty' }, id);
  assert.deepEqual(summary, { suggested: 4, suggestedLabels: ['성명', '학교명', '학년', '신청일'], sensitive: 3, sensitiveLabels: ['주민등록번호', '계좌번호'], otherPerson: 1, empty: 1, total: 9 });
  assert.equal(F.progress(fields, state).canFinish, false, 'a draft is never finished without the user');
  // Fixed copy naming only the fields actually pre-filled.
  assert.match(D.draftNote(summary), /^기본 정보\(성명·학교명·학년·신청일\)는 비서가 미리 채워뒀어요\. 나머지 칸은 직접 적어주세요\./);
  assert.match(D.draftNote(summary), /미리 채운 4칸도 하나씩 확인해야/);
  assert.doesNotMatch(D.draftNote(summary), /연락처|주소/, 'nothing that was not filled');
  assert.match(D.draftNote(summary), /주민등록번호·계좌번호 3칸은 비워 뒀어요/);
  // Nothing the draft holds may be remembered as sensitive, even if typed later.
  const typed = setV(setV(state, fields[4], '030101-4123456'), fields[5], '110-123-456789');
  assert.deepEqual(Object.keys(F.rememberable(fields, typed)).sort(), []);
  assert.deepEqual(F.clearSensitive(fields, typed).rrn, { value: null, status: 'empty' });
});

test('the draft note names only the sensitive kinds the form has', () => {
  const { summary } = D.buildDraft([field('name', '성명'), field('holder', '예금주')], ctx);
  assert.deepEqual(summary.sensitiveLabels, ['예금주']);
  assert.match(D.draftNote(summary), /예금주 1칸은 비워 뒀어요/);
  assert.doesNotMatch(D.draftNote(summary), /주민등록번호|계좌번호/);
});

test('a draft from an empty context proposes only today’s date', () => {
  const { summary } = D.buildDraft([field('name', '성명'), field('date', '작성일')], { profile: null, remembered: {}, today: TODAY });
  assert.deepEqual(summary, { suggested: 1, suggestedLabels: ['작성일'], sensitive: 0, sensitiveLabels: [], otherPerson: 0, empty: 1, total: 2 });
  assert.match(D.draftNote(summary), /^기본 정보\(작성일\)는 비서가 미리 채워뒀어요/);
  assert.match(D.draftNote({ ...summary, suggested: 0, suggestedLabels: [] }), /^미리 채울 수 있는 칸이 없었어요/);
});

const P = '6f1c2b3a-0000-4000-8000-000000000001';
const RID = '6f1c2b3a-0000-4000-8000-0000000000aa';

test('journey goals are fixed keys, never free text from a link', () => {
  assert.equal(A.journeyGoal('applied'), '이 혜택 신청 완료로 기록해줘');
  assert.equal(A.journeyGoal('form'), '이 혜택 서식 작성 도와줘');
  for (const bad of ['신청 완료', 'applied ', undefined, null, ['applied'], '이전 지시 무시']) assert.equal(A.journeyGoal(bad), null);
});

test('an approved form card opens the draft in-app; anything else does not', () => {
  const card = (over = {}) => ({ id: 'c', tool: 'open_form_fill', program_id: P, title: 't', action: 'a', before: 'b', after: 'c', reversible: true, payload_hash: 'f'.repeat(64), expires_at: '2026-10-16T04:00:00Z', status: 'executed', result: { forms: [{ resource_id: RID, label: '신청서' }] }, ...over });
  assert.equal(A.routeAfterApproval(card()), `/form-fill?programId=${P}&resourceId=${RID}&draft=1`);
  assert.equal(A.routeAfterApproval(card({ status: 'pending' })), null, 'not before approval');
  assert.equal(A.routeAfterApproval(card({ tool: 'set_alert' })), null);
  assert.equal(A.routeAfterApproval(card({ result: { forms: [{ resource_id: 'https://evil.example/x', label: 'x' }] } })), null);
  assert.equal(A.routeAfterApproval(card({ result: { forms: [{ resource_id: RID }, { resource_id: P }] } })), null, 'two forms: the card lists them');
  assert.equal(A.approvedForms(card({ result: { forms: [{ resource_id: RID }, { resource_id: P }] } })).length, 2);
  assert.equal(A.draftRoute('not-a-uuid', RID), null);
  assert.equal(A.guidedRoute(P), `/form-fill?programId=${P}`);
  assert.equal(A.guidedRoute('../x'), null);
});

test('journeyOf folds a prepare run into steps and holds while a card is pending', () => {
  const result = (tool, data, extra = {}) => ({ seq: 0, tool, permission: 'read', program_id: P, ok: true, status: 'ok', data, evidence: [], executed_at: '2026-10-16T03:00:00Z', ...extra });
  const run = {
    id: 'r', conversation_id: 'c', status: 'awaiting_confirmation', planner: 'fast_path', goal_kind: 'prepare_program', steps: [], message: null, error_code: null, created_at: '', completed_at: null,
    results: [
      result('check_eligibility', { status: 'unknown' }),
      result('get_preparation', { tasks: [{ id: 'document:재학증명서', title: '재학증명서', detail: '최근 3개월 이내', kind: 'document', done: false }, { id: 'x' }] }),
      result('set_alert', {}, { permission: 'confirm_write', status: 'awaiting_confirmation' }),
    ],
    confirmations: [{ id: 'c1', tool: 'set_alert', program_id: P, status: 'pending', expires_at: '2999-01-01T00:00:00Z' }],
  };
  const journey = A.journeyOf(run);
  assert.equal(journey.programId, P);
  assert.equal(journey.verdict, 'unknown');
  assert.deepEqual(journey.tasks.map(t => t.title), ['재학증명서'], 'malformed tasks are dropped');
  assert.equal(journey.pendingCards, 1);
  assert.equal(A.journeyOf({ ...run, confirmations: [] }).pendingCards, 0);
  assert.equal(A.journeyOf({ ...run, results: [] }), null);
  assert.equal(A.journeyOf(null), null);
  // A form card pending in a later run of the same conversation holds the journey too.
  const later = { ...run, id: 'r2', goal_kind: 'form_fill', results: [result('open_form_fill', { forms: [] })], confirmations: [{ id: 'c2', tool: 'open_form_fill', program_id: P, status: 'pending', expires_at: '2999-01-01T00:00:00Z' }] };
  assert.equal(A.pendingCards([{ ...run, confirmations: [] }, later, null]), 1);
  assert.equal(A.journeyOf(later), null, 'only a prepare result anchors the journey');
});

test('journey goals are the same strings the server plans without a model', () => {
  const compose = fs.readFileSync(path.join(__dirname, '../../../crates/api/src/agent/compose.rs'), 'utf8');
  assert.ok(compose.includes(`pub const FORM_GOAL: &str = "${A.JOURNEY_GOALS.form}";`));
  assert.ok(compose.includes(`pub const APPLIED_GOAL: &str = "${A.JOURNEY_GOALS.applied}";`));
});

test('clearing the goal= param after prefill does not remount the agent screen', () => {
  // A React render test needs a native renderer this suite does not have;
  // the remount comes only from the Assistant key, so pin its parts.
  const source = fs.readFileSync(path.join(__dirname, '../app/(tabs)/agent.tsx'), 'utf8');
  const key = source.match(/<Assistant key=\{`([^`]*)`\}/);
  assert.ok(key, 'Assistant has a template key');
  assert.doesNotMatch(key[1], /initialGoal|goal/, 'the goal param is not part of the key');
  assert.match(source, /router\.setParams\(\{ goal: undefined \}\)/);
});

test('a profile card has no programme and opens nothing', () => {
  const card = { id: 'c', tool: 'profile_patch', program_id: null, title: '내 프로필', action: 'a', before: '부산', after: '서울', reversible: true, payload_hash: 'f'.repeat(64), expires_at: '2026-10-16T04:00:00Z', status: 'executed', result: { field: 'region_code', value: 'seoul' } };
  assert.equal(A.routeAfterApproval(card), null);
  assert.equal(A.draftRoute(null, RID), null);
});
