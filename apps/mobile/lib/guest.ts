import AsyncStorage from "@react-native-async-storage/async-storage";
import { useRouter } from "expo-router";
import { useAuthStore } from "@/store/auth";
import { requireLogin } from "./navigation";

// 비회원은 이 기기에 아무것도 남기지 않는다. 맞춤 조건과 추천은 로그인한 회원만 쓴다.

/** v1.0.0까지 비회원 조건을 기기에 남기던 저장 키. 이제는 읽지 않고 지우기만 한다 */
export const LEGACY_GUEST_DRAFT_KEY = "mazimi-onboarding-v1";

/** 예전 빌드가 남긴 기기 초안을 지운다. 앱 시작 때 한 번 부르고, 실패해도 앱 시작을 막지 않는다 */
export async function purgeLegacyGuestData(): Promise<void> {
  try { await AsyncStorage.removeItem(LEGACY_GUEST_DRAFT_KEY); } catch { /* 다음 시작 때 다시 지운다 */ }
}

/** 로그인 화면. return=1이면 로그인을 마치고 지금 화면으로 돌아온다 */
export const LOGIN_RETURN_PATH = "/login?return=1" as const;

export type GuestState = {
  /** 세션 복원이 끝났고 로그인하지 않았다. 복원 중에는 false다 */
  isGuest: boolean;
  /** 저장된 세션을 아직 확인하는 중이다. 이때는 잠금도 회원 화면도 단정하지 않는다 */
  restoring: boolean;
  /** 로그인 화면을 바로 연다 (잠금 영역의 "로그인 필요" 버튼) */
  openLogin: () => void;
  /** 이유를 먼저 알리고 로그인 화면으로 보낸다 (찜·알림처럼 눌러서 막히는 기능) */
  promptLogin: (message: string) => void;
};

/**
 * 비회원 판정. 세션 복원 중(restoring)이거나, 저장된 로그인이 있는데 서버에 닿지 못한 경우(sessionError)는
 * 비회원이 아니다. 후자를 비회원으로 보면 장애 때 회원에게 잠금·로그인 화면을 보여 준다.
 */
export function isGuestState(state: { isLoading: boolean; user: unknown; sessionError: boolean }): boolean {
  return !state.isLoading && !state.user && !state.sessionError;
}

/**
 * member: a member is loaded. guest: no stored login (isGuestState).
 * pending: a stored login is still restoring; pending-error: the API could not confirm it.
 * Only "guest" may see a login prompt; the pending states are a member whose
 * session is not back yet.
 */
export type MemberState = "member" | "guest" | "pending" | "pending-error";
export function memberStateOf(state: { isLoading: boolean; user: unknown; sessionError: boolean }): MemberState {
  if (state.user) return "member";
  if (isGuestState(state)) return "guest";
  return state.sessionError && !state.isLoading ? "pending-error" : "pending";
}
/**
 * What a tap on a members-only control does in each state (components/MemberAction).
 * run: do it now. guest: the login nudge. queue: keep it until the restore ends.
 * unconfirmed: a short "연결을 확인하고 있어요" message, never a login prompt.
 */
export type MemberTap = "run" | "guest" | "queue" | "unconfirmed";
export function memberTap(state: MemberState): MemberTap {
  return state === "member" ? "run" : state === "guest" ? "guest" : state === "pending" ? "queue" : "unconfirmed";
}
/** A queued tap once the state moves on; null while still pending. */
export function settleQueuedTap(state: MemberState): Exclude<MemberTap, "queue"> | null {
  const tap = memberTap(state);
  return tap === "queue" ? null : tap;
}

/**
 * A tap on a members-only control (components/MemberAction): `ready` is false
 * while the data the action decides on is loading, `busy` when another tap is
 * already kept. A member whose data is not ready is kept too, so a toggle never
 * acts on a guess.
 */
export function tapStep(state: MemberState, ready: boolean, busy: boolean): MemberTap | "ignore" {
  if (busy) return "ignore";
  const tap = memberTap(state);
  return tap === "run" && !ready ? "queue" : tap;
}
/** What a kept tap does now; null keeps waiting (still restoring, or the member's data is still loading). */
export function replayStep(state: MemberState, ready: boolean): Exclude<MemberTap, "queue"> | null {
  const settled = settleQueuedTap(state);
  return settled === "run" && !ready ? null : settled;
}

export function useMemberState(): MemberState {
  return useAuthStore(memberStateOf);
}

/** 세션 복원이 끝났고, 로그인한 회원도 저장된 로그인도 없으면 true */
export function useIsGuest(): boolean {
  return useAuthStore(isGuestState);
}

export function useGuest(): GuestState {
  const router = useRouter();
  const isGuest = useAuthStore(isGuestState);
  const restoring = useAuthStore(s => s.isLoading);
  return {
    isGuest,
    restoring,
    openLogin: () => router.push(LOGIN_RETURN_PATH),
    promptLogin: message => requireLogin(router, message),
  };
}
