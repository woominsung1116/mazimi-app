// One style per category: every screen that shows a category reads this table (user decision 2026-10-31).
//   image  3D illustration, 496px: home 마감 임박 tiles and the home 비서 banner
//   hero   3D illustration, 640px (tools/design/export-category-heroes.mjs): programme detail hero, on the tile colour
//   glyph  flat two-tone alpha mask, 96px, tinted with `accent`: benefit card icon box
//   tile   light tint of the image's hued pixels: tile, card icon box, type badge and detail hero background
//   accent a saturated, darker colour of the same hue at WCAG AA 4.5:1 on `tile`: glyph tint and type badge text
// tile/accent come from `node tools/design/category-colors.mjs`; `--check` fails when this table drifts.
// 장학금 keeps the Figma graduation cap; `default` (no category at all) keeps the Figma paper plane. Their Figma originals
// exist only at 496px, so those two heroes draw at 496 (about 2.8x at 180pt), not 640.
// Asset provenance (source files, sizes, hashes): assets/categories/source.json.
// `corporate:<code>`: 기업혜택 chips (mazimi_core::corporate_categories) with their own flat glyph. Only food (카페·외식)
// has its own 3D image (the user's iced coffee); the others keep corporate's 3D tag, image and colours (user decision).
// travel and other have no glyph and use `corporate`.
import type { ImageSourcePropType } from "react-native";
import { fieldOf, providersOf, type FieldCategory, type ProviderCategory } from "./fields";

export const CORPORATE_SUBCATEGORIES = ["dev", "design", "culture", "food", "shopping", "study", "telecom"] as const;
type CorporateSubcategory = (typeof CORPORATE_SUBCATEGORIES)[number];
export type CategoryCode = ProviderCategory | FieldCategory | `corporate:${CorporateSubcategory}` | "campus" | "agent" | "default";
export type CategoryStyle = { image: ImageSourcePropType; hero: ImageSourcePropType; glyph: ImageSourcePropType; tile: string; accent: string };

