/**
 * 서류 작성 도우미 — field model, label mapping, suggestions and progress.
 *
 * Pure logic (no React Native imports) so node tests exercise it directly.
 * Rules this module enforces:
 * - A suggestion is only ever a proposal: it starts as `suggested`, is never
 *   drawn until the user confirms or edits it, and a required box cannot be
 *   finished before that.
 * - Sensitive kinds (주민등록번호, 계좌번호, 예금주) are never suggested, never
 *   remembered and are cleared from memory after export. Only the numeric
 *   ones (주민등록번호, 계좌번호) are typed as hidden digits.
 * - Fields about another person (보호자, 부모 …) get no suggestion.
 */

import { sensitiveRuns } from './digits';
import { cleanSignature, type Signature } from './signature';
export type { Signature, Stroke } from './signature';

export type FieldType = 'text' | 'multiline' | 'date' | 'checkbox' | 'signature' | 'stamp';
export type FieldKind =
  | 'name' | 'name_en' | 'birth_date' | 'phone' | 'address' | 'email'
  | 'school' | 'major' | 'grade' | 'student_id' | 'application_date'
  | 'rrn' | 'account' | 'bank' | 'account_holder' | 'signature' | 'stamp';
export type DateFormat = 'YYYY.MM.DD' | 'YYYY-MM-DD' | 'YYYY. MM. DD.' | 'YYYY년 MM월 DD일' | 'YYMMDD';
/** One part of a date printed as separate blanks. `year2`: the form prints '20' before the
 * blank ('20  년'), so the box takes the last two digits. Twin of DATE_PARTS in crates/core. */
export type DatePart = 'year2' | 'year4' | 'month' | 'day';
export const DATE_PARTS: readonly DatePart[] = ['year2', 'year4', 'month', 'day'];

export const FIELD_TYPES: readonly FieldType[] = ['text', 'multiline', 'date', 'checkbox', 'signature', 'stamp'];
export const FIELD_KINDS: readonly FieldKind[] = ['name', 'name_en', 'birth_date', 'phone', 'address', 'email', 'school', 'major', 'grade', 'student_id', 'application_date', 'rrn', 'account', 'bank', 'account_holder', 'signature', 'stamp'];
export const DATE_FORMATS: readonly DateFormat[] = ['YYYY.MM.DD', 'YYYY-MM-DD', 'YYYY. MM. DD.', 'YYYY년 MM월 DD일', 'YYMMDD'];
/** Never prefilled. 예금주 too: the account may be a guardian's, so the user's name is a guess. */
export const SENSITIVE_KINDS: readonly FieldKind[] = ['rrn', 'account', 'account_holder'];
/** Sensitive kinds typed as hidden digits. 예금주 is a name, so it keeps the normal keyboard. */
export const NUMERIC_SECRET_KINDS: readonly FieldKind[] = ['rrn', 'account'];
/** The only values that may be remembered on this device, and only with opt-in. */
export const REMEMBERABLE_KINDS = ['name', 'phone', 'address', 'email'] as const;
export type RememberKey = typeof REMEMBERABLE_KINDS[number];
export type Remembered = Partial<Record<RememberKey, string>>;

export const KIND_LABELS: Record<FieldKind, string> = {
  name: '성명', name_en: '영문 성명', birth_date: '생년월일', phone: '연락처', address: '주소', email: '이메일',
  school: '학교명', major: '학과', grade: '학년', student_id: '학번', application_date: '신청일',
  rrn: '주민등록번호', account: '계좌번호', bank: '은행명', account_holder: '예금주', signature: '서명', stamp: '도장(인)',
};

