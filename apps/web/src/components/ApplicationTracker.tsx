"use client";

import type { ApplicationStatus } from "@/lib/api";

interface ApplicationTrackerProps {
  status: ApplicationStatus;
  onChange?: (status: ApplicationStatus) => void;
  disabled?: boolean;
}

// Linear steps. "abandoned" sits outside the line and is rendered separately.
const STEPS: { value: ApplicationStatus; label: string }[] = [
  { value: "interested", label: "관심" },
  { value: "planning", label: "예정" },
  { value: "applying", label: "진행중" },
  { value: "applied", label: "완료" },
  { value: "waiting", label: "대기" },
  { value: "received", label: "수혜" },
];

const STEP_ACTIVE_COLORS: Record<ApplicationStatus, string> = {
  interested: "bg-gray-500",
  planning: "bg-teal-600",
  applying: "bg-yellow-500",
  applied: "bg-green-600",
  waiting: "bg-orange-500",
  received: "bg-purple-600",
  abandoned: "bg-red-500",
};

export default function ApplicationTracker({
  status,
  onChange,
  disabled = false,
}: ApplicationTrackerProps) {
  const abandoned = status === "abandoned";
  const currentIndex = STEPS.findIndex((s) => s.value === status);

  return (
    <div className="w-full space-y-2">
      <div className="flex items-center gap-1 w-full">
        {STEPS.map((step, i) => {
          const isActive = !abandoned && i <= currentIndex;
          const isCurrent = step.value === status;
          return (
            <button
              key={step.value}
              type="button"
              disabled={disabled}
              aria-pressed={isCurrent}
              onClick={() => onChange?.(step.value)}
              className="flex-1 flex flex-col items-center gap-1 group disabled:opacity-50"
            >
              <div
                className={`h-2 w-full rounded-full transition-colors ${
                  isActive
                    ? STEP_ACTIVE_COLORS[status]
                    : "bg-gray-200"
                }`}
              />
              <span
                className={`text-xs transition-colors ${
                  isCurrent
                    ? "font-semibold text-gray-900"
                    : isActive
                      ? "text-gray-600"
                      : "text-gray-400"
                }`}
              >
                {step.label}
              </span>
            </button>
          );
        })}
      </div>
      <button
        type="button"
        disabled={disabled}
        aria-pressed={abandoned}
        onClick={() => onChange?.("abandoned")}
        className={`inline-flex items-center gap-1.5 rounded-full border px-2.5 py-1 text-xs transition-colors disabled:opacity-50 ${
          abandoned
            ? "border-red-200 bg-red-50 font-semibold text-red-700"
            : "border-gray-200 text-gray-500 hover:border-red-200 hover:text-red-600"
        }`}
      >
        <span className={`h-2 w-2 rounded-full ${abandoned ? STEP_ACTIVE_COLORS.abandoned : "bg-gray-300"}`} />
        {abandoned ? "신청을 중단했어요" : "신청 중단으로 표시"}
      </button>
    </div>
  );
}
