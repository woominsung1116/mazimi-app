import { ApiError, type AiExtractionAutoCheck } from "@/lib/api";

/** Korean names for AI extraction fact types, shared by the queue and the
 * per-program review panel. */
export const typeName: Record<string, string> = {
  age: "연령",
  region: "거주 지역",
  enrollment: "학적",
  employment: "취업 상태",
  income: "소득 기준",
  document: "제출 서류",
  application_window: "신청 기간",
};

export const statusName: Record<string, string> = {
  pending: "검토 대기",
  flagged: "낮은 신뢰·경고",
  invalid: "검증 실패",
  approved: "승인됨",
  rejected: "거절됨",
  superseded: "원문 변경으로 대체",
  reverted: "자동 반영 취소됨",
};

/** Why an agreeing fact was not applied automatically (`auto_check.hold`). */
const holdName: Record<string, string> = {
  auto_apply_disabled: "자동 반영이 꺼져 있음",
  type_not_enabled: "자동 반영 대상 유형이 아님",
  run_cap_reached: "실행당 자동 반영 상한 도달",
  overwrite_requires_confirmation: "기존 값과 다름",
  manual_edit_required: "규칙 구조상 수동 수정 필요",
  human_decision_exists: "같은 사실을 사람이 거절·취소함",
  auto_verification_failed: "자동 검증 재확인 실패",
  source_changed: "원문 변경됨",
  evidence_invalid: "근거 재검증 실패",
  apply_error: "반영 중 오류",
};

/** One line for the reviewer: did the rule parser read the same value? */
export function autoCheckLabel(check: AiExtractionAutoCheck | null): string | null {
  if (!check) return null;
  if (check.verdict === "agree") {
    const hold = check.hold ? ` · 보류: ${holdName[check.hold] ?? check.hold}` : "";
    return `규칙 검증 일치 (${check.rule ?? "-"})${hold}`;
  }
  const reason = check.reason ? `: ${check.reason}` : "";
  return check.verdict === "disagree" ? `규칙 검증 불일치${reason}` : `규칙으로 읽을 수 없음${reason}`;
}

export function show(value: unknown): string {
  if (value === null || value === undefined) return "없음";
  if (typeof value === "string") {
    const time = /^\d{4}-\d{2}-\d{2}T/.test(value) ? Date.parse(value) : NaN;
    return Number.isNaN(time) ? value : new Date(time).toLocaleString("ko-KR", { timeZone: "Asia/Seoul" });
  }
  return JSON.stringify(value);
}

export function describe(fact: { fact_type: string; value: Record<string, unknown> }): string {
  const v = fact.value;
  switch (fact.fact_type) {
    case "age":
      return `만 ${v.min_age ?? "?"}세 ~ ${v.max_age ?? "?"}세 (기준: ${v.basis === "man_age" ? "만나이" : String(v.basis)})`;
    case "region":
    case "enrollment":
    case "employment":
      return ((v.codes ?? v.statuses) as string[] | undefined)?.join(", ") ?? show(v);
    case "income": {
      const name =
        v.criterion === "median_income_percent" ? "기준 중위소득 %" : v.criterion === "income_decile" ? "소득분위" : "학자금 지원구간";
      return `${name} ${String(v.comparator)} ${String(v.value)}`;
    }
    case "document":
      return `${String(v.name)} (${v.required ? "필수" : "선택"})${v.notes ? ` · ${String(v.notes)}` : ""}`;
    case "application_window":
      return v.kind === "dated"
        ? `${v.start_date ?? "?"} ~ ${v.end_date ?? "?"} (KST)`
        : v.kind === "always_open"
          ? "상시 모집"
          : "예산 소진 시 마감";
    default:
      return show(v);
  }
}

export function errorMessage(error: unknown): string {
  if (error instanceof ApiError) {
    const body = error.body as { error?: unknown; code?: unknown } | undefined;
    if (error.status === 401 || error.status === 403) return "관리자 권한을 다시 확인하세요.";
    if ((error.status === 400 || error.status === 409) && typeof body?.error === "string") return body.error;
    return "요청을 처리하지 못했습니다. 잠시 후 다시 시도하세요.";
  }
  return "서버에 연결하지 못했습니다. 네트워크를 확인하세요.";
}