/** Fractions of the page crop box, origin top-left, unrotated page. */
export interface Rect { x: number; y: number; w: number; h: number }
export interface TemplateField {
  id: string; page: number; rect: Rect; type: FieldType; label: string;
  mapping: FieldKind | null; required: boolean; format?: DateFormat; font_size?: number;
  /** Original AcroForm field name when imported from the PDF itself. */
  acro_name?: string;
  /** FieldGroup id: this checkbox is one option of that question. */
  group?: string;
  /** A text box holding one part of a date ('20 [26] 년 [10] 월'). */
  part?: DatePart;
}
/** Checkbox options answering one question: `single` keeps one tick, `multi` any number. */
export type GroupMode = 'single' | 'multi';
export interface FieldGroup { id: string; mode: GroupMode; label: string }
export const MAX_GROUPS = 100;
export interface FormTemplate {
  version: 1; content_hash: string; page_count: number; source: 'admin' | 'acroform'; fields: TemplateField[];
  /** Absent when the template has none (servers never send an empty list). */
  groups?: FieldGroup[];
  /** Official file is HWP/HWPX: content_hash is the PDF Mazimi converted from the file hashing to source_sha256. */
  derived?: { source_sha256: string; converter: string };
}

// ---------------------------------------------------------------------------
// Label → kind mapping. Order matters: the first match wins.
// ---------------------------------------------------------------------------

export const MAPPING_RULES: readonly { kind: FieldKind; pattern: RegExp }[] = [
  // '주민등록(상) 주소' is an address, not the registration number.
  { kind: 'rrn', pattern: /주민(등록)?번호|주민등록(?!.*(주소|거주지))|외국인등록번호|residentregistration|residentno|\brrn\b|jumin/i },
  { kind: 'account_holder', pattern: /예금주|accountholder/i },
  { kind: 'bank', pattern: /은행(명)?|금융기관|bankname|^bank$/i },
  { kind: 'account', pattern: /계좌|accountno|accountnumber|\bacct/i },
  { kind: 'signature', pattern: /서명|signature|^sign$/i },
  { kind: 'stamp', pattern: /\(인\)|날인|도장|직인|seal|stamp/i },
  { kind: 'application_date', pattern: /신청(일|일자|날짜)|작성(일|일자)|제출(일|일자)|접수일|applicationdate|applydate|dateofapplication/i },
  { kind: 'birth_date', pattern: /생년월일|출생일|birth|dob/i },
  { kind: 'email', pattern: /e-?mail|이메일|전자우편|메일주소/i },
  { kind: 'phone', pattern: /휴대(폰|전화)|핸드폰|연락처|전화(번호)?|mobile|phone|\btel\b|cellphone/i },
  { kind: 'address', pattern: /주소|거주지|소재지|address/i },
  { kind: 'student_id', pattern: /학번|studentid|studentno/i },
  { kind: 'major', pattern: /학과|전공|학부|major|department|\bdept/i },
  { kind: 'grade', pattern: /학년|grade|yearlevel/i },
  { kind: 'school', pattern: /학교|대학(교)?명?|재학교|school|university|college/i },
  { kind: 'name_en', pattern: /영문(성명|이름)|englishname|nameinenglish/i },
  { kind: 'name', pattern: /성명|이름|신청인|신청자|^name$|fullname|applicantname/i },
];

/** A field about someone else: the user's own data is never suggested there. */
const OTHER_PERSON = /보호자|부모|부친|모친|아버지|어머니|배우자|대리인|추천인|담당자|교수|지도교사|가구원|guardian|parent|father|mother|spouse|agent|referee/i;

export function classifyLabel(label: string): FieldKind | null {
  const text = label.replace(/[\s_]+/g, '');
  if (!text) return null;
  return MAPPING_RULES.find(rule => rule.pattern.test(text))?.kind ?? null;
}
export function isOtherPerson(label: string): boolean {
  return OTHER_PERSON.test(label.replace(/[\s_]+/g, ''));
}
/** Sensitive if either the admin mapping or the label says so: a label "계좌번호" mapped to something else stays sensitive. */
export function isSensitive(field: Pick<TemplateField, 'mapping' | 'label'>): boolean {
  return sensitiveKindOf(field) !== null;
}
/** The sensitive kind the mapping or the label names (mapping first), or null. */
export function sensitiveKindOf(field: Pick<TemplateField, 'mapping' | 'label'>): FieldKind | null {
  if (field.mapping && SENSITIVE_KINDS.includes(field.mapping)) return field.mapping;
  const byLabel = classifyLabel(field.label);
  return byLabel && SENSITIVE_KINDS.includes(byLabel) ? byLabel : null;
}
/** Hidden-digit input: either the mapping or the label names 주민등록번호 or 계좌번호. */
export function isNumericSecret(field: Pick<TemplateField, 'mapping' | 'label'>): boolean {
  const byLabel = classifyLabel(field.label);
  return (!!field.mapping && NUMERIC_SECRET_KINDS.includes(field.mapping)) || (!!byLabel && NUMERIC_SECRET_KINDS.includes(byLabel));
}
export function defaultTypeFor(kind: FieldKind | null): FieldType {
  if (kind === 'signature') return 'signature';
  if (kind === 'stamp') return 'stamp';
  if (kind === 'birth_date' || kind === 'application_date') return 'date';
  return 'text';
}

