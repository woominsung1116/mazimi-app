import type { ProfileInput, UserProfile } from "./api";
import { askedAnswers, optionalAnswersForSave, type OptionalAnswers } from "./optionalAnswers";
import { EMPLOYMENT_LABELS, ENROLLMENT_LABELS } from "./profileCodes";

/** An answer to one profile field: a code, `null` = cleared, `undefined` = not spoken to (the stored value is kept). */
export type FieldAnswer = string | null | undefined;

/**
 * The 학적 question of onboarding step 2. Each choice states both sides: the code it answers, or
 * `undefined` where it says nothing. 재학·휴학·졸업 say nothing about work; 취업 준비 says nothing
 * about enrolment (a final-year student can be preparing for work), so a stored 학적 and school stay.
 */
export const PREVIEW_STATUS_CHOICES: readonly { id: string; label: string; enrollment: FieldAnswer; employment: FieldAnswer }[] = [
  { id: "enrolled", label: ENROLLMENT_LABELS.enrolled, enrollment: "enrolled", employment: undefined },
  { id: "on_leave", label: ENROLLMENT_LABELS.on_leave, enrollment: "on_leave", employment: undefined },
  { id: "graduated", label: ENROLLMENT_LABELS.graduated, enrollment: "graduated", employment: undefined },
  { id: "preparing", label: EMPLOYMENT_LABELS.preparing, enrollment: undefined, employment: "preparing" },
];

/** `asked` when the answer speaks to the field ("" counts as cleared), else the stored value. */
function answered(asked: FieldAnswer, stored: string | null | undefined): string | null {
  return asked === undefined ? stored ?? null : asked?.trim() || null;
}

/** What the preview asked: the onboarding answers and the optional 시·군·구 chosen there. */
export interface PreviewAnswers {
  /** The onboarding nickname. Absent = this save does not ask for one (no key sent). */
  nickname?: string;
  region_code: string;
  birth_year: number;
  /** The 시·군·구 picked on the preview page; "" when none was. */
  city_code: string;
  /** See FieldAnswer: undefined keeps the stored value, null clears it. */
  enrollment_status: FieldAnswer;
  employment_status: FieldAnswer;
  /** A typed 학교명; undefined when the box was left blank. */
  school_name: FieldAnswer;
  income_bracket: number | null;
  /** The optional answers asked on onboarding; null = skipped (keeps the saved answer). */
  optional?: OptionalAnswers;
}

/**
 * The profile "이 조건 저장" posts. POST /profile replaces the whole row, so
 * this is a partial update built on the saved profile: only what the preview
 * asked changes. `age_band` and every other answer given elsewhere are kept,
 * and a 시·군·구 saved elsewhere is kept unless one was picked here, as long as
 * it lies in the saved sido (`districtCodes` = codes of the asked sido).
 * Income, 학적, 취업 상태 or 학교명 left unanswered keep the saved value. An optional answer
 * (무주택, 혼인·자녀, 중위소득 구간) skipped here keeps the saved one.
 */
export function previewProfileForSave(
  current: UserProfile | null,
  asked: PreviewAnswers,
  districtCodes: readonly string[],
): ProfileInput {
  const keptCity = current?.city_code && districtCodes.includes(current.city_code) ? current.city_code : null;
  return {
    ...(current && {
      school_year: current.school_year,
      major_group: current.major_group,
      kosaf_support_bracket: current.kosaf_support_bracket,
      housing_type: current.housing_type,
      household_size: current.household_size,
      // The four group flags are not sent: no screen asks them, and the
      // server keeps a key a save omits (a cached value would be stored as
      // an answer).
      preferred_categories: current.preferred_categories,
      school_type: current.school_type,
      age_band: current.age_band,
    }),
    // A nickname key makes the server require one on the first save; "" is sent as null.
    ...(asked.nickname !== undefined && { nickname: asked.nickname.trim() || null }),
    region_code: asked.region_code,
    birth_year: asked.birth_year,
    city_code: asked.city_code || keptCity,
    // One question on the preview (PREVIEW_STATUS_CHOICES): the side a choice does not speak
    // to, and a blank 학교명, keep the saved answer (given on mobile, say).
    enrollment_status: answered(asked.enrollment_status, current?.enrollment_status),
    employment_status: answered(asked.employment_status, current?.employment_status),
    school_name: answered(asked.school_name, current?.school_name),
    income_bracket: asked.income_bracket ?? current?.income_bracket ?? null,
    // Hidden questions send nothing: the server keeps any key a save omits.
    ...(asked.optional ? askedAnswers(optionalAnswersForSave(current, asked.optional)) : {}),
  };
}
