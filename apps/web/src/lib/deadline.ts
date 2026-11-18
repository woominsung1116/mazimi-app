// Deadlines in Korea's calendar; the same rules as apps/mobile/lib/deadline.ts.
// A date-only "YYYY-MM-DD" (recommendation `deadline`) is a KST calendar day,
// not UTC midnight: parsed as UTC it would read one day early before 09:00 KST.

const KST_OFFSET_MS = 9 * 3600_000;
const DAY_MS = 86400_000;
const DATE_ONLY = /^(\d{4})-(\d{2})-(\d{2})$/;

type DeadlineFields = {
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

/** The server's `effective_deadline`, else the earliest of the three end dates. */
export function effectiveDeadline(program: DeadlineFields): string | null {
  if (program.effective_deadline) return program.effective_deadline;
  const candidates = [program.deadline_at, program.application_end_at, program.benefit_valid_until]
    .filter((value): value is string => !!value && Number.isFinite(deadlineEndMs(value)));
  if (!candidates.length) return null;
  return candidates.reduce((min, value) => (deadlineEndMs(value) < deadlineEndMs(min) ? value : min));
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

/** "2026년 10월 21일" ("2026. 10. 21." with `month` "numeric"): the KST day of the last instant before the deadline. */
export function formatDeadlineDate(value: string, month: "long" | "numeric" = "long"): string | null {
  const end = deadlineEndMs(value);
  if (!Number.isFinite(end)) return null;
  return new Date(end - 1).toLocaleDateString("ko-KR", { timeZone: "Asia/Seoul", year: "numeric", month, day: "numeric" });
}
