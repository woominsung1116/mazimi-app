// User-facing labels for stored codes. Kept byte-identical at
// apps/web/src/lib/displayLabels.ts and apps/mobile/constants/displayLabels.ts
// (each next to its districts.generated.ts); the clients' check scripts
// assert both copies match and compare the lists with crates/core. Display
// only: stored values are never changed.

import { DISTRICT_REGION_LABELS, type DistrictRegion } from "./districts.generated";

// ── Program types ───────────────────────────────────────────────────────────

/**
 * Canonical program_type keys with display labels: a copy of
 * mazimi_core::PROGRAM_TYPES (crates/core/src/lib.rs). A core test asserts the
 * two lists are identical; change both together. Admin forms submit the key.
 */
export const PROGRAM_TYPES: { value: string; label: string }[] = [
  { value: "scholarship", label: "장학금" },
  { value: "policy", label: "정책" },
  { value: "support", label: "청년정책" },
  { value: "housing", label: "주거" },
  { value: "benefit", label: "생활" },
  { value: "welfare", label: "복지" },
  { value: "financial_product", label: "금융" },
  { value: "employment", label: "취업" },
  { value: "corporate_benefit", label: "기업혜택" },
  { value: "campus_benefit", label: "대학" },
];

/** Legacy keys still read from older rows. */
const PROGRAM_TYPE_ALIASES = new Map([["youth_policy", "청년정책"], ["financial", "금융"]]);

/** Shown for a program_type key with no label. Never the key itself. */
export const PROGRAM_TYPE_FALLBACK = "혜택";

/** Korean label of a program_type key. */
export function programTypeLabel(programType: string): string {
  return PROGRAM_TYPES.find(t => t.value === programType)?.label
    ?? PROGRAM_TYPE_ALIASES.get(programType)
    ?? PROGRAM_TYPE_FALLBACK;
}

// ── Regions ─────────────────────────────────────────────────────────────────

/** Neutral line for a programme whose regions have no label. */
export const REGION_UNKNOWN_LABEL = "지역 정보 확인 필요";

const SIDO_CODES = Object.keys(DISTRICT_REGION_LABELS) as DistrictRegion[];
const NATIONWIDE = new Set(["nationwide", "all", "전국"]);
const HANGUL_NAME = /^[가-힣 ]+$/;

/**
 * Full and older names, as mazimi_core::canonical_region_code reads them
 * (codes and short labels come from DISTRICT_REGION_LABELS). 광주시 is a city
 * in 경기, not 광주광역시, so it is absent. A check script compares this table
 * with the Rust match arms.
 */
export const REGION_NAME_ALIASES: Record<DistrictRegion, readonly string[]> = {
  seoul: ["서울시", "서울특별시"],
  busan: ["부산시", "부산광역시"],
  daegu: ["대구시", "대구광역시"],
  incheon: ["인천시", "인천광역시"],
  gwangju: ["광주광역시"],
  daejeon: ["대전시", "대전광역시"],
  ulsan: ["울산시", "울산광역시"],
  sejong: ["세종시", "세종특별자치시"],
  gyeonggi: ["경기도"],
  gangwon: ["강원도", "강원특별자치도"],
  chungbuk: ["충청북도"],
  chungnam: ["충청남도"],
  jeonbuk: ["전라북도", "전북특별자치도"],
  jeonnam: ["전라남도"],
  gyeongbuk: ["경상북도"],
  gyeongnam: ["경상남도"],
  jeju: ["제주도", "제주특별자치도"],
};

/** Canonical 시·도 code for a code in any case, its short label or a full name; null otherwise. */
function sidoCode(value: string): DistrictRegion | null {
  const key = value.trim().toLowerCase();
  return SIDO_CODES.find(code => code === key || DISTRICT_REGION_LABELS[code] === key || REGION_NAME_ALIASES[code].includes(key)) ?? null;
}

/**
 * Korean label of a stored region value, as mazimi_core::region_display:
 * "ulsan", "Ulsan" or "울산광역시" → "울산", "ulsan:남구" → "울산 남구",
 * "전국". A district that is not a Korean name (e.g. a 5-digit code) is
 * dropped. "" for "unknown" and any value without a label; never the raw value.
 */
export function regionLabel(value: string | null | undefined): string {
  const trimmed = value?.trim() ?? "";
  if (!trimmed) return "";
  if (NATIONWIDE.has(trimmed.toLowerCase())) return "전국";
  const colon = trimmed.indexOf(":");
  const code = sidoCode(colon < 0 ? trimmed : trimmed.slice(0, colon));
  if (!code) return "";
  const label = DISTRICT_REGION_LABELS[code];
  const district = colon < 0 ? "" : trimmed.slice(colon + 1).trim();
  return HANGUL_NAME.test(district) ? `${label} ${district}` : label;
}

/** Distinct labels of a region list in order; ["전국"] when it is nationwide. */
function regionLabels(values: readonly string[]): string[] {
  // An empty list is confirmed nationwide (crates/worker/src/regions.rs).
  if (values.length === 0) return ["전국"];
  const labels: string[] = [];
  const sidos = new Set<DistrictRegion>();
  for (const value of values) {
    const label = regionLabel(value);
    if (!label) continue;
    const code = value.includes(":") ? null : sidoCode(value);
    if (code) sidos.add(code);
    if (!labels.includes(label)) labels.push(label);
  }
  return labels.includes("전국") || sidos.size === SIDO_CODES.length ? ["전국"] : labels;
}

/**
 * One line for a region list: labels joined with " · "; "전국" for an empty
 * list or one naming every 시·도; "" for null (not recorded) or when no value
 * has a label (the caller then shows nothing or REGION_UNKNOWN_LABEL).
 */
export function regionsLabel(values: readonly string[] | null | undefined): string {
  return values ? regionLabels(values).join(" · ") : "";
}

type RegionScoped = { regions?: readonly string[] | null; program_type?: string | null };

/** regionsLabel of a programme; "" for a school-scoped campus offer, whose region says nothing about eligibility. */
export function programRegionsLabel(program: RegionScoped): string {
  return program.program_type === "campus_benefit" ? "" : regionsLabel(program.regions);
}

/** Short chip text of a programme's regions: the first label plus a count ("부산 외 3"). */
export function programRegionsChip(program: RegionScoped): string {
  if (program.program_type === "campus_benefit" || !program.regions) return "";
  const labels = regionLabels(program.regions);
  return labels.length > 1 ? `${labels[0]} 외 ${labels.length - 1}` : labels[0] ?? "";
}

// ── 신청방법 ─────────────────────────────────────────────────────────────────

// 보조금24 lists 신청방법 as codes joined by "||", e.g.
// "정부24온라인신청||방문신청||직접입력"; "직접입력" only marks that the agency
// typed its own text elsewhere.
const METHOD_LABELS = new Map([
  ["정부24온라인신청", "정부24 온라인 신청"],
  ["기타 온라인신청", "온라인 신청"],
  ["방문신청", "방문 신청"],
  ["신청불필요", "별도 신청 없음"],
  ["직접입력", ""],
]);

/** Display text of a programme's 신청방법; other text is kept as written, "" when nothing is left. */
export function applicationMethodText(value: string | null | undefined): string {
  return (value ?? "")
    .split("||")
    .map(part => part.trim())
    .map(part => METHOD_LABELS.get(part) ?? part)
    .filter(Boolean)
    .join(" · ");
}
