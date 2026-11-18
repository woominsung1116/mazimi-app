// Same rules as apps/mobile/lib/optionalAnswers.ts.
//
// Three optional profile questions (무주택, 혼인·자녀, 기준 중위소득 구간).
// Every one can be skipped. The income band is the user's own choice; the app
// never computes it.
//
// Collect only what is used. Each question is shown and saved only while its
// flag is on; a hidden question renders nothing and no save sends its keys
// (the server still stores and evaluates what it has).
//  - NEXT_PUBLIC_PROFILE_CONDITION_QUESTIONS_ENABLED: 무주택 and 혼인·자녀. Off unless "true".
//  - NEXT_PUBLIC_INCOME_QUESTION_ENABLED: 소득 구간 (기준 중위소득 %). On unless "false":
//    the server's matching reads income_median_band.

/** 무주택 and 혼인·자녀 are shown and saved. Off by default. */
export const PROFILE_CONDITION_QUESTIONS_ENABLED = isOn(process.env.NEXT_PUBLIC_PROFILE_CONDITION_QUESTIONS_ENABLED);
/** 소득 구간 (기준 중위소득 %) is shown and saved. On by default. */
export const INCOME_QUESTION_ENABLED = !isOff(process.env.NEXT_PUBLIC_INCOME_QUESTION_ENABLED);
/** At least one optional question is shown. */
export const ANY_OPTIONAL_QUESTION_ENABLED = PROFILE_CONDITION_QUESTIONS_ENABLED || INCOME_QUESTION_ENABLED;

function isOn(value: string | undefined): boolean {
  return ["true", "1", "yes", "on"].includes((value ?? "").trim().toLowerCase());
}
function isOff(value: string | undefined): boolean {
  return ["false", "0", "no", "off"].includes((value ?? "").trim().toLowerCase());
}

/** The answer keys these questions own. */
export const OPTIONAL_ANSWER_KEYS = ["homeless_household", "marital_status", "newlywed", "children_count", "income_median_band"] as const;
export type OptionalAnswerKey = (typeof OPTIONAL_ANSWER_KEYS)[number];
/** Keys of 무주택 and 혼인·자녀. */
export const HOUSEHOLD_ANSWER_KEYS: readonly OptionalAnswerKey[] = ["homeless_household", "marital_status", "newlywed", "children_count"];

/** The keys asked and saved under the current flags. */
export const ASKED_OPTIONAL_KEYS: readonly OptionalAnswerKey[] = [
  ...(PROFILE_CONDITION_QUESTIONS_ENABLED ? HOUSEHOLD_ANSWER_KEYS : []),
  ...(INCOME_QUESTION_ENABLED ? (["income_median_band"] as const) : []),
];

/** `profile` without the keys of hidden questions (what a save sends). */
export function withoutOptionalAnswers<T extends object>(profile: T): T {
  const out = { ...profile } as Record<string, unknown>;
  for (const key of OPTIONAL_ANSWER_KEYS) if (!ASKED_OPTIONAL_KEYS.includes(key)) delete out[key];
  return out as T;
}

/** Only the answers of questions that are shown (what a save or preview may send). */
export function askedAnswers(answers: OptionalAnswers): Partial<OptionalAnswers> {
  const out: Partial<OptionalAnswers> = {};
  for (const key of ASKED_OPTIONAL_KEYS) (out as Record<string, unknown>)[key] = answers[key];
  return out;
}

export type MaritalStatus = "unmarried" | "married";
export type IncomeMedianBand = "le50" | "le60" | "le100" | "le120" | "le150" | "le180" | "gt180" | "unknown";

export interface OptionalAnswers {
  homeless_household: boolean | null;
  marital_status: MaritalStatus | null;
  /** Only meaningful when married. */
  newlywed: boolean | null;
  children_count: number | null;
  income_median_band: IncomeMedianBand | null;
}

export const EMPTY_OPTIONAL_ANSWERS: OptionalAnswers = {
  homeless_household: null,
  marital_status: null,
  newlywed: null,
  children_count: null,
  income_median_band: null,
};

/** The official 기준 중위소득 table (보건복지부). */
export const MEDIAN_INCOME_TABLE_URL = "https://www.mohw.go.kr/menu.es?mid=a10708010900";

export const INCOME_MEDIAN_BANDS: { value: IncomeMedianBand; label: string }[] = [
  { value: "le50", label: "50% 이하" },
  { value: "le60", label: "50% 초과 ~ 60% 이하" },
  { value: "le100", label: "60% 초과 ~ 100% 이하" },
  { value: "le120", label: "100% 초과 ~ 120% 이하" },
  { value: "le150", label: "120% 초과 ~ 150% 이하" },
  { value: "le180", label: "150% 초과 ~ 180% 이하" },
  { value: "gt180", label: "180% 초과" },
  { value: "unknown", label: "모름" },
];

export const OPTIONAL_COPY = {
  intro: "선택 질문이에요. 답하지 않아도 돼요.",
  optional: "선택",
  homelessTitle: "무주택 여부",
  homelessHelp: "본인이 속한 가구(주민등록상 세대) 기준이에요. 세대원 모두 집을 갖고 있지 않으면 '무주택'이에요.",
  homelessYes: "무주택",
  homelessNo: "주택 있음",
  maritalTitle: "혼인·자녀",
  maritalHelp: "법률상 혼인(혼인신고) 기준이에요. 지금 혼인신고가 된 상태면 '기혼', 혼인한 적이 없거나 이혼·사별했다면 '미혼'을 골라 주세요. 사실혼은 '미혼'이에요.",
  unmarried: "미혼",
  married: "기혼",
  newlyweds: "혼인신고 후 7년 이내예요",
  childrenTitle: "자녀 수",
  /** Intro when the income question is the only one shown. */
  incomeIntro: "선택 질문이에요. 답하면 소득 조건이 있는 혜택을 더 정확히 걸러드려요.",
  incomeTitle: "소득 구간 (기준 중위소득 %)",
  incomeHelp: "가구 소득이 기준 중위소득의 몇 %인지 공식 표에서 확인해 골라 주세요. 모르면 '모름'을 고르거나 건너뛰어도 돼요.",
  incomeLink: "기준 중위소득 표 보기 (보건복지부)",
  skip: "건너뛰기",
} as const;

/** Answers from a saved profile. */
export function savedOptionalAnswers(profile: Partial<OptionalAnswers> | null | undefined): OptionalAnswers {
  return {
    homeless_household: profile?.homeless_household ?? null,
    marital_status: profile?.marital_status ?? null,
    newlywed: profile?.marital_status === "married" ? profile?.newlywed ?? null : null,
    children_count: profile?.children_count ?? null,
    income_median_band: profile?.income_median_band ?? null,
  };
}

/**
 * The optional answers a save sends: an answer given now wins; a question
 * left unanswered keeps the saved answer (the server also keeps any key a save
 * omits). 신혼 goes with 기혼.
 */
export function optionalAnswersForSave(
  current: Partial<OptionalAnswers> | null | undefined,
  asked: OptionalAnswers,
): OptionalAnswers {
  const saved = savedOptionalAnswers(current);
  const marital_status = asked.marital_status ?? saved.marital_status;
  const newlywed =
    marital_status !== "married"
      ? null
      : asked.marital_status
        ? asked.newlywed
        : (asked.newlywed ?? saved.newlywed);
  return {
    homeless_household: asked.homeless_household ?? saved.homeless_household,
    marital_status,
    newlywed,
    children_count: asked.children_count ?? saved.children_count,
    income_median_band: asked.income_median_band ?? saved.income_median_band,
  };
}