// ---------------------------------------------------------------------------
// Values
// ---------------------------------------------------------------------------

export const MAX_VALUE_LENGTH = 500;
/** Text values only: removes control characters (newlines kept for multi-line fields) and caps length. */
export function cleanValue(value: string, type: FieldType): string {
  const stripped = type === 'multiline' ? value.replace(/[\u0000-\u0009\u000B-\u001F\u007F]/g, '') : value.replace(/[\u0000-\u001F\u007F]/g, ' ');
  return stripped.slice(0, MAX_VALUE_LENGTH);
}

export interface DateParts { y: number; m: number; d: number }
export function isValidDate({ y, m, d }: DateParts): boolean {
  if (!Number.isInteger(y) || !Number.isInteger(m) || !Number.isInteger(d) || y < 1900 || y > 2100 || m < 1 || m > 12 || d < 1) return false;
  return d <= new Date(Date.UTC(y, m, 0)).getUTCDate();
}
export function formatDate(parts: DateParts, format: DateFormat = 'YYYY.MM.DD'): string {
  if (!isValidDate(parts)) throw new Error('날짜를 다시 확인해 주세요.');
  const y = String(parts.y), m = String(parts.m).padStart(2, '0'), d = String(parts.d).padStart(2, '0');
  switch (format) {
    case 'YYYY-MM-DD': return `${y}-${m}-${d}`;
    case 'YYYY. MM. DD.': return `${y}. ${m}. ${d}.`;
    case 'YYYY년 MM월 DD일': return `${y}년 ${m}월 ${d}일`;
    case 'YYMMDD': return `${y.slice(2)}${m}${d}`;
    default: return `${y}.${m}.${d}`;
  }
}
/** Reads back any supported format (YYMMDD assumes 19xx/20xx by proximity to today). */
export function parseDate(value: string, today = new Date()): DateParts | null {
  const digits = value.match(/\d+/g) ?? [];
  let parts: DateParts | null = null;
  if (digits.length === 3) parts = { y: Number(digits[0]), m: Number(digits[1]), d: Number(digits[2]) };
  else if (digits.length === 1 && digits[0].length === 8) parts = { y: Number(digits[0].slice(0, 4)), m: Number(digits[0].slice(4, 6)), d: Number(digits[0].slice(6)) };
  else if (digits.length === 1 && digits[0].length === 6) {
    const yy = Number(digits[0].slice(0, 2)), current = seoulDate(today).y % 100;
    parts = { y: (yy > current ? 1900 : 2000) + yy, m: Number(digits[0].slice(2, 4)), d: Number(digits[0].slice(4)) };
  }
  if (parts && parts.y < 100) parts.y += 2000;
  return parts && isValidDate(parts) ? parts : null;
}
/** The digits a date-part box holds: '26' for year2 in 2026, '2026', '10', '05'. */
export function datePart(parts: DateParts, part: DatePart): string {
  switch (part) {
    case 'year2': return String(parts.y % 100).padStart(2, '0');
    case 'year4': return String(parts.y);
    case 'month': return String(parts.m).padStart(2, '0');
    default: return String(parts.d).padStart(2, '0');
  }
}
/** What the user typed into a date-part box, as the box prints it ('2026' -> '26' in a year2
 * box); unchanged when it is not a plausible value for that part. */
