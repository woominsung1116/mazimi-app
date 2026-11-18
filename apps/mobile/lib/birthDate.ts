// 생년월일 (v1.0.0 피드백 F2.5). 나이는 해마다 바뀌고 일부 규칙은 만 나이를 쓰므로
// 나이 대신 생년월일을 받는다. 서버 계약: profile.birth_date "YYYY-MM-DD" (Asia/Seoul 달력 날짜).

/** 서버가 받는 가장 이른 날짜 (api-contract §1) */
export const BIRTH_DATE_MIN = "1920-01-01";
/** 서버가 받는 최소 만 나이 (api-contract §1: under_minimum_age) */
export const MIN_MAN_AGE = 14;

const pattern = /^(\d{4})-(\d{2})-(\d{2})$/;
const pad = (n: number) => String(n).padStart(2, "0");

export type YearMonthDay = { year: number; month: number; day: number };

/** 오늘의 한국 달력 날짜 */
export function kstToday(now: Date = new Date()): YearMonthDay {
  const kst = new Date(now.getTime() + 9 * 3600_000);
  return { year: kst.getUTCFullYear(), month: kst.getUTCMonth() + 1, day: kst.getUTCDate() };
}

export function daysInMonth(year: number, month: number) {
  return new Date(Date.UTC(year, month, 0)).getUTCDate();
}

export function formatBirthDate({ year, month, day }: YearMonthDay) {
  return `${year}-${pad(month)}-${pad(day)}`;
}

/** 달력에 있는 날짜면 연·월·일, 아니면 null */
export function parseBirthDate(value: string | null | undefined): YearMonthDay | null {
  const match = pattern.exec(value?.trim() ?? "");
  if (!match) return null;
  const [year, month, day] = match.slice(1).map(Number);
  if (month < 1 || month > 12 || day < 1 || day > daysInMonth(year, month)) return null;
  return { year, month, day };
}

/** 만 나이. 생일이 지나지 않았으면 한 살 적다 */
export function manAge(birth: YearMonthDay, today: YearMonthDay = kstToday()) {
  const before = today.month < birth.month || (today.month === birth.month && today.day < birth.day);
  return today.year - birth.year - (before ? 1 : 0);
}

export type BirthDateProblem = "invalid" | "future" | "too_old" | "too_young";

/** 저장할 수 없는 이유, 문제없으면 null */
export function birthDateProblem(value: string, today: YearMonthDay = kstToday()): BirthDateProblem | null {
  const birth = parseBirthDate(value);
  if (!birth) return "invalid";
  const key = formatBirthDate(birth);
  if (key > formatBirthDate(today)) return "future";
  if (key < BIRTH_DATE_MIN) return "too_old";
  if (manAge(birth, today) < MIN_MAN_AGE) return "too_young";
  return null;
}

export function validBirthDate(value: string, today?: YearMonthDay) {
  return birthDateProblem(value, today) === null;
}

export const BIRTH_DATE_MESSAGES: Record<BirthDateProblem, string> = {
  invalid: "생년월일을 끝까지 골라 주세요.",
  future: "오늘 이후 날짜는 고를 수 없어요.",
  too_old: "1920년 1월 1일 이후 날짜를 골라 주세요.",
  too_young: `만 ${MIN_MAN_AGE}세 이상만 이용할 수 있어요.`,
};

export type BirthDateSave = {
  /** The DOB part does not block saving. */
  ok: boolean;
  /** What saveConditions gets: a full date, or "" to keep the stored birth year. */
  birthDate: string;
  birthYear: number;
  /** Red error under the picker: only once the person edited the field. */
  showError: boolean;
  /** Grey hint while the field is untouched: a stored year only, or nothing stored yet. */
  hint: "stored_year" | "missing" | null;
};

/**
 * Profile edit (M5): a profile with only birth_year must not block saving (a
 * nickname change still saves). The full date is required only once the person
 * edits the DOB field; until then the stored year is kept and a soft hint asks
 * for the rest.
 */
export function birthDateSave({ birthDate, storedYear, touched }: { birthDate: string; storedYear: number | null | undefined; touched: boolean }, today: YearMonthDay = kstToday()): BirthDateSave {
  if (validBirthDate(birthDate, today)) return { ok: true, birthDate, birthYear: Number(birthDate.slice(0, 4)), showError: false, hint: null };
  const year = storedYear ?? 0;
  const yearUsable = Number.isInteger(year) && year >= 1920 && today.year - year >= MIN_MAN_AGE;
  if (!touched && yearUsable) return { ok: true, birthDate: "", birthYear: year, showError: false, hint: "stored_year" };
  return { ok: false, birthDate: "", birthYear: 0, showError: touched, hint: touched ? null : "missing" };
}

export const BIRTH_DATE_HINTS: Record<NonNullable<BirthDateSave["hint"]>, string> = {
  stored_year: "월·일까지 고르면 만 나이로 더 정확히 확인해요. 그대로 저장하면 저장된 출생연도를 써요.",
  missing: "생년월일을 골라 주세요.",
};
