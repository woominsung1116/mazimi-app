import { useQuery } from "@tanstack/react-query";
import { api, ApiRequestError, type ProfileInput } from "./api";
import { useOnboardingStore, getBirthYear } from "@/store/onboarding";
import { useAuthStore } from "@/store/auth";
import { cityCodeFor, findRegion } from "@/constants/regions";
import { toEmploymentCode, toEnrollmentCode } from "./profileCodes";
import { memberPreviewProfile } from "./memberProfile";
import { askedAnswers } from "./optionalAnswers";

/** Never invent age, region or income when the user has not supplied them. */
export function useBenefitProfile(): ProfileInput | null {
  const user = useAuthStore(s => s.user);
  const restoring = useAuthStore(s => s.isLoading);
  const local = useOnboardingStore();
  const { data } = useQuery({ queryKey: ["profile", "me", user?.id], queryFn: api.getProfile, enabled: !!user });
  const saved = data?.profile;
  if (user) {
    // A member's saved conditions are authoritative. A draft may belong to a
    // previous guest, or contain an edit the member has not saved yet.
    const region = findRegion(saved?.region_code ?? "")?.code;
    return memberPreviewProfile(saved, region);
  }
  if (restoring) return null;
  const birthYear = local.birthYear || getBirthYear(local.age);
  const region = findRegion(local.region)?.code;
  if (!birthYear || !region) return null;
  return { birth_year: birthYear, region_code: region, city_code: cityCodeFor(region, local.cityCode) || null, enrollment_status: toEnrollmentCode(local.enrollmentStatus), employment_status: toEmploymentCode(local.employmentStatus), school_name: local.schoolName || null, income_bracket: local.incomeBracket, ...(local.optional && askedAnswers(local.optional)) };
}

export type ProfileState = "loading" | "found" | "none" | "error";
/** Shown when an action needs the saved conditions and the read failed. "Try again", never "you have none". */
export const PROFILE_RETRY_TOAST = "저장한 조건을 불러오지 못했어요. 잠시 후 다시 눌러 주세요.";

/** found / none (404 or empty) / error, from the profile read (same query and cache as useBenefitProfile). */
export function profileStateOf(query: { isSuccess: boolean; isError: boolean; error: unknown; data?: { profile?: unknown } | null }): ProfileState {
  // Loaded conditions stay "found" even if a background refetch then failed (TanStack keeps data, sets isError).
  if (query.data?.profile) return "found";
  if (query.isSuccess) return "none";
  if (query.isError) return query.error instanceof ApiRequestError && query.error.status === 404 ? "none" : "error";
  return "loading";
}

/** This member's saved-profile state, and a refetch for the "try again" path. */
export function useProfileStatus(): { state: ProfileState; retry: () => void } {
  const user = useAuthStore(s => s.user);
  const query = useQuery({ queryKey: ["profile", "me", user?.id], queryFn: api.getProfile, enabled: !!user });
  return { state: user ? profileStateOf(query) : "loading", retry: () => { void query.refetch(); } };
}
