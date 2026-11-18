"use client";

import { useState } from "react";
import { api } from "@/lib/api";
import { setBookmarkToIntent } from "@/lib/bookmarkSync";

interface BookmarkButtonProps {
  programId: string;
  initialBookmarked?: boolean;
}

export default function BookmarkButton({
  programId,
  initialBookmarked = false,
}: BookmarkButtonProps) {
  const [bookmarked, setBookmarked] = useState(initialBookmarked);
  const [loading, setLoading] = useState(false);
  const [failed, setFailed] = useState(false);

  const isSaved = async () => (await api.getSaved()).items.some((item) => item.id === programId);

  async function toggle() {
    if (loading) return;
    setLoading(true);
    setFailed(false);
    // The tap's intent goes to the server (idempotent PUT), so a retry after a
    // lost response is safe and a save can never delete the bookmark.
    const want = !bookmarked;
    setBookmarked(want);
    try {
      const res = await setBookmarkToIntent(
        { put: (value) => api.setBookmark(programId, value), toggle: () => api.toggleBookmark(programId), isSaved },
        want,
      );
      setBookmarked(res.bookmarked);
      if (res.bookmarked !== want) setFailed(true);
    } catch {
      // The request may have reached the server: show what the server has now, not the old state.
      setFailed(true);
      try {
        setBookmarked(await isSaved());
      } catch {
        setBookmarked(!want);
      }
    } finally {
      setLoading(false);
    }
  }

  return (
    <button
      type="button"
      onClick={toggle}
      disabled={loading}
      aria-label={bookmarked ? "북마크 해제" : "북마크"}
      title={failed ? "저장하지 못했어요. 다시 시도해 주세요." : undefined}
      className={`p-2 rounded-full transition-colors ${
        bookmarked
          ? "text-teal-600 bg-teal-50"
          : "text-gray-400 hover:text-teal-500 hover:bg-teal-50"
      } ${failed ? "ring-2 ring-red-300" : ""} disabled:opacity-50`}
    >
      <svg
        className="h-5 w-5"
        fill={bookmarked ? "currentColor" : "none"}
        stroke="currentColor"
        viewBox="0 0 24 24"
      >
        <path
          strokeLinecap="round"
          strokeLinejoin="round"
          strokeWidth={2}
          d="M4.318 6.318a4.5 4.5 0 000 6.364L12 20.364l7.682-7.682a4.5 4.5 0 00-6.364-6.364L12 7.636l-1.318-1.318a4.5 4.5 0 00-6.364 0z"
        />
      </svg>
    </button>
  );
}
