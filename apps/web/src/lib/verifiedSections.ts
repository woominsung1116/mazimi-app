import type { RecommendationItem, RecommendationResult } from "./api";

// Same rules as apps/mobile/lib/verifiedSections.ts.

// "조건 확인됨" / "확인 필요" (plan "verified conditions"). `verified` is the
// only field an eligibility claim may rest on, and only while the server's
// `verified_section_enabled` flag is on. With the flag off nothing changes:
// cards keep the `eligibility_status` wording and the list stays one list.

export type CardStatus = "verified" | "pass" | "unknown";

/** What a recommendation card says about eligibility. */
export function cardStatus(item: Pick<RecommendationItem, "verified" | "eligibility_status">, sectionEnabled: boolean): CardStatus {
  if (sectionEnabled) return item.verified === true ? "verified" : "unknown";
  return item.eligibility_status === "pass" ? "pass" : "unknown";
}

/** Labels of what is left to check, shown on 확인 필요 rows while the sections are on. */
export function remainingLabels(item: Pick<RecommendationItem, "verified" | "remaining_checks">, sectionEnabled: boolean): string[] {
  if (!sectionEnabled || item.verified === true) return [];
  const labels: string[] = [];
  for (const check of item.remaining_checks ?? []) if (check.label && !labels.includes(check.label)) labels.push(check.label);
  return labels;
}

export interface Sections<T> {
  verified: T[];
  needsCheck: T[];
}

/** The two sections, in server order within each; null while the flag is off. */
export function verifiedSections<T extends Pick<RecommendationItem, "verified">>(
  result: Pick<RecommendationResult, "verified_section_enabled">,
  items: T[],
): Sections<T> | null {
  if (result.verified_section_enabled !== true) return null;
  return {
    verified: items.filter(item => item.verified === true),
    needsCheck: items.filter(item => item.verified !== true),
  };
}
