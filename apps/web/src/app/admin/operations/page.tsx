"use client";

import Link from "next/link";
import { useCallback, useEffect, useState } from "react";
import {
  adminApi,
  DataQualityIssue,
  OperationalEvent,
  OperationalEventSeverity,
  ProgramLifecycleItem,
  ProgramMaintenanceResponse,
} from "@/lib/api";

const severityStyle: Record<OperationalEventSeverity, string> = {
  info: "bg-blue-50 text-blue-700 border-blue-200",
  warning: "bg-amber-50 text-amber-700 border-amber-200",
  error: "bg-red-50 text-red-700 border-red-200",
  critical: "bg-rose-100 text-rose-800 border-rose-300",
};

const severityLabel: Record<OperationalEventSeverity, string> = {
  info: "정보",
  warning: "주의",
  error: "오류",
  critical: "긴급",
};

const issueLabel: Record<string, string> = {
  invalid_title: "제목 형식 오류",
  invalid_age_range: "나이 범위 오류",
  negative_benefit_amount: "음수 혜택 금액",
  invalid_date_range: "신청 기간 오류",
  unknown_region: "알 수 없는 지역",
};

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : "요청을 처리하지 못했습니다.";
}

function formatDate(value: string | null): string {
  if (!value) return "-";
  return new Intl.DateTimeFormat("ko-KR", {
    dateStyle: "medium",
    timeStyle: "short",
  }).format(new Date(value));
}

function LifecycleQueue({
  title,
  total,
  items,
  tone,
}: {
  title: string;
  total: number;
  items: ProgramLifecycleItem[];
  tone: "red" | "amber";
}) {
  const toneClass =
    tone === "red"
      ? "border-red-200 bg-red-50 text-red-700"
      : "border-amber-200 bg-amber-50 text-amber-700";
  return (
    <section className="space-y-3">
      <h2 className="font-semibold text-gray-900">
        {title}
        <span className="ml-2 text-sm font-normal text-gray-400">
          {total.toLocaleString()}건
        </span>
      </h2>
      {items.length === 0 ? (
        <div className="rounded-xl border border-gray-200 bg-white p-6 text-center text-sm text-gray-500">
          해당 프로그램이 없습니다.
        </div>
      ) : (
        <div className="divide-y divide-gray-100 overflow-hidden rounded-xl border border-gray-200 bg-white">
          {items.map((program) => (
            <div
              key={program.id}
              className="flex flex-wrap items-center justify-between gap-3 p-4"
            >
              <div className="min-w-0">
                <Link
                  href={`/admin/programs/${program.id}`}
                  className="font-medium text-gray-900 hover:text-teal-700"
                >
                  {program.title}
                </Link>
                <p className="mt-1 text-xs text-gray-500">
                  {program.provider_name || "제공처 미입력"}
                </p>
              </div>
              <span className={`rounded-full border px-2.5 py-1 text-xs ${toneClass}`}>
                {formatDate(program.effective_end_at)}
              </span>
            </div>
          ))}
        </div>
      )}
    </section>
  );
}

