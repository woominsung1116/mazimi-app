"use client";

import { useCallback, useEffect, useState } from "react";
import { adminApi, type AssistantRunAudit } from "@/lib/api";

const statusName: Record<string, string> = {
  received: "접수", planning: "계획", tool_running: "도구 실행", awaiting_confirmation: "승인 대기",
  executing: "실행", validating_response: "응답 검증", completed: "완료", failed: "실패",
  timed_out: "시간 초과", cancelled: "취소",
};
const plannerName: Record<string, string> = {
  fast_path: "규칙", fast_path_fallback: "규칙(모델 대신)", model: "모델", refused: "거절", fallback: "대체 안내",
};

/** Read-only audit of agent runs. The API returns metadata only: no goals,
 * messages, tool results or user ids, so nothing here identifies a user. */
export default function AssistantRunsPage() {
  const [data, setData] = useState<AssistantRunAudit | null>(null);
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(true);
  // State changes only in the request's callbacks, so the mount effect sets
  // none synchronously; a refresh resets loading and error first.
  const fetchRuns = useCallback(
    () =>
      adminApi
        .listAssistantRuns(100)
        .then(
          (runs) => {
            setData(runs);
            setError("");
          },
          (e) => setError(e instanceof Error ? e.message : "불러오지 못했습니다."),
        )
        .finally(() => setLoading(false)),
    [],
  );
  const load = () => {
    setLoading(true);
    setError("");
    void fetchRuns();
  };
  useEffect(() => {
    void fetchRuns();
  }, [fetchRuns]);

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-xl font-bold">AI 비서 실행</h1>
          <p className="text-sm text-gray-600">
            최근 실행의 계획 방식, 도구 순서, 승인·실행 수와 지연 시간입니다. 요청 문장과 사용자 정보는 표시하지 않습니다.
          </p>
        </div>
        <button
          type="button"
          onClick={load}
          disabled={loading}
          className="rounded border border-gray-300 px-3 py-1.5 text-sm disabled:opacity-50"
        >
          {loading ? "불러오는 중" : "새로고침"}
        </button>
      </div>
      {error && <p role="alert" className="text-sm text-red-700">{error}</p>}
      {data && (
        <section className="flex flex-wrap gap-2 text-sm" aria-label="최근 7일 상태별 실행 수">
          {data.last_7_days.length === 0 && <span className="text-gray-500">최근 7일 실행이 없습니다.</span>}
          {data.last_7_days.map((row) => (
            <span key={row.status} className="rounded bg-gray-100 px-2 py-1">
              {statusName[row.status] ?? row.status} {row.count}
            </span>
          ))}
        </section>
      )}
      {data && (
        <div className="overflow-x-auto">
          <table className="min-w-full text-left text-sm">
            <thead className="border-b text-gray-600">
              <tr>
                <th className="py-2 pr-4">시각</th>
                <th className="py-2 pr-4">상태</th>
                <th className="py-2 pr-4">계획</th>
                <th className="py-2 pr-4">목표 유형</th>
                <th className="py-2 pr-4">도구</th>
                <th className="py-2 pr-4">승인/실행</th>
                <th className="py-2 pr-4">지연</th>
                <th className="py-2 pr-4">오류</th>
              </tr>
            </thead>
            <tbody>
              {data.items.map((run) => (
                <tr key={run.id} className="border-b align-top">
                  <td className="py-2 pr-4 whitespace-nowrap">{new Date(run.created_at).toLocaleString("ko-KR")}</td>
                  <td className="py-2 pr-4">{statusName[run.status] ?? run.status}</td>
                  <td className="py-2 pr-4">
                    {run.planner ? plannerName[run.planner] ?? run.planner : "-"}
                    {run.model ? <span className="block text-xs text-gray-500">{run.model}</span> : null}
                  </td>
                  <td className="py-2 pr-4">{run.goal_kind ?? "-"}</td>
                  <td className="py-2 pr-4">
                    <ol className="space-y-0.5">
                      {run.tools.map((tool) => (
                        <li key={tool.seq} className={tool.permission === "confirm_write" ? "text-amber-800" : ""}>
                          {tool.seq + 1}. {tool.tool} · {tool.status}
                          {tool.latency_ms !== null ? ` · ${tool.latency_ms}ms` : ""}
                          {tool.error_code ? ` · ${tool.error_code}` : ""}
                        </li>
                      ))}
                    </ol>
                  </td>
                  <td className="py-2 pr-4">{run.confirmations}/{run.executed}</td>
                  <td className="py-2 pr-4">{run.latency_ms !== null ? `${run.latency_ms}ms` : "-"}</td>
                  <td className="py-2 pr-4">{run.error_code ?? ""}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}
