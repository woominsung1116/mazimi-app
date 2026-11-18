"use client";

import Link from "next/link";
import { useCallback, useEffect, useState } from "react";
import {
  adminApi,
  type AiExtractionAutoRun,
  type AiExtractionQueueFilter,
  type AiExtractionQueueItem,
  type AiExtractionRevertReport,
} from "@/lib/api";
import { autoCheckLabel, describe, errorMessage, show, statusName, typeName } from "./labels";

const filters: { value: AiExtractionQueueFilter; label: string; hint: string }[] = [
  { value: "pending", label: "예외 (검토 필요)", hint: "규칙 검증으로 자동 반영되지 않은 HIGH 신뢰 사실" },
  { value: "audit", label: "감사 표본", hint: "자동 반영된 사실 중 무작위로 뽑힌, 사람이 확인할 항목" },
  { value: "auto_applied", label: "자동 반영", hint: "규칙 파서가 같은 값을 읽어 자동 반영된 사실" },
  { value: "flagged", label: "낮은 신뢰·경고", hint: "MEDIUM/LOW 신뢰도이거나 검증 경고가 있는 사실" },
  { value: "reverted", label: "자동 반영 취소", hint: "관리자가 되돌린 자동 반영 사실" },
];

const triggerName: Record<AiExtractionAutoRun["trigger"], string> = {
  nightly: "야간",
  queue: "원문 변경",
  manual: "수동",
};

function NoteField({ note, setNote }: { note: string; setNote: (v: string) => void }) {
  return (
    <label className="block text-sm">
      메모 (10자 이상, 감사 확인·취소 근거)
      <textarea
        value={note}
        onChange={(e) => setNote(e.target.value)}
        maxLength={1000}
        className="mt-1 w-full rounded border border-gray-300 p-2 text-sm"
      />
    </label>
  );
}

function Report({ report }: { report: AiExtractionRevertReport }) {
  return (
    <div className="rounded border border-indigo-200 bg-indigo-50 p-3 text-sm">
      {report.dry_run ? (
        <p>되돌릴 자동 반영 사실 {report.matched}건 (아직 아무것도 바꾸지 않았습니다)</p>
      ) : (
        <p>
          {report.matched}건 중 {report.reverted.length}건 되돌림
          {report.skipped.length > 0 && `, ${report.skipped.length}건 보류`}
        </p>
      )}
      {report.truncated && (
        <p className="text-xs text-amber-800">1,000건까지만 처리했습니다. 같은 되돌리기를 한 번 더 실행하세요.</p>
      )}
      {report.skipped.map((s) => (
        <p key={s.candidate_id} className="text-xs text-amber-800">
          보류 {s.candidate_id.slice(0, 8)}: {s.reason}
        </p>
      ))}
      {report.kept.map((k) => (
        <p key={`${k.candidate_id}-${k.target}`} className="text-xs text-gray-600">
          유지 {k.target}: 자동 반영 뒤 바뀐 값이라 그대로 둠
        </p>
      ))}
    </div>
  );
}