export function normalizePart(value: string, part: DatePart): string {
  const digits = value.replace(/\D+/g, '');
  if (!digits) return value.trim();
  if (part === 'year2') return digits.length === 4 && digits.startsWith('20') ? digits.slice(2) : digits.length <= 2 ? digits.padStart(2, '0') : value.trim();
  if (part === 'year4') return digits.length === 2 ? `20${digits}` : digits;
  return digits.length <= 2 ? digits.padStart(2, '0') : value.trim();
}
/** Calendar date in Korea. Forms are dated in KST regardless of device time zone. */
export function seoulDate(now: Date): DateParts {
  const kst = new Date(now.getTime() + 9 * 3600_000);
  return { y: kst.getUTCFullYear(), m: kst.getUTCMonth() + 1, d: kst.getUTCDate() };
}

// ---------------------------------------------------------------------------
// Suggestions — proposals the user must confirm, never silent fills.
// ---------------------------------------------------------------------------

export interface SuggestionContext {
  profile?: { school_name?: string | null; school_year?: number | null; birth_year?: number | null } | null;
  remembered: Remembered;
  today: Date;
}
export interface Suggestion { value: string; source: 'profile' | 'remembered' | 'today'; sourceLabel: string }

export function suggest(field: TemplateField, ctx: SuggestionContext): Suggestion | null {
  const kind = field.mapping;
  if (!kind || field.type === 'checkbox' || field.type === 'signature' || isSensitive(field) || isOtherPerson(field.label)) return null;
  // A seal only ever takes a valid seal name; a date box only a date.
  if (field.type === 'stamp') return kind === 'name' || kind === 'stamp' ? stampSuggestion(ctx) : null;
  if (field.type === 'date' && kind !== 'application_date') return null;
  const remembered = (key: RememberKey): Suggestion | null => {
    const value = ctx.remembered[key]?.trim();
    return value ? { value: cleanValue(value, field.type), source: 'remembered', sourceLabel: '이 기기에 기억한 정보' } : null;
  };
  switch (kind) {
    case 'name': return remembered('name');
    case 'stamp': return null; // a non-stamp box mapped to stamp gets no value
    case 'phone': return remembered('phone');
    case 'address': return remembered('address');
    case 'email': return remembered('email');
    case 'school': return ctx.profile?.school_name ? { value: cleanValue(ctx.profile.school_name, field.type), source: 'profile', sourceLabel: '마지미 프로필' } : null;
    case 'grade': return ctx.profile?.school_year ? { value: String(ctx.profile.school_year), source: 'profile', sourceLabel: '마지미 프로필' } : null;
    case 'application_date': return { value: field.part ? datePart(seoulDate(ctx.today), field.part) : formatDate(seoulDate(ctx.today), field.format), source: 'today', sourceLabel: '오늘 날짜' };
    default: return null;
  }
}
function stampSuggestion(ctx: SuggestionContext): Suggestion | null {
  const name = ctx.remembered.name?.replace(/\s+/g, '');
  return name && isStampName(name) ? { value: name, source: 'remembered', sourceLabel: '이 기기에 기억한 정보' } : null;
}
/** Hint shown under a field that has no suggestion. Never a value. */
export function hintFor(field: TemplateField, ctx: SuggestionContext): string | null {
  if (field.mapping && SENSITIVE_KINDS.includes(field.mapping)) return '작성할 때만 입력해요. 저장하지 않고, 파일을 만든 뒤 바로 지워요.';
  if (isOtherPerson(field.label)) return '본인이 아닌 분의 정보예요. 직접 입력해 주세요.';
  if (field.mapping === 'birth_date' && ctx.profile?.birth_year) return `프로필에는 출생 연도(${ctx.profile.birth_year}년)만 있어요. 월·일을 포함해 입력해 주세요.`;
  return null;
}
export function isStampName(value: string): boolean {
  return /^[가-힣A-Za-z]{1,4}$/.test(value);
}

