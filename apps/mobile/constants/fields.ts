// Category tabs (v1.0.0 feedback F5.1, F5.3, F5.4).
// Two axes from the server (scratchpad api-contract §4): provider (정책/장학금/기업혜택)
// and field (주거/생활/교육/복지/금융/취업). A server without them still gets a
// working tab through the legacy program_type equivalents below.

export const PROVIDER_TABS = [
  { label: "정책", value: "policy" },
  { label: "장학금", value: "scholarship" },
  { label: "기업혜택", value: "corporate" },
] as const;

export const FIELD_TABS = [
  { label: "주거", value: "housing" },
  { label: "생활", value: "living" },
  { label: "교육", value: "education" },
  { label: "복지", value: "welfare" },
  { label: "금융", value: "finance" },
  { label: "취업", value: "employment" },
] as const;

export type ProviderCategory = (typeof PROVIDER_TABS)[number]["value"];
export type FieldCategory = (typeof FIELD_TABS)[number]["value"];

/** Tab state: "" (전체), "campus_benefit" (대학), "provider:<code>" or "field:<code>". */
export type CategoryTab = string;
export const ALL_TAB: CategoryTab = "";
export const CAMPUS_TAB: CategoryTab = "campus_benefit";
export const providerTab = (code: ProviderCategory): CategoryTab => `provider:${code}`;
export const fieldTab = (code: FieldCategory): CategoryTab => `field:${code}`;

const isProvider = (value: string): value is ProviderCategory => PROVIDER_TABS.some(tab => tab.value === value);
const isField = (value: string): value is FieldCategory => FIELD_TABS.some(tab => tab.value === value);

export function parseTab(tab: CategoryTab): { provider?: ProviderCategory; field?: FieldCategory; campus?: true } {
  if (tab === CAMPUS_TAB) return { campus: true };
  const [axis, code = ""] = tab.split(":");
  if (axis === "provider" && isProvider(code)) return { provider: code };
  if (axis === "field" && isField(code)) return { field: code };
  return {};
}

/** Legacy program_type per tab, for servers without ?provider / ?field. */
const LEGACY_PROGRAM_TYPE: Record<string, string> = {
  "provider:policy": "policy",
  "provider:scholarship": "scholarship",
  "provider:corporate": "corporate_benefit",
  "field:housing": "housing",
  "field:living": "benefit",
  // The server files every scholarship under 교육 (contract §4 rule 1).
  "field:education": "scholarship",
  "field:welfare": "welfare",
  "field:finance": "financial_product",
  "field:employment": "employment",
};

/** Old program_type values other screens still link with (e.g. deep links) → a tab. */
export function tabFromLegacy(value: string | undefined): CategoryTab {
  if (!value) return ALL_TAB;
  if (value === CAMPUS_TAB) return CAMPUS_TAB;
  if (parseTab(value).provider || parseTab(value).field) return value;
  const legacy = Object.entries(LEGACY_PROGRAM_TYPE).find(([, type]) => type === value);
  if (legacy) return legacy[0];
  if (value === "financial") return fieldTab("finance");
  return ALL_TAB;
}

/** GET /api/v1/programs params for a tab. `categories`: the server serves provider_category (else legacy program_type). */
export function tabQuery(tab: CategoryTab, categories: boolean): { provider?: string; field?: string; program_type?: string } {
  const parsed = parseTab(tab);
  if (!parsed.provider && !parsed.field) return {};
  if (categories) return parsed.provider ? { provider: parsed.provider } : { field: parsed.field };
  return { program_type: LEGACY_PROGRAM_TYPE[tab] };
}

type Categorised = { program_type: string; provider_category?: string | null; provider_categories?: string[] | null; field_category?: string | null };

/** Field of a programme: the server's field_category, else the legacy equivalent of program_type (null when none). */
export function fieldOf(item: Categorised): FieldCategory | null {
  if (item.field_category && isField(item.field_category)) return item.field_category;
  switch (item.program_type) {
    case "housing": return "housing";
    case "benefit": case "corporate_benefit": case "campus_benefit": return "living";
    case "scholarship": return "education";
    case "welfare": return "welfare";
    case "financial": case "financial_product": return "finance";
    case "employment": return "employment";
    default: return null;
  }
}

/**
 * Provider tabs of a programme: the server's provider_categories when it serves them (an empty list is an answer:
 * a school's own offer is only under the school-scoped 대학 tab, a bank deposit only under 금융), else the legacy
 * equivalent of program_type.
 */
export function providersOf(item: Categorised): ProviderCategory[] {
  if (Array.isArray(item.provider_categories)) return item.provider_categories.filter(isProvider);
  if (item.provider_category) return [item.provider_category].filter(isProvider);
  if (item.program_type === "campus_benefit") return [];
  if (item.program_type === "scholarship") return ["scholarship"];
  if (item.program_type === "corporate_benefit") return ["corporate"];
  return ["policy"];
}

/** Client-side tab filter (recommendations, saved list): same rule as the server's ?provider / ?field. */
export function matchesTab(item: Categorised, tab: CategoryTab): boolean {
  const parsed = parseTab(tab);
  if (parsed.campus) return item.program_type === "campus_benefit";
  if (parsed.provider) return providersOf(item).includes(parsed.provider);
  if (parsed.field) return fieldOf(item) === parsed.field;
  return true;
}

export const FIELD_LABELS: Record<FieldCategory, string> = Object.fromEntries(FIELD_TABS.map(tab => [tab.value, tab.label])) as Record<FieldCategory, string>;

/** Card label: the field (F5.4). `fallback` (usually programTypeLabel) when no field is known. */
export function fieldLabelOf(item: Categorised & { field_label?: string | null }, fallback: string): string {
  const field = fieldOf(item);
  return item.field_label?.trim() || (field ? FIELD_LABELS[field] : fallback);
}
