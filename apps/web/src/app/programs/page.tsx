"use client";

import { useEffect, useState } from "react";
import Link from "next/link";
import { api, type ApiProgram, displayTitle, formatBenefit } from "@/lib/api";
import DeadlineBadge from "@/components/DeadlineBadge";
import { effectiveDeadline } from "@/lib/deadline";
import { sharedSession } from "@/lib/session";
import { programTypeLabel, regionLabel, programRegionsChip, programRegionsLabel } from "@/lib/displayLabels";

export default function ProgramsPage() {
  const [typeFilter, setTypeFilter] = useState("");
  const [regionFilter, setRegionFilter] = useState("");
  const [search, setSearch] = useState("");
  const [query, setQuery] = useState("");
  const [retry, setRetry] = useState(0);
  const [result, setResult] = useState<{ key: string; items: ApiProgram[]; error: boolean } | null>(null);
  const requestKey = JSON.stringify([typeFilter, regionFilter, query, retry]);
  const loading = result?.key !== requestKey;
  const programs = loading ? [] : result.items;

  useEffect(() => {
    const controller = new AbortController();
    sharedSession()
      .then((session) => api.getPrograms({
        program_type: typeFilter || undefined,
        region: regionFilter || undefined,
        q: query || undefined,
      }, controller.signal, session.status === "unauthenticated"))
      .then((data) => {
        if (!controller.signal.aborted) setResult({ key: requestKey, items: data.items || [], error: false });
      })
      .catch(() => {
        if (!controller.signal.aborted) setResult({ key: requestKey, items: [], error: true });
      });
    return () => controller.abort();
  }, [typeFilter, regionFilter, query, requestKey]);

  return (
    <main className="min-h-screen px-4 py-8">
      <div className="w-full max-w-md mx-auto">
        {/* Header */}
        <div className="flex items-center justify-between mb-6">
          <h1 className="text-2xl font-bold text-gray-900">
            전체 프로그램
          </h1>
          <Link
            href="/"
            className="text-sm text-gray-500 hover:text-gray-700 transition-colors"
          >
            홈으로
          </Link>
        </div>

        {/* Search: finds every programme, including ones the default list leaves out */}
        <form
          role="search"
          className="mb-4 flex gap-2"
          onSubmit={(event) => {
            event.preventDefault();
            setQuery(search.trim());
          }}
        >
          <input
            type="search"
            value={search}
            onChange={(event) => setSearch(event.target.value)}
            placeholder="혜택 이름으로 찾기"
            aria-label="혜택 검색"
            className="flex-1 rounded-lg border border-gray-200 bg-white px-3 py-2 text-sm"
          />
          <button type="submit" className="rounded-lg bg-teal-600 px-4 py-2 text-sm font-medium text-white">
            검색
          </button>
        </form>

        {/* Filters */}
        <div className="flex gap-2 mb-6 flex-wrap">
          {/* Type filters */}
          {["", "scholarship", "support"].map((t) => (
            <button
              key={t || "all-type"}
              type="button"
              onClick={() => setTypeFilter(t)}
              className={`rounded-full px-4 py-1.5 text-sm font-medium transition-all ${
                typeFilter === t
                  ? "bg-teal-600 text-white"
                  : "bg-white text-gray-600 border border-gray-200 hover:border-gray-300"
              }`}
            >
              {t ? programTypeLabel(t) : "전체"}
            </button>
          ))}

          <div className="w-px bg-gray-200 mx-1" />

          {/* Region filters */}
          {["", "busan", "daegu"].map((r) => (
            <button
              key={r || "all-region"}
              type="button"
              onClick={() => setRegionFilter(r)}
              className={`rounded-full px-4 py-1.5 text-sm font-medium transition-all ${
                regionFilter === r
                  ? "bg-teal-600 text-white"
                  : "bg-white text-gray-600 border border-gray-200 hover:border-gray-300"
              }`}
            >
              {r ? regionLabel(r) : "전체 지역"}
            </button>
          ))}
        </div>

        {/* List */}
        {loading ? (
          <div className="flex justify-center py-16">
            <div className="h-8 w-8 border-2 border-teal-600 border-t-transparent rounded-full animate-spin" />
          </div>
        ) : result?.error ? (
          <div role="alert" className="rounded-xl bg-white p-8 text-center shadow-sm border border-gray-100">
            <p className="text-gray-500">프로그램을 불러오지 못했어요.</p>
            <button type="button" onClick={() => setRetry(value => value + 1)} className="mt-4 text-teal-700 font-medium">다시 시도</button>
          </div>
        ) : programs.length === 0 ? (
          <div className="rounded-xl bg-white p-8 text-center shadow-sm border border-gray-100">
            <p className="text-gray-500">등록된 프로그램이 없어요</p>
          </div>
        ) : (
          <div className="space-y-3">
            {programs.map((program) => (
              <Link
                key={program.id}
                href={`/programs/${program.id}`}
                className="block rounded-xl bg-white p-4 shadow-sm border border-gray-100 hover:shadow-md transition-shadow"
              >
                <div className="flex items-center gap-2 mb-1.5">
                  <span className="text-xs font-medium text-gray-500 bg-gray-50 px-2 py-0.5 rounded">
                    {programTypeLabel(program.program_type)}
                  </span>
                  {programRegionsChip(program) && (
                    <span className="text-xs font-medium text-gray-500 bg-gray-50 px-2 py-0.5 rounded" title={programRegionsLabel(program)}>
                      {programRegionsChip(program)}
                    </span>
                  )}
                  {effectiveDeadline(program) && <DeadlineBadge deadline={effectiveDeadline(program)!} />}
                </div>
                <h3 className="font-semibold text-gray-900">
                  {displayTitle(program)}
                </h3>
                {program.youth_fit_label && (
                  <p className="mt-1 text-xs text-gray-500">{program.youth_fit_label}</p>
                )}
                <p className="text-sm text-gray-500 mt-0.5 mb-2">
                  {program.provider_name}
                </p>
                <p className="text-sm font-medium text-teal-600">
                  {formatBenefit(program)}
                </p>
              </Link>
            ))}
          </div>
        )}
      </div>
    </main>
  );
}
