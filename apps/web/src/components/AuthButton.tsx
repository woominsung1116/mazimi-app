"use client";

import Link from "next/link";
import { usePathname, useSearchParams } from "next/navigation";
import { signOut } from "@/lib/api";
import { useAuthSession } from "@/lib/session";

/** Header sign-in / sign-out control, shown on every page. */
export function AuthButton() {
  const session = useAuthSession();
  const pathname = usePathname();
  const query = useSearchParams().toString();

  if (session.status === "loading") {
    return <div className="h-8 w-14 rounded-lg bg-gray-100 animate-pulse" aria-hidden="true" />;
  }

  if (session.status === "authenticated") {
    return (
      <button
        type="button"
        onClick={() => {
          // Revokes the refresh token, clears the cookies and the API cache;
          // a full load drops every in-memory copy of the account's data.
          // eslint-disable-next-line @next/next/no-location-assign-relative-destination
          void signOut().finally(() => window.location.assign("/"));
        }}
        className="rounded-lg px-2.5 py-1.5 text-sm font-medium text-gray-600 hover:bg-gray-100 transition-colors"
      >
        로그아웃
      </button>
    );
  }

  const back = `${pathname}${query ? `?${query}` : ""}`;
  return (
    <Link
      href={pathname === "/login" ? "/login" : `/login?callbackUrl=${encodeURIComponent(back)}`}
      className="rounded-lg bg-teal-600 px-2.5 py-1.5 text-sm font-semibold text-white hover:bg-teal-700 transition-colors"
    >
      로그인
    </Link>
  );
}
