"use client";

import Link from "next/link";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  adminApi,
  ApiError,
  MonitoredSourceProgram,
  SourceCandidateDetail,
  SourceCandidateListItem,
  SourceCandidateStatus,
  SourceReviewResolution,
} from "@/lib/api";
import CandidateApprovalForm from "./CandidateApprovalForm";

type MonitorReviewStatus =
  | "change_review"
  | "unreachable_review"
  | "verification_due";
type QueueStatus = "review" | MonitorReviewStatus;
type QueueFilter = "all" | QueueStatus;

const QUEUE_STATUSES: QueueStatus[] = [
  "review",
  "change_review",
  "unreachable_review",
  "verification_due",
];

const STATUS_META: Record<
  QueueStatus,
  { label: string; badge: string; description: string }
> = {
  review: {
    label: "신규 후보",
    badge: "bg-violet-100 text-violet-700",
    description: "공식 페이지에서 발견한 신규 혜택 후보입니다.",
  },
  change_review: {
    label: "내용 변경",
    badge: "bg-amber-100 text-amber-800",
    description: "공식 페이지 내용이 이전 검증본과 달라졌습니다.",
  },
  unreachable_review: {
    label: "접속 실패",
    badge: "bg-red-100 text-red-700",
    description: "공식 페이지가 반복적으로 응답하지 않았습니다.",
  },
  verification_due: {
    label: "정기 검증",
    badge: "bg-blue-100 text-blue-700",
    description: "정기 재검증 기한이 지났습니다.",
  },
};

function statusMeta(status: SourceCandidateStatus) {
  if (status in STATUS_META) {
    return STATUS_META[status as QueueStatus];
  }
  return {
    label: status,
    badge: "bg-gray-100 text-gray-700",
    description: "검토 상태를 확인하세요.",
  };
}

function formatDate(value: string | null): string {
  if (!value) return "-";
  return new Intl.DateTimeFormat("ko-KR", {
    dateStyle: "medium",
    timeStyle: "short",
  }).format(new Date(value));
}

/** Crawler-discovered URLs are rendered as links only when they are http(s). */
function isHttpUrl(value: string): boolean {
  try {
    const url = new URL(value);
    return url.protocol === "https:" || url.protocol === "http:";
  } catch {
    return false;
  }
}

function shortHash(value: string | null): string {
  if (!value) return "-";
  return value.length > 14 ? value.slice(0, 14) + "…" : value;
}

function getErrorMessage(error: unknown): string {
  if (error instanceof ApiError) {
    const body = error.body as { error?: string } | undefined;
    return body?.error || "요청을 처리하지 못했습니다.";
  }
  return "요청을 처리하지 못했습니다.";
}

function StatusBadge({ status }: { status: SourceCandidateStatus }) {
  const meta = statusMeta(status);
  return (
    <span
      className={
        "inline-flex items-center rounded-full px-2.5 py-1 text-xs font-semibold " +
        meta.badge
      }
    >
      {meta.label}
    </span>
  );
}

