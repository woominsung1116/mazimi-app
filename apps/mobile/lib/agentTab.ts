import AsyncStorage from "@react-native-async-storage/async-storage";
import type { MemberState } from "./guest";

/** The last agent-on answer a member saw on this device. Not personal data: a server feature flag. */
export const AGENT_ENABLED_KEY = "mazimi_agent_enabled_v1";

export async function readAgentEnabled(): Promise<boolean | null> {
  try { const value = await AsyncStorage.getItem(AGENT_ENABLED_KEY); return value === "true" ? true : value === "false" ? false : null; }
  catch { return null; }
}

export async function writeAgentEnabled(enabled: boolean): Promise<void> {
  try { await AsyncStorage.setItem(AGENT_ENABLED_KEY, String(enabled)); } catch { /* next answer writes it again */ }
}

/**
 * Whether the 비서 tab is hidden. A guest always sees it (locked, one nudge).
 * For a member, or a session still restoring, the server's answer decides; until
 * it arrives the last answer seen on this device does, so the bar keeps its shape.
 * Unknown either way: shown.
 */
export function agentTabHiddenFor(state: MemberState, enabledNow: boolean | undefined, lastEnabled: boolean | null): boolean {
  if (state === "guest") return false;
  const enabled = typeof enabledNow === "boolean" ? enabledNow : lastEnabled;
  return enabled === false;
}
