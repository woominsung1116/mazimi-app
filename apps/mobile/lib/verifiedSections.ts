import type { RecommendationItem, RecommendationResult } from "./api";

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

/** A saved row's card status: cardStatus of its recommendation item, undefined when there is none
 * (or the recommendations have not loaded), so the card keeps its own default. */
export function savedCardStatus(items: readonly Pick<RecommendationItem, "program_id" | "verified" | "eligibility_status">[] | undefined, sectionEnabled: boolean, programId: string): CardStatus | undefined {
  const item = items?.find(candidate => candidate.program_id === programId);
  return item ? cardStatus(item, sectionEnabled) : undefined;
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
