import React from "react";
import { useRouter } from "expo-router";
import { EmptyState } from "@/components/FigmaUI";
import { SessionErrorState } from "@/components/SessionProblem";
import { LOGIN_RETURN_PATH, useMemberState } from "@/lib/guest";

/**
 * What a members-only screen shows when no member is loaded.
 * - pending (the stored session is still restoring, or the API could not
 *   confirm it): a loading state or the retry state. Never a login prompt:
 *   this person may well be a member.
 * - guest: one non-blocking login nudge.
 */
export default function NonMemberState({ title = "로그인하고 이어서 이용하세요", description, onLogin }: { title?: string; description?: string; onLogin?: () => void }) {
  const router = useRouter();
  const state = useMemberState();
  if (state === "pending-error") return <SessionErrorState />;
  if (state === "pending") return <EmptyState loading title="정보를 불러오고 있어요" />;
  return <EmptyState title={title} description={description} action="로그인하기" onAction={onLogin ?? (() => router.push(LOGIN_RETURN_PATH))} />;
}
