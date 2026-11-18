// Deadlines in Korea's calendar. A date-only "YYYY-MM-DD" (recommendation
// `deadline`) is a KST calendar day, not UTC midnight: parsed as UTC it would
// read one day early before 09:00 KST.

const KST_OFFSET_MS = 9 * 3600_000;
const DAY_MS = 86400_000;
const DATE_ONLY = /^(\d{4})-(\d{2})-(\d{2})$/;

type DeadlineFields = {
  /** v1 feedback server: KST date of the deadline ("YYYY-MM-DD"). Preferred when present. */
  deadline_date?: string | null;
  effective_deadline?: string | null;
  deadline_at?: string | null;
  application_end_at?: string | null;
  benefit_valid_until?: string | null;
};

/** Day number of the KST calendar day of `ms`. */
function kstDay(ms: number): number {
  return Math.floor((ms + KST_OFFSET_MS) / DAY_MS);
}

/** Instant a deadline value ends: a date-only value ends at 24:00 KST that day. */
export function deadlineEndMs(value: string): number {
  const date = DATE_ONLY.exec(value);
  if (date) return Date.UTC(+date[1], +date[2] - 1, +date[3] + 1) - KST_OFFSET_MS;
  return Date.parse(value);
}

/** Days from today to the deadline's day, both in KST; negative once it passed. */
export function daysLeft(value: string, now = Date.now()): number {
  const end = deadlineEndMs(value);
  if (!Number.isFinite(end)) return NaN;
  if (end <= now) return -1;
  // The last instant still inside the deadline decides its day.
  return kstDay(end - 1) - kstDay(now);
}

/** The server's `deadline_date` or `effective_deadline`, else the earliest of the three end dates. */
export function effectiveDeadline(program: DeadlineFields): string | null {
  if (program.deadline_date && DATE_ONLY.test(program.deadline_date)) return program.deadline_date;
  if (program.effective_deadline) return program.effective_deadline;
  const candidates = [program.deadline_at, program.application_end_at, program.benefit_valid_until]
    .filter((value): value is string => !!value && Number.isFinite(deadlineEndMs(value)));
  if (!candidates.length) return null;
  return candidates.reduce((min, value) => (deadlineEndMs(value) < deadlineEndMs(min) ? value : min));
}

/** Red from D-2 down (D-2, D-1, 오늘 마감): the one D-day colour rule (feedback F4.6). */
export const URGENT_DAYS = 2;
/** Home 마감 임박 shows only deadlines this many days away or fewer (F4.4). */
export const IMMINENT_DAYS = 7;

export type DeadlineTone = "urgent" | "open" | "closed" | "none";

/** Badge and text colours per tone. Every D-day in the app reads these. */
export const DEADLINE_TONE_COLORS: Record<DeadlineTone, { background: string; text: string; onBackground: string }> = {
  // Figma D-day badges 34065:6204 (Orange) and 34065:6216 (Primary); ended/rolling read as DeepGray.
  urgent: { background: "#F75B2B", text: "#F75B2B", onBackground: "#FFFFFF" },
  open: { background: "#34BB84", text: "#239969", onBackground: "#FFFFFF" },
  closed: { background: "#8894A1", text: "#8894A1", onBackground: "#FFFFFF" },
  none: { background: "#8894A1", text: "#8894A1", onBackground: "#FFFFFF" },
};

/** Days left to the programme's deadline (KST), or null when it has none. */
export function programDaysLeft(program: DeadlineFields, now = Date.now()): number | null {
  const end = effectiveDeadline(program);
  if (!end) return null;
  const days = daysLeft(end, now);
  return Number.isNaN(days) ? null : days;
}

/** Tone of the programme's D-day: urgent at D-2 and below, closed once passed, none without a date. */
export function deadlineTone(
  program: DeadlineFields & { program_status?: string; available?: boolean },
  now = Date.now(),
): DeadlineTone {
  if (program.available === false || program.program_status === "closed") return "closed";
  const days = programDaysLeft(program, now);
  if (days === null) return "none";
  if (days < 0) return "closed";
  return days <= URGENT_DAYS ? "urgent" : "open";
}

/** Still open and ending within `IMMINENT_DAYS` days (today counts). */
export function isImminent(program: DeadlineFields & { program_status?: string }, now = Date.now()): boolean {
  if (program.program_status === "closed") return false;
  const days = programDaysLeft(program, now);
  return days !== null && days >= 0 && days <= IMMINENT_DAYS;
}

/** Card label: 마감·종료 (no longer in the catalogue), 마감, 오늘 마감, D-n, 상시 or 일정 확인. */
export function deadlineLabel(
  program: DeadlineFields & { program_status?: string; available?: boolean },
  now = Date.now(),
): string {
  if (program.available === false) return "마감·종료";
  if (program.program_status === "closed") return "마감";
  const end = effectiveDeadline(program);
  if (!end) return program.program_status === "always_open" ? "상시" : "일정 확인";
  const days = daysLeft(end, now);
  if (Number.isNaN(days)) return "일정 확인";
  return days < 0 ? "마감" : days === 0 ? "오늘 마감" : `D-${days}`;
}
