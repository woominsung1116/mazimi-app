// 서류 도우미 values said to the 비서, in form context: values never leave the
// device (lib/formFill/chatFill.ts, digits.ts, draft.ts). Runs the shipped
// TypeScript against the production form definitions; the network is a mock
// that fails the test if it is ever called with a dictation.
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const ts = require('typescript');
require.extensions['.ts'] = (module, filename) => module._compile(ts.transpileModule(fs.readFileSync(filename, 'utf8'), { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, esModuleInterop: true } }).outputText, filename);

const C = require('../lib/formFill/chatFill.ts');
const Dg = require('../lib/formFill/digits.ts');
const D = require('../lib/formFill/draft.ts');
const F = require('../lib/formFill/fields.ts');

const VALIDATION = path.join(__dirname, '../../../docs/validation');
const load = name => {
  const raw = JSON.parse(fs.readFileSync(path.join(VALIDATION, 'form-templates', `${name}.template.json`), 'utf8'));
  delete raw.derived; // the boxes are what the parse reads
  return F.parseTemplate(raw, raw.content_hash, 'pdf');
};
const ctxOf = (name, programId = '11111111-1111-4111-8111-111111111111', resourceId = '22222222-2222-4222-8222-222222222222') => ({ programId, resourceId, template: load(name) });
const INTERVIEW = ctxOf('wonju-ai-interview-experience');
const STUDY = ctxOf('wonju-study-cafe');
const PNU = ctxOf('pnu-hardship-support');
const PROBE_FILE = JSON.parse(fs.readFileSync(path.join(VALIDATION, 'form-dictation-probes.json'), 'utf8'));
const PROBES = PROBE_FILE.probes;

/** The real dispatch with a mocked network: returns where the message went. */
async function route(text, ctx, chip = false) {
  const sent = [];
  const where = await C.dispatchMessage(text, ctx, chip, {
    server: async t => { sent.push(t); return 'server'; },
    local: () => 'local',
  });
  return { where, sent };
}

test('in form context no dictation probe from reviews 2 and 3 reaches the network', async () => {
  assert.ok(PROBES.length >= 90);
  for (const ctx of [INTERVIEW, STUDY, PNU]) {
    for (const probe of PROBES) {
      const { where, sent } = await route(probe, ctx);
      assert.equal(where, 'local', probe);
      assert.deepEqual(sent, [], probe);
    }
  }
});

const COMMANDS = require('../lib/formFill/chatCommands.json');

test('outside form context a message goes to the server, but a dictation is held and only FORM_GOAL is sent (review 4 M2)', async () => {
  for (const text of ['마감 언제야?', '알림 켜줘', '소득 기준이 뭐야?']) {
    const { where, sent } = await route(text, null);
    assert.equal(where, 'server', text);
    assert.deepEqual(sent, [text]);
  }
  for (const text of ['원주 AI 면접 체험 신청서에 이름 김마지, 학교 부산대로 적어줘', '이름 칸에 김마지', '학교: 부산대', '연락처는 010-1234-5678로 넣어줘']) {
    const sent = []; const held = [];
    await C.dispatchMessage(text, null, false, { server: async t => { sent.push(t); }, local: () => assert.fail(text), hold: async t => { held.push(t); sent.push(C.FORM_GOAL); } });
    assert.deepEqual(sent, [C.FORM_GOAL], text);
    assert.deepEqual(held, [text]);
  }
  assert.ok(COMMANDS.server_goals.includes(C.FORM_GOAL));
  const t0 = 5_000_000;
  C.holdDictation('u1', '이름은 김마지로 적어줘', t0);
  assert.equal(C.takeHeldDictation('u2', t0), null, 'another account');
  assert.equal(C.takeHeldDictation('u1', t0 + 1), '이름은 김마지로 적어줘');
  assert.equal(C.takeHeldDictation('u1', t0 + 2), null, 'taken once');
  C.holdDictation('u1', 'x', t0);
  assert.equal(C.takeHeldDictation('u1', t0 + C.HAND_OFF_MS + 1), null, 'expired');
});

