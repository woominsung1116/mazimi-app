import React from "react";
import { BenefitCard, type BenefitCardProgram } from "./FigmaUI";
import type { RecommendationItem } from "@/lib/api";
import { cardStatus } from "@/lib/verifiedSections";

/** The card fields a recommendation item already carries (shared with the home list). */
export function recommendationCardProgram(data: RecommendationItem): BenefitCardProgram {
  return {
    id: data.program_id, title: data.title, title_display: data.title_display ?? null, program_type: data.program_type,
    deadline_at: data.deadline,
    provider_category: data.provider_category, provider_categories: data.provider_categories,
    field_category: data.field_category, field_label: data.field_label,
    summary: data.summary ?? null, summary_display: data.summary_display ?? null,
    provider_name: data.provider_name ?? null, company_name: data.company_name ?? null,
  };
}

/** Recommendations use the same source card as exploration (34065:2930).
 * The card body is the item's own fields (F6.6), with no per-row /programs/{id}
 * request (M2). A row without summary_display/summary shows no body line: the
 * card never builds a sentence from the match reasons (the status chips carry them).
 * Keep unknown eligibility explicit; the detailed checks live on the benefit.
 * `sectionEnabled` = the server's `verified_section_enabled`: only then does a
 * row say 조건 확인됨 (`verified`). */
export default function RecommendationCard({ data, sectionEnabled = false }: { data: RecommendationItem; sectionEnabled?: boolean }) {
  return <BenefitCard program={recommendationCardProgram(data)} eligibility={cardStatus(data, sectionEnabled)} />;
}
