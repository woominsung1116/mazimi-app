import React, { useCallback, useEffect, useRef, useState } from "react";
import { useFocusEffect } from "expo-router";
import UndoToast, { useUndoToast } from "./UndoToast";
import { replayStep, tapStep, useMemberState } from "@/lib/guest";
import { createTapHold } from "@/lib/tapHold";

/** Shown when a member taps a members-only control while the API cannot confirm the session, or a kept tap waits too long. */
export const PENDING_ERROR_TOAST = "연결을 확인하고 있어요. 연결되면 다시 눌러 주세요.";
/** Shown when another tap arrives while one is kept. */
export const BUSY_TOAST = "앞에서 누른 동작을 처리하고 있어요.";
export { HOLD_LIMIT_MS } from "@/lib/tapHold";

/**
 * A members-only action. `run` is the screen's live handler (read through a ref
 * on replay, so it sees the member's real state); `ready: false` holds the tap
 * until the data it decides on has loaded (e.g. the saved list before 찜).
 */
export type MemberHandler = { run: (intent?: unknown) => void; ready?: boolean };

/**
 * Runs a members-only action from a tap, whatever state the session is in:
 * - member, data ready: runs the latest handler now.
 * - guest: `onGuest` (the one login nudge).
 * - pending, or a member whose data is still loading: keeps the tap (one at a
 *   time; others are ignored), `waiting` names its control for a spinner, and
 *   the latest handler runs once the member and the data are there. If the
 *   restore ends as a guest the nudge shows then; if unconfirmed, the toast.
 *   Leaving the screen drops the kept tap.
 * - pending-error: a short toast (`onUnconfirmed` replaces it), never a login prompt.
 */
export function useMemberAction(handlers: Record<string, MemberHandler>, { onUnconfirmed }: { onUnconfirmed?: (message: string) => void } = {}) {
  const state = useMemberState();
  const toast = useUndoToast();
  const latest = useRef(handlers);
  latest.current = handlers;
  const [waiting, setWaiting] = useState<string | null>(null);
  const onExpire = useRef<() => void>(() => {});
  const hold = useRef<ReturnType<typeof createTapHold> | null>(null);
  if (!hold.current) hold.current = createTapHold(setWaiting, () => onExpire.current());
  const clear = useCallback(() => hold.current!.clear(), []);
  // A tap kept on a screen the person has left never fires there later.
  useFocusEffect(useCallback(() => clear, [clear]));
  const say = useCallback((text: string) => { if (onUnconfirmed) onUnconfirmed(text); else toast.show({ text }); }, [onUnconfirmed, toast.show]);
  const unconfirmed = useCallback(() => say(PENDING_ERROR_TOAST), [say]);
  onExpire.current = unconfirmed;
  const waitingReady = waiting !== null && latest.current[waiting]?.ready !== false;
  useEffect(() => {
    const kept = hold.current!.current();
    const settled = kept && replayStep(state, latest.current[kept.key]?.ready !== false);
    if (!kept || !settled) return;
    const next = hold.current!.take()!;
    if (settled === "run") latest.current[next.key]?.run(next.intent);
    else if (settled === "guest") next.onGuest();
    else unconfirmed();
  }, [state, waitingReady, unconfirmed, clear]);
  const run = useCallback((key: string, onGuest: () => void, intent?: unknown) => {
    const step = tapStep(state, latest.current[key]?.ready !== false, hold.current!.current() !== null);
    if (step === "ignore") say(BUSY_TOAST);
    else if (step === "run") latest.current[key]?.run(intent);
    else if (step === "guest") onGuest();
    else if (step === "unconfirmed") unconfirmed();
    else hold.current!.keep({ key, intent, onGuest });
  }, [state, say, unconfirmed]);
  useEffect(() => () => hold.current!.clear(), []);
  const toastElement = <UndoToast toast={toast.toast} onHide={toast.hide} style={{ bottom: 96 }} />;
  return { run, say, waiting, busy: waiting !== null, toastElement };
}

/**
 * 찜 is a server toggle. A tap means "make it saved" (or "not saved"); act only
 * when that is not already true. If the saved list failed to load, nothing is
 * known about the bookmark, so the toggle is never sent ("retry"): a guess here
 * could delete a bookmark the member has.
 */
export function saveDecision(wantSaved: boolean, isSaved: boolean, listFailed = false): "toggle" | "already-saved" | "already-unsaved" | "retry" {
  if (listFailed) return "retry";
  if (wantSaved === isSaved) return isSaved ? "already-saved" : "already-unsaved";
  return "toggle";
}
export const ALREADY_SAVED_TOAST = "이미 관심 혜택에 있어요";
export const SAVED_UNKNOWN_TOAST = "관심 혜택 상태를 확인하지 못했어요. 잠시 후 다시 눌러 주세요.";
