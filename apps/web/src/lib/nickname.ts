// Same rule as apps/mobile/lib/conditions.ts validNickname and the server's
// nickname check: 1–30 characters after trimming. The nickname is required
// before onboarding moves on (v1.0.0 feedback F2.6).
export const NICKNAME_MAX = 30;

export function validNickname(nickname: string): boolean {
  const trimmed = nickname.trim();
  return trimmed.length >= 1 && trimmed.length <= NICKNAME_MAX;
}