/** Copy sheet for forms the app cannot fill (HWP, flat PDF): common fields with the user's values. */
export const GUIDED_KINDS: readonly FieldKind[] = ['name', 'birth_date', 'phone', 'address', 'email', 'school', 'major', 'grade', 'student_id', 'application_date', 'rrn', 'account'];
export interface GuidedRow { kind: FieldKind; label: string; value: string | null; sourceLabel?: string; hint: string | null; sensitive: boolean }
export function guidedRows(ctx: SuggestionContext): GuidedRow[] {
  return GUIDED_KINDS.map(kind => {
    const field: TemplateField = { id: kind, page: 0, rect: { x: 0, y: 0, w: 1, h: 1 }, type: defaultTypeFor(kind), label: KIND_LABELS[kind], mapping: kind, required: false };
    const s = suggest(field, ctx);
    return { kind, label: KIND_LABELS[kind], value: s?.value ?? null, sourceLabel: s?.sourceLabel, hint: hintFor(field, ctx), sensitive: SENSITIVE_KINDS.includes(kind) };
  });
}

// ---------------------------------------------------------------------------
// Field state and guided progress
// ---------------------------------------------------------------------------

export type FieldStatus = 'empty' | 'suggested' | 'confirmed' | 'na';
/**
 * A box's value, typed by what the box takes. Only text goes through
 * `cleanValue` and its length cap; a signature is strokes, a checkbox a flag.
 */
export type FieldValue =
  | { kind: 'text'; value: string }
  | ({ kind: 'signature' } & Signature)
  | { kind: 'check'; on: boolean };
export type ValueKind = FieldValue['kind'];
/** `value` is null when the box is empty or marked 해당 없음. */
export interface FieldState { value: FieldValue | null; status: FieldStatus; sourceLabel?: string }
export type FormState = Record<string, FieldState>;

export function valueKindOf(type: FieldType): ValueKind {
  return type === 'checkbox' ? 'check' : type === 'signature' ? 'signature' : 'text';
}
export const textValue = (value: string): FieldValue => ({ kind: 'text', value });
/** The text of a text box ('' for anything else). */
export function textOf(s: FieldState | undefined): string {
  return s?.value?.kind === 'text' ? s.value.value : '';
}
/** Holds an answer: non-empty text, a drawn signature, or a checkbox answered either way. */
export function hasValue(s: FieldState | undefined): boolean {
  return !!s?.value && (s.value.kind !== 'text' || !!s.value.value);
}
const EMPTY: FieldState = { value: null, status: 'empty' };

export function initialState(fields: TemplateField[], ctx: SuggestionContext): FormState {
  const state: FormState = {};
  for (const field of fields) {
    const suggestion = suggest(field, ctx);
    state[field.id] = suggestion ? { value: textValue(suggestion.value), status: 'suggested', sourceLabel: suggestion.sourceLabel } : EMPTY;
  }
  return state;
}

/** What the user typed, drew or picked: always confirmed by the act of entering it. Null clears the box. */
export function setValue(state: FormState, field: TemplateField, value: FieldValue | null, fields: TemplateField[] = [], groups: FieldGroup[] = []): FormState {
  if (value && value.kind !== valueKindOf(field.type)) throw new Error(`${field.type} 칸에 ${value.kind} 값을 넣을 수 없어요.`);
  if (!value) return { ...state, [field.id]: EMPTY };
  if (value.kind === 'check') {
    // An explicit "unchecked" is an answer, not an empty field.
    const next: FormState = { ...state, [field.id]: { value: { kind: 'check', on: value.on === true }, status: 'confirmed' } };
    if (value.on !== true) return next;
    // One answer: options of a single group, or of one imported radio (shared acro_name).
    const single = !!field.group && groups.some(g => g.id === field.group && g.mode === 'single');
    const sameQuestion = (other: TemplateField) =>
      (single && other.group === field.group) || (!!field.acro_name && other.acro_name === field.acro_name);
    for (const other of fields) if (other.id !== field.id && other.type === 'checkbox' && sameQuestion(other)) next[other.id] = { value: { kind: 'check', on: false }, status: 'confirmed' };
    return next;
  }
  if (value.kind === 'signature') {
    const sig = cleanSignature(value.strokes, value.aspect);
    return { ...state, [field.id]: sig ? { value: { kind: 'signature', ...sig }, status: 'confirmed' } : EMPTY };
  }
  const typed = cleanValue(value.value, field.type);
  const clean = field.part && typed.trim() ? normalizePart(typed, field.part) : typed;
  return { ...state, [field.id]: clean.trim() ? { value: textValue(clean), status: 'confirmed' } : EMPTY };
}
/** Confirms a suggestion. A date split into 년/월/일 boxes is one answer: its other parts are confirmed with it. */
export function confirmSuggestion(state: FormState, field: TemplateField, fields: TemplateField[] = []): FormState {
  const next = { ...state };
  for (const f of [field, ...dateSiblings(fields, field)]) {
    const current = state[f.id];
    if (current?.status === 'suggested') next[f.id] = { value: current.value, status: 'confirmed', sourceLabel: current.sourceLabel };
  }
  return next;
}
/** The other 년/월/일 boxes of the same date: application_date parts on the same page and printed line. */
export function dateSiblings(fields: TemplateField[], field: TemplateField): TemplateField[] {
  const isPart = (f: TemplateField) => f.mapping === 'application_date' && !!f.part;
  if (!isPart(field)) return [];
  const mid = (f: TemplateField) => f.rect.y + f.rect.h / 2;
  return fields.filter(f => f.id !== field.id && isPart(f) && f.page === field.page && Math.abs(mid(f) - mid(field)) <= Math.max(f.rect.h, field.rect.h) / 2);
}
export function markNotApplicable(state: FormState, field: TemplateField): FormState {
  return { ...state, [field.id]: { value: null, status: 'na' } };
}

