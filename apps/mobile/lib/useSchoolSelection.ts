import { useQuery } from "@tanstack/react-query";
import { api } from "./api";
import { useAuthStore } from "@/store/auth";
import { useOnboardingStore } from "@/store/onboarding";
import { toEnrollmentCode } from "./profileCodes";

/**
 * A saved null means deliberately unselected; never revive an old draft.
 * `enrollmentStatus` is the canonical code (query parameter for campus programs).
 */
export function useSchoolSelection() {
  const user = useAuthStore(s => s.user);
  const restoring = useAuthStore(s => s.isLoading);
  const schoolName = useOnboardingStore(s => s.schoolName);
  const enrollmentStatus = useOnboardingStore(s => s.enrollmentStatus);
  const { data } = useQuery({ queryKey: ["profile", "me", user?.id], queryFn: api.getProfile, enabled: !!user });
  const selected = user ? data?.profile?.school_name : restoring ? null : schoolName;
  return {
    school: selected?.trim() || "",
    enrollmentStatus: toEnrollmentCode(user ? data?.profile?.enrollment_status : enrollmentStatus) ?? "",
    showCampus: !!selected?.trim(),
  };
}
