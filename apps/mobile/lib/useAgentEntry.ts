import { useEffect, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { api } from "./api";
import { useAuthStore } from "../store/auth";
import { useMemberState } from "./guest";
import { agentTabHiddenFor, readAgentEnabled, writeAgentEnabled } from "./agentTab";

/**
 * Whether the 비서 entry shows (its home banner slide). Guests see it locked with one nudge.
 * It hides only for a member whose server says the agent is off. The last answer seen on
 * this device decides until the status query settles, so the carousel keeps its shape.
 */
export function useAgentEntryHidden(): boolean {
  const user = useAuthStore(s => s.user);
  const memberState = useMemberState();
  const agent = useQuery({ queryKey: ["agent-status", user?.id], queryFn: ({ signal }) => api.getAgentStatus(signal), enabled: !!user, staleTime: 60_000, retry: false });
  const [lastEnabled, setLastEnabled] = useState<boolean | null>(null);
  useEffect(() => { void readAgentEnabled().then(setLastEnabled); }, []);
  const enabledNow = agent.data?.enabled;
  useEffect(() => { if (user && typeof enabledNow === "boolean") { setLastEnabled(enabledNow); void writeAgentEnabled(enabledNow); } }, [user, enabledNow]);
  // A failed status check falls back to shown, never to an old stored "off".
  return agentTabHiddenFor(memberState, enabledNow, agent.isError ? null : lastEnabled);
}