test('in form context only closed-vocabulary commands and server goals reach the server (review 4 H1, H3)', async () => {
  for (const ctx of [INTERVIEW, STUDY, PNU]) {
    for (const text of [...COMMANDS.probes.server, ...COMMANDS.server_goals]) {
      const { where, sent } = await route(text, ctx);
      assert.equal(where, 'server', text);
      assert.deepEqual(sent, [text]);
    }
    for (const text of COMMANDS.probes.local) {
      const { where, sent } = await route(text, ctx);
      assert.equal(where, 'local', text);
      assert.deepEqual(sent, [], text);
    }
  }
  // A chip is the server's own goal, whatever it says.
  assert.equal((await route('이 혜택 서식 작성 도와줘', INTERVIEW, true)).where, 'server');
  for (const text of ['이름은 김마지 맞나요?', '나 기초수급자인데 서류에 뭐 적어야 돼?']) assert.equal((await route(text, INTERVIEW)).where, 'local', text);
});

test('no command word is a field word, a sensitive-context word or a value shape', () => {
  for (const w of [...COMMANDS.targets, ...COMMANDS.stems, ...COMMANDS.words]) {
    assert.ok(!COMMANDS.sensitive_context.some(c => w.includes(c)), w);
    assert.ok(!/\p{N}/u.test(w), w);
  }
  for (const w of [...COMMANDS.targets, ...COMMANDS.words]) assert.equal(F.classifyLabel(w), null, w);
  // Names spelled from vocabulary pieces stay: a particle is never read as a verb ending.
  for (const name of ['하은', '나은', '이은', '다은', '주은', '지우', '하니', '써니']) assert.equal(C.commandOnly(`${name} 해줘`), false, name);
});

test('a value said as a topic is never echoed (review 4 H2)', () => {
  for (const [text, value] of [['9001011234567은 여기 적어줘', '9001011234567'], ['900101-1234567은 여기 적어줘', '900101'], ['구공공일공일일이삼사오육칠은 어디 적어?', '구공공'], ['110-123-456789는 계좌 칸에', '456789'], ['홍길동은 예금주 칸에 적어줘', '홍길동'], ['우울증은 병명 칸에 적어줘', '우울증'], ['홍길동은 예금주야', '홍길동']]) {
    for (const ctx of [INTERVIEW, STUDY, PNU]) {
      const fill = C.parseLocal(text, ctx);
      assert.deepEqual(fill.fields, [], text);
      for (const n of fill.notes) assert.ok(!n.includes(value), `${text}: ${n}`);
    }
  }
  // Before its field word with a topic particle, a value fills the box.
  assert.deepEqual(filled('김마지는 이름 칸에 적어줘', INTERVIEW), [[['p1-f01', 'p1-f19', 'p2-f09'], '김마지']]);
  assert.deepEqual(filled('다은을 이름 칸에 넣어줘', INTERVIEW), [[['p1-f01', 'p1-f19', 'p2-f09'], '다은']]);
});

test('a question about a field says so, not that a value was refused (review 4 M3)', () => {
  for (const text of ['연락처 뭐 적어야 해?', '학교 어디 적어야 해?', '학교 칸 어디야?']) {
    const fill = C.parseLocal(text, INTERVIEW);
    assert.deepEqual(fill.fields, [], text);
    assert.deepEqual(fill.notes, [C.FIELD_QUESTION_NOTE], text);
  }
});

test('a form that is loading, failed or one of several is still form context (review 4 M1, L3)', async () => {
  for (const held of ['loading', 'failed', 'several']) {
    const ctx = C.heldContext('p', held);
    const { where, sent } = await route('이름은 김마지로 적어줘', ctx);
    assert.equal(where, 'local');
    assert.deepEqual(sent, []);
    const fill = C.parseLocal('이름은 김마지로 적어줘', ctx);
    assert.deepEqual(fill.fields, []);
    assert.deepEqual(fill.notes, [C.HELD_NOTES[held]]);
    assert.equal((await route('알림 꺼줘', ctx)).where, 'server');
  }
});

const filled = (text, ctx) => C.parseLocal(text, ctx).fields.map(f => [f.ids, f.value]);