export default function AiExtractionQueuePage() {
  const [filter, setFilter] = useState<AiExtractionQueueFilter>("pending");
  const [items, setItems] = useState<AiExtractionQueueItem[] | null>(null);
  const [runs, setRuns] = useState<AiExtractionAutoRun[]>([]);
  const [note, setNote] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [preview, setPreview] = useState<{ runId: string; report: AiExtractionRevertReport } | null>(null);
  const [report, setReport] = useState<AiExtractionRevertReport | null>(null);

  const load = useCallback(async (next: AiExtractionQueueFilter) => {
    const [queue, auto] = await Promise.all([
      adminApi.listAiExtractionQueue(next),
      adminApi.listAiExtractionAutoRuns(),
    ]);
    setItems(queue.items);
    setRuns(auto.runs);
  }, []);

  useEffect(() => {
    let live = true;
    Promise.all([adminApi.listAiExtractionQueue(filter), adminApi.listAiExtractionAutoRuns()])
      .then(([queue, auto]) => {
        if (!live) return;
        setItems(queue.items);
        setRuns(auto.runs);
      })
      .catch((e) => live && setError(errorMessage(e)));
    return () => {
      live = false;
    };
  }, [filter]);

  const noteOk = () => {
    if (note.trim().length >= 10) return true;
    setError("메모를 10자 이상 입력하세요.");
    return false;
  };

  async function act(task: () => Promise<void>) {
    setBusy(true);
    setError("");
    try {
      await task();
      await load(filter);
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  }

  const confirmAudit = (id: string) =>
    noteOk() &&
    act(async () => {
      await adminApi.confirmAiExtractionAudit(id, note.trim());
    });

  const revertOne = (id: string) =>
    noteOk() &&
    act(async () => {
      setReport(await adminApi.revertAiExtraction({ candidate_id: id, note: note.trim() }));
    });

  // Two steps: a dry run shows how many facts the run applied, then the
  // admin confirms with the note.
  const previewRun = (runId: string) =>
    noteOk() &&
    act(async () => {
      setReport(null);
      setPreview({
        runId,
        report: await adminApi.revertAiExtraction({ run_id: runId, note: note.trim(), dry_run: true }),
      });
    });

  const revertRun = (runId: string) =>
    noteOk() &&
    act(async () => {
      setReport(await adminApi.revertAiExtraction({ run_id: runId, note: note.trim() }));
      setPreview(null);
    });

  const active = filters.find((f) => f.value === filter);

  return (
    <div className="space-y-6">
      <header className="space-y-1">
        <h1 className="text-2xl font-bold">AI 추출 검토</h1>
        <p className="text-sm text-gray-600">
          규칙 파서가 같은 근거 문장에서 같은 값을 독립적으로 읽은 사실은 운영 설정에 따라 자동 반영됩니다. 이 화면은
          기본으로 예외만 보여 줍니다. 자동 반영된 사실은 감사 표본으로 확인하고, 잘못된 실행은 한 번에 되돌릴 수
          있습니다.
        </p>
      </header>

      {error && (
        <p role="alert" className="text-sm text-red-700">
          {error}
        </p>
      )}

      <div className="flex flex-wrap gap-2" role="tablist" aria-label="검토 큐 필터">
        {filters.map((f) => (
          <button
            key={f.value}
            type="button"
            role="tab"
            aria-selected={filter === f.value}
            onClick={() => {
              setItems(null);
              setFilter(f.value);
            }}
            className={`rounded border px-3 py-1 text-sm ${
              filter === f.value ? "border-teal-700 bg-teal-700 text-white" : "bg-white"
            }`}
          >
            {f.label}
          </button>
        ))}
      </div>
      {active && <p className="text-xs text-gray-500">{active.hint}</p>}

      <NoteField note={note} setNote={setNote} />
      {report && <Report report={report} />}

      <section className="space-y-3">
        {items === null && <p className="text-sm text-gray-500">불러오는 중…</p>}
        {items?.length === 0 && <p className="text-sm">해당하는 사실이 없습니다.</p>}
        <ul className="space-y-3">
          {items?.map((item) => {
            const auto = item.applied_by === "auto_verified" && item.status === "approved";
            const audit = auto && item.audit_sample && !item.audit_reviewed_at;
            return (
              <li key={item.id} className="space-y-1 rounded border bg-white p-3">
                <div className="flex flex-wrap items-center gap-2 text-sm">
                  <Link href={`/admin/programs/${item.program_id}`} className="font-medium text-teal-800 underline">
                    {item.program_title}
                  </Link>
                  <span className="font-semibold">{typeName[item.fact_type] ?? item.fact_type}</span>
                  <span>{describe(item)}</span>
                  <span className="rounded bg-gray-100 px-1 text-xs">{statusName[item.status] ?? item.status}</span>
                  {auto && (
                    <span className="rounded bg-indigo-100 px-1 text-xs text-indigo-800" title={item.auto_rule ?? undefined}>
                      자동
                    </span>
                  )}
                  {audit && <span className="rounded bg-purple-100 px-1 text-xs text-purple-800">감사 표본</span>}
                </div>
                {item.evidence && <p className="text-xs text-gray-700">근거: “{item.evidence.quote}”</p>}
                {autoCheckLabel(item.auto_check) && (
                  <p className="text-xs text-gray-600">{autoCheckLabel(item.auto_check)}</p>
                )}
                {item.reviewed_at && <p className="text-xs text-gray-500">반영 {show(item.reviewed_at)}</p>}
                {auto && (
                  <div className="flex gap-2 pt-1">
                    {audit && (
                      <button
                        type="button"
                        disabled={busy}
                        onClick={() => void confirmAudit(item.id)}
                        className="rounded bg-teal-700 px-3 py-1 text-xs text-white disabled:opacity-50"
                      >
                        감사 확인
                      </button>
                    )}
                    <button
                      type="button"
                      disabled={busy}
                      onClick={() => void revertOne(item.id)}
                      className="rounded border border-red-300 px-3 py-1 text-xs text-red-800 disabled:opacity-50"
                    >
                      이 사실 되돌리기
                    </button>
                  </div>
                )}
              </li>
            );
          })}
        </ul>
      </section>

      <section className="space-y-3">
        <h2 className="text-lg font-semibold">자동 반영 실행</h2>
        {runs.length === 0 && <p className="text-sm text-gray-500">자동 반영한 실행이 없습니다.</p>}
        <ul className="space-y-2">
          {runs.map((run) => (
            <li key={run.id} className="flex flex-wrap items-center gap-3 rounded border bg-white p-3 text-sm">
              <span className="font-medium">{show(run.started_at)}</span>
              <span className="rounded bg-gray-100 px-1 text-xs">{triggerName[run.trigger] ?? run.trigger}</span>
              <span>
                자동 반영 {run.auto_applied} · 유지 {run.still_applied} · 취소 {run.reverted} · 감사 대기 {run.audit_open}
                {run.auto_cap_skips > 0 && ` · 상한 보류 ${run.auto_cap_skips}`}
              </span>
              {run.still_applied > 0 &&
                (preview?.runId === run.id ? (
                  <span className="flex items-center gap-2">
                    <span className="text-xs text-red-800">{preview.report.matched}건을 되돌립니다.</span>
                    <button
                      type="button"
                      disabled={busy}
                      onClick={() => void revertRun(run.id)}
                      className="rounded bg-red-700 px-3 py-1 text-xs text-white disabled:opacity-50"
                    >
                      실행 전체 되돌리기 확정
                    </button>
                    <button type="button" onClick={() => setPreview(null)} className="rounded border px-3 py-1 text-xs">
                      취소
                    </button>
                  </span>
                ) : (
                  <button
                    type="button"
                    disabled={busy}
                    onClick={() => void previewRun(run.id)}
                    className="rounded border border-red-300 px-3 py-1 text-xs text-red-800 disabled:opacity-50"
                  >
                    실행 되돌리기
                  </button>
                ))}
            </li>
          ))}
        </ul>
      </section>
    </div>
  );
}
