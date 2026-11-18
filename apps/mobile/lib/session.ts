import { api } from "./api";
import { takeFirstSignup } from "./firstSignup";
import { thisDevicePushToken } from "./notifications";
import { useAuthStore } from "@/store/auth";

/** Upper bound on how long each server-side revocation may hold up sign-out. */
const SERVER_SIGN_OUT_TIMEOUT_MS = 2500;

async function bounded(revocation: Promise<unknown>): Promise<void> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  const timeout = new Promise<void>(resolve => { timer = setTimeout(resolve, SERVER_SIGN_OUT_TIMEOUT_MS); });
  try {
    await Promise.race([revocation.catch(() => {}), timeout]);
  } finally {
    if (timer) clearTimeout(timer);
  }
}

/**
 * User-initiated sign-out. Server-side revocation (push token, refresh token)
 * is awaited while the credentials are still attached, but only for a short,
 * bounded time: clearing the local session never depends on the network
 * succeeding.
 *
 * The order matters. This device's push token (only this device's, so the
 * account's other devices stay subscribed) is removed first with refresh disabled,
 * so an expired access token cannot mint a new refresh pair in the middle of
 * ending the session. The refresh token is then read again, because a request
 * that was in flight may have rotated it meanwhile, and the latest one is
 * revoked. `/auth/logout` authenticates by that token itself, so an expired
 * access token does not block the revocation.
 */
export async function signOut(): Promise<void> {
  takeFirstSignup(); // a pending welcome belongs to the account that signed up
  // Bounded too: fetching the Expo token can wait on the network.
  let pushToken: string | null = null;
  await bounded(thisDevicePushToken().then(token => { pushToken = token; }));
  if (useAuthStore.getState().token && pushToken) {
    await bounded(api.unregisterPushToken({ token: pushToken, skipRefresh: true }));
  }
  const { refreshToken, logout } = useAuthStore.getState();
  // The push token rides along: with an expired access token the DELETE
  // above is refused, and /auth/logout (authenticated by the refresh token)
  // removes this device's row instead.
  if (refreshToken) await bounded(api.logout(refreshToken, pushToken));
  await logout();
}
