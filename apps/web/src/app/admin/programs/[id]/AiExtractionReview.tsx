"use client";
import { useCallback, useEffect, useMemo, useState } from "react";
import { adminApi, type AiExtractionChange, type AiExtractionReview as Review } from "@/lib/api";
import { autoCheckLabel, describe, errorMessage, show, statusName, typeName } from "../../ai-extraction/labels";

const OPEN = new Set(["pending", "flagged"]);
const CONTEXT_CHARS = 80;
const targetName: Record<string, string> = {
  "programs.min_age": "최소 나이",
  "programs.max_age": "최대 나이",
  "programs.application_start_at": "신청 시작",
  "programs.application_end_at": "신청 마감",
  "eligibility_rules.hard_filter_json": "자격 규칙(제외 조건)",
  "eligibility_rules.rule_json": "자격 규칙(설명·정렬)",
  program_documents: "제출 서류 목록",
};

/** Source excerpt around the evidence, sliced by code point like the server. */
function Evidence({ text, start, end }: { text: string; start: number; end: number }) {
  const chars = Array.from(text);
  const from = Math.max(0, start - CONTEXT_CHARS);
  const to = Math.min(chars.length, end + CONTEXT_CHARS);
  return (
    <p className="whitespace-pre-wrap rounded bg-gray-50 p-2 text-xs leading-relaxed text-gray-700">
      {from > 0 && "…"}
      {chars.slice(from, start).join("")}
      <mark className="rounded bg-yellow-200 px-0.5 text-gray-900">{chars.slice(start, end).join("")}</mark>
      {chars.slice(end, to).join("")}
      {to < chars.length && "…"}
    </p>
  );
}

function ChangeRow({ change }: { change: AiExtractionChange }) {
  return (
    <li className="text-xs">
      <span className="font-medium">{targetName[change.target] ?? change.target}</span>: {show(change.current)} →{" "}
      <span className="font-medium text-teal-800">{show(change.proposed)}</span>
      {change.overwrites && <span className="ml-1 rounded bg-red-100 px-1 text-red-800">기존 값 덮어쓰기</span>}
    </li>
  );
}

