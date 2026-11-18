// 찜 with the person's intent. The server's PUT /programs/{id}/bookmark
// {bookmarked} is idempotent: it sets the state and returns the state the row is
// in, so the same request can be sent again after a lost response. Older
// servers only have POST, a toggle; there the state is read before any second
// toggle. No React or RN imports, so the node checks call these directly.
// Kept byte-identical at apps/mobile/lib/bookmarkSync.ts and
// apps/web/src/lib/bookmarkSync.ts; both clients' checks assert the copies match.

export type SavedList<T extends { id: string }> = { items: T[] } & Record<string, unknown>;
export type BookmarkResult = { bookmarked: boolean; corrected: boolean };

type Failure = { status?: number; message?: string; body?: { error?: unknown } } | null | undefined;
/** The new server's own 404 for an unknown programme (not a missing route). */
export const PROGRAM_NOT_FOUND_COPY = "혜택을 찾을 수 없어요.";
const isNetworkFailure = (error: unknown) => { const status = (error as Failure)?.status; return status === 0 || status === undefined; };
/** An older server without PUT: 405 (the route only has POST), or a 404 that is not the programme-not-found answer. */
export const isMissingPut = (error: unknown) => {
  const e = error as Failure;
  return e?.status === 405 || (e?.status === 404 && e?.message !== PROGRAM_NOT_FOUND_COPY && e?.body?.error !== PROGRAM_NOT_FOUND_COPY);
};

export type BookmarkTransport = {
  /** PUT {bookmarked}: idempotent, returns the final state. */
  put: (bookmarked: boolean) => Promise<{ bookmarked: boolean }>;
  /** POST: the old toggle, for servers without PUT. */
  toggle: () => Promise<{ bookmarked: boolean }>;
  /** Whether the programme is in the server's saved list right now. */
  isSaved: () => Promise<boolean>;
};

/**
 * Sets the bookmark to `want`.
 * - PUT, retried up to `networkRetries` times on a network failure only: safe,
 *   because a repeated PUT changes nothing (a response lost after the server
 *   committed is simply sent again).
 * - Older server (no PUT): one toggle; if its answer is not `want`, the server
 *   state is read first and a second toggle is sent only if it is still not
 *   `want`. Never a blind retry of a toggle.
 */
export async function setBookmarkToIntent(transport: BookmarkTransport, want: boolean, networkRetries = 2): Promise<BookmarkResult> {
  for (let attempt = 0; ; attempt += 1) {
    try { const result = await transport.put(want); return { bookmarked: result.bookmarked, corrected: false }; }
    catch (error) {
      if (isMissingPut(error)) break;
      if (!isNetworkFailure(error) || attempt >= networkRetries) throw error;
    }
  }
  const first = await transport.toggle();
  if (first.bookmarked === want) return { bookmarked: first.bookmarked, corrected: false };
  // The cache was stale (saved elsewhere, or an earlier response was lost). Read before touching it again.
  if ((await transport.isSaved()) === want) return { bookmarked: want, corrected: true };
  const second = await transport.toggle();
  return { bookmarked: second.bookmarked, corrected: true };
}

/** The saved list after the server answered `bookmarked` for `id` (newest first, like the API). */
export function savedAfterToggle<T extends { id: string }>(old: SavedList<T> | undefined, id: string, program: T | undefined, bookmarked: boolean): SavedList<T> | undefined {
  if (!old) return old;
  const rest = old.items.filter(item => item.id !== id);
  return { ...old, items: bookmarked && program ? [program, ...rest] : rest };
}

export const ALREADY_UNSAVED_TOAST = "이미 관심 혜택에서 빠져 있어요";
