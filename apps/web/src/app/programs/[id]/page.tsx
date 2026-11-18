"use client";

import { useEffect, useState } from "react";
import { useParams } from "next/navigation";
import Link from "next/link";
import { api, ApiError, type ApiProgram, type ApplicationResource, displayTitle, formatBenefit, hasBenefitAmount, resourceMetaLine, safeDestinationUrl, safeResourceUrl } from "@/lib/api";
import DeadlineBadge from "@/components/DeadlineBadge";
import { effectiveDeadline, formatDeadlineDate } from "@/lib/deadline";
import { safeHttpsUrl } from "@/lib/safeUrl";
import { programTypeLabel, programRegionsChip, programRegionsLabel } from "@/lib/displayLabels";
import { displayLines } from "@/lib/displayText";

export default function ProgramDetailPage() {
  const params = useParams();
  const id = params.id as string;

  const [program, setProgram] = useState<ApiProgram | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  // 404: no such programme, or it closed or was retired (the API cannot
  // tell which); a saved one stays in 저장한 항목.
  const [gone, setGone] = useState(false);

  useEffect(() => {
    if (!id) return;
    api
      .getProgram(id)
      .then((data) => {
        setProgram(data);
        setLoading(false);
      })
      .catch((err) => {
        setGone(err instanceof ApiError && err.status === 404);
        setError(err.message);
        setLoading(false);
      });
  }, [id]);

  if (loading) {
    return (
      <main className="flex min-h-screen items-center justify-center px-4">
        <div className="h-8 w-8 border-2 border-teal-600 border-t-transparent rounded-full animate-spin" />
      </main>
    );
  }

  if (error || !program) {
    return (
      <main className="flex min-h-screen items-center justify-center px-4">
        <div className="text-center space-y-4">
          <p className="text-gray-700">{gone ? "찾을 수 없거나 마감된 혜택이에요" : "프로그램 정보를 불러오지 못했어요"}</p>
          {gone && <p className="text-sm text-gray-500">저장한 혜택이라면 저장한 항목에 그대로 남아 있어요.</p>}
          <Link
            href="/programs"
            className="inline-block rounded-xl bg-teal-600 px-6 py-3 text-sm font-semibold text-white hover:bg-teal-700 transition-colors"
          >
            목록으로 돌아가기
          </Link>
        </div>
      </main>
    );
  }

  // The server picks and labels the destination; the client re-validates it
  // and never falls back to a raw URL. Opening a link is not applying.
  const destination = program.application_policy?.destination ?? null;
  const destinationUrl = safeDestinationUrl(destination);
  // A corporate offer shows one official passage; the full terms stay on the
  // official page, linked right under it.
  const isCorporate = program.program_type === "corporate_benefit";
  // "공식 안내 원문": a corporate offer's passage stays verbatim, never split or cleaned.
  const summaryLines = isCorporate ? (program.summary?.trim() ? [program.summary.trim()] : []) : displayLines(program.summary_lines, program.summary_display, program.summary);
  const officialPageUrl = isCorporate ? safeHttpsUrl(program.official_url) : null;
  const noticeUrl = safeHttpsUrl(program.official_url);
  const deadline = effectiveDeadline(program);
  const deadlineDate = deadline ? formatDeadlineDate(deadline) : null;
  const resources = program.application_policy?.resources;

  return (
    <main className={`min-h-screen px-4 py-8 ${destinationUrl ? "pb-64" : "pb-24"}`}>
      <div className="w-full max-w-md mx-auto">
        {/* Back */}
        <Link
          href="/programs"
          className="inline-flex items-center gap-1 text-sm text-gray-500 hover:text-gray-700 mb-6 transition-colors"
        >
          <svg
            className="h-4 w-4"
            fill="none"
            stroke="currentColor"
            viewBox="0 0 24 24"
          >
            <path
              strokeLinecap="round"
              strokeLinejoin="round"
              strokeWidth={2}
              d="M15 19l-7-7 7-7"
            />
          </svg>
          목록으로
        </Link>

        {/* Header */}
        <div className="mb-6">
          <div className="flex items-center gap-2 mb-2">
            <span className="text-xs font-medium text-gray-500 bg-gray-100 px-2 py-0.5 rounded">
              {programTypeLabel(program.program_type)}
            </span>
            {programRegionsChip(program) && (
              <span className="text-xs font-medium text-gray-500 bg-gray-100 px-2 py-0.5 rounded" title={programRegionsLabel(program)}>
                {programRegionsChip(program)}
              </span>
            )}
            {deadline && <DeadlineBadge deadline={deadline} />}
          </div>
          <h1 className="text-2xl font-bold text-gray-900 mb-1">
            {displayTitle(program)}
          </h1>
          <p className="text-sm text-gray-500">{program.provider_name || program.company_name}</p>
          {program.youth_fit_label && (
            <p className="mt-2 inline-block rounded bg-gray-100 px-2 py-0.5 text-xs text-gray-600">
              {program.youth_fit_label}
            </p>
          )}
        </div>

        {/* Benefit: shown only when an amount is stated */}
        {hasBenefitAmount(program) && (
          <div className="rounded-xl bg-teal-50 p-5 mb-6">
            <p className="text-sm text-teal-700 mb-1">지원 금액</p>
            <p className="text-xl font-bold text-teal-600">{formatBenefit(program)}</p>
          </div>
        )}

        {/* Summary (a corporate offer shows the official page's own sentence) */}
        {summaryLines.length > 0 && (
          <div className="rounded-xl bg-white p-5 shadow-sm border border-gray-100 mb-4">
            <h2 className="text-base font-semibold text-gray-900 mb-2">
              {isCorporate ? "공식 안내 원문 · 세부 조건은 공식 페이지에서 확인" : "요약"}
            </h2>
            {/* Server-cleaned lines, or the raw summary cleaned here (no "○ …○ …") */}
            {summaryLines.length > 1 ? (
              <ul className="list-disc space-y-1 pl-5 text-sm text-gray-600 leading-relaxed">
                {summaryLines.map((line, index) => <li key={index}>{line}</li>)}
              </ul>
            ) : (
              <p className="text-sm text-gray-600 leading-relaxed">{summaryLines[0]}</p>
            )}
            {isCorporate && (
              officialPageUrl ? (
                <a
                  href={officialPageUrl}
                  target="_blank"
                  rel="noopener noreferrer"
                  className="mt-3 inline-block text-sm font-medium text-teal-700 underline"
                >
                  공식 페이지에서 전체 조건 확인
                </a>
              ) : (
                <p className="mt-3 text-sm text-gray-500">전체 조건은 공식 페이지에서 확인해 주세요.</p>
              )
            )}
          </div>
        )}

        {/* Deadline: the earliest end date (application, notice or validity) */}
        {deadlineDate && (
          <div className="rounded-xl bg-white p-5 shadow-sm border border-gray-100 mb-6">
            <h2 className="text-base font-semibold text-gray-900 mb-2">
              신청 기한
            </h2>
            <p className="text-sm text-gray-600">
              {deadlineDate}
            </p>
          </div>
        )}

        {/* One quiet line: the official notice decides. No source labels or dates. */}
        <p className="mb-6 text-xs text-gray-500">
          최종 조건은 공식 공고에서 확인해 주세요
          {noticeUrl && (
            <>
              {" · "}
              <a href={noticeUrl} target="_blank" rel="noopener noreferrer" className="font-medium text-teal-700 underline">공식 공고 보기</a>
            </>
          )}
        </p>

        {/* Only what the person can act on: no pipeline status */}
        {resources && (resources.notice || resources.templates.length > 0) && <OfficialResources resources={resources} />}

        {/* Official destination */}
        {destination && destinationUrl && (
          // Sits on top of BottomNav (fixed, z-40), not under it.
          <div className="fixed left-0 right-0 z-50 bg-white border-t border-gray-100 px-4 py-4" style={{ bottom: "var(--bottom-nav-height)" }}>
            <div className="max-w-md mx-auto space-y-2">
              <p className="text-xs text-gray-500 text-center">{destination.explanation}</p>
              <a
                href={destinationUrl}
                target="_blank"
                rel="noopener noreferrer"
                className="flex items-center justify-center gap-2 w-full rounded-xl bg-teal-600 px-6 py-3.5 text-base font-semibold text-white hover:bg-teal-700 transition-colors"
              >
                {destination.label}
                <svg
                  className="h-4 w-4"
                  fill="none"
                  stroke="currentColor"
                  viewBox="0 0 24 24"
                >
                  <path
                    strokeLinecap="round"
                    strokeLinejoin="round"
                    strokeWidth={2}
                    d="M10 6H6a2 2 0 00-2 2v10a2 2 0 002 2h10a2 2 0 002-2v-4M14 4h6m0 0v6m0-6L10 14"
                  />
                </svg>
              </a>
            </div>
          </div>
        )}
      </div>
    </main>
  );
}

