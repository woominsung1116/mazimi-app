import { create } from "zustand";
import { findRegion } from "@/constants/regions";
import { EMPTY_OPTIONAL_ANSWERS, type OptionalAnswers } from "@/lib/optionalAnswers";

// 입력 중인 조건을 들고 있는 메모리 초안이다. 기기에 저장하지 않는다:
// 회원의 조건은 서버 프로필이 원본이고, 비회원은 아무것도 남기지 않는다.
// 예전 빌드가 남긴 기기 초안은 lib/guest.ts purgeLegacyGuestData가 지운다

interface OnboardingState {
  // Step 0 (nickname)
  nickname: string;
  // Step 1
  region: string;
  /** 시·군·구 city_code inside `region`, or "" when not chosen. */
  cityCode: string;
  // Step 2
  age: string;
  birthYear: number;
  /** "YYYY-MM-DD", or "" until all of 연·월·일 are chosen. birthYear follows it. */
  birthDate: string;
  enrollmentStatus: string;
  schoolName: string;
  /** school_type answer of the 학적 follow-up ("university" | "graduate"), "" when unanswered. */
  schoolType: string;
  incomeBracket: number | null;
  // Step 3
  employmentStatus: string;
  /** Optional 무주택 / 혼인·자녀 / 중위소득 answers; null = skipped. */
  optional: OptionalAnswers;

  setNickname: (nickname: string) => void;
  setRegion: (region: string) => void;
  setCityCode: (cityCode: string) => void;
  setAge: (age: string) => void;
  setBirthDate: (birthDate: string) => void;
  setEnrollmentStatus: (status: string) => void;
  setSchoolName: (name: string) => void;
  setSchoolType: (schoolType: string) => void;
  setIncomeBracket: (bracket: number | null) => void;
  setEmploymentStatus: (status: string) => void;
  setOptional: (optional: OptionalAnswers) => void;
  /** The 학적 step's pick: the status and its follow-up; no status, no school. */
  chooseEnrollment: (status: string, schoolType: string) => void;

  // Legacy step setters kept for any callers that used the old API
  setStep1: (region: string, birthYear: number) => void;

  reset: () => void;
}

export const useOnboardingStore = create<OnboardingState>()((set) => ({
  nickname: "",
  region: "",
  cityCode: "",
  age: "",
  birthYear: 0,
  birthDate: "",
  enrollmentStatus: "",
  schoolName: "",
  schoolType: "",
  incomeBracket: null,
  employmentStatus: "",
  optional: EMPTY_OPTIONAL_ANSWERS,

  setNickname: (nickname) => set({ nickname }),
  // A 시·군·구 belongs to its sido.
  setRegion: (region) => set((state) => ({ region, cityCode: findRegion(state.region)?.code === findRegion(region)?.code ? state.cityCode : "" })),
  setCityCode: (cityCode) => set({ cityCode }),
  setAge: (age) => set({ age, birthYear: getBirthYear(age) }),
  // 생년월일을 다 고르면 출생연도도 함께 정해진다. 덜 고르면 둘 다 비운다
  setBirthDate: (birthDate) => set({ birthDate, birthYear: birthDate ? Number(birthDate.slice(0, 4)) : 0, age: "" }),
  setEnrollmentStatus: (enrollmentStatus) => set({ enrollmentStatus }),
  setSchoolName: (schoolName) => set({ schoolName }),
  setSchoolType: (schoolType) => set({ schoolType }),
  setIncomeBracket: (incomeBracket) => set({ incomeBracket }),
  // Any pick, 선택 안 함 included, is the user's answer.
  setEmploymentStatus: (employmentStatus) => set({ employmentStatus }),
  setOptional: (optional) => set({ optional }),
  chooseEnrollment: (enrollmentStatus, schoolType) => set({
    enrollmentStatus, schoolType,
    ...(!enrollmentStatus && { schoolName: "" }),
  }),

  // Legacy — kept for backward compatibility with any existing callers
  setStep1: (region, birthYear) => {
    const age = String(new Date().getFullYear() - birthYear);
    set({ region, age, birthYear });
  },

  reset: () =>
    set({
      nickname: "",
      region: "",
      cityCode: "",
      age: "",
  birthYear: 0,
      birthDate: "",
      enrollmentStatus: "",
      schoolName: "",
      schoolType: "",
      incomeBracket: null,
      employmentStatus: "",
      optional: EMPTY_OPTIONAL_ANSWERS,
    }),
}));

/**
 * Derive birth year from age string outside the store.
 * Usage: getBirthYear(store.age)
 */
export function getBirthYear(age: string): number {
  const parsed = parseInt(age, 10);
  if (isNaN(parsed) || parsed <= 0) return 0;
  return new Date().getFullYear() - parsed;
}
