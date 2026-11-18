/**
 * 주민등록번호- and account-shaped digit runs, by structure: the port of
 * crates/core/src/sensitive_digits.rs. Both read
 * docs/validation/sensitive-digit-cases.json; change the two together.
 *
 * A digit is any decimal digit (any script), or a Hangul numeral syllable
 * inside a run of six or more. Digit groups join across a gap of one to three
 * characters that holds no Hangul syllable and no Latin word of two letters or
 * more (so `-`, ` `, `·`, `ㅡ`, `一`, a lone `o` join). A 주민등록번호 is
 * thirteen digits with a 1–8 seventh digit (six then seven), or six digits, a
 * mark and a 1–8 digit; otherwise ten or more digits that are not a phone
 * number are an account number.
 */
export type Shape = 'rrn' | 'account';

const HANGUL_NUMERALS: Record<string, string> = { 영: '0', 공: '0', 일: '1', 이: '2', 삼: '3', 사: '4', 오: '5', 육: '6', 륙: '6', 칠: '7', 팔: '8', 구: '9' };
const ZEROS = [0xff10, 0x0660, 0x06f0, 0x0966, 0x09e6, 0x0e50];

function decimal(c: string): string | null {
  if (/^[0-9]$/.test(c)) return c;
  const code = c.codePointAt(0) ?? 0;
  for (const zero of ZEROS) if (code >= zero && code < zero + 10) return String(code - zero);
  return /^\p{N}$/u.test(c) ? '5' : null;
}
const syllable = (c: string) => c >= '가' && c <= '힣';
const latin = (c: string | undefined) => !!c && /^[A-Za-z]$/.test(c);

function phone(digits: string): boolean {
  const rest = digits.startsWith('82') ? digits.slice(2) : null;
  const national = rest !== null && rest.length >= 10 && rest.length <= 11 && !rest.startsWith('0') ? `0${rest}` : digits;
  const len = national.length;
  const at = (i: number) => national[i] ?? 'x';
  return (at(0) === '0' && at(1) === '1' && '016789'.includes(at(2)) && len >= 10 && len <= 11)
    || (at(0) === '0' && at(1) === '2' && len >= 9 && len <= 10)
    || (at(0) === '0' && at(1) >= '3' && at(1) <= '6' && len >= 10 && len <= 11)
    || (national.startsWith('070') && len === 11)
    || (national.startsWith('050') && len >= 11 && len <= 12);
}

/** The sensitive runs of `text`: [start, end, shape] as code-point indices. */
export function sensitiveRuns(text: string): [number, number, Shape][] {
  const chars = [...text];
  const n = chars.length;
  const digit: (string | null)[] = chars.map(decimal);
  for (let i = 0; i < n;) {
    if (!(chars[i] in HANGUL_NUMERALS)) { i++; continue; }
    const start = i;
    while (i < n && chars[i] in HANGUL_NUMERALS) i++;
    if (i - start >= 6) for (let k = start; k < i; k++) digit[k] = HANGUL_NUMERALS[chars[k]];
  }
  const latinWord = (k: number) => latin(chars[k]) && (latin(chars[k - 1]) || latin(chars[k + 1]));
  const joiner = (k: number) => digit[k] === null && !syllable(chars[k]) && !latinWord(k);
  const out: [number, number, Shape][] = [];
  for (let i = 0; i < n;) {
    if (digit[i] === null) { i++; continue; }
    const start = i;
    let end = i;
    let digits = '';
    let firstGroup: [number, boolean] | null = null;
    let j = i;
    while (j < n) {
      const d = digit[j];
      if (d !== null) { digits += d; end = j + 1; j++; continue; }
      let gap = 0;
      while (j + gap < n && joiner(j + gap)) gap++;
      if (gap >= 1 && gap <= 3 && j + gap < n && digit[j + gap] !== null) {
        if (!firstGroup) firstGroup = [digits.length, chars.slice(j, j + gap).some(c => c !== ' ' && c !== '\t' && c !== '\n')];
        j += gap;
      } else break;
    }
    const seventh = digits.length > 6 && digits[6] >= '1' && digits[6] <= '8';
    const rrn = seventh && ((digits.length === 13 && (!firstGroup || firstGroup[0] === 6))
      || (!!firstGroup && firstGroup[0] === 6 && firstGroup[1] && digits.length <= 13));
    if (rrn) out.push([start, end, 'rrn']);
    else if (digits.length >= 10 && !phone(digits)) out.push([start, end, 'account']);
    i = Math.max(end, i + 1);
  }
  return out;
}

