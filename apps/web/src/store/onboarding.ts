import { create } from "zustand";
import { EMPTY_OPTIONAL_ANSWERS, type OptionalAnswers } from "@/lib/optionalAnswers";
import type { FieldAnswer } from "@/lib/profileSave";

export interface OnboardingState {
  /** Required on step 1 (lib/nickname); sent with "이 조건 저장". */
  nickname: string;
  region: string;
  birthYear: number;
  /** Canonical enrollment code (lib/profileCodes); undefined when step 2's choice does not speak to it. */
  enrollmentStatus: FieldAnswer;
  /** Canonical employment code (lib/profileCodes); undefined when step 2's choice does not speak to it. */
  employmentStatus: FieldAnswer;
  /** True once step 2 was answered, even when both codes are empty. */
  statusAnswered: boolean;
  /** Typed 학교명; undefined when left blank. */
  schoolName: FieldAnswer;
  incomeBracket: number | null;
  /** The optional 무주택 / 혼인·자녀 / 중위소득 answers; null = skipped. */
  optional: OptionalAnswers;
  setOptional: (optional: OptionalAnswers) => void;
  setStep1: (region: string, birthYear: number, nickname: string) => void;
  setStep2: (
    enrollmentStatus: FieldAnswer,
    employmentStatus: FieldAnswer,
    school: FieldAnswer,
    income: number | null
  ) => void;
  reset: () => void;
}

export const useOnboardingStore = create<OnboardingState>((set) => ({
  nickname: "",
  region: "",
  birthYear: 0,
  enrollmentStatus: undefined,
  employmentStatus: undefined,
  statusAnswered: false,
  schoolName: undefined,
  incomeBracket: null,
  optional: EMPTY_OPTIONAL_ANSWERS,

  setOptional: (optional) => set({ optional }),

  setStep1: (region, birthYear, nickname) => set({ region, birthYear, nickname: nickname.trim() }),

  setStep2: (enrollmentStatus, employmentStatus, schoolName, incomeBracket) =>
    set({ enrollmentStatus, employmentStatus, statusAnswered: true, schoolName, incomeBracket }),

  reset: () =>
    set({
      nickname: "",
      region: "",
      birthYear: 0,
      enrollmentStatus: undefined,
      employmentStatus: undefined,
      statusAnswered: false,
      schoolName: undefined,
      incomeBracket: null,
      optional: EMPTY_OPTIONAL_ANSWERS,
    }),
}));