test('values map onto the production templates on this device', () => {
  assert.deepEqual(filled('이름은 김마지, 연락처는 010-1234-5678로 넣어줘', INTERVIEW), [
    [['p1-f01', 'p1-f19', 'p2-f09'], '김마지'],
    [['p1-f07'], '010-1234-5678'],
  ]);
  assert.deepEqual(filled('성명은 김마지로 적어줘', STUDY), [[['p1-f01', 'p1-f16', 'p3-f09', 'p4-f05'], '김마지']]);
  assert.deepEqual(filled('학교는 부산대, 학과는 경영학과로 써줘', PNU), [[['p1-f01'], '부산대'], [['p1-f02'], '경영학과']]);
  assert.deepEqual(filled('이름 옆에 연락처 010-1234-5678 넣어줘', INTERVIEW), [[['p1-f07'], '010-1234-5678']]);
  assert.deepEqual(filled('우편번호는 26464로 적어줘', INTERVIEW), [[['p1-f06'], '26464']]);
  // A date box gets the date in its own format.
  const birth = INTERVIEW.template.fields.find(f => f.id === 'p1-f03');
  assert.deepEqual(filled('생년월일 칸에 2000.01.02로 적어줘', INTERVIEW), [[['p1-f03'], F.formatDate({ y: 2000, m: 1, d: 2 }, birth.format)]]);
  // The value before its field word (review 3 H1).
  assert.deepEqual(filled('홍길동이라고 이름 칸에 적어줘', INTERVIEW), [[['p1-f01', 'p1-f19', 'p2-f09'], '홍길동']]);
  assert.deepEqual(filled('김마지를 이름 칸에 넣어줘', INTERVIEW), [[['p1-f01', 'p1-f19', 'p2-f09'], '김마지']]);
});

test('a short name keeps every syllable (review 3 M1, H2)', () => {
  for (const [text, value] of [['이름은 다은 적어줘', '다은'], ['이름 다은 적어줘', '다은'], ['이름 나은 써줘', '나은'], ['이름 이은 적어 줘', '이은'], ['이름은 하은 적어줘', '하은'], ['이름 하은 적어줘', '하은'], ['성함은 하은 써줘', '하은'], ['이름은 하은으로 적어줘', '하은'], ['이름은 써니라고 적어줘', '써니']]) {
    assert.deepEqual(filled(text, INTERVIEW).map(f => f[1]), [value], text);
  }
});

test('a value stops at a clause; anything else is reported, never guessed or echoed', () => {
  assert.deepEqual(filled('이름은 김마지 이고 우울증 있어요 적어줘', INTERVIEW).map(f => f[1]), ['김마지']);
  const long = C.parseLocal('학과는 정신과 치료 다시 받는 중 적어줘', PNU);
  assert.deepEqual(long.fields, []);
  assert.ok(long.notes.every(n => !n.includes('정신과') && !n.includes('치료')), long.notes.join(' / '));
  const none = C.parseLocal('학교는 부산대, 학과는 경영학과로 써줘', INTERVIEW);
  assert.deepEqual(none.fields, []);
  assert.ok(none.notes.some(n => n.includes('‘학교’ 칸이 없어')));
  // 작성일 printed as 년/월/일 part boxes: one date fills each part (the form helper's contract).
  const written = C.parseLocal('작성일은 2026.10.31로 적어줘', INTERVIEW);
  assert.deepEqual(written.notes, []);
  assert.deepEqual(C.localRows(written), ['작성일 (6칸): 2026.10.31']);
  assert.deepEqual(C.localValues(written), { 'p1-f16': '2026', 'p1-f17': '10', 'p1-f18': '31', 'p2-f05': '2026', 'p2-f06': '10', 'p2-f07': '31' });
  assert.ok(C.parseLocal('작성일은 김마지로 적어줘', INTERVIEW).notes.some(n => n.includes('날짜로 읽지 못해')));
  const unknown = C.parseLocal('질병 칸에 우울증이라고 적어줘', INTERVIEW);
  assert.deepEqual(unknown.fields, []);
  assert.ok(unknown.notes.every(n => !n.includes('우울증')));
});