function ProgramReviewCard({
  candidateId,
  program,
  note,
  busy,
  onNoteChange,
  onResolve,
}: {
  candidateId: string;
  program: MonitoredSourceProgram;
  note: string;
  busy: boolean;
  onNoteChange: (value: string) => void;
  onResolve: (
    programId: string,
    candidateId: string,
    resolution: SourceReviewResolution
  ) => void;
}) {
  const reviewable = !["current", "retired"].includes(
    program.verification_state
  );

  return (
    <article className="rounded-xl border border-gray-200 bg-white p-4">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div className="min-w-0">
          <div className="flex flex-wrap items-center gap-2">
            <h3 className="font-semibold text-gray-900">{program.title}</h3>
            <span
              className={
                "rounded-full px-2 py-0.5 text-xs font-medium " +
                (program.is_active
                  ? "bg-green-100 text-green-700"
                  : "bg-gray-100 text-gray-600")
              }
            >
              {program.is_active ? "게시 중" : "게시 보류"}
            </span>
          </div>
          <p className="mt-1 text-xs text-gray-500">
            {program.provider_name || "제공처 미입력"} ·{" "}
            {program.benefit_category || "분류 미입력"}
          </p>
        </div>
        <Link
          href={"/admin/programs/" + program.program_id}
          target="_blank"
          className="shrink-0 text-sm font-medium text-teal-700 hover:text-teal-900"
        >
          프로그램 편집 ↗
        </Link>
      </div>

      {program.summary && (
        <div className="mt-4 rounded-lg bg-gray-50 p-3">
          <p className="mb-1 text-xs font-semibold text-gray-500">
            현재 게시된 요약
          </p>
          <p className="whitespace-pre-wrap text-sm leading-6 text-gray-700">
            {program.summary}
          </p>
        </div>
      )}

      <dl className="mt-4 grid grid-cols-2 gap-x-4 gap-y-2 text-xs">
        <div>
          <dt className="text-gray-400">검증 상태</dt>
          <dd className="mt-0.5 font-medium text-gray-700">
            {program.verification_state}
          </dd>
        </div>
        <div>
          <dt className="text-gray-400">다음 검증일</dt>
          <dd className="mt-0.5 font-medium text-gray-700">
            {formatDate(program.review_due_at)}
          </dd>
        </div>
      </dl>

      {program.last_error && (
        <p className="mt-3 rounded-lg bg-red-50 px-3 py-2 text-xs text-red-700">
          {program.last_error}
        </p>
      )}

      {reviewable ? (
        <div className="mt-4 border-t border-gray-100 pt-4">
          <label className="block text-xs font-semibold text-gray-600">
            검토 근거 메모
          </label>
          <textarea
            value={note}
            onChange={(event) => onNoteChange(event.target.value)}
            maxLength={1000}
            rows={2}
            placeholder="공식 페이지에서 확인한 내용과 판단 근거를 남겨주세요."
            className="mt-2 w-full resize-y rounded-lg border border-gray-300 px-3 py-2 text-sm outline-none transition focus:border-teal-500 focus:ring-2 focus:ring-teal-100"
          />
          <div className="mt-3 flex flex-wrap gap-2">
            <button
              type="button"
              disabled={busy}
              onClick={() =>
                onResolve(
                  program.program_id,
                  candidateId,
                  "confirmed_unchanged"
                )
              }
              className="rounded-lg bg-teal-600 px-3 py-2 text-xs font-semibold text-white hover:bg-teal-700 disabled:opacity-50"
            >
              변경 없음 확인
            </button>
            <button
              type="button"
              disabled={busy}
              onClick={() =>
                onResolve(program.program_id, candidateId, "program_updated")
              }
              className="rounded-lg border border-blue-300 bg-blue-50 px-3 py-2 text-xs font-semibold text-blue-700 hover:bg-blue-100 disabled:opacity-50"
            >
              수정 완료 후 재게시
            </button>
            <button
              type="button"
              disabled={busy}
              onClick={() =>
                onResolve(program.program_id, candidateId, "retired")
              }
              className="rounded-lg border border-red-300 bg-red-50 px-3 py-2 text-xs font-semibold text-red-700 hover:bg-red-100 disabled:opacity-50"
            >
              혜택 종료
            </button>
          </div>
        </div>
      ) : (
        <p className="mt-4 border-t border-gray-100 pt-3 text-xs text-gray-500">
          이 프로그램 출처는 현재 {program.verification_state} 상태입니다.
        </p>
      )}
    </article>
  );
}