export default function OperationsPage() {
  const [events, setEvents] = useState<OperationalEvent[]>([]);
  const [issues, setIssues] = useState<DataQualityIssue[]>([]);
  const [maintenance, setMaintenance] =
    useState<ProgramMaintenanceResponse | null>(null);
  const [notes, setNotes] = useState<Record<string, string>>({});
  const [loading, setLoading] = useState(true);
  const [workingId, setWorkingId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  // State changes only in the request's callbacks (the mount effect calls
  // this); the error clears once a reload succeeds.
  const load = useCallback(
    () =>
      Promise.all([
        adminApi.listOperationalEvents({ status: "open", limit: 100 }),
        adminApi.listOperationalEvents({
          status: "acknowledged",
          limit: 100,
        }),
        adminApi.listDataQualityIssues({ status: "open", limit: 100 }),
        adminApi.getProgramMaintenance(100),
      ])
        .then(
          ([open, acknowledged, dataQuality, maintenanceData]) => {
            setEvents(
              [...open.items, ...acknowledged.items].sort(
                (left, right) =>
                  new Date(right.last_occurred_at).getTime() -
                  new Date(left.last_occurred_at).getTime()
              )
            );
            setIssues(dataQuality.items);
            setMaintenance(maintenanceData);
            setError(null);
          },
          (loadError) => setError(errorMessage(loadError))
        )
        .finally(() => setLoading(false)),
    []
  );

  useEffect(() => {
    void load();
  }, [load]);

  const reviewEvent = async (
    event: OperationalEvent,
    action: "acknowledge" | "resolve"
  ) => {
    const note = notes[event.id]?.trim();
    if (action === "resolve" && !note) {
      setError("해결 처리에는 조치 내용을 입력해야 합니다.");
      return;
    }
    setWorkingId(event.id);
    setError(null);
    try {
      await adminApi.reviewOperationalEvent(event.id, { action, note });
      await load();
    } catch (reviewError) {
      setError(errorMessage(reviewError));
    } finally {
      setWorkingId(null);
    }
  };

  const reviewIssue = async (
    issue: DataQualityIssue,
    action: "recheck" | "ignore"
  ) => {
    const note = notes[issue.id]?.trim();
    if (action === "ignore" && !note) {
      setError("예외 처리에는 근거를 입력해야 합니다.");
      return;
    }
    setWorkingId(issue.id);
    setError(null);
    try {
      await adminApi.reviewDataQualityIssue(issue.id, { action, note });
      await load();
    } catch (reviewError) {
      setError(errorMessage(reviewError));
    } finally {
      setWorkingId(null);
    }
  };

  return (
    <div className="space-y-8">
      <div>
        <h1 className="text-xl font-bold text-gray-900">운영 상태</h1>
        <p className="mt-1 text-sm text-gray-500">
          워커 장애와 추천 데이터 품질 위반을 확인하고 처리합니다.
        </p>
      </div>

      {error && (
        <div className="rounded-lg border border-red-200 bg-red-50 px-4 py-3 text-sm text-red-700">
          {error}
        </div>
      )}
      {loading && <p className="text-sm text-gray-500">불러오는 중...</p>}

      {!loading && (
        <section className="space-y-3">
          <h2 className="font-semibold text-gray-900">데이터 운영 요약</h2>
          <div className="grid grid-cols-2 gap-3 lg:grid-cols-4">
            <div className="rounded-xl border border-red-200 bg-white p-4">
              <p className="text-xs text-gray-500">활성 상태로 남은 만료 데이터</p>
              <p className="mt-1 text-xl font-bold text-red-600">
                {maintenance?.expired.total.toLocaleString() ?? "-"}
              </p>
            </div>
            <div className="rounded-xl border border-amber-200 bg-white p-4">
              <p className="text-xs text-gray-500">30일 내 만료 예정</p>
              <p className="mt-1 text-xl font-bold text-amber-600">
                {maintenance?.expiring.total.toLocaleString() ?? "-"}
              </p>
            </div>
            <div className="rounded-xl border border-violet-200 bg-white p-4">
              <p className="text-xs text-gray-500">잠재 중복 그룹</p>
              <p className="mt-1 text-xl font-bold text-violet-600">
                {maintenance?.duplicates.total.toLocaleString() ?? "-"}
              </p>
            </div>
            <Link
              href="/admin/sources"
              className="rounded-xl border border-blue-200 bg-white p-4 hover:border-blue-400"
            >
              <p className="text-xs text-gray-500">출처 재검토 필요</p>
              <p className="mt-1 text-xl font-bold text-blue-600">
                {maintenance?.source_reviews.total.toLocaleString() ?? "-"}
              </p>
            </Link>
          </div>
        </section>
      )}

      {!loading && maintenance?.daily_jobs && (
        <section className="space-y-3">
          <h2 className="font-semibold text-gray-900">매일 수집 상태</h2>
          <div className="divide-y rounded-xl border bg-white">
            {maintenance.daily_jobs.map(job => <div key={job.name} className="flex flex-wrap justify-between gap-3 p-4 text-sm">
              <div><p className="font-medium">{job.name}</p><p className="mt-1 text-xs text-gray-500">최근 성공 {formatDate(job.last_success_at)}</p>
                {(job.details.error || job.details.reason) ? <p className="mt-1 text-xs text-red-700">{String(job.details.error || job.details.reason)}</p> : null}
              </div>
              <span className={job.stale || job.status === "failed" || job.status === "disabled" || job.status === "degraded" ? "text-amber-700" : "text-green-700"}>
                {({ success: "완료", running: "진행 중", failed: "실패 · 자동 재시도", disabled: "키 또는 권한 설정 필요", degraded: "완료 · 결과 이상(공고 급감·전부 마감)", pending: "대기" } as Record<string,string>)[job.status] ?? job.status}{job.stale ? " · 26시간 내 성공 없음" : ""}
              </span>
            </div>)}
          </div>
        </section>
      )}

      {!loading && maintenance && (
        <LifecycleQueue
          title="만료 후 활성 상태"
          total={maintenance.expired.total}
          items={maintenance.expired.items}
          tone="red"
        />
      )}

      {!loading && maintenance && (
        <LifecycleQueue
          title="30일 내 만료 예정"
          total={maintenance.expiring.total}
          items={maintenance.expiring.items}
          tone="amber"
        />
      )}

      {!loading && maintenance && (
        <section className="space-y-3">
          <h2 className="font-semibold text-gray-900">
            잠재 중복
            <span className="ml-2 text-sm font-normal text-gray-400">
              {maintenance.duplicates.total.toLocaleString()}그룹
            </span>
          </h2>
          {maintenance.duplicates.items.length === 0 ? (
            <div className="rounded-xl border border-gray-200 bg-white p-6 text-center text-sm text-gray-500">
              공식 URL 또는 제공처·제목이 일치하는 중복 후보가 없습니다.
            </div>
          ) : (
            maintenance.duplicates.items.map((group) => (
              <article
                key={`${group.match_type}:${group.match_key}`}
                className="rounded-xl border border-violet-200 bg-white p-5"
              >
                <div className="flex flex-wrap items-center justify-between gap-2">
                  <p className="text-xs font-semibold text-violet-700">
                    {group.match_type === "official_url"
                      ? "공식 URL 일치"
                      : "제공처·제목 일치"}
                  </p>
                  <span className="text-xs text-gray-500">
                    전체 {group.item_count}건 · 활성 {group.active_count}건
                  </span>
                </div>
                <div className="mt-3 space-y-2">
                  {group.program_ids.map((programId, index) => (
                    <Link
                      key={programId}
                      href={`/admin/programs/${programId}`}
                      className="block rounded-lg bg-gray-50 px-3 py-2 text-sm text-gray-800 hover:bg-violet-50 hover:text-violet-800"
                    >
                      {group.titles[index]}
                      <span className="ml-2 text-xs text-gray-400">
                        {group.providers[index] || "제공처 미입력"}
                      </span>
                    </Link>
                  ))}
                </div>
              </article>
            ))
          )}
        </section>
      )}

      {!loading && maintenance && maintenance.source_reviews.total > 0 && (
        <section className="rounded-xl border border-blue-200 bg-blue-50 p-5">
          <div className="flex flex-wrap items-center justify-between gap-3">
            <div>
              <h2 className="font-semibold text-blue-900">출처 재검토 필요</h2>
              <p className="mt-1 text-sm text-blue-700">
                검증 기한 초과 또는 출처 보류 상태 {maintenance.source_reviews.total.toLocaleString()}건이 있습니다.
              </p>
            </div>
            <Link
              href="/admin/sources"
              className="rounded-lg bg-blue-700 px-3 py-2 text-sm font-semibold text-white hover:bg-blue-800"
            >
              출처 검토로 이동
            </Link>
          </div>
        </section>
      )}

      {!loading && (
        <section className="space-y-3">
          <div className="flex items-center justify-between">
            <h2 className="font-semibold text-gray-900">
              운영 이벤트
              <span className="ml-2 text-sm font-normal text-gray-400">
                {events.length}건
              </span>
            </h2>
            <button
              type="button"
              onClick={() => void load()}
              className="text-sm font-medium text-teal-600 hover:text-teal-800"
            >
              새로고침
            </button>
          </div>

          {events.length === 0 ? (
            <div className="rounded-xl border border-gray-200 bg-white p-8 text-center text-sm text-gray-500">
              처리할 운영 이벤트가 없습니다.
            </div>
          ) : (
            events.map((event) => (
              <article
                key={event.id}
                className="rounded-xl border border-gray-200 bg-white p-5 shadow-sm"
              >
                <div className="flex flex-wrap items-start justify-between gap-3">
                  <div>
                    <div className="flex flex-wrap items-center gap-2">
                      <span
                        className={`rounded-full border px-2 py-0.5 text-xs font-semibold ${severityStyle[event.severity]}`}
                      >
                        {severityLabel[event.severity]}
                      </span>
                      <span className="text-xs text-gray-400">
                        {event.component} · {event.event_type}
                      </span>
                      {event.status === "acknowledged" && (
                        <span className="rounded-full bg-gray-100 px-2 py-0.5 text-xs text-gray-600">
                          확인됨
                        </span>
                      )}
                    </div>
                    <p className="mt-2 font-medium text-gray-900">
                      {event.message}
                    </p>
                    <p className="mt-1 text-xs text-gray-500">
                      최근 {new Date(event.last_occurred_at).toLocaleString("ko-KR")}
                      {event.occurrence_count > 1 &&
                        ` · ${event.occurrence_count.toLocaleString()}회 발생`}
                    </p>
                  </div>
                  {event.delivered_at ? (
                    <span className="text-xs text-emerald-600">웹훅 전달됨</span>
                  ) : event.last_delivery_error ? (
                    <span className="text-xs text-red-600">
                      웹훅 재시도 중 ({event.delivery_attempt_count}회)
                    </span>
                  ) : null}
                </div>

                <details className="mt-3 text-xs text-gray-500">
                  <summary className="cursor-pointer select-none">세부 정보</summary>
                  <pre className="mt-2 overflow-x-auto rounded-lg bg-gray-50 p-3 whitespace-pre-wrap break-all">
                    {JSON.stringify(event.details, null, 2)}
                  </pre>
                </details>

                <div className="mt-4 flex flex-col gap-2 sm:flex-row">
                  <input
                    value={notes[event.id] ?? ""}
                    onChange={(input) =>
                      setNotes((current) => ({
                        ...current,
                        [event.id]: input.target.value,
                      }))
                    }
                    maxLength={1000}
                    placeholder="확인 내용 또는 해결 조치"
                    className="min-w-0 flex-1 rounded-lg border border-gray-300 px-3 py-2 text-sm focus:border-teal-500 focus:outline-none focus:ring-2 focus:ring-teal-100"
                  />
                  {event.status === "open" && (
                    <button
                      type="button"
                      disabled={workingId === event.id}
                      onClick={() => void reviewEvent(event, "acknowledge")}
                      className="rounded-lg border border-gray-300 px-3 py-2 text-sm font-medium text-gray-700 hover:bg-gray-50 disabled:opacity-50"
                    >
                      확인
                    </button>
                  )}
                  <button
                    type="button"
                    disabled={workingId === event.id}
                    onClick={() => void reviewEvent(event, "resolve")}
                    className="rounded-lg bg-teal-600 px-3 py-2 text-sm font-medium text-white hover:bg-teal-700 disabled:opacity-50"
                  >
                    해결 완료
                  </button>
                </div>
              </article>
            ))
          )}
        </section>
      )}

      {!loading && (
        <section className="space-y-3">
          <h2 className="font-semibold text-gray-900">
            데이터 품질
            <span className="ml-2 text-sm font-normal text-gray-400">
              {issues.length}건
            </span>
          </h2>
          {issues.length === 0 ? (
            <div className="rounded-xl border border-gray-200 bg-white p-8 text-center text-sm text-gray-500">
              검토할 데이터 품질 이슈가 없습니다.
            </div>
          ) : (
            issues.map((issue) => (
              <article
                key={issue.id}
                className="rounded-xl border border-gray-200 bg-white p-5 shadow-sm"
              >
                <div className="flex flex-wrap items-start justify-between gap-3">
                  <div>
                    <p className="text-xs font-semibold text-amber-700">
                      {issueLabel[issue.issue_code] ?? issue.issue_code}
                    </p>
                    <Link
                      href={`/admin/programs/${issue.program_id}`}
                      className="mt-1 block font-medium text-gray-900 hover:text-teal-700"
                    >
                      {issue.program_title}
                    </Link>
                    <p className="mt-1 text-xs text-gray-500">
                      감지 {new Date(issue.last_detected_at).toLocaleString("ko-KR")}
                    </p>
                  </div>
                  <code className="max-w-full overflow-x-auto rounded bg-gray-50 px-2 py-1 text-xs text-gray-600">
                    {JSON.stringify(issue.original_value)}
                  </code>
                </div>
                <div className="mt-4 flex flex-col gap-2 sm:flex-row">
                  <input
                    value={notes[issue.id] ?? ""}
                    onChange={(input) =>
                      setNotes((current) => ({
                        ...current,
                        [issue.id]: input.target.value,
                      }))
                    }
                    maxLength={1000}
                    placeholder="예외 처리 근거(선택)"
                    className="min-w-0 flex-1 rounded-lg border border-gray-300 px-3 py-2 text-sm focus:border-teal-500 focus:outline-none focus:ring-2 focus:ring-teal-100"
                  />
                  <button
                    type="button"
                    disabled={workingId === issue.id}
                    onClick={() => void reviewIssue(issue, "recheck")}
                    className="rounded-lg bg-teal-600 px-3 py-2 text-sm font-medium text-white hover:bg-teal-700 disabled:opacity-50"
                  >
                    수정 재검증
                  </button>
                  <button
                    type="button"
                    disabled={workingId === issue.id}
                    onClick={() => void reviewIssue(issue, "ignore")}
                    className="rounded-lg border border-gray-300 px-3 py-2 text-sm font-medium text-gray-700 hover:bg-gray-50 disabled:opacity-50"
                  >
                    근거 남기고 예외
                  </button>
                </div>
              </article>
            ))
          )}
        </section>
      )}
    </div>
  );
}