type Resources = NonNullable<NonNullable<ApiProgram["application_policy"]>["resources"]>;

/** 공식 신청처 · 제출 양식: the reviewed notice and blank forms, as on mobile. Links only; submission stays with the institution. */
function OfficialResources({ resources }: { resources: Resources }) {
  const link = (resource: ApplicationResource, text: string) => {
    const href = safeResourceUrl(resource);
    return href ? (
      <a
        href={href}
        target="_blank"
        rel="noopener noreferrer"
        aria-label={`${text} ${resource.label} (새 창)`}
        className="inline-flex items-center rounded-lg border border-gray-200 px-4 py-2 text-sm font-medium text-gray-700 hover:bg-gray-50"
      >
        {text}
      </a>
    ) : null;
  };
  return (
    <section aria-label="공식 신청처 · 제출 양식" className="rounded-xl bg-white p-5 shadow-sm border border-gray-100 mb-6 space-y-3">
      <h2 className="text-base font-semibold text-gray-900">공식 신청처 · 제출 양식</h2>
      {resources.notice && link(resources.notice, "공식 공고 보기")}
      {resources.templates.map((resource) => (
        <div key={resource.id} className="space-y-1">
          <p className="text-sm font-medium text-gray-900">{resource.label}</p>
          <p className="text-xs text-gray-500">{resourceMetaLine(resource)}</p>
          {link(resource, "공식 양식 받기")}
        </div>
      ))}
      {resources.templates.length > 0 && <p className="text-xs text-gray-500">제출은 기관에 직접 해 주세요.</p>}
    </section>
  );
}
