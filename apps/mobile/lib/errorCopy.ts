// Copy shown when an API call fails. Server error text (English, or internal
// detail) is never shown to the user; only the HTTP status picks the line.

export const NOT_FOUND_PROGRAM = "찾을 수 없거나 마감된 혜택이에요";
export const GENERIC_NOT_FOUND = "요청한 정보를 찾을 수 없어요.";

const statusOf = (error: unknown): number | null => {
  const status = (error as { status?: unknown } | null)?.status;
  return typeof status === "number" ? status : null;
};

/** Korean generic copy for a failed request. `notFound` replaces the 404 line. */
export function errorCopy(error: unknown, { notFound = NOT_FOUND_PROGRAM }: { notFound?: string } = {}): string {
  const status = statusOf(error);
  // null: not an HTTP error; 0: the request never got a response.
  if (status === null || status === 0) return "연결을 확인한 뒤 다시 시도해 주세요.";
  if (status === 404) return notFound;
  if (status === 401) return "로그인이 필요해요. 다시 로그인해 주세요.";
  if (status === 403) return "이 기능을 쓸 수 있는 권한이 없어요.";
  if (status === 429) return "요청이 많아요. 잠시 후 다시 시도해 주세요.";
  if (status >= 500) return "잠시 후 다시 시도해 주세요.";
  return "요청을 처리하지 못했어요. 다시 시도해 주세요.";
}

/**
 * Text for a failed API call on a generic screen. lib/api.ts request() already
 * turns every failure into Korean copy, so Korean text on an HTTP error is kept
 * (it may be the server's own user copy); anything else goes through errorCopy.
 */
export function apiErrorText(error: unknown, notFound = GENERIC_NOT_FOUND): string {
  const message = (error as { message?: unknown } | null)?.message;
  if (statusOf(error) !== null && typeof message === "string" && /[가-힣]/.test(message)) return message;
  return errorCopy(error, { notFound });
}
