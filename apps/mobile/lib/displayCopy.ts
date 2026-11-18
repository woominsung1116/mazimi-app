// Pure copy rules of the home / explore / detail screens (v1.0.0 feedback).
// Kept free of React Native so scripts/check-home-feedback.cjs can load them.
import { daysLeft, deadlineLabel } from "./deadline";
import { formatBenefit, hasBenefitAmount, summaryLinesOf, type ApiProgram, type RecommendationItem } from "./api";
import { cardStatus } from "./verifiedSections";
import { normalizeDisplayText } from "./displayText";

/** "○○을" / "○○를": the object particle after a title (관심 혜택 해제 34068:5419). A title that
 * does not end in a Hangul syllable (a digit, a bracket) keeps the neutral "을(를)". */
export function withObjectParticle(word: string): string {
  const text = word.trim();
  const code = text.charCodeAt(text.length - 1) - 0xac00;
  if (!text || !(code >= 0 && code <= 11171)) return `${text}을(를)`;
  return `${text}${code % 28 === 0 ? "를" : "을"}`;
}

type AmountRow = Pick<RecommendationItem, "verified" | "eligibility_status"> & { program_type: string; benefit_amount_monthly?: number | null; benefit_amount_semester?: number | null };
/** 홈 hero "지원금 최대" (34065:6176): the largest amount a candidate itself states, compared per
 * year (monthly × 12, semester × 2) and shown in its own unit ("월 20만원"). Never a sum: the
 * candidates may not be combinable. Only 조건 확인됨 rows count (cardStatus with the result's own
 * `verified_section_enabled`): the hero invites an application, so it never quotes a row the server
 * has not confirmed. null when no such row states an amount (the hero keeps the count). */
export function largestStatedAmount(result: { verified_section_enabled?: boolean; items: readonly AmountRow[] } | undefined): string | null {
  const sectionEnabled = result?.verified_section_enabled === true;
  let best: { yearly: number; label: string } | null = null;
  for (const item of result?.items ?? []) {
    if (item.program_type === "corporate_benefit" || cardStatus(item, sectionEnabled) !== "verified") continue;
    const monthly = item.benefit_amount_monthly ?? 0, semester = item.benefit_amount_semester ?? 0;
    const options = [
      monthly > 0 ? { yearly: monthly * 12, label: formatBenefit({ ...item, benefit_amount_monthly: monthly, benefit_amount_semester: null } as ApiProgram) } : null,
      semester > 0 ? { yearly: semester * 2, label: formatBenefit({ ...item, benefit_amount_monthly: null, benefit_amount_semester: semester } as ApiProgram) } : null,
    ];
    for (const option of options) if (option && option.label && (!best || option.yearly > best.yearly)) best = option;
  }
  return best?.label ?? null;
}

/** 마감일 알림 설정 (34065:12485): the worker sends D-7, D-3 and D-1 alerts (crates/worker/src/alerts.rs);
 * only the days still ahead are named. null without a deadline date or with no alert day left
 * (then the bell toggles without the sheet). */
export function alertSheetMessage(deadlineDate: string | null | undefined, now = Date.now()): string | null {
  const match = /^(\d{4})-(\d{2})-(\d{2})$/.exec(deadlineDate ?? "");
  if (!match) return null;
  const left = daysLeft(match[0], now);
  // The worker runs on the hour, 09:00–20:00 KST (crates/worker/src/main.rs): today's alert
  // day still counts only before its last run at 20:00.
  const hourKst = new Date(now + 9 * 3600_000).getUTCHours();
  const ahead = [7, 3, 1].filter(d => left > d || (left === d && hourKst < 20));
  if (!ahead.length) return null;
  return `${match[1]}년 ${Number(match[2])}월 ${Number(match[3])}일 마감\n${ahead.map(d => `${d}일`).join("·")} 전에 알림을 보내드릴게요.`;
}

/** Guest gates on the detail screen (F7.3). */
export const GUEST_SAVE_REASON = "관심 혜택을 저장하려면 가입·로그인이 필요해요.";
export const GUEST_ALERT_REASON = "마감 알림을 받으려면 가입·로그인이 필요해요.";
export const GUEST_DOCUMENT_REASON = "서류를 직접 추가하려면 가입·로그인이 필요해요.";
export const GUEST_CONDITIONS_REASON = "내 조건으로 확인하려면 가입·로그인하고 조건을 입력해 주세요.";
export const GUEST_LOCAL_REASON = "내 지역 혜택을 보려면 가입·로그인하고 지역을 입력해 주세요.";
export const GUEST_RECOMMEND_REASON = "맞춤 혜택은 가입·로그인하고 내 조건을 입력하면 볼 수 있어요.";

/** The one quiet line under a programme: the official notice decides (no source labels or dates). */
export const FINAL_CHECK_LINE = "최종 조건은 공식 공고에서 확인해 주세요";

/** Wording of the official link (F6.7). */
export const OFFICIAL_LINK_LABEL = "공식 안내 페이지로 이동";

/** The server's generic destination label reads as "공식 안내 페이지로 이동"; specific ones stay. */
export function destinationButtonLabel(label: string | null | undefined): string {
  if (!label || label === "공식 안내에서 계속") return OFFICIAL_LINK_LABEL;
  return label;
}

export type SummarySection = { title: string; lines: string[] };

/** The notice says nothing is to be submitted (server flag `no_documents_required`). */
export const NO_DOCUMENTS_REQUIRED = "공고상 별도 제출 서류 없음";

/** 필요 서류 lines when there is no checklist: the "none required" sentence, else the notice's own text (empty when it has none). */
export function documentTextLines(program: Pick<ApiProgram, "no_documents_required" | "submission_documents">): string[] {
  return program.no_documents_required ? [NO_DOCUMENTS_REQUIRED] : normalizeDisplayText(program.submission_documents).lines;
}

/** "혜택 요약" sheet (F5.7): 정보 · 조건 · 서류 · 신청 방법 from detail data only; an empty section says where to check. */
export function summarySections(program: ApiProgram, conditions: string[], applicationText: string): SummarySection[] {
  const info = [
    ...summaryLinesOf(program),
    ...(hasBenefitAmount(program) ? [`지원 금액: ${formatBenefit(program)}`] : []),
    `운영: ${program.provider_name || program.company_name || "공식 안내 확인"}`,
    `기간: ${deadlineLabel(program)}`,
  ];
  const docs = program.required_documents?.length
    ? program.required_documents.map(doc => `${doc.document_name}${doc.is_required === false ? " (해당 시)" : ""}`)
    : documentTextLines(program);
  const how = normalizeDisplayText(applicationText).lines;
  return [
    { title: "정보", lines: info },
    { title: "조건", lines: conditions.length ? conditions : ["자격 조건은 공식 안내에서 확인해 주세요."] },
    { title: "서류", lines: [...(docs.length ? docs : ["필요 서류는 공식 안내에서 확인해 주세요."]), ...(program.required_document_notes ?? []).flatMap(note => normalizeDisplayText(note).lines)] },
    { title: "신청 방법", lines: how.length ? how : ["공식 신청처의 안내에 따라 신청해 주세요."] },
  ];
}