export default function SourceReviewPage() {
  const [items, setItems] = useState<SourceCandidateListItem[]>([]);
  const [filter, setFilter] = useState<QueueFilter>("all");
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [detail, setDetail] = useState<SourceCandidateDetail | null>(null);
  const [notes, setNotes] = useState<Record<string, string>>({});
  const [busyProgramId, setBusyProgramId] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [detailFor, setDetailFor] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);

  // Both loaders change state only in the request's callbacks, so the
  // effects below set none synchronously; the refresh button and the review
  // actions reset loading and errors themselves (`reloadQueue`).
  const loadQueue = useCallback(
    () =>
      Promise.all(
        QUEUE_STATUSES.map((status) =>
          adminApi.listSourceCandidates({ status, page: 1, per_page: 100 })
        )
      )
        .then(
          (responses) => {
            const unique = new Map<string, SourceCandidateListItem>();
            responses.flatMap((response) => response.items).forEach((item) => {
              unique.set(item.id, item);
            });
            const next = Array.from(unique.values()).sort((left, right) => {
              const leftDate = left.last_checked_at || left.discovered_at;
              const rightDate = right.last_checked_at || right.discovered_at;
              return new Date(rightDate).getTime() - new Date(leftDate).getTime();
            });
            setItems(next);
            setSelectedId((current) => current || next[0]?.id || null);
            setError(null);
          },
          (loadError) => setError(getErrorMessage(loadError))
        )
        .finally(() => setLoading(false)),
    []
  );

  const reloadQueue = () => {
    setLoading(true);
    setError(null);
    return loadQueue();
  };

  // `detailFor` is the candidate the shown detail (or its error) belongs to:
  // a selection that differs from it is still loading. Only the latest
  // request may land, so a slow answer for an earlier selection is dropped.
  const latestDetail = useRef<string | null>(null);
  const loadDetail = useCallback((candidateId: string) => {
    latestDetail.current = candidateId;
    const current = () => latestDetail.current === candidateId;
    return adminApi
      .getSourceCandidate(candidateId)
      .then(
        (loaded) => {
          if (!current()) return;
          setDetail(loaded);
          setError(null);
        },
        (loadError) => {
          if (!current()) return;
          setDetail(null);
          setError(getErrorMessage(loadError));
        }
      )
      .finally(() => {
        if (current()) setDetailFor(candidateId);
      });
  }, []);
  const detailLoading = selectedId !== null && detailFor !== selectedId;

  useEffect(() => {
    void loadQueue();
  }, [loadQueue]);

  useEffect(() => {
    if (selectedId) void loadDetail(selectedId);
  }, [loadDetail, selectedId]);

  const filteredItems = useMemo(
    () =>
      filter === "all"
        ? items
        : items.filter((item) => item.status === filter),
    [filter, items]
  );

  const counts = useMemo(() => {
    const result: Record<QueueFilter, number> = {
      all: items.length,
      review: 0,
      change_review: 0,
      unreachable_review: 0,
      verification_due: 0,
    };
    for (const item of items) {
      if (item.status in STATUS_META) {
        result[item.status as QueueStatus] += 1;
      }
    }
    return result;
  }, [items]);

  const resolveSource = async (
    programId: string,
    candidateId: string,
    resolution: SourceReviewResolution
  ) => {
    const note = (notes[programId] || "").trim();
    if (note.length < 2) {
      setError("판단 근거 메모를 2자 이상 입력해주세요.");
      return;
    }
    if (
      resolution === "program_updated" &&
      !window.confirm("프로그램 내용을 먼저 수정했는지 확인하셨나요?")
    ) {
      return;
    }
    if (
      resolution === "retired" &&
      !window.confirm("이 출처의 혜택을 종료 처리할까요?")
    ) {
      return;
    }

    setBusyProgramId(programId);
    setError(null);
    setMessage(null);
    try {
      const outcome = await adminApi.reviewProgramSource(programId, {
        candidate_id: candidateId,
        resolution,
        note,
      });
      setNotes((current) => ({ ...current, [programId]: "" }));
      setMessage(
        outcome.is_active
          ? "검토가 기록되었고 프로그램이 다시 게시되었습니다."
          : "검토가 기록되었습니다. 다른 출처 보류가 있으면 게시 보류가 유지됩니다."
      );
      await reloadQueue();
      if (QUEUE_STATUSES.includes(outcome.candidate_status as QueueStatus)) {
        await loadDetail(candidateId);
      } else {
        setSelectedId(null);
        setDetail(null);
        setDetailFor(null);
      }
    } catch (reviewError) {
      setError(getErrorMessage(reviewError));
    } finally {
      setBusyProgramId(null);
    }
  };

  const completeCandidateReview = async () => {
    setMessage("신규 혜택 후보 검토가 기록되었습니다.");
    setSelectedId(null);
    setDetail(null);
    setDetailFor(null);
    await reloadQueue();
  };

  const filters: Array<{ value: QueueFilter; label: string }> = [
    { value: "all", label: "전체" },
    { value: "review", label: "신규 후보" },
    { value: "change_review", label: "내용 변경" },
    { value: "unreachable_review", label: "접속 실패" },
    { value: "verification_due", label: "정기 검증" },
  ];

  return (
    <div className="space-y-5">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <h1 className="text-xl font-bold text-gray-900">공식 출처 검토</h1>
          <p className="mt-1 text-sm text-gray-500">
            변경되거나 만료된 공식 페이지를 확인한 뒤 프로그램 게시 상태를
            복구하거나 종료합니다.
          </p>
        </div>
        <button
          type="button"
          onClick={() => void reloadQueue()}
          disabled={loading}
          className="rounded-lg border border-gray-300 bg-white px-3 py-2 text-sm font-medium text-gray-700 hover:bg-gray-50 disabled:opacity-50"
        >
          {loading ? "새로고침 중…" : "새로고침"}
        </button>
      </div>

      {error && (
        <div className="rounded-lg border border-red-200 bg-red-50 px-4 py-3 text-sm text-red-700">
          {error}
        </div>
      )}
      {message && (
        <div className="rounded-lg border border-teal-200 bg-teal-50 px-4 py-3 text-sm text-teal-800">
          {message}
        </div>
      )}

      <div className="flex gap-2 overflow-x-auto pb-1">
        {filters.map((item) => (
          <button
            key={item.value}
            type="button"
            onClick={() => setFilter(item.value)}
            className={
              "whitespace-nowrap rounded-full px-3 py-2 text-xs font-semibold transition " +
              (filter === item.value
                ? "bg-gray-900 text-white"
                : "border border-gray-200 bg-white text-gray-600 hover:bg-gray-50")
            }
          >
            {item.label} {counts[item.value]}
          </button>
        ))}
      </div>

      <div className="grid min-h-[560px] gap-4 lg:grid-cols-[340px_minmax(0,1fr)]">
        <section className="overflow-hidden rounded-xl border border-gray-200 bg-white">
          <div className="border-b border-gray-100 px-4 py-3">
            <h2 className="text-sm font-semibold text-gray-900">검토 대기 목록</h2>
          </div>
          {loading ? (
            <p className="p-5 text-sm text-gray-500">목록을 불러오는 중...</p>
          ) : filteredItems.length === 0 ? (
            <div className="p-8 text-center">
              <p className="font-medium text-gray-700">검토할 출처가 없습니다.</p>
              <p className="mt-1 text-xs text-gray-400">
                워커가 변경이나 만료를 감지하면 여기에 표시됩니다.
              </p>
            </div>
          ) : (
            <ul className="max-h-[680px] divide-y divide-gray-100 overflow-y-auto">
              {filteredItems.map((item) => (
                <li key={item.id}>
                  <button
                    type="button"
                    onClick={() => setSelectedId(item.id)}
                    className={
                      "w-full px-4 py-4 text-left transition hover:bg-gray-50 " +
                      (selectedId === item.id ? "bg-teal-50" : "")
                    }
                  >
                    <div className="flex items-center justify-between gap-2">
                      <StatusBadge status={item.status} />
                      <span className="text-[11px] text-gray-400">
                        {formatDate(item.last_checked_at)}
                      </span>
                    </div>
                    <p className="mt-2 line-clamp-2 text-sm font-semibold text-gray-900">
                      {item.page_title || item.organization_name}
                    </p>
                    <p className="mt-1 truncate text-xs text-gray-500">
                      {item.domain}
                    </p>
                    <p className="mt-2 text-xs text-gray-400">
                      연결 프로그램 {item.monitored_programs.length}개
                    </p>
                  </button>
                </li>
              ))}
            </ul>
          )}
        </section>

        <section className="min-w-0 rounded-xl border border-gray-200 bg-white">
          {detailLoading ? (
            <p className="p-6 text-sm text-gray-500">출처 상세를 불러오는 중...</p>
          ) : !detail ? (
            <div className="flex min-h-[360px] items-center justify-center p-8 text-sm text-gray-400">
              왼쪽에서 검토할 출처를 선택하세요.
            </div>
          ) : (
            <div>
              <div className="border-b border-gray-100 p-5">
                <div className="flex flex-wrap items-center gap-2">
                  <StatusBadge status={detail.status} />
                  <span className="text-xs text-gray-400">
                    HTTP {detail.http_status ?? "-"} · 실패{" "}
                    {detail.consecutive_failures}회
                  </span>
                </div>
                <h2 className="mt-3 text-lg font-bold text-gray-900">
                  {detail.page_title || detail.organization_name}
                </h2>
                <p className="mt-1 text-sm text-gray-500">
                  {statusMeta(detail.status).description}
                </p>
                {isHttpUrl(detail.url) ? (
                  <a
                    href={detail.url}
                    target="_blank"
                    rel="noreferrer"
                    className="mt-3 block break-all text-sm font-medium text-teal-700 hover:underline"
                  >
                    공식 페이지 열기 ↗
                  </a>
                ) : (
                  <p className="mt-3 break-all text-sm text-gray-500">
                    열 수 없는 주소: {detail.url}
                  </p>
                )}

                <dl className="mt-4 grid grid-cols-2 gap-3 rounded-lg bg-gray-50 p-3 text-xs sm:grid-cols-4">
                  <div>
                    <dt className="text-gray-400">이전 해시</dt>
                    <dd className="mt-1 font-mono text-gray-700">
                      {shortHash(detail.previous_content_hash)}
                    </dd>
                  </div>
                  <div>
                    <dt className="text-gray-400">현재 해시</dt>
                    <dd className="mt-1 font-mono text-gray-700">
                      {shortHash(detail.content_hash)}
                    </dd>
                  </div>
                  <div>
                    <dt className="text-gray-400">최근 성공</dt>
                    <dd className="mt-1 text-gray-700">
                      {formatDate(detail.last_successful_check_at)}
                    </dd>
                  </div>
                  <div>
                    <dt className="text-gray-400">다음 확인</dt>
                    <dd className="mt-1 text-gray-700">
                      {formatDate(detail.next_check_at)}
                    </dd>
                  </div>
                </dl>

                {detail.last_error && (
                  <p className="mt-3 rounded-lg bg-red-50 px-3 py-2 text-xs text-red-700">
                    {detail.last_error}
                  </p>
                )}
              </div>

              <div className="space-y-5 p-5">
                {detail.review_text && (
                  <div>
                    <h3 className="text-sm font-semibold text-gray-900">
                      현재 공식 페이지 검토 텍스트
                    </h3>
                    <p className="mt-1 text-xs text-gray-400">
                      아래 원문과 각 프로그램의 현재 요약을 대조해 사실만
                      갱신하세요.
                    </p>
                    <pre className="mt-3 max-h-64 overflow-auto whitespace-pre-wrap break-words rounded-xl border border-gray-200 bg-gray-50 p-4 font-sans text-xs leading-6 text-gray-700">
                      {detail.review_text}
                    </pre>
                  </div>
                )}

                {detail.status === "review" && (
                  <CandidateApprovalForm
                    candidate={detail}
                    onCompleted={completeCandidateReview}
                  />
                )}

                {detail.status !== "review" && (
                  <div>
                  <h3 className="text-sm font-semibold text-gray-900">
                    연결 프로그램 {detail.monitored_programs.length}개
                  </h3>
                  <div className="mt-3 space-y-3">
                    {detail.monitored_programs.map((program) => (
                      <ProgramReviewCard
                        key={program.program_id}
                        candidateId={detail.id}
                        program={program}
                        note={notes[program.program_id] || ""}
                        busy={busyProgramId === program.program_id}
                        onNoteChange={(value) =>
                          setNotes((current) => ({
                            ...current,
                            [program.program_id]: value,
                          }))
                        }
                        onResolve={(programId, candidateId, resolution) =>
                          void resolveSource(
                            programId,
                            candidateId,
                            resolution
                          )
                        }
                      />
                    ))}
                    {detail.monitored_programs.length === 0 && (
                      <p className="rounded-lg bg-gray-50 p-4 text-sm text-gray-500">
                        이 출처에 연결된 프로그램이 없습니다.
                      </p>
                    )}
                  </div>
                  </div>
                )}
              </div>
            </div>
          )}
        </section>
      </div>
    </div>
  );
}