export default function AiExtractionReview({ programId }: { programId: string }) {
  const [data, setData] = useState<Review | null>(null);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [showAll, setShowAll] = useState(false);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [confirmed, setConfirmed] = useState<Set<string>>(new Set());
  const [note, setNote] = useState("");

  const load = useCallback(async () => {
    setData(await adminApi.getAiExtraction(programId));
    setSelected(new Set());
    setConfirmed(new Set());
  }, [programId]);
  useEffect(() => {
    let live = true;
    adminApi
      .getAiExtraction(programId)
      .then((d) => live && setData(d))
      .catch(() => live && setError("AI 추출 후보를 불러오지 못했습니다."));
    return () => {
      live = false;
    };
  }, [programId]);

  const sources = useMemo(() => new Map(data?.sources.map((s) => [s.id, s]) ?? []), [data]);
  const visible = (data?.candidates ?? []).filter((c) => showAll || c.status === "pending");
  const toggle = (set: Set<string>, id: string, update: (s: Set<string>) => void) => {
    const next = new Set(set);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    update(next);
  };

  async function decide(decision: "approve" | "reject") {
    const ids = [...selected];
    if (!ids.length) return setError("검토할 사실을 선택하세요.");
    if (note.trim().length < 10) return setError("검토 메모를 10자 이상 입력하세요.");
    setBusy(true);
    setError("");
    try {
      await adminApi.reviewAiExtraction(programId, {
        decisions: ids.map((id) => ({ candidate_id: id, decision, confirm_overwrite: confirmed.has(id) })),
        note: note.trim(),
      });
      setNote("");
      await load();
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <section className="space-y-4 rounded-xl border bg-white p-6">
      <h2 className="text-lg font-semibold">AI 추출 후보 검토</h2>
      <p className="text-sm text-gray-600">
        AI가 공식 원문에서 찾은 자격·서류·기간 후보입니다. 규칙 파서가 같은 근거 문장에서 같은 값을 독립적으로 읽은
        사실만 운영 설정에 따라 자동 반영되고(&ldquo;자동&rdquo; 표시), 나머지는 근거 문장(노란색)을 원문과 대조한 뒤
        사실별로 승인하거나 거절하세요. 승인 시 원문이 바뀌었는지 서버가 다시 확인합니다.
      </p>
      {error && (
        <p role="alert" className="text-sm text-red-700">
          {error}
        </p>
      )}
      <div className="flex flex-wrap items-center gap-3 text-sm">
        <label className="flex items-center gap-1">
          <input type="checkbox" checked={showAll} onChange={(e) => setShowAll(e.target.checked)} />
          낮은 신뢰·검증 실패·처리된 후보도 보기
        </label>
        <button
          type="button"
          className="rounded border px-3 py-1"
          onClick={() => void load().catch(() => setError("새로고침 실패"))}
        >
          새로고침
        </button>
      </div>
      {data && !visible.length && <p className="text-sm">검토할 후보가 없습니다.</p>}
      <ul className="space-y-3">
        {visible.map((c) => {
          const source = sources.get(c.cache_id);
          const open = OPEN.has(c.status) && !c.stale;
          const overwrites = c.plan?.changes.some((change) => change.overwrites) ?? false;
          return (
            <li key={c.id} className="space-y-2 rounded border p-3">
              <div className="flex flex-wrap items-center gap-2 text-sm">
                {open && (
                  <input
                    type="checkbox"
                    aria-label={`${typeName[c.fact_type] ?? c.fact_type} 선택`}
                    checked={selected.has(c.id)}
                    onChange={() => toggle(selected, c.id, setSelected)}
                  />
                )}
                <span className="font-semibold">{typeName[c.fact_type] ?? c.fact_type}</span>
                <span>{describe(c)}</span>
                <span className="rounded bg-gray-100 px-1 text-xs">{statusName[c.status] ?? c.status}</span>
                {c.confidence && <span className="rounded bg-gray-100 px-1 text-xs">신뢰도 {c.confidence}</span>}
                {c.stale && <span className="rounded bg-orange-100 px-1 text-xs text-orange-800">원문 변경됨</span>}
                {c.applied_by === "auto_verified" && (
                  <span className="rounded bg-indigo-100 px-1 text-xs text-indigo-800" title={c.auto_rule ?? undefined}>
                    자동
                  </span>
                )}
                {c.audit_sample && !c.audit_reviewed_at && c.status === "approved" && (
                  <span className="rounded bg-purple-100 px-1 text-xs text-purple-800">감사 표본</span>
                )}
              </div>
              {autoCheckLabel(c.auto_check) && (
                <p
                  className={`text-xs ${c.auto_check?.verdict === "agree" ? "text-indigo-800" : "text-gray-600"}`}
                >
                  {autoCheckLabel(c.auto_check)}
                </p>
              )}
              {c.revert_note && <p className="text-xs text-gray-500">취소 메모: {c.revert_note}</p>}
              {c.evidence && source ? (
                <Evidence text={source.source_text} start={c.evidence.start} end={c.evidence.end} />
              ) : (
                <p className="text-xs text-gray-500">근거 위치 없음</p>
              )}
              {c.plan && (
                <ul className="space-y-0.5">
                  {c.plan.changes.map((change) => (
                    <ChangeRow key={change.target} change={change} />
                  ))}
                  {c.plan.notes.map((n) => (
                    <li key={n} className="text-xs text-gray-500">
                      {n}
                    </li>
                  ))}
                  {c.plan.blockers.map((b) => (
                    <li key={b} className="text-xs text-red-700">
                      {b}
                    </li>
                  ))}
                </ul>
              )}
              {[...c.validation.errors, ...c.validation.flags].map((m) => (
                <p key={m} className="text-xs text-amber-800">
                  {m}
                </p>
              ))}
              {open && overwrites && (
                <label className="flex items-center gap-1 text-xs text-red-800">
                  <input
                    type="checkbox"
                    checked={confirmed.has(c.id)}
                    onChange={() => toggle(confirmed, c.id, setConfirmed)}
                  />
                  기존 값과 다름을 확인했고 덮어씁니다
                </label>
              )}
              {c.review_note && <p className="text-xs text-gray-500">검토 메모: {c.review_note}</p>}
            </li>
          );
        })}
      </ul>
      {visible.some((c) => OPEN.has(c.status) && !c.stale) && (
        <div className="space-y-2 border-t pt-4">
          <label className="block text-sm">
            검토 메모 (10자 이상, 승인·거절 근거)
            <textarea
              value={note}
              onChange={(e) => setNote(e.target.value)}
              maxLength={1000}
              className="mt-1 w-full rounded border border-gray-300 p-2 text-sm"
            />
          </label>
          <div className="flex gap-2">
            <button
              type="button"
              disabled={busy}
              onClick={() => void decide("approve")}
              className="rounded bg-teal-700 px-4 py-2 text-sm text-white disabled:opacity-50"
            >
              선택 승인
            </button>
            <button
              type="button"
              disabled={busy}
              onClick={() => void decide("reject")}
              className="rounded border px-4 py-2 text-sm disabled:opacity-50"
            >
              선택 거절
            </button>
          </div>
        </div>
      )}
    </section>
  );
}