export const CATEGORY_STYLE: Record<CategoryCode, CategoryStyle> = {
  policy: { image: require("../assets/categories/policy.png"), hero: require("../assets/categories/hero/policy.png"), glyph: require("../assets/figma/icon-policy.png"), tile: "#F0F2ED", accent: "#577633" },
  scholarship: { image: require("../assets/figma/scholarship.png"), hero: require("../assets/figma/scholarship.png"), glyph: require("../assets/categories/glyph/scholarship.png"), tile: "#EDF0EB", accent: "#4A7733" },
  corporate: { image: require("../assets/categories/corporate.png"), hero: require("../assets/categories/hero/corporate.png"), glyph: require("../assets/categories/glyph/corporate.png"), tile: "#EFF6FA", accent: "#3074A5" },
  "corporate:dev": { image: require("../assets/categories/corporate.png"), hero: require("../assets/categories/hero/corporate.png"), glyph: require("../assets/categories/glyph/corp-dev.png"), tile: "#EFF6FA", accent: "#3074A5" },
  "corporate:design": { image: require("../assets/categories/corporate.png"), hero: require("../assets/categories/hero/corporate.png"), glyph: require("../assets/categories/glyph/corp-design.png"), tile: "#EFF6FA", accent: "#3074A5" },
  "corporate:culture": { image: require("../assets/categories/corporate.png"), hero: require("../assets/categories/hero/corporate.png"), glyph: require("../assets/categories/glyph/corp-culture.png"), tile: "#EFF6FA", accent: "#3074A5" },
  "corporate:food": { image: require("../assets/categories/corp-food.png"), hero: require("../assets/categories/hero/corp-food.png"), glyph: require("../assets/categories/glyph/corp-food.png"), tile: "#F7F1EB", accent: "#91653A" },
  "corporate:shopping": { image: require("../assets/categories/corporate.png"), hero: require("../assets/categories/hero/corporate.png"), glyph: require("../assets/categories/glyph/corp-shopping.png"), tile: "#EFF6FA", accent: "#3074A5" },
  "corporate:study": { image: require("../assets/categories/corporate.png"), hero: require("../assets/categories/hero/corporate.png"), glyph: require("../assets/categories/glyph/corp-study.png"), tile: "#EFF6FA", accent: "#3074A5" },
  "corporate:telecom": { image: require("../assets/categories/corporate.png"), hero: require("../assets/categories/hero/corporate.png"), glyph: require("../assets/categories/glyph/corp-telecom.png"), tile: "#EFF6FA", accent: "#3074A5" },
  campus: { image: require("../assets/categories/campus.png"), hero: require("../assets/categories/hero/campus.png"), glyph: require("../assets/categories/glyph/campus.png"), tile: "#F1EFF5", accent: "#7C5CB9" },
  housing: { image: require("../assets/categories/housing.png"), hero: require("../assets/categories/hero/housing.png"), glyph: require("../assets/categories/glyph/housing.png"), tile: "#EFF4F8", accent: "#407496" },
  living: { image: require("../assets/categories/living.png"), hero: require("../assets/categories/hero/living.png"), glyph: require("../assets/categories/glyph/living.png"), tile: "#F8F2ED", accent: "#98633B" },
  education: { image: require("../assets/categories/education.png"), hero: require("../assets/categories/hero/education.png"), glyph: require("../assets/categories/glyph/education.png"), tile: "#F2EFF2", accent: "#A348A8" },
  welfare: { image: require("../assets/categories/welfare.png"), hero: require("../assets/categories/hero/welfare.png"), glyph: require("../assets/categories/glyph/welfare.png"), tile: "#F9F0F0", accent: "#B54D4D" },
  finance: { image: require("../assets/categories/finance.png"), hero: require("../assets/categories/hero/finance.png"), glyph: require("../assets/figma/icon-coin.png"), tile: "#EBEEEA", accent: "#477733" },
  employment: { image: require("../assets/categories/employment.png"), hero: require("../assets/categories/hero/employment.png"), glyph: require("../assets/categories/glyph/employment.png"), tile: "#ECEEF2", accent: "#496BAA" },
  agent: { image: require("../assets/categories/agent.png"), hero: require("../assets/categories/hero/agent.png"), glyph: require("../assets/categories/glyph/agent.png"), tile: "#F2F4EF", accent: "#5E7833" },
  default: { image: require("../assets/figma/benefit.png"), hero: require("../assets/figma/benefit.png"), glyph: require("../assets/figma/icon-policy.png"), tile: "#F0F6ED", accent: "#4D7A34" },
};

type Categorised = Parameters<typeof fieldOf>[0] & { corporate_category?: string | null };

const isCorporateSubcategory = (code: string): code is CorporateSubcategory => (CORPORATE_SUBCATEGORIES as readonly string[]).includes(code);

/**
 * A programme's category, first match wins. Who offers it beats the field: the server always fills a field
 * (a company or school offer falls back to 생활, every scholarship is 교육), so the field says least about these.
 * 1. a corporate offer's 기업혜택 chip, when the chip has a glyph;
 * 2. 장학금: the graduation cap;
 * 3. any other corporate offer (chip travel/other or not stored yet): the corporate tag;
 * 4. a school's own offer: campus;
 * 5. the field (policies); 6. the first provider tab (a server without the field column); 7. `default`.
 */
export function categoryOf(item: Categorised): CategoryCode {
  const providers = providersOf(item);
  const chip = item.corporate_category ?? "";
  if (providers.includes("corporate") && isCorporateSubcategory(chip)) return `corporate:${chip}`;
  if (providers.includes("scholarship")) return "scholarship";
  if (providers.includes("corporate")) return "corporate";
  if (item.program_type === "campus_benefit") return "campus";
  return fieldOf(item) ?? providers[0] ?? "default";
}

export const categoryStyleOf = (item: Categorised): CategoryStyle => CATEGORY_STYLE[categoryOf(item)];
