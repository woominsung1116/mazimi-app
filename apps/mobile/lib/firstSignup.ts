// 회원가입을 환영합니다 (Figma 28023:1751) is for a real first sign-up only. The
// server says so (`is_new` on login, app/login.tsx); the last onboarding step
// takes the mark once. No other way into /onboarding (홈, 마이페이지, 신청
// 도우미's 프로필 확인) can set it. In memory: a reload mid-onboarding skips the
// welcome, never shows it to an existing member. Pure (no React Native).
let firstSignup = false;

/** Called once, when login answers `is_new: true`. */
export function markFirstSignup(): void {
  firstSignup = true;
}

/** True once after `markFirstSignup`, then false. Also called to drop the mark on a login
 * that is not new and on logout, so it never reaches another account's onboarding. */
export function takeFirstSignup(): boolean {
  const pending = firstSignup;
  firstSignup = false;
  return pending;
}