/**
 * Whether a field still blocks 작성 완료. Only required fields do: an optional
 * box (a page that does not apply to everyone) never blocks, and a suggestion
 * on it stays a suggestion — it is not drawn unless the user confirms it.
 */
function blocks(field: TemplateField, s: FieldState | undefined): boolean {
  if (!field.required || s?.status === 'na') return false;
  return s?.status !== 'confirmed' || !hasValue(s);
}
export interface Progress { total: number; completed: number; requiredRemaining: number; unconfirmed: number; canFinish: boolean }
export function progress(fields: TemplateField[], state: FormState): Progress {
  let completed = 0, requiredRemaining = 0, unconfirmed = 0;
  for (const field of fields) {
    const s = state[field.id];
    if (s?.status === 'suggested') unconfirmed++;
    if (blocks(field, s)) requiredRemaining++;
    if (s && (s.status === 'na' || (s.status === 'confirmed' && hasValue(s)))) completed++;
  }
  return { total: fields.length, completed, requiredRemaining, unconfirmed, canFinish: requiredRemaining === 0 };
}
/** Next field needing attention (an unconfirmed suggestion or an unfinished required field) after `from` (−1 = start), wrapping; −1 when none. Optional empty fields are not pending. */
export function nextPending(fields: TemplateField[], state: FormState, from: number, direction: 1 | -1 = 1): number {
  const n = fields.length;
  const needs = (i: number) => { const s = state[fields[i].id]; return s?.status === 'suggested' || blocks(fields[i], s); };
  for (let step = 1; step <= n; step++) {
    const i = (((from + direction * step) % n) + n) % n;
    if (needs(i)) return i;
  }
  return -1;
}

/** Called after export and on leaving the screen. */
export function clearSensitive(fields: TemplateField[], state: FormState): FormState {
  const next = { ...state };
  for (const field of fields) if (isSensitive(field)) next[field.id] = EMPTY;
  return next;
}
/** Names of the sensitive kinds (주민등록번호, 계좌번호, 예금주) this form actually held a value for, each once. */
export function sensitiveFilledLabels(fields: TemplateField[], state: FormState): string[] {
  const out: string[] = [];
  for (const field of fields) {
    const kind = sensitiveKindOf(field);
    if (kind && hasValue(state[field.id]) && !out.includes(KIND_LABELS[kind])) out.push(KIND_LABELS[kind]);
  }
  return out;
}

