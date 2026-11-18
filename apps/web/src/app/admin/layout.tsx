"use client";

import Link from "next/link";
import { usePathname, useRouter } from "next/navigation";
import { useEffect, useState } from "react";
import { loadSession } from "@/lib/session";

const navItems = [
  { href: "/admin", label: "대시보드" },
  { href: "/admin/programs", label: "프로그램 관리" },
  { href: "/admin/sources", label: "출처 검토" },
  { href: "/admin/ai-extraction", label: "AI 추출 검토" },
  { href: "/admin/assistant-runs", label: "AI 비서 실행" },
  { href: "/admin/operations", label: "운영 상태" },
  { href: "/admin/sync", label: "데이터 동기화" },
];

// ── Auth guard states ──
type AuthState = "loading" | "authenticated" | "unauthenticated" | "forbidden" | "unavailable";

// UX only: every admin API route enforces AdminUser (403) on the server.
async function checkAdmin(signal: AbortSignal): Promise<AuthState> {
  const session = await loadSession(signal);
  if (session.status !== "authenticated") return session.status === "unauthenticated" ? "unauthenticated" : "unavailable";
  return session.user.role === "admin" ? "authenticated" : "forbidden";
}

export default function AdminLayout({
  children,
}: {
  children: React.ReactNode;
}) {
  const pathname = usePathname();
  const router = useRouter();
  const [retry, setRetry] = useState(0);
  const [focusCheck, setFocusCheck] = useState(0);
  const [checked, setChecked] = useState<{ pathname: string; retry: number; state: AuthState } | null>(null);
  // A new route or retry shows the spinner; a focus re-check keeps the screen
  // until the answer arrives, then replaces it (sign-out elsewhere closes it).
  const authState = checked && checked.pathname === pathname && checked.retry === retry ? checked.state : "loading";

  useEffect(() => {
    const onFocus = () => setFocusCheck(value => value + 1);
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  }, []);

  useEffect(() => {
    // Aborting on every new check means only the latest answer can apply:
    // a slow response cannot authorize a newer session or another route.
    const controller = new AbortController();
    void checkAdmin(controller.signal).then(state => {
      if (!controller.signal.aborted) setChecked({ pathname, retry, state });
    });
    return () => controller.abort();
  }, [pathname, retry, focusCheck]);

  // ── Loading state ──
  if (authState === "loading") {
    return (
      <div className="min-h-screen bg-gray-50 flex items-center justify-center">
        <div className="text-center">
          <div className="inline-block w-8 h-8 border-4 border-teal-200 border-t-teal-600 rounded-full animate-spin" />
          <p className="mt-3 text-sm text-gray-500">권한 확인 중...</p>
        </div>
      </div>
    );
  }

  if (authState === "unavailable") {
    return <div className="min-h-screen bg-gray-50 flex items-center justify-center"><div className="bg-white rounded-xl border border-gray-200 p-8 max-w-sm text-center"><h2 className="text-lg font-bold text-gray-900 mb-2">권한을 확인하지 못했어요</h2><p className="text-sm text-gray-500 mb-6">연결을 확인한 뒤 다시 시도해 주세요.</p><button type="button" onClick={() => setRetry(value => value + 1)} className="w-full px-4 py-2.5 bg-teal-600 text-white text-sm font-medium rounded-lg">다시 시도</button></div></div>;
  }

  // ── Unauthenticated: redirect to login ──
  if (authState === "unauthenticated") {
    return (
      <div className="min-h-screen bg-gray-50 flex items-center justify-center">
        <div className="bg-white rounded-xl border border-gray-200 shadow-sm p-8 max-w-sm text-center">
          <svg className="w-12 h-12 mx-auto text-gray-400 mb-4" fill="none" viewBox="0 0 24 24" stroke="currentColor">
            <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={1.5}
              d="M16.5 10.5V6.75a4.5 4.5 0 10-9 0v3.75m-.75 11.25h10.5a2.25 2.25 0 002.25-2.25v-6.75a2.25 2.25 0 00-2.25-2.25H6.75a2.25 2.25 0 00-2.25 2.25v6.75a2.25 2.25 0 002.25 2.25z" />
          </svg>
          <h2 className="text-lg font-bold text-gray-900 mb-2">로그인 필요</h2>
          <p className="text-sm text-gray-500 mb-6">
            관리자 페이지에 접근하려면 로그인이 필요합니다.
          </p>
          <button
            onClick={() => router.push(`/login?callbackUrl=${encodeURIComponent(pathname)}`)}
            className="w-full px-4 py-2.5 bg-teal-600 text-white text-sm font-medium rounded-lg hover:bg-teal-700 transition-colors"
          >
            로그인하기
          </button>
        </div>
      </div>
    );
  }

  // ── Forbidden: not an admin ──
  if (authState === "forbidden") {
    return (
      <div className="min-h-screen bg-gray-50 flex items-center justify-center">
        <div className="bg-white rounded-xl border border-red-200 shadow-sm p-8 max-w-sm text-center">
          <svg className="w-12 h-12 mx-auto text-red-400 mb-4" fill="none" viewBox="0 0 24 24" stroke="currentColor">
            <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={1.5}
              d="M18.364 18.364A9 9 0 005.636 5.636m12.728 12.728A9 9 0 015.636 5.636m12.728 12.728L5.636 5.636" />
          </svg>
          <h2 className="text-lg font-bold text-gray-900 mb-2">접근 권한 없음</h2>
          <p className="text-sm text-gray-500 mb-6">
            관리자 권한이 필요합니다. 관리자 계정으로 로그인하세요.
          </p>
          <button
            onClick={() => router.push("/")}
            className="w-full px-4 py-2.5 bg-gray-600 text-white text-sm font-medium rounded-lg hover:bg-gray-700 transition-colors"
          >
            홈으로 돌아가기
          </button>
        </div>
      </div>
    );
  }

  // ── Authenticated admin: render the admin layout ──
  return (
    <div className="min-h-screen bg-gray-50 flex flex-col">
      <header className="bg-white border-b border-gray-200 px-4 py-3 flex items-center justify-between">
        <div className="flex items-center gap-3">
          <Link href="/" className="text-gray-400 hover:text-gray-600 text-sm">
            ← 앱으로
          </Link>
          <span className="text-gray-300">|</span>
          <span className="text-base font-bold text-gray-900">관리자</span>
        </div>
        <span className="text-xs text-gray-400 bg-gray-100 px-2 py-1 rounded-full">
          Admin
        </span>
      </header>

      <div className="flex flex-1">
        {/* Desktop sidebar */}
        <nav className="w-52 bg-white border-r border-gray-200 p-4 hidden md:block shrink-0">
          <p className="text-xs font-semibold text-gray-400 uppercase tracking-wider mb-3 px-3">
            메뉴
          </p>
          <ul className="space-y-1">
            {navItems.map((item) => {
              const isActive =
                item.href === "/admin"
                  ? pathname === "/admin"
                  : pathname.startsWith(item.href);
              return (
                <li key={item.href}>
                  <Link
                    href={item.href}
                    className={`block px-3 py-2 rounded-lg text-sm font-medium transition-colors ${
                      isActive
                        ? "bg-teal-50 text-teal-700"
                        : "text-gray-600 hover:bg-gray-100"
                    }`}
                  >
                    {item.label}
                  </Link>
                </li>
              );
            })}
          </ul>
        </nav>

        {/* Mobile bottom nav */}
        <nav className="md:hidden fixed bottom-0 left-0 right-0 bg-white border-t border-gray-200 flex z-10">
          {navItems.map((item) => {
            const isActive =
              item.href === "/admin"
                ? pathname === "/admin"
                : pathname.startsWith(item.href);
            return (
              <Link
                key={item.href}
                href={item.href}
                className={`flex-1 py-3 text-center text-xs font-medium transition-colors ${
                  isActive ? "text-teal-700" : "text-gray-500"
                }`}
              >
                {item.label}
              </Link>
            );
          })}
        </nav>

        <main className="flex-1 p-4 md:p-6 pb-20 md:pb-6 min-w-0">
          {children}
        </main>
      </div>
    </div>
  );
}