test('주민번호, 계좌, 예금주, 서명, 도장 and number-like values are refused and never echoed (review 3 H3)', () => {
  const secrets = /900101|1234567|456789|구공공|공일|九|아홉|⑨|홍길동|김마지/;
  for (const text of [
    '주민번호는 900101-1234567로 적어줘', '계좌번호는 110-123-456789로 써줘', '예금주는 홍길동으로 적어줘', '예.금.주는 홍길동 적어줘',
    '서명 칸에 김마지 적어줘', '도장 칸에 김마지 넣어줘', '이름은 9공0101-1234567 적어줘', '주소는 900101--------1234567 적어줘',
    '주소는 900101    -    1234567 적어줘', '주소는 900101 끝 1234567 적어줘', '주소는 구공공 일공일 일이삼 사오육칠 적어줘',
    '주소는 일일공 일이삼 사오육칠팔구 적어줘', '주소는 九〇〇一〇一-一二三四五六七 적어줘', '주소는 아홉공공하나공하나 적어줘',
    '주소는 110 dash 123 dash 456789 적어줘', '이름은 ⑨⓪⓪①⓪①-①②③④⑤⑥⑦ 적어줘', '이름은 900101ㅡ1234567 적어줘', '이름은 구공공일공일 일이삼사오육칠 적어줘',
  ]) {
    const fill = C.parseLocal(text, INTERVIEW);
    assert.deepEqual(fill.fields, [], text);
    for (const n of fill.notes) assert.doesNotMatch(n, secrets, `${text}: ${n}`);
    for (const row of C.localRows(fill)) assert.doesNotMatch(row, secrets, text);
  }
});

test('the digit skeleton matches the server, and whole values follow per-kind rules (shared fixture)', () => {
  const fixture = JSON.parse(fs.readFileSync(path.join(VALIDATION, 'sensitive-digit-cases.json'), 'utf8'));
  for (const c of fixture.cases) assert.deepEqual(Dg.sensitiveRuns(c.text).map(r => r[2]), c.shapes, c.text);
  for (const v of fixture.values) assert.equal(Dg.valueProblem(v.kind, v.value), v.problem, `${v.kind}: ${v.value}`);
  for (const v of ['900101ㅡ1234567', '구공공일공일 일이삼사오육칠', '1234567890']) assert.ok(F.looksSensitive(v), v);
  for (const v of ['010-1234-5678', '2000.01.01', '홍길동']) assert.ok(!F.looksSensitive(v), v);
});

test('the local card waits for 승인; the hand-off is one account and one form, for a few minutes', () => {
  const fill = C.parseLocal('이름은 김마지, 연락처는 010-1234-5678로 넣어줘', INTERVIEW);
  const run = C.localRun(fill, 'local-1', new Date('2026-10-31T00:00:00Z'));
  assert.equal(run.confirmations[0].status, 'pending');
  assert.equal(run.message, null, 'the card says what is placed; no answer restates it');
  assert.ok(!JSON.stringify(run.confirmations).includes('김마지'), 'the card text names boxes, the rows show values');
  assert.equal(C.decideLocal(run, false).confirmations[0].status, 'rejected');
  assert.equal(C.decideLocal(run, true).confirmations[0].status, 'executed');
  // Nothing to place: no card, the notes in the message.
  const empty = C.localRun(C.parseLocal('병명 우울증', INTERVIEW), 'local-2');
  assert.deepEqual(empty.confirmations, []);
  assert.ok(empty.message?.answer, 'nothing placed: the notes say why');
  const t0 = 1_000_000;
  C.handChatValues('u1', 'p', 'r', C.localValues(fill), t0);
  assert.equal(C.chatValuesFor('u2', 'p', 'r', t0), null, 'another account');
  assert.equal(C.chatValuesFor('u1', 'p', 'other', t0), null, 'another form');
  assert.deepEqual(C.chatValuesFor('u1', 'p', 'r', t0 + 1000), { 'p1-f01': '김마지', 'p1-f19': '김마지', 'p2-f09': '김마지', 'p1-f07': '010-1234-5678' });
  assert.equal(C.chatValuesFor('u1', 'p', 'r', t0 + C.HAND_OFF_MS + 1), null, 'expired');
});