// ---------------------------------------------------------------------------
// Value-level rules (device only): a value is judged as a whole, not by
// finding one shape in it (review 3 H3).
// ---------------------------------------------------------------------------

const NATIVE_NUMERALS = ['하나', '다섯', '여섯', '일곱', '여덟', '아홉', '둘', '셋', '넷', '열'];
const CJK_NUMERALS = /[〇零一二三四五六七八九十]/u;

/** Numeral characters in a word: digits of any script, circled and CJK
 * numerals, Hangul numeral syllables, native numerals (하나 … 아홉). */
function numeralChars(word: string): number {
  let rest = word;
  let count = 0;
  for (const n of NATIVE_NUMERALS) while (rest.includes(n)) { rest = rest.replace(n, ''); count += n.length; }
  for (const c of rest) if (/\p{N}/u.test(c) || CJK_NUMERALS.test(c) || c in HANGUL_NUMERALS) count++;
  return count;
}

/** Numerals in the words that are mostly numerals ("구공공", "9공0101-…"):
 * a value shape when six or more. */
export function numeralCount(text: string): number {
  return text.split(/\s+/).reduce((sum, word) => {
    const letters = [...word.replace(/[^\p{L}\p{N}]/gu, '')].length;
    const n = numeralChars(word);
    return sum + (letters && n * 2 > letters ? n : 0);
  }, 0);
}

export type ValueProblem = 'sensitive' | 'shape' | null;

/** Whether a value said for a box of this kind may be proposed: 'sensitive'
 * (an rrn, an account, or mostly numerals where a number does not belong) or
 * 'shape' (not what such a box holds). `kind` null: a label word box. */
export function valueProblem(kind: string | null, value: string): ValueProblem {
  if (sensitiveRuns(value).length) return 'sensitive';
  const digits = [...value].filter(c => /\p{N}/u.test(c)).map(c => decimal(c) ?? '5').join('');
  const numerals = numeralCount(value);
  switch (kind) {
    case 'phone': return phone(digits) ? null : numerals >= 6 ? 'sensitive' : 'shape';
    case 'birth_date': case 'application_date': return digits.length <= 8 ? null : 'sensitive';
    case 'student_id': return /^\d{5,10}$/.test(value.replace(/[\s-]/g, '')) ? null : numerals >= 6 ? 'sensitive' : 'shape';
    default: break;
  }
  const groups = value.match(/\p{N}+/gu) ?? [];
  if (kind === 'address') {
    // A building number or a zip has at most five digits; six or more, or
    // numerals spelled out, are a number that is not an address.
    // More than ten digits in all is a split number ("11012동 34567호 8901번지").
    if (groups.some(g => g.length >= 6) || digits.length > 10 || numeralCount(value.replace(/\p{Nd}/gu, '')) >= 6) return 'sensitive';
    return groups.length <= 3 ? null : 'shape';
  }
  if (numerals >= 6) return 'sensitive';
  switch (kind) {
    case 'name': return /^[\p{Script=Hangul}A-Za-z]+( [\p{Script=Hangul}A-Za-z]+)?$/u.test(value) && value.length <= 20 ? null : 'shape';
    case 'name_en': return /^[A-Za-z][A-Za-z '\-.]*$/.test(value) ? null : 'shape';
    case 'school': case 'major': case 'bank': return /^[\p{Script=Hangul}A-Za-z0-9 ·()\-]+$/u.test(value) && digits.length <= 2 ? null : 'shape';
    case 'email': return /^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(value) ? null : 'shape';
    case 'grade': return /^\d{1,2}(학년)?$/.test(value) ? null : 'shape';
    default: return groups.every(g => g.length <= 5) ? null : 'shape';
  }
}
