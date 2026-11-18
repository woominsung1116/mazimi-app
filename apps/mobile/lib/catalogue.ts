import { useQuery } from "@tanstack/react-query";
import { api, ApiRequestError, type ProfileInput } from "./api";
import { useAuthStore } from "@/store/auth";

/**
 * Whether the server serves the provider/field axes (provider_category on list
 * items). undefined until known; false on an older server or a failed probe,
 * so tabs fall back to the legacy program_type filter instead of an unfiltered list.
 */
export function useCategorySupport(): boolean | undefined {
  const probe = useQuery({
    queryKey: ["programs-capability"],
    queryFn: async () => { const page = await api.getPrograms({ per_page: 1 }); return page.items.length === 0 || "provider_category" in page.items[0]; },
    staleTime: 30 * 60_000, retry: 1,
  });
  if (probe.isError) return false;
  return probe.data;
}

/**
 * Whether 직접 추가 (document-items) exists on this server. Probed once per app
 * session for guests and members alike; undefined until known and false on a
 * failed probe, so the feature stays hidden instead of offering a login that
 * leads to nothing (M4).
 */
export function useDocumentItemsSupport(): boolean | undefined {
  const probe = useQuery({ queryKey: ["document-items-capability"], queryFn: api.hasDocumentItemsRoute, staleTime: Infinity, gcTime: Infinity, retry: 1 });
  if (probe.isError) return false;
  return probe.data;
}

/**
 * A member's recommendations (GET /api/v1/recommendations, the saved profile).
 * One query key for every screen so they share one result and one invalidation.
 * Guests have no profile, so no request is made (feedback F7.1).
 */
export function useRecommendations(profile: ProfileInput | null) {
  const user = useAuthStore(s => s.user);
  return useQuery({
    queryKey: ["recommend", profile, user?.id],
    queryFn: () => api.getRecommendations(profile!),
    enabled: !!profile && !!user,
    // 409 profile_required: the saved conditions are incomplete. Retrying will not help.
    retry: (count, error) => !(error instanceof ApiRequestError && (error.status === 409 || error.status === 401)) && count < 2,
  });
}