test('approved values go into the draft as proposals, never into sensitive boxes', () => {
  const ctx = { profile: null, remembered: {}, today: new Date('2026-10-31T03:00:00Z') };
  const fields = INTERVIEW.template.fields;
  const base = D.buildDraft(fields, ctx);
  const draft = D.applyChatValues(base, fields, { 'p1-f01': '김​마지', 'p1-f07': '010-1234-5678', 'p1-f20': '김마지', 'p1-f06': '9001011234567' });
  assert.deepEqual(draft.state['p1-f01'], { value: F.textValue('김마지'), status: 'suggested', sourceLabel: '비서에게 말한 값' });
  // Date parts go into the 년/월/일 boxes as each box prints them.
  const dated = D.applyChatValues(base, fields, C.localValues(C.parseLocal('작성일은 2026.10.31로 적어줘', INTERVIEW)));
  for (const [id, v] of [['p1-f16', '2026'], ['p1-f17', '10'], ['p1-f18', '31'], ['p2-f07', '31']]) assert.deepEqual(dated.state[id].value, F.textValue(v), id);
  assert.deepEqual(draft.state['p1-f20'], base.state['p1-f20'], 'a signature box');
  assert.deepEqual(draft.state['p1-f06'], base.state['p1-f06'], 'an rrn-shaped value');
  const rrnField = { id: 'r', page: 0, rect: { x: 0.1, y: 0.1, w: 0.3, h: 0.04 }, type: 'text', label: '주민등록번호', mapping: 'rrn', required: true };
  assert.deepEqual(D.applyChatValues(D.buildDraft([rrnField], ctx), [rrnField], { r: '홍길동' }).state.r, { value: null, status: 'empty' });
  assert.ok(F.progress(fields, draft.state).unconfirmed >= 2);
});

test('a field word qualified by another person or the holder never fills the applicant\'s box (review 5 M1)', () => {
  const names = /홍길동|박철수|홍판서|김영희/;
  for (const ctx of [INTERVIEW, STUDY, PNU]) for (const text of PROBE_FILE.other_person) {
    const fill = C.parseLocal(text, ctx);
    assert.deepEqual(fill.fields, [], text);
    for (const n of fill.notes) assert.doesNotMatch(n, names, `${text}: ${n}`);
  }
  // Several fields in one message are not qualifiers of each other.
  assert.deepEqual(filled('이름은 김마지 연락처는 010-1234-5678로 적어줘', INTERVIEW).map(f => f[1]), ['김마지', '010-1234-5678']);
});

test('before form context, any message with form intent that is not a pure command is held (review 5 M3)', async () => {
  for (const text of PROBE_FILE.outside_form) {
    const sent = []; const held = [];
    await C.dispatchMessage(text, null, false, { server: async t => { sent.push(t); }, local: () => assert.fail(text), hold: async t => { held.push(t); sent.push(C.FORM_GOAL); } });
    assert.deepEqual(held, [text], text);
    assert.deepEqual(sent, [C.FORM_GOAL], text);
  }
  for (const text of ['서류 뭐 필요해?', '장애인 장학금 찾아줘', '소득 기준 뭐야?', '기초수급자 장학금 찾아줘', '부산대 장학금 찾아줘', '이 혜택 신청 완료로 기록해줘']) {
    assert.equal(C.dictatesOutsideForm(text), false, text);
  }
});

test('hidden characters never pass as a command; journey goals are server goals; names never pass (review 5 L1, L5, point 2)', () => {
  const tags = [...'rrn 900101'].map(c => String.fromCodePoint(0xe0000 + c.codePointAt(0))).join('');
  assert.equal(C.commandOnly('알림 꺼줘' + tags), false);
  assert.equal(C.commandOnly('알림​ 꺼줘'), false);
  const { JOURNEY_GOALS } = require('../lib/agent.ts');
  for (const goal of Object.values(JOURNEY_GOALS)) assert.ok(COMMANDS.server_goals.includes(goal), goal);
  const given = ['서연', '하은', '다은', '지아', '도하', '민준', '서준', '지우', '하윤', '수아', '이은', '나은', '주은', '하니', '써니', '예준', '시우', '지호', '유나', '소이'];
  for (const name of [...given, ...given.map(g => `김${g}`), '부산대', '경영학과', '수급자', '장애인', '휴학']) {
    assert.equal(C.commandOnly(name), false, name);
    assert.equal(C.commandOnly(`${name}로 해줘`), false, name);
  }
});