/** Values the user may opt to remember on this device. Never sensitive, never another person's. */
export function rememberable(fields: TemplateField[], state: FormState): Remembered {
  const out: Remembered = {};
  for (const field of fields) {
    const s = state[field.id];
    const kind = field.mapping;
    const text = textOf(s).trim();
    if (!s || s.status !== 'confirmed' || !text || !kind || isSensitive(field) || isOtherPerson(field.label) || (field.type !== 'text' && field.type !== 'multiline')) continue;
    if ((REMEMBERABLE_KINDS as readonly string[]).includes(kind)) out[kind as RememberKey] = text;
  }
  return sanitizeRemembered(out);
}
/** Whitelists stored data on read and write; anything unexpected is dropped. */
export function sanitizeRemembered(raw: unknown): Remembered {
  const out: Remembered = {};
  if (!raw || typeof raw !== 'object') return out;
  for (const key of REMEMBERABLE_KINDS) {
    const value = (raw as Record<string, unknown>)[key];
    if (typeof value === 'string' && value.trim() && value.length <= 200 && !looksSensitive(value)) out[key] = value.trim();
  }
  return out;
}
/** An rrn- or account-shaped value is never stored, whatever the label said
 * (the digit skeleton shared with the server, ./digits.ts). */
export function looksSensitive(value: string): boolean {
  return sensitiveRuns(value).length > 0;
}

// ---------------------------------------------------------------------------
// Template validation (mirrors the server's checks)
// ---------------------------------------------------------------------------

export const MAX_TEMPLATE_FIELDS = 200;
/** Zero-width, bidi and other invisible characters; twin of `is_invisible` in crates/core form_templates.rs. */
const INVISIBLE = /[\u00AD\u200B-\u200F\u202A-\u202E\u2060-\u2064\u2066-\u2069\uFEFF]/;
/**
 * Validates a server template against the official file's verified hash
 * (`expectedHash` = the resource's content_hash). A derived template is bound
 * to that hash through `derived.source_sha256`; its own content_hash names the
 * converted PDF the app then downloads and checks. Pass the resource's
 * `format`: a derived template is accepted only for an HWP/HWPX resource, so
 * boxes drawn on Mazimi's PDF are never laid over an agency PDF.
 */
