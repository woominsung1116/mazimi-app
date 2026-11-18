// The one kept tap of a members-only screen (components/MemberAction), without
// React, so the node checks can drive it with mock timers.

/** A kept tap is dropped after this long, so a request that never answers cannot hold a spinner forever. */
export const HOLD_LIMIT_MS = 10_000;

export type KeptTap = { key: string; intent: unknown; onGuest: () => void };

/**
 * Holds at most one tap. `keep` refuses a second one while one is held (the
 * caller tells the person). The hold expires after `limitMs`: the tap is dropped
 * and `onExpire` runs. `take` hands the tap over for its replay and stops the
 * timer, so a tap runs at most once (replay or expiry, never both).
 */
export function createTapHold(onChange: (key: string | null) => void, onExpire: () => void, limitMs = HOLD_LIMIT_MS) {
  let kept: KeptTap | null = null;
  let timer: ReturnType<typeof setTimeout> | null = null;
  const stop = () => { if (timer) { clearTimeout(timer); timer = null; } };
  const clear = () => { const had = kept !== null; kept = null; stop(); if (had) onChange(null); };
  return {
    current: () => kept,
    keep(tap: KeptTap): boolean {
      if (kept) return false;
      kept = tap; onChange(tap.key);
      timer = setTimeout(() => { if (kept) { clear(); onExpire(); } }, limitMs);
      return true;
    },
    take(): KeptTap | null { const tap = kept; clear(); return tap; },
    clear,
  };
}
