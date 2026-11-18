import type { ProfileInput, UserProfile } from "./api";

/** Flags the server stores as false when unanswered; no screen asks for them yet. */
const UNASKED_FLAGS = ["has_disability", "is_low_income_household", "is_multicultural_family", "veteran_family"] as const;

/**
 * A member's saved conditions as a recommendation request. A stored `false`
 * for an unasked flag is not an answer, so it is sent as unknown (`null`):
 * otherwise a rule such as "기초생활수급 청년" would hide the programme from
 * everyone who was never asked. `null` when birth year or region is missing.
 */
export function memberPreviewProfile(saved: UserProfile | null | undefined, region: string | undefined): ProfileInput | null {
  if (!saved?.birth_year || !region) return null;
  const profile: ProfileInput = { ...saved, birth_year: saved.birth_year, region_code: region };
  for (const flag of UNASKED_FLAGS) profile[flag] = saved[flag] === true ? true : null;
  return profile;
}
