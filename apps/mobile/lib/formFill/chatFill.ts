/**
 * 서류 도우미 values said to the 비서, handled on this device.
 *
 * Principle: in form context, values never leave the device. While a form is
 * in context (the form helper is open, the 비서 is on a programme with a
 * template, or the last card opened one), an outgoing message that could
 * dictate a value is not sent: it is parsed here against the template the
 * device already holds, shown as a local confirm card, and only 승인 puts the
 * values into the draft, as suggestions. Nothing about it reaches the server.
 *
 * Fail closed (an allow-list, not a cue list): in form context a typed
 * message goes to the server only when it is one of the server's own fixed
 * goals, or when every word in it is from the closed command vocabulary in
 * chatCommands.json (tool nouns, request verbs, function words) and it holds
 * no field word, sensitive-context word, 칸/란, `:`/`=`, `@` or numeral. Any
 * other word (a name, a school, a diagnosis, a typo) may be a value, so the
 * message stays here, even when nothing in it can be matched to a box.
 */
import { classifyLabel, datePart, isOtherPerson, MAPPING_RULES, parseDate, formatDate, sensitiveKindOf, type FieldKind, type FormTemplate, type TemplateField } from './fields';
import { sensitiveRuns, valueProblem } from './digits';
import COMMANDS from './chatCommands.json';
import type { AgentConfirmation, AgentMessage, AgentRun } from '../agentTypes';

/** `held`: the programme has a form this device cannot parse against yet
 * (still loading, failed to load, or several forms): a message still stays
 * here, and the card says why nothing was filled. */
export interface FormContext { programId: string; resourceId: string; template: FormTemplate; held?: 'loading' | 'failed' | 'several' }

export const EMPTY_TEMPLATE: FormTemplate = { version: 1, content_hash: '', page_count: 1, source: 'admin', fields: [] };
/** A form context for a programme whose form is not in hand. */
export function heldContext(programId: string, held: NonNullable<FormContext['held']>): FormContext {
  return { programId, resourceId: '', template: EMPTY_TEMPLATE, held };
}
export const HELD_NOTES: Record<NonNullable<FormContext['held']>, string> = {
  loading: '서식 정보를 아직 불러오지 못해 보내지 않았어요. 잠시 후 다시 말해 주세요.',
  failed: '서식 정보를 불러오지 못해 보내지 않았어요. 작성 도우미에서 직접 입력해 주세요.',
  several: '이 혜택은 서식이 여러 개라 넣지 않았어요. 작성 도우미에서 서식을 열고 다시 말해 주세요.',
};

// ---------------------------------------------------------------------------
// Form context: the open form helper registers itself.
// ---------------------------------------------------------------------------

let openForm: FormContext | null = null;
/** The form helper registers the form it has open (and clears it on leaving). */
export function setOpenForm(ctx: FormContext | null): void { openForm = ctx; }
export function openFormContext(): FormContext | null { return openForm; }

// ---------------------------------------------------------------------------
// Words. Closed grammatical classes; field kinds come from the label rules.
// ---------------------------------------------------------------------------

type Meaning = { kind: FieldKind } | { label: string } | { refused: string } | { long: string };

/** Spoken field words the label rules do not read as a whole word. */
const CHAT_WORDS: Record<string, Meaning> = {
  성함: { kind: 'name' }, 생일: { kind: 'birth_date' }, 메일: { kind: 'email' }, 폰번호: { kind: 'phone' }, 집주소: { kind: 'address' },
  우편번호: { label: '우편번호' }, 성별: { label: '성별' }, 나이: { label: '나이' },
  사인: { refused: '서명' }, 인감: { refused: '도장' }, 통장번호: { refused: '계좌번호' },
  자기소개: { long: '자기소개' }, 자소서: { long: '자기소개' }, 지원동기: { long: '지원동기' }, 동기: { long: '지원동기' },
  사유: { long: '사유' }, 신청사유: { long: '사유' }, 계획: { long: '계획' }, 소감: { long: '소감' }, 포부: { long: '포부' },
};
const REFUSED_LABELS: Partial<Record<FieldKind, string>> = { rrn: '주민등록번호', account: '계좌번호', account_holder: '예금주', signature: '서명', stamp: '도장' };

