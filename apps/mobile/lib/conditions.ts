import type { QueryClient } from "@tanstack/react-query";
import { api, ApiRequestError, type UserProfile } from "./api";
import { invalidateAfterWrite } from "./writeEffects";
import { cityCodeFor, findRegion } from "@/constants/regions";
import { useOnboardingStore } from "@/store/onboarding";
import { useAuthStore } from "@/store/auth";
import { toEmploymentCode, toEnrollmentCode } from "./profileCodes";
import { kstToday, type YearMonthDay } from "./birthDate";
import { ANY_OPTIONAL_QUESTION_ENABLED, askedAnswers, optionalAnswersForSave, withoutOptionalAnswers, type OptionalAnswers } from "./optionalAnswers";

export type ConditionDraft = {
  nickname: string; region: string; cityCode: string; birthYear: number; enrollmentStatus: string; schoolName: string; employmentStatus: string; incomeBracket: number | null;
  /** Optional answers. Onboarding merges them over the saved ones (a skip keeps it); the edit screen replaces them (a skip clears it). */
  optional?: OptionalAnswers; replaceOptional?: boolean;
};

/** 닉네임은 필수다. 공백만 있는 값은 비어 있는 것으로 본다 */
export const NICKNAME_MAX = 30;
export function validNickname(nickname: string) {
  const trimmed = nickname.trim();
  return trimmed.length >= 1 && trimmed.length <= NICKNAME_MAX;
}

/** Legacy birth_year path. The year is the KST calendar year, like every other age rule. */
export function validAge(birthYear: number, today: YearMonthDay = kstToday()) {
  const age = today.year - birthYear;
  return !!birthYear && age >= 15 && age <= 100;
}

/** A real calendar date "YYYY-MM-DD". Screens check the bounds (lib/birthDate validBirthDate); the server checks them again. */
function calendarDate(value: string) {
  const match = /^(\d{4})-(\d{2})-(\d{2})$/.exec(value);
  if (!match) return false;
  const [year, month, day] = match.slice(1).map(Number);
  const date = new Date(Date.UTC(year, month - 1, day));
  return date.getUTCFullYear() === year && date.getUTCMonth() === month - 1 && date.getUTCDate() === day;
}

/** The stored row without identity and server bookkeeping: the server takes the user from the session.
 * Everything else goes back as stored, columns this build does not know included: POST /profile nulls
 * what a body omits, so an installed build must not erase a field the server adds later. */
function storedAnswers(profile: UserProfile): Omit<UserProfile, "user_id" | "profile_version" | "updated_at" | "profile_image_url"> {
  const { user_id: _user, profile_version: _version, updated_at: _updated, profile_image_url: _image, ...answers } = profile;
  return answers;
}

/** 회원의 조건을 서버 프로필에 저장한다. 비회원이거나 지역·나이가 올바르지 않으면 저장하지 않고 false.
 * `birthDate`: "YYYY-MM-DD" from the date-of-birth step; sent as birth_date with the matching birth_year.
 * `schoolType`: the 학적 follow-up answer, sent as school_type ("" clears it); omitted keeps the stored one. */
export async function saveConditions(client: QueryClient, userId: string | undefined, draft: ConditionDraft & { birthDate?: string; schoolType?: string }) {
  const region = findRegion(draft.region)?.code;
  const birthDate = draft.birthDate && calendarDate(draft.birthDate) ? draft.birthDate : undefined;
  const savedBirthYear = birthDate ? Number(birthDate.slice(0, 4)) : draft.birthYear;
  // 비회원 조건은 기기에도 남기지 않는다
  if (!userId || (draft.birthDate ? !birthDate : !validAge(savedBirthYear)) || !region) return false;
  // A city belongs to its province.
  const cityCode = cityCodeFor(region, draft.cityCode);
  if (userId) {
    let existing;
    try { existing = await api.getProfile(); }
    catch (e) { if (!(e instanceof ApiRequestError && e.status === 404)) throw e; }
    // A hidden question's keys are never sent (the server keeps what it has).
    // The stored answers, without identity or server bookkeeping: the server takes the user from the session.
    const previous = existing?.profile && withoutOptionalAnswers(storedAnswers(existing.profile));
    const schoolName = draft.schoolName.trim() || null;
    const schoolChanged = (previous?.school_name ?? null) !== schoolName;
    const result = await api.saveProfile({
      ...previous, nickname: draft.nickname.trim() || undefined,
      // 생년월일이 없으면 저장된 birth_date를 다시 보내지 않는다 (연도가 다르면 서버가 거절한다)
      birth_year: savedBirthYear, birth_date: birthDate, region_code: region,
      // School metadata belongs to its school.
      city_code: cityCode || null,
      school_name: schoolName, school_year: schoolChanged ? null : previous?.school_year,
      // 사람이 고른 과정은 학교를 바꿔도 그대로다. 답이 없으면 학교에 딸린 값으로 다룬다
      school_type: draft.schoolType !== undefined ? draft.schoolType || null : schoolChanged ? null : previous?.school_type,
      major_group: schoolChanged ? null : previous?.major_group,
      enrollment_status: toEnrollmentCode(draft.enrollmentStatus),
      employment_status: toEmploymentCode(draft.employmentStatus), income_bracket: draft.incomeBracket,
      ...(ANY_OPTIONAL_QUESTION_ENABLED && draft.optional && askedAnswers(draft.replaceOptional ? draft.optional : optionalAnswersForSave(previous, draft.optional))),
    });
    client.setQueryData(["profile", "me", userId], result);
    // The save waits for the profile and 추천 only; the lists refresh behind it.
    await invalidateAfterWrite(client, 'profile_patch', { userId, programId: null }, key => key[0] === 'profile' || key[0] === 'recommend');
  }
  // 저장하는 사이 로그아웃되면 회원 정보를 기기에 남기지 않는다
  if (userId && useAuthStore.getState().user?.id !== userId) return true;
  const { nickname, birthYear, schoolName, incomeBracket } = draft;
  const enrollmentStatus = toEnrollmentCode(draft.enrollmentStatus) ?? "", employmentStatus = toEmploymentCode(draft.employmentStatus) ?? "";
  useOnboardingStore.setState({ nickname, region, cityCode, birthYear, age: String(kstToday().year - birthYear), enrollmentStatus, schoolName, employmentStatus, incomeBracket, ...(ANY_OPTIONAL_QUESTION_ENABLED && draft.optional && { optional: draft.optional }) });
  return true;
}
