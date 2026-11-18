"use client";

import { daysLeft } from "@/lib/deadline";

interface DeadlineBadgeProps {
  /** An end date; pass `effectiveDeadline(program)`, the earliest of the programme's end dates. */
  deadline: string;
}

export default function DeadlineBadge({ deadline }: DeadlineBadgeProps) {
  const dDay = daysLeft(deadline);
  if (Number.isNaN(dDay)) return null;

  if (dDay < 0) {
    return (
      <span className="inline-flex items-center rounded-full bg-gray-100 px-2.5 py-0.5 text-xs font-medium text-gray-500">
        마감
      </span>
    );
  }

  let colorClasses: string;
  let ariaLabel: string;

  if (dDay === 0) {
    colorClasses = "bg-red-100 text-red-700";
    ariaLabel = "오늘 마감";
  } else if (dDay <= 3) {
    colorClasses = "bg-red-100 text-red-700";
    ariaLabel = `${dDay}일 후 마감`;
  } else if (dDay <= 7) {
    colorClasses = "bg-amber-100 text-amber-700";
    ariaLabel = `${dDay}일 후 마감`;
  } else {
    colorClasses = "bg-gray-100 text-gray-600";
    ariaLabel = `${dDay}일 후 마감`;
  }

  const label = dDay === 0 ? "D-Day" : `D-${dDay}`;

  return (
    <span
      aria-label={ariaLabel}
      className={`inline-flex items-center rounded-full px-2.5 py-0.5 text-xs font-semibold ${colorClasses}`}
    >
      {label}
    </span>
  );
}