/** Particles, 칸/란 markers and copulas a word may end with; longest first. */
const PARTICLES = ['칸에는', '란에는', '칸에', '칸은', '칸엔', '칸이', '란에', '란은', '칸', '란', '이라고', '라고', '이에요', '예요', '이요', '이고', '인데', '으로', '에는', '에도', '은요', '는요', '은', '는', '이', '가', '을', '를', '에', '로', '도', '요'];
const STRONG = new Set(['칸에는', '란에는', '칸에', '칸은', '칸엔', '칸이', '란에', '란은', '칸', '란', '은', '는', '이', '가', '을', '를', '은요', '는요']);
/** Particles a value may end with and lose ("부산대로", "홍길동이라고"). */
const VALUE_ENDS = new Set(['이라고', '라고', '으로', '로', '이에요', '예요', '이요', '이고', '인데', '요']);
const MARKER = /^(칸|란)(에는|에|은|엔|이)?$/;

/** Writing verbs: a stem and a short ending ("적어줘", "입력하세요", "남겨줘"). */
const WRITE_VERB = /^(적|써|쓰|넣|채우|채워|입력|기입|기재|작성|기록|표기|등록|올려|올리|남겨|남기|박아)(어|아|여|해|하|으|줘|주|세|요|고|면|야|도|서|놔|둬|는|은|을|게|래|자|기|ㄱ)*$/;
/** 하다 requests ("해줘", "해 주세요", "부탁해"). */
const DO_VERB = /^(해|하|부탁|ㄱㄱ)(줘|주|세|요|해|드|려|립|니|다|ㄱ)*$/;
const CONNECTOR = /^(그리고|하고|또|및|랑|이랑)$/;
/** A copula said as its own word ends a value ("김마지 이고 …"). */
const COPULA_WORD = /^(이고|이며|인데|이에요|예요|입니다|이요)$/;
const POSITION = /^(옆|옆칸|아래|아래칸|밑|위|다음)(에|에는)?$/;
const FORM_NOUN = /^(서류|서식|신청서|지원서|동의서|확인서|양식|도우미|소득란|신청서류)/;
const QUESTION_END = /(\?|나요|까|까요|니|냐|죠|어때|뭐야|뭐예요|어디야)$/;
const ASKING = /^(뭐|무엇|어디|어떻게|언제|몇|왜|무슨|어떤)/;

/** Field kinds read as a whole word ("학교", "계좌번호"; not "대학생"). */
const WHOLE_RULES = MAPPING_RULES.map(rule => ({ kind: rule.kind, pattern: new RegExp(`^(?:${rule.pattern.source})$`, 'i') }));
function wordKind(word: string): FieldKind | null {
  const whole = (w: string) => WHOLE_RULES.find(rule => rule.pattern.test(w))?.kind ?? null;
  const direct = whole(word);
  if (direct) return direct;
  for (const end of ['번호', '명', '란']) if (word.length > end.length && word.endsWith(end)) { const k = whole(word.slice(0, -end.length)); if (k) return k; }
  return null;
}