export function parseTemplate(raw: unknown, expectedHash: string, format: string | null): FormTemplate {
  const fail = (why: string): never => { throw new Error(`양식 정의가 올바르지 않아요 (${why}).`); };
  if (!raw || typeof raw !== 'object') fail('형식');
  const t = raw as Record<string, unknown>;
  if (t.version !== 1) fail('버전');
  const hex = (v: unknown): v is string => typeof v === 'string' && /^[0-9a-f]{64}$/.test(v);
  let derived: FormTemplate['derived'];
  if (t.derived !== undefined && t.derived !== null) {
    if (format !== 'hwp' && format !== 'hwpx') fail('변환 출처');
    const d = t.derived as Record<string, unknown>;
    if (typeof d !== 'object' || Object.keys(d).some(k => k !== 'source_sha256' && k !== 'converter')) fail('변환 출처');
    if (!hex(d.source_sha256) || d.source_sha256 !== expectedHash.toLowerCase() || !hex(t.content_hash) || t.content_hash === d.source_sha256) fail('양식 파일 버전 불일치');
    if (typeof d.converter !== 'string' || !/^[\x20-\x7e]{1,200}$/.test(d.converter) || !d.converter.trim()) fail('변환 출처');
    derived = { source_sha256: d.source_sha256 as string, converter: d.converter as string };
  } else if (typeof t.content_hash !== 'string' || t.content_hash !== expectedHash.toLowerCase()) fail('양식 파일 버전 불일치');
  if (!Number.isInteger(t.page_count) || (t.page_count as number) < 1 || (t.page_count as number) > 50) fail('쪽수');
  if (t.source !== 'admin' && t.source !== 'acroform') fail('출처');
  if (!Array.isArray(t.fields) || t.fields.length < 1 || t.fields.length > MAX_TEMPLATE_FIELDS) fail('항목 수');
  // Groups: the server's rules (crates/core form_templates.rs validate_groups).
  let groups: FieldGroup[] | undefined;
  if (t.groups !== undefined && t.groups !== null) {
    if (!Array.isArray(t.groups) || t.groups.length > MAX_GROUPS) fail('선택 묶음 수');
    const seen = new Set<string>();
    groups = (t.groups as unknown[]).map(g => {
      const group = g as Record<string, unknown>;
      if (!group || typeof group !== 'object' || Object.keys(group).some(k => k !== 'id' && k !== 'mode' && k !== 'label')) fail('선택 묶음 형식');
      if (typeof group.id !== 'string' || !/^[a-z0-9_-]{1,40}$/.test(group.id) || seen.has(group.id)) fail('선택 묶음 ID');
      seen.add(group.id as string);
      if (group.mode !== 'single' && group.mode !== 'multi') fail('선택 묶음 방식');
      if (typeof group.label !== 'string' || !group.label.trim() || [...group.label].length > 80 || /\p{Cc}/u.test(group.label) || INVISIBLE.test(group.label)) fail('선택 묶음 이름');
      return { id: group.id as string, mode: group.mode as GroupMode, label: group.label as string };
    });
  }
  const ids = new Set<string>();
  const fields = (t.fields as unknown[]).map(f => {
    const field = f as Record<string, unknown>;
    const rect = field.rect as Record<string, unknown> | undefined;
    if (typeof field.id !== 'string' || !/^[A-Za-z0-9_.-]{1,64}$/.test(field.id) || ids.has(field.id)) fail('항목 ID');
    ids.add(field.id as string);
    if (!Number.isInteger(field.page) || (field.page as number) < 0 || (field.page as number) >= (t.page_count as number)) fail('쪽 번호');
    const n = (v: unknown) => typeof v === 'number' && Number.isFinite(v);
    if (!rect || ![rect.x, rect.y, rect.w, rect.h].every(n)) fail('위치');
    const { x, y, w, h } = rect as unknown as Rect;
    if (x < 0 || y < 0 || w < 0.005 || h < 0.005 || x + w > 1.0001 || y + h > 1.0001) fail('위치 범위');
    if (!FIELD_TYPES.includes(field.type as FieldType)) fail('항목 종류');
    // Same rule as the server: 1–80 code points, no control characters.
    if (typeof field.label !== 'string' || !field.label.trim() || [...field.label].length > 80 || /\p{Cc}/u.test(field.label)) fail('항목 이름');
    if (field.mapping !== null && field.mapping !== undefined && !FIELD_KINDS.includes(field.mapping as FieldKind)) fail('연결 정보');
    if (field.format !== undefined && field.format !== null && !DATE_FORMATS.includes(field.format as DateFormat)) fail('날짜 형식');
    if (field.font_size !== undefined && field.font_size !== null && (!n(field.font_size) || (field.font_size as number) < 5 || (field.font_size as number) > 24)) fail('글자 크기');
    if (field.part !== undefined && field.part !== null && (field.type !== 'text' || !DATE_PARTS.includes(field.part as DatePart))) fail('날짜 부분');
    const out: TemplateField = { id: field.id as string, page: field.page as number, rect: { x, y, w, h }, type: field.type as FieldType, label: (field.label as string).trim(), mapping: (field.mapping as FieldKind | null) ?? null, required: field.required === true };
    if (field.format) out.format = field.format as DateFormat;
    if (field.font_size) out.font_size = field.font_size as number;
    if (typeof field.acro_name === 'string') out.acro_name = field.acro_name.slice(0, 200);
    if (field.part) out.part = field.part as DatePart;
    if (field.group !== undefined && field.group !== null) {
      if (typeof field.group !== 'string' || !groups?.some(g => g.id === field.group)) fail('선택 묶음 참조');
      if (out.type !== 'checkbox') fail('선택 묶음에는 체크 칸만');
      out.group = field.group as string;
    }
    return out;
  });
  for (const group of groups ?? []) {
    const pages = fields.filter(f => f.group === group.id).map(f => f.page);
    if (pages.length < 2) fail('선택 묶음 선택지 2개 이상');
    if (pages.some(p => p !== pages[0])) fail('선택 묶음 같은 쪽');
  }
  return { version: 1, content_hash: t.content_hash as string, page_count: t.page_count as number, source: t.source as FormTemplate['source'], fields, ...(groups?.length ? { groups } : {}), ...(derived ? { derived } : {}) };
}

/** Fields in reading order: page, then top-to-bottom, then left-to-right. */
export function readingOrder(fields: TemplateField[]): TemplateField[] {
  return [...fields].sort((a, b) => a.page - b.page || (Math.abs(a.rect.y - b.rect.y) > 0.01 ? a.rect.y - b.rect.y : a.rect.x - b.rect.x));
}
