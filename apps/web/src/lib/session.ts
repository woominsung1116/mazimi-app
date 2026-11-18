"use client";

import { useEffect, useState } from "react";
import { ApiError, api, type SessionUser } from "@/lib/api";

export type AuthSession =
  | { status: "loading" }
  | { status: "authenticated"; user: SessionUser }
  | { status: "unauthenticated" }
  | { status: "unavailable" };

/** Classify a `/auth/me` outcome. Only a 401 means signed out. */
export async function loadSession(signal?: AbortSignal): Promise<AuthSession> {
  try {
    return { status: "authenticated", user: await api.getMe(signal) };
  } catch (error) {
    return error instanceof ApiError && error.status === 401 ? { status: "unauthenticated" } : { status: "unavailable" };
  }
}

// One /auth/me per page load, shared by every component that asks. Sign-in
// and sign-out are full navigations, which start a fresh page and cache.
let shared: Promise<AuthSession> | null = null;
export function sharedSession(): Promise<AuthSession> {
  shared ??= loadSession().then(session => {
    if (session.status === "unavailable") shared = null; // ask again next time
    return session;
  });
  return shared;
}

/** The signed-in account, asked of the API through the session proxy. */
export function useAuthSession(): AuthSession {
  const [session, setSession] = useState<AuthSession>({ status: "loading" });
  useEffect(() => {
    let live = true;
    void sharedSession().then(next => { if (live) setSession(next); });
    return () => { live = false; };
  }, []);
  return session;
}
