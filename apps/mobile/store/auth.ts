import { create } from "zustand";
import { useOnboardingStore } from "./onboarding";
import { tokenStorage as SecureStore } from "@/lib/tokenStorage";
import { API_BASE_URL } from "../lib/apiBaseUrl";

const SECURE_STORE_TOKEN_KEY = "mazimi_auth_token";
const SECURE_STORE_REFRESH_KEY = "mazimi_refresh_token";
let refreshFlight: { token: string; promise: Promise<boolean> } | null = null;
/**
 * Bumped by every logout. A restoreSession that started before a logout drops
 * its result, so a late /auth/me can never bring back a session the user ended.
 */
let sessionEpoch = 0;
/** The refresh exchange is bounded like the reads a tap waits on. */
export const REFRESH_TIMEOUT_MS = 15_000;
/**
 * A timed-out refresh may still have rotated the token on the server. The server
 * accepts the old token for REFRESH_ROTATION_GRACE_SECS (60 s) after a rotation,
 * so a retry inside that window continues the chain instead of reading as a
 * replay that revokes every session.
 */
export const REFRESH_GRACE_MS = 60_000;
let refreshTimedOutAt: number | null = null;
/** Retry a refresh that timed out less than REFRESH_GRACE_MS ago (called on the next foreground). */
export function retryTimedOutRefresh(now = Date.now()): Promise<boolean> | null {
  if (refreshTimedOutAt === null || now - refreshTimedOutAt >= REFRESH_GRACE_MS) return null;
  refreshTimedOutAt = null;
  return useAuthStore.getState().refreshAccessToken().catch(() => false);
}

/** The account is gone: the API's own 404 for /auth/me, not a proxy or route 404 (that is an outage). */
async function accountGone(res: Response): Promise<boolean> {
  if (res.status !== 404) return false;
  try { const body = await res.json(); return body?.error === "User not found"; } catch { return false; }
}

export interface AuthUser {
  id: string;
  nickname: string;
  image: string | null;
}

interface AuthState {
  token: string | null;
  refreshToken: string | null;
  user: AuthUser | null;
  isLoading: boolean;
  /** Stored credentials exist but the server could not be reached to restore them. */
  sessionError: boolean;

  /** Clears the local session. Server-side revocation lives in lib/session.ts signOut(). */
  logout: () => Promise<void>;
  restoreSession: () => Promise<void>;
  refreshAccessToken: () => Promise<boolean>;
  _setToken: (token: string | null) => void;
}

