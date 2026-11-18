// Canonical profile codes shared with the server contract.
// Clients SEND these codes and DISPLAY the Korean labels. Values persisted by
// older builds (Korean labels, legacy English keys) are mapped before sending.

export const ENROLLMENT_CODES = ["enrolled", "on_leave", "graduated", "research_student"] as const;
export type EnrollmentCode = (typeof ENROLLMENT_CODES)[number];

export const EMPLOYMENT_CODES = ["employed", "new_hire", "unemployed", "preparing", "self_employed", "student", "other"] as const;
export type EmploymentCode = (typeof EMPLOYMENT_CODES)[number];

export const ENROLLMENT_LABELS: Record<EnrollmentCode, string> = {
  enrolled: "재학", on_leave: "휴학", graduated: "졸업", research_student: "수료 후 연구생",
};

export const EMPLOYMENT_LABELS: Record<EmploymentCode, string> = {
  employed: "재직", new_hire: "신입 재직", unemployed: "미취업", preparing: "취업 준비",
  self_employed: "자영업", student: "학생", other: "기타",
};

/** A 학적 pick: the status changes and the school goes with "no status". 취업 상태 is left as answered. */
export function pickEnrollment<D extends { enrollmentStatus: string; schoolName: string }>(draft: D, status: string): D {
  return { ...draft, enrollmentStatus: status, ...(!status && { schoolName: "" }) };
}

// Keys are compared with whitespace removed.
const ENROLLMENT_ALIASES: Record<string, EnrollmentCode> = {
  "재학": "enrolled", "재학중": "enrolled", "대학생": "enrolled", "졸업예정": "enrolled",
  "휴학": "on_leave", "휴학중": "on_leave", "휴학생": "on_leave", leave: "on_leave", leave_of_absence: "on_leave",
  "졸업": "graduated", "졸업생": "graduated",
  "수료후연구생": "research_student", "연구생": "research_student",
};

const EMPLOYMENT_ALIASES: Record<string, EmploymentCode> = {
  "재직": "employed", "재직중": "employed", "직장인": "employed",
  "신입": "new_hire", "신입사원": "new_hire",
  "미취업": "unemployed", "무직": "unemployed",
  "취업준비생": "preparing", "취업준비": "preparing", "구직중": "preparing",
  "자영업": "self_employed", "사업자": "self_employed",
  "학생": "student", "재학생": "student",
  "기타": "other",
};

function normalize<T extends string>(value: string | null | undefined, codes: readonly T[], aliases: Record<string, T>): string | null {
  const key = value?.trim() ?? "";
  if (!key) return null;
  if ((codes as readonly string[]).includes(key)) return key;
  // Unknown values pass through so the server, not the client, decides.
  return aliases[key.replace(/\s+/g, "")] ?? key;
}

/** Canonical enrollment code for a stored or displayed value; null when empty. */
export function toEnrollmentCode(value: string | null | undefined): string | null {
  return normalize(value, ENROLLMENT_CODES, ENROLLMENT_ALIASES);
}

/** Canonical employment code for a stored or displayed value; null when empty. */
export function toEmploymentCode(value: string | null | undefined): string | null {
  return normalize(value, EMPLOYMENT_CODES, EMPLOYMENT_ALIASES);
}

// An unrecognised value is shown as written only when it is already Korean
// text; a raw code reads as "기타".
const shown = (value: string) => (/[가-힣]/.test(value) ? value : "기타");

/** Korean label for an enrollment value; "" when empty, "기타" for an unrecognised code. */
export function enrollmentLabel(value: string | null | undefined): string {
  const code = toEnrollmentCode(value);
  return code ? ENROLLMENT_LABELS[code as EnrollmentCode] ?? shown(code) : "";
}

/** Korean label for an employment value; "" when empty, "기타" for an unrecognised code. */
export function employmentLabel(value: string | null | undefined): string {
  const code = toEmploymentCode(value);
  return code ? EMPLOYMENT_LABELS[code as EmploymentCode] ?? shown(code) : "";
}
