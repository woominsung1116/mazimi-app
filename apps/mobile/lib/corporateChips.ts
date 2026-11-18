// 기업혜택 category chips (GET /api/v1/programs/corporate-categories). Pure, so
// scripts/check-corporate-categories.cjs can run it without React.
import type { CorporateCategoryCounts } from "./api";

export interface CorporateChip { code: string; label: string; count: number }

/** The list filters the counts must share so a chip's number is its list's total. */
export interface ChipFilters { q?: string; status?: string; region?: string; income_band?: string }

/** Same q, status, region and 소득 구간 as the list; the counts endpoint keeps to the corporate tab itself. */
export function countFilters(filters: ChipFilters): ChipFilters {
  const out: ChipFilters = {};
  if (filters.q) out.q = filters.q;
  if (filters.status) out.status = filters.status;
  if (filters.region) out.region = filters.region;
  if (filters.income_band) out.income_band = filters.income_band;
  return out;
}

/**
 * 전체 first, then the server's chips in its order. None on a server without the
 * category axis (`supported` false or still unknown), without counts, or with
 * fewer than two categories (one chip would filter to the same list as 전체). `numbers` false hides the counts (they belong to
 * other filters while the new ones load); a chip shows its number when count > 0.
 */
export function corporateChips(counts: CorporateCategoryCounts | undefined, supported: boolean | undefined, numbers = true): CorporateChip[] {
  if (supported !== true || !counts || counts.items.length < 2) return [];
  return [{ code: "", label: "전체", count: 0 }, ...counts.items.map(c => ({ ...c, count: numbers ? c.count : 0 }))];
}

/** corporate_category for the list: only on 기업혜택, only on a server that filters by it. */
export function chipQuery(selected: string, corporate: boolean, supported: boolean | undefined): string | undefined {
  return corporate && supported === true && selected ? selected : undefined;
}

/** The selection to keep once fresh counts arrive: a chip they no longer show is dropped. */
export function keptSelection(selected: string, counts: CorporateCategoryCounts | undefined): string {
  if (!selected || !counts) return selected;
  return counts.items.some(c => c.code === selected) ? selected : "";
}