export const useAuthStore = create<AuthState>((set, get) => ({
  token: null,
  refreshToken: null,
  user: null,
  isLoading: true,
  sessionError: false,

  logout: async () => {
    // Revoke in-memory access before any asynchronous storage operation.
    // A late refresh must not restore a session the user just ended.
    sessionEpoch += 1;
    set({ token: null, refreshToken: null, user: null, isLoading: false, sessionError: false });
    useOnboardingStore.getState().reset();
    await Promise.all([
      SecureStore.deleteItemAsync(SECURE_STORE_TOKEN_KEY),
      SecureStore.deleteItemAsync(SECURE_STORE_REFRESH_KEY),
      SecureStore.deleteItemAsync("mazimi_pending_application"),
    ]);
  },

  refreshAccessToken: async () => {
    const { refreshToken } = get();
    if (!refreshToken) return false;
    // Rotating refresh credentials can only be exchanged once. All requests
    // that fail together share the same exchange instead of invalidating it.
    if (refreshFlight?.token === refreshToken) return refreshFlight.promise;
    const promise = (async () => {
      // Bounded like every read a tap waits on (Android's fetch has no timeout).
      // A timeout is an outage: it throws, it is never "refresh rejected", so it
      // never logs anyone out.
      const controller = new AbortController();
      const timer = setTimeout(() => controller.abort(), REFRESH_TIMEOUT_MS);
      let res: Response;
      try {
        res = await fetch(`${API_BASE_URL}/api/v1/auth/refresh`, {
          method: "POST", headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ refresh_token: refreshToken }), signal: controller.signal,
        });
      } catch {
        if (controller.signal.aborted) refreshTimedOutAt = Date.now();
        throw new Error("인증 서버에 연결하지 못했어요. 잠시 후 다시 시도해 주세요.");
      } finally { clearTimeout(timer); }
      refreshTimedOutAt = null;
      if ([400, 401, 403].includes(res.status)) return false;
      if (!res.ok) throw new Error("인증 서버에 연결하지 못했어요. 잠시 후 다시 시도해 주세요.");
      const data: { token: string; refresh_token: string } = await res.json();
      if (get().refreshToken !== refreshToken) return false;
      // The server has already revoked the old refresh token. Persist the new
      // one first: a kill between the two writes must leave the new refresh
      // token (with a stale access token, which refresh repairs) rather than
      // the revoked one, whose next use would read as a replay.
      await SecureStore.setItemAsync(SECURE_STORE_REFRESH_KEY, data.refresh_token);
      if (get().refreshToken !== refreshToken) return false;
      await SecureStore.setItemAsync(SECURE_STORE_TOKEN_KEY, data.token);
      if (get().refreshToken !== refreshToken) return false;
      set({ token: data.token, refreshToken: data.refresh_token });
      return true;
    })();
    const flight = { token: refreshToken, promise };
    refreshFlight = flight;
    try { return await promise; }
    finally { if (refreshFlight === flight) refreshFlight = null; }
  },

  // Safe to call again: a retry after an outage restores the same stored session.
  restoreSession: async () => {
    set({ isLoading: true });
    let hadCredentials = false;
    const epoch = sessionEpoch;
    // A logout while this ran wins: nothing below may write the session back.
    const ended = () => sessionEpoch !== epoch;
    try {
      // Restore only a session established on this device. Development env
      // tokens must never silently select an account or enter a public bundle.
      const storedToken = await SecureStore.getItemAsync(SECURE_STORE_TOKEN_KEY);
      const storedRefresh = await SecureStore.getItemAsync(
        SECURE_STORE_REFRESH_KEY
      );

      if (storedToken) {
        hadCredentials = true;
        // Try with existing access token first
        const res = await fetch(`${API_BASE_URL}/api/v1/auth/me`, {
          headers: { Authorization: `Bearer ${storedToken}` },
        });

        if (ended()) return;
        if (res.ok) {
          const user: AuthUser = await res.json();
          if (ended()) return;
          set({
            token: storedToken,
            refreshToken: storedRefresh,
            user,
            isLoading: false,
            sessionError: false,
          });
          return;
        }

        // 404: the account no longer exists (withdrawn on another device, or reset).
        // That is an answer, not an outage: end the session instead of retrying forever.
        if (await accountGone(res)) { if (!ended()) await get().logout(); return; }
        // A service outage is not evidence that stored credentials expired.
        if (res.status !== 401 && res.status !== 403) throw new Error("Session unavailable");
        // Access token expired — try refresh
        if (storedRefresh) {
          if (ended()) return;
          set({ refreshToken: storedRefresh });
          const refreshed = await get().refreshAccessToken();
          if (ended()) return;
          if (refreshed) {
            const retryRes = await fetch(`${API_BASE_URL}/api/v1/auth/me`, {
              headers: { Authorization: `Bearer ${get().token}` },
            });
            if (ended()) return;
            if (retryRes.ok) {
              const user: AuthUser = await retryRes.json();
              if (ended()) return;
              set({ user, isLoading: false, sessionError: false });
              return;
            }
            if (await accountGone(retryRes)) { if (!ended()) await get().logout(); return; }
            if (retryRes.status !== 401 && retryRes.status !== 403) throw new Error("Session unavailable");
          }
        }

        // Both tokens invalid — clear
        if (!ended()) await get().logout();
      } else {
        set({ isLoading: false, sessionError: false });
      }
    } catch {
      // Tokens stay stored; screens show a retry instead of the guest view.
      if (!ended()) set({ isLoading: false, sessionError: hadCredentials });
    }
  },

  _setToken: (token) => set({ token }),
}));
