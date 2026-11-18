// Canonical profile codes shared with the server contract.
// The client SENDS these codes and DISPLAYS the Korean labels.

export const ENROLLMENT_CODES = ["enrolled", "on_leave", "graduated", "research_student"] as const;
export type EnrollmentCode = (typeof ENROLLMENT_CODES)[number];

export const EMPLOYMENT_CODES = ["employed", "new_hire", "unemployed", "preparing", "self_employed", "student", "other"] as const;
export type EmploymentCode = (typeof EMPLOYMENT_CODES)[number];

// Labels match apps/mobile/lib/profileCodes.ts so the same answer reads the
// same on both clients.
export const ENROLLMENT_LABELS: Record<EnrollmentCode, string> = {
  enrolled: "재학",
  on_leave: "휴학",
  graduated: "졸업",
  research_student: "수료 후 연구생",
};

export const EMPLOYMENT_LABELS: Record<EmploymentCode, string> = {
  employed: "재직",
  new_hire: "신입 재직",
  unemployed: "미취업",
  preparing: "취업 준비",
  self_employed: "자영업",
  student: "학생",
  other: "기타",
};

export function isEnrollmentCode(value: string): value is EnrollmentCode {
  return (ENROLLMENT_CODES as readonly string[]).includes(value);
}

export function isEmploymentCode(value: string): value is EmploymentCode {
  return (EMPLOYMENT_CODES as readonly string[]).includes(value);
}

// An unrecognised value is shown as written only when it is already Korean
// text; a raw code reads as "기타".
const shown = (value: string) => (/[가-힣]/.test(value) ? value : "기타");

/** Korean label for a stored code; "" when empty, "기타" for an unrecognised code. */
export function enrollmentLabel(code: string | null | undefined): string {
  if (!code) return "";
  return isEnrollmentCode(code) ? ENROLLMENT_LABELS[code] : shown(code);
}

/** Korean label for a stored code; "" when empty, "기타" for an unrecognised code. */
export function employmentLabel(code: string | null | undefined): string {
  if (!code) return "";
  return isEmploymentCode(code) ? EMPLOYMENT_LABELS[code] : shown(code);
}