/** A word without punctuation inside it ("예.금.주" is 예금주) or invisible characters. */
const squash = (w: string) => w.replace(/[\p{Cf}.·\-_'"‘’“”]/gu, '');

function split(core: string): { stem: string; particle: string } {
  for (const p of PARTICLES) if (core.length > p.length && core.endsWith(p)) return { stem: core.slice(0, -p.length), particle: p };
  return { stem: core, particle: '' };
}

function meaningOf(stem: string): Meaning | null {
  const w = squash(stem).toLowerCase();
  if (!w) return null;
  if (CHAT_WORDS[w]) return CHAT_WORDS[w];
  const kind = wordKind(w);
  if (!kind) return null;
  return REFUSED_LABELS[kind] ? { refused: REFUSED_LABELS[kind] as string } : { kind };
}

interface Token { text: string; start: number; trail: string }

function tokenize(text: string): Token[] {
  const out: Token[] = [];
  const re = /[^\s,，;]+[,，;]?/gu;
  for (const m of text.normalize('NFC').matchAll(re)) {
    let word = m[0];
    // "학교:부산대", "병명=우울증": the field word keeps its colon.
    const colon = word.search(/[:=]/);
    const parts = colon > 0 && colon < word.length - 1 ? [word.slice(0, colon + 1), word.slice(colon + 1)] : [word];
    for (const part of parts) {
      let core = part.replace(/[=]$/, ':');
      let trail = '';
      while (/[,，;.!?~。]$/.test(core) && core.length > 1) { trail = core.endsWith('?') || trail === '?' ? '?' : core.slice(-1); core = core.slice(0, -1); }
      out.push({ text: core.replace(/\p{Cf}/gu, ''), start: m.index ?? 0, trail });
    }
  }
  return out;
}

type Role =
  | { role: 'field'; meaning: Meaning; strong: boolean; len: number }
  | { role: 'unknown'; len: number }
  | { role: 'swallowed' } | { role: 'marker' } | { role: 'verb' } | { role: 'do' } | { role: 'connector' } | { role: 'position' } | { role: 'form' } | { role: 'value' };

function roles(tokens: Token[], ctx: FormContext): Role[] {
  const out: Role[] = tokens.map(() => ({ role: 'value' }));
  const labels = ctx.template.fields.map(f => squash(f.label).replace(/\s/g, ''));
  const labelWord = (stem: string) => {
    const w = squash(stem);
    // A word of this template's own labels ("우편번호", "성별") is a field word too.
    return w.length >= 2 && labels.some(l => l === w || l.endsWith(w)) ? ({ label: w } as Meaning) : null;
  };
  for (let i = 0; i < tokens.length; i++) {
    const t = tokens[i].text;
    const prev = out[i - 1];
    // Right after a field word, a word is its value, whatever it spells ("하은", "다은").
    const afterField = i > 0 && !tokens[i - 1].trail && prev && (prev.role === 'field' || prev.role === 'unknown' || prev.role === 'swallowed');
    if (MARKER.test(t)) { out[i] = { role: 'marker' }; continue; }
    if (afterField && !WRITE_VERB.test(t) && !POSITION.test(t)) continue;
    if (WRITE_VERB.test(t)) { out[i] = { role: 'verb' }; continue; }
    if (DO_VERB.test(t)) { out[i] = { role: 'do' }; continue; }
    if (CONNECTOR.test(t)) { out[i] = { role: 'connector' }; continue; }
    if (POSITION.test(t)) { out[i] = { role: 'position' }; continue; }
    const colon = t.endsWith(':');
    const core = colon ? t.slice(0, -1) : t;
    const { stem, particle } = split(core);
    const meaning = meaningOf(core) ?? meaningOf(stem) ?? labelWord(stem) ?? labelWord(core);
    if (meaning) {
      const marker = !tokens[i].trail && i + 1 < tokens.length && MARKER.test(tokens[i + 1].text);
      out[i] = { role: 'field', meaning, strong: colon || marker || STRONG.has(particle), len: marker ? 2 : 1 };
      if (marker) { out[i + 1] = { role: 'swallowed' }; i++; }
      continue;
    }
    if (FORM_NOUN.test(squash(t))) { out[i] = { role: 'form' }; continue; }
    // An unknown field word: with a marker ("질병 칸에", "질병칸에"), a colon, or a topic at a clause start.
    const startOfClause = i === 0 || !!tokens[i - 1].trail || ['verb', 'do', 'connector', 'form', 'position'].includes(prev?.role ?? '');
    if (/칸에?$|란에?$/.test(t) && t.length > 2) { out[i] = { role: 'unknown', len: 1 }; continue; }
    if (!tokens[i].trail && i + 1 < tokens.length && MARKER.test(tokens[i + 1].text)) { out[i] = { role: 'unknown', len: 2 }; out[i + 1] = { role: 'swallowed' }; i++; continue; }
    // "홍길동은 예금주 칸에": a topic right before a field word is that field's value, not a field.
    const next = tokens[i + 1]?.text.replace(/:$/, '') ?? '';
    const nextIsField = !!next && !!(meaningOf(next) ?? meaningOf(split(next).stem) ?? labelWord(split(next).stem));
    if (colon || (startOfClause && (particle === '은' || particle === '는') && i + 1 < tokens.length && !tokens[i].trail && !nextIsField)) { out[i] = { role: 'unknown', len: 1 }; continue; }
  }
  return out;
}

/** The value words after the field word at `i`: up to its verb, a connector, the next field word or the clause end. */
function valueWords(tokens: Token[], rs: Role[], i: number, len: number): [number, number] {
  const from = i + len;
  if (tokens[from - 1]?.trail || rs[from]?.role === 'position') return [from, from];
  let to = from;
  while (to < tokens.length && !['verb', 'do', 'connector', 'field', 'unknown', 'marker', 'form'].includes(rs[to].role) && !COPULA_WORD.test(tokens[to].text)) {
    to++;
    if (tokens[to - 1].trail) break;
    if (['이고', '인데', '이며'].some(c => tokens[to - 1].text.endsWith(c))) break;
  }
  return [from, to];
}

// ---------------------------------------------------------------------------
// The decision: local or server.
// ---------------------------------------------------------------------------

const SERVER_GOALS = new Set<string>(COMMANDS.server_goals);
const TARGETS = new Set<string>(COMMANDS.targets);
const WORDS = new Set<string>(COMMANDS.words);
const COMMAND_PARTICLES = [...COMMANDS.particles].sort((a, b) => b.length - a.length);
const longestFirst = (list: string[]) => [...list].sort((a, b) => b.length - a.length).join('|');
const COMMAND_VERB = new RegExp(`^(${longestFirst(COMMANDS.stems)})((?:${longestFirst(COMMANDS.endings)})*)$`);
const NUMERAL = /[\p{N}〇零一二三四五六七八九十]/u;

/** The server's own fixed goals (journey turns, chips): a server-written
 * string holds no value, so it goes to the server even when typed. */
export function isServerGoal(text: string): boolean {
  return SERVER_GOALS.has(text.normalize('NFC').trim().replace(/\s+/g, ' '));
}

/** A word made only of tool nouns ("마감일", "신청완료"). */
function targetRun(word: string): boolean {
  if (!word) return false;
  const ok: boolean[] = [true];
  for (let end = 1; end <= word.length; end++) {
    ok[end] = false;
    for (let start = Math.max(0, end - 6); start < end - 1 && !ok[end]; start++) ok[end] = ok[start] && TARGETS.has(word.slice(start, end));
  }
  return ok[word.length];
}

/** A word of the closed command vocabulary. Function words match whole;
 * a one-syllable verb stem needs an ending ("열어줘", not "열"); a noun may
 * carry one particle. */
function commandWord(word: string): boolean {
  const w = word.normalize('NFC').replace(/\p{Cf}/gu, '');
  if (WORDS.has(w)) return true;
  const verb = COMMAND_VERB.exec(w);
  if (verb && ([...verb[1]].length > 1 || verb[2])) return true;
  if (targetRun(w)) return true;
  const particle = COMMAND_PARTICLES.find(p => w.length > p.length && w.endsWith(p));
  return !!particle && targetRun(w.slice(0, -particle.length));
}

/** A word that names a field kind or a sensitive context (병명, 소득, 수급). */
function fieldOrSensitive(word: string): boolean {
  const core = word.replace(/:$/, '');
  if (COMMANDS.sensitive_context.some(c => squash(core).includes(c))) return true;
  return !!(meaningOf(core) ?? meaningOf(split(core).stem));
}

/** Every word is closed command vocabulary, and nothing in it can be a value. */
export function commandOnly(text: string): boolean {
  // Checked text is sent text: invisible characters (a hidden payload) never pass.
  if (/[:=@]/.test(text) || NUMERAL.test(text) || /\p{Cf}/u.test(text)) return false;
  const tokens = tokenize(text);
  if (!tokens.length) return false;
  return tokens.every(t => !/(칸|란)(에|에는|은|엔|이)?$/.test(t.text) && !fieldOrSensitive(t.text) && commandWord(t.text));
}

/** In form context, whether this message stays on the device. */
export function staysLocal(text: string, ctx: FormContext | null): boolean {
  return !!ctx && !isServerGoal(text) && !commandOnly(text);
}

/** Outside form context, a message with form intent that is not a pure
 * command: held on this device, never sent. Form intent is a field word, a
 * form noun (서류, 신청서), a sensitive-context word used as a label
 * ("병명은 …", "병명: …"), a 칸/란 marker, `:`/`=`, `@` or an rrn/account
 * shape. A pure command (`commandOnly`) is never held, so "서류 뭐 필요해?" and
 * "장애인 장학금 찾아줘" still reach the 비서. The same allow-list as in form
 * context, with form intent as the trigger instead of a verb list. */
export function dictatesOutsideForm(text: string): boolean {
  if (commandOnly(text)) return false;
  if (/\S[:=]/.test(text) || text.includes('@') || /\p{Cf}/u.test(text) || sensitiveRuns(text).length) return true;
  return tokenize(text).some(t => {
    const core = t.text.replace(/:$/, '');
    const { stem, particle } = split(core);
    if (meaningOf(core) ?? meaningOf(stem)) return true;
    if (FORM_NOUN.test(squash(core)) || /(칸|란)(에|에는|은|엔|이)?$/.test(core)) return true;
    // A sensitive-context word as the label of a statement: "병명은 우울증이야".
    return COMMANDS.sensitive_context.includes(squash(stem)) && ['은', '는', '이', '가'].includes(particle);
  });
}

/** The fixed goal sent in place of a held dictation (compose.rs FORM_GOAL). */
export const FORM_GOAL = '이 혜택 서식 작성 도와줘';

/**
 * Where a message goes. A chip or a server goal: the server. In form
 * context: the server only for a command (see `commandOnly`), else this
 * device. Outside form context: the server, except a dictation, which is held
 * on this device (`hold`) while the server gets the fixed FORM_GOAL.
 */
export async function dispatchMessage<T>(text: string, ctx: FormContext | null, chip: boolean, send: { server: (text: string) => Promise<T>; local: (fill: LocalFill) => T; hold?: (text: string) => Promise<T> }): Promise<T> {
  if (chip) return send.server(text);
  if (ctx) return staysLocal(text, ctx) ? send.local(parseLocal(text, ctx)) : send.server(text);
  if (send.hold && dictatesOutsideForm(text)) return send.hold(text);
  return send.server(text);
}

// A dictation said before any form was in context: kept in memory for one
// account until the form's card arrives, then parsed here. Never sent.
let heldText: { userId: string; text: string; at: number } | null = null;
export function holdDictation(userId: string, text: string, now = Date.now()): void { heldText = { userId, text, at: now }; }
/** The held dictation for this account, once (it is taken). */
export function takeHeldDictation(userId: string, now = Date.now()): string | null {
  const held = heldText;
  if (!held || held.userId !== userId || now - held.at > HAND_OFF_MS) { if (held && now - held.at > HAND_OFF_MS) heldText = null; return null; }
  heldText = null;
  return held.text;
}

// ---------------------------------------------------------------------------
// The local parse: field → value against the template on this device.
// ---------------------------------------------------------------------------

/** `values`: per-box values when they differ, e.g. the 년/월/일 parts of one date. */
export interface LocalField { ids: string[]; label: string; value: string; values?: Record<string, string> }
export interface LocalFill { programId: string; resourceId: string; fields: LocalField[]; notes: string[] }

const MAX_WORDS: Partial<Record<FieldKind, number>> = { name: 2, name_en: 3, school: 3, major: 3, address: 8, email: 1, student_id: 1, grade: 2, bank: 2 };

function fillable(field: TemplateField): boolean {
  return (field.type === 'text' || field.type === 'date') && !sensitiveKindOf(field) && !isOtherPerson(field.label) && !field.group;
}

function targets(meaning: Meaning, fields: TemplateField[]): TemplateField[] | 'no_field' | 'ambiguous' {
  if ('kind' in meaning) {
    const mapped = fields.filter(f => fillable(f) && f.mapping === meaning.kind);
    if (mapped.length) return mapped;
    const labelled = fields.filter(f => fillable(f) && !f.mapping && classifyLabel(f.label) === meaning.kind);
    return labelled.length === 1 ? labelled : labelled.length ? 'ambiguous' : 'no_field';
  }
  if ('label' in meaning) {
    const hits = fields.filter(f => fillable(f) && !f.mapping && squash(f.label).replace(/\s/g, '').includes(meaning.label));
    return hits.length === 1 ? hits : hits.length ? 'ambiguous' : 'no_field';
  }
  return 'no_field';
}

const display = (m: Meaning) => 'kind' in m ? ({ name: '이름', name_en: '영문 이름', birth_date: '생년월일', phone: '연락처', address: '주소', email: '이메일', school: '학교', major: '학과', grade: '학년', student_id: '학번', application_date: '신청일', bank: '은행' } as Partial<Record<FieldKind, string>>)[m.kind] ?? '그 칸' : 'label' in m ? m.label : 'refused' in m ? m.refused : m.long;

/** The value text: the words, without quotes and one value-ending particle. A
 * one- or two-syllable value keeps its last syllable unless it is a value end
 * ("다은", "하은" stay whole; "하은이라고" is "하은"). */
function valueText(tokens: Token[], from: number, to: number, beforeField = false): string {
  const words = tokens.slice(from, to).map(t => t.text);
  let last = words[words.length - 1].replace(/^['"‘“]+|['"’”]+$/g, '');
  const { stem, particle } = split(last);
  const syllables = [...stem].length;
  // Before a field word the last particle is grammar, never part of the value ("다은을 이름 칸에").
  if (beforeField && /^(이라고|라고|을|를|은|는)$/.test(particle)) last = stem;
  else if (particle && (VALUE_ENDS.has(particle) ? syllables >= 1 : syllables >= 3 && STRONG.has(particle))) last = stem;
  words[words.length - 1] = last;
  words[0] = words[0].replace(/^['"‘“]+/, '');
  return words.join(' ').trim();
}

export const UNKNOWN_FIELD_NOTE = '말한 칸을 이 서식에서 찾지 못해 넣지 않았어요.';
export const SENSITIVE_NOTE = '주민등록번호·계좌번호처럼 보이는 숫자는 채팅으로 받지 않아요. 작성 도우미에서 직접 입력해 주세요.';
export const QUALIFIED_FIELD_NOTE = '누구의 칸인지 확실하지 않아 넣지 않았어요. 보호자·추천인 같은 다른 사람 칸은 작성 도우미에서 직접 적어 주세요.';
export const SENSITIVE_FIELD_NOTE = '주민등록번호·계좌·예금주·서명·도장은 채팅으로 받지 않아요. 작성 도우미에서 직접 입력해 주세요.';
export const FIELD_QUESTION_NOTE = '서식 칸 질문은 작성 도우미에서 칸을 눌러 확인해 주세요.';
const NO_VALUE_NOTE = '서식 칸과 값을 찾지 못했어요. "학교는 부산대로 적어줘"처럼 칸 이름과 값을 함께 말해 주세요.';
const NO_VALUE_ROLES = ['field', 'swallowed', 'marker', 'verb', 'do', 'position', 'form', 'connector'];

export function parseLocal(text: string, ctx: FormContext): LocalFill {
  const tokens = tokenize(text);
  const rs = roles(tokens, ctx);
  const fields = ctx.template.fields;
  const out: LocalFill = { programId: ctx.programId, resourceId: ctx.resourceId, fields: [], notes: [] };
  if (ctx.held || !ctx.resourceId) { out.notes.push(HELD_NOTES[ctx.held ?? 'loading']); return out; }
  // Only field words and command words ("학교 칸 어디야?", "연락처 뭐 적어야 해?"): no value was said.
  if (rs.some(r => r.role === 'field') && tokens.every((t, i) => NO_VALUE_ROLES.includes(rs[i].role) || commandWord(t.text))) {
    const asks = QUESTION_END.test(text.trim()) || tokens.some(t => ASKING.test(t.text));
    out.notes.push(asks ? FIELD_QUESTION_NOTE : NO_VALUE_NOTE);
    return out;
  }
  const used = new Map<string, number>();
  const note = (n: string) => { if (!out.notes.includes(n)) out.notes.push(n); };
  for (let i = 0; i < tokens.length; i++) {
    const r = rs[i];
    if (r.role !== 'field' && r.role !== 'unknown') continue;
    let [from, to] = valueWords(tokens, rs, i, r.len);
    let before = false;
    // The value before the field word: "홍길동이라고 이름 칸에 적어줘", "김마지를 이름 칸에 넣어줘", "홍길동은 예금주 칸에".
    if (from === to && i > 0 && rs[i - 1].role === 'value' && /(이라고|라고|을|를|은|는)$/.test(tokens[i - 1].text) && ['verb', 'do', undefined].includes(rs[i + r.len]?.role)) {
      to = i; from = i - 1; before = true;
      while (from > 0 && rs[from - 1].role === 'value' && !tokens[from - 1].trail) from--;
    }
    if (r.role === 'unknown') {
      // Never echo the word: it may be a value said as a topic ("900101-1234567은 여기").
      const word = split(tokens[i].text.replace(/:$/, '')).stem;
      note(valueProblem(null, word) === 'sensitive' ? SENSITIVE_NOTE : UNKNOWN_FIELD_NOTE);
      continue;
    }
    // A field word qualified by bare nouns at the clause start ("보호자 성명은",
    // "통장 주인 이름은"): another person's box, the holder's, or unclear.
    // It never binds to the applicant's boxes.
    let q = i;
    // (A value said before its field, "김마지를 이름 칸에", is not a qualifier.)
    while (!before && q > 0 && rs[q - 1].role === 'value' && !tokens[q - 1].trail) q--;
    const clauseStart = q === 0 || !!tokens[q - 1].trail || ['verb', 'do', 'connector', 'form', 'position'].includes(rs[q - 1].role);
    if (q < i && clauseStart) {
      const spoken = tokens.slice(q, i + 1).map(t => split(t.text.replace(/:$/, '')).stem).join('');
      note(sensitiveKindOf({ label: spoken, mapping: null }) ? SENSITIVE_FIELD_NOTE : QUALIFIED_FIELD_NOTE);
      continue;
    }
    const m = r.meaning;
    if ('refused' in m) { note(`${m.refused}은(는) 채팅으로 받지 않아요. 작성 도우미에서 직접 입력해 주세요.`); continue; }
    if ('long' in m) { note(`${m.long}은(는) 긴 글이라 비서가 대신 쓰지 않아요. 작성 도우미에서 직접 적어 주세요.`); continue; }
    if (from === to) continue;
    if (!r.strong && to - from > 1 && !/(이라고|라고|으로|로)$/.test(tokens[to - 1].text)) continue;
    const value = valueText(tokens, from, to, before);
    if (!value) continue;
    const kind = 'kind' in m ? m.kind : null;
    const problem = valueProblem(kind, value);
    if (problem === 'sensitive') { note(SENSITIVE_NOTE); continue; }
    if (to - from > (kind ? MAX_WORDS[kind] ?? 3 : 3) || problem === 'shape') { note(`‘${display(m)}’에 말한 내용이 칸에 맞지 않아 넣지 않았어요. 작성 도우미에서 직접 적어 주세요.`); continue; }
    const boxes = targets(m, fields);
    if (boxes === 'no_field') { note(`이 서식에는 ‘${display(m)}’ 칸이 없어 넣지 않았어요.`); continue; }
    if (boxes === 'ambiguous') { note(`‘${display(m)}’ 칸이 여러 개라 넣지 않았어요. 작성 도우미에서 직접 골라 주세요.`); continue; }
    let final = value;
    let perBox: Record<string, string> | undefined;
    // A date: a date box gets it in its format; 년/월/일 part boxes each get their part
    // (the same contract as the form helper's 작성일 suggestion).
    if (boxes.some(b => b.type === 'date' || b.part)) {
      const parts = parseDate(value);
      if (!parts) { note(`‘${display(m)}’은(는) 날짜로 읽지 못해 넣지 않았어요.`); continue; }
      final = formatDate(parts, boxes.find(b => b.type === 'date')?.format);
      if (boxes.some(b => b.part)) perBox = Object.fromEntries(boxes.map(b => [b.id, b.part ? datePart(parts, b.part) : formatDate(parts, b.format)]));
    }
    const ids = boxes.map(b => b.id);
    if (ids.some(id => used.has(id))) {
      const k = used.get(ids.find(id => used.has(id)) as string) as number;
      out.fields.splice(k, 1);
      note(`‘${display(m)}’에 값이 두 번 나와 넣지 않았어요.`);
      continue;
    }
    ids.forEach(id => used.set(id, out.fields.length));
    const label = boxes.every(b => b.part) ? boxes[0].label.replace(/\s*(연도|년도|년|월|일)$/, '') || display(m) : boxes[0].label;
    out.fields.push(perBox ? { ids, label, value: final, values: perBox } : { ids, label, value: final });
  }
  if (!out.fields.length && !out.notes.length) note(NO_VALUE_NOTE);
  return out;
}

/** The card's rows: "인적사항 성명: 김마지". */
export function localRows(fill: LocalFill): string[] {
  return fill.fields.map(f => `${f.label}${f.ids.length > 1 ? ` (${f.ids.length}칸)` : ''}: ${f.value}`);
}

/** Values for the draft, by box id. */
export function localValues(fill: LocalFill): Record<string, string> {
  const out: Record<string, string> = {};
  for (const f of fill.fields) for (const id of f.ids) out[id] = f.values?.[id] ?? f.value;
  return out;
}

// ---------------------------------------------------------------------------
// In-memory hand-off to the form helper screen: one form, one account,
// a few minutes (another account on the device never reads it). Never stored.
// ---------------------------------------------------------------------------

export const HAND_OFF_MS = 5 * 60_000;
let pending: { key: string; values: Record<string, string>; at: number } | null = null;
const keyOf = (userId: string, programId: string, resourceId: string) => `${userId}:${programId}:${resourceId}`;

export function handChatValues(userId: string, programId: string, resourceId: string, values: Record<string, string>, now = Date.now()): void {
  pending = { key: keyOf(userId, programId, resourceId), values, at: now };
}
/** The values for this account's form while the hand-off is fresh. Reading
 * does not consume (a screen may render twice); a new hand-off or the time
 * limit ends it. */
export function chatValuesFor(userId: string, programId: string, resourceId: string, now = Date.now()): Record<string, string> | null {
  if (!pending) return null;
  if (now - pending.at > HAND_OFF_MS) { pending = null; return null; }
  return pending.key === keyOf(userId, programId, resourceId) ? pending.values : null;
}

// ---------------------------------------------------------------------------
// The local turn, shaped like a run so the 비서 screen draws it with its own
// cards. It never reaches the server.
// ---------------------------------------------------------------------------

export const LOCAL_PREFIX = 'local-';

export function localRun(fill: LocalFill, id: string, now: Date = new Date()): AgentRun {
  const labels = [...new Set(fill.fields.map(f => f.label))];
  // The card says what is placed; the answer only says what was not (its notes).
  const message: AgentMessage | null = fill.notes.length ? {
    answer: fill.notes.join(' '),
    verdict: 'not_evaluated', citations: [], suggested_actions: [], missing_profile_fields: [], warnings: [],
  } : null;
  const card: AgentConfirmation = {
    id: `${id}-card`, tool: 'open_form_fill', program_id: fill.programId,
    title: '서류 작성 도우미', action: `대화에서 말한 값 넣기: ${labels.join(', ')}`,
    before: '작성 전', after: '말한 값을 미리 넣음 (칸마다 확인해야 반영, 제출은 직접)', reversible: true,
    payload_hash: '', expires_at: new Date(now.getTime() + HAND_OFF_MS).toISOString(), status: 'pending',
  };
  return {
    id, conversation_id: '', status: fill.fields.length ? 'awaiting_confirmation' : 'completed', planner: null, goal_kind: null,
    steps: [], results: [], message, confirmations: fill.fields.length ? [card] : [], error_code: null,
    created_at: now.toISOString(), completed_at: fill.fields.length ? null : now.toISOString(),
  };
}

/** The local run after 승인 or 취소. */
export function decideLocal(run: AgentRun, approve: boolean, now: Date = new Date()): AgentRun {
  return {
    ...run, status: 'completed', completed_at: now.toISOString(),
    confirmations: run.confirmations.map(c => ({ ...c, status: approve ? 'executed' : 'rejected' })),
  };
}
