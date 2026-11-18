"use client";

import { useState } from "react";
import {
  adminApi,
  ApiError,
  ReviewSourceCandidateInput,
  SourceCandidateDetail,
} from "@/lib/api";

interface FormState {
  title: string;
  summary: string;
  providerName: string;
  benefitCategory: string;
  programStatus: string;
  applicationStartAt: string;
  applicationEndAt: string;
  deadlineAt: string;
  minAge: string;
  maxAge: string;
  regions: string;
  tags: string;
  publish: boolean;
}

function toLocalDateTime(value: string | null | undefined): string {
  if (!value) return "";
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return "";
  const local = new Date(date.getTime() - date.getTimezoneOffset() * 60_000);
  return local.toISOString().slice(0, 16);
}

function optionalNumber(value: string): number | null {
  const trimmed = value.trim();
  if (!trimmed) return null;
  const parsed = Number(trimmed);
  return Number.isInteger(parsed) && parsed >= -2_147_483_648 && parsed <= 2_147_483_647
    ? parsed
    : null;
}

function isValidOptionalInteger(value: string): boolean {
  return !value.trim() || optionalNumber(value) !== null;
}

function optionalIsoDate(value: string): string | null {
  if (!value) return null;
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? null : date.toISOString();
}

function listFromCsv(value: string): string[] {
  return Array.from(
    new Set(
      value
        .split(",")
        .map((item) => item.trim())
        .filter(Boolean)
    )
  );
}

function errorMessage(error: unknown): string {
  if (error instanceof ApiError) {
    const body = error.body as { error?: string } | undefined;
    return body?.error || "후보 검토 요청에 실패했습니다.";
  }
  return "후보 검토 요청에 실패했습니다.";
}

function draftForm(candidate: SourceCandidateDetail): FormState {
  const draft = candidate.extracted_payload || {};
  return {
    title: draft.title || candidate.page_title || "",
    summary: draft.summary || "",
    providerName: draft.provider_name || candidate.organization_name,
    benefitCategory: draft.benefit_category || "discount",
    programStatus: draft.program_status || "active",
    applicationStartAt: toLocalDateTime(draft.application_start_at),
    applicationEndAt: toLocalDateTime(draft.application_end_at),
    deadlineAt: toLocalDateTime(draft.deadline_at),
    minAge: draft.min_age == null ? "" : String(draft.min_age),
    maxAge: draft.max_age == null ? "" : String(draft.max_age),
    regions: draft.regions?.join(", ") || "",
    tags: draft.tags?.join(", ") || "",
    publish: false,
  };
}

export default function CandidateApprovalForm({
  candidate,
  onCompleted,
}: {
  candidate: SourceCandidateDetail;
  onCompleted: () => Promise<void> | void;
}) {
  const [form, setForm] = useState<FormState>(() => draftForm(candidate));
  const [rejectReason, setRejectReason] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // A new candidate object (another candidate, or the same one reloaded)
  // restarts the form from its draft, during render rather than in an effect.
  const [formFor, setFormFor] = useState(candidate);
  if (formFor !== candidate) {
    setFormFor(candidate);
    setForm(draftForm(candidate));
    setRejectReason("");
    setError(null);
  }

  const setField = <K extends keyof FormState>(
    key: K,
    value: FormState[K]
  ) => setForm((current) => ({ ...current, [key]: value }));

  const approve = async () => {
    if (!form.title.trim() || !form.summary.trim()) {
      setError("제목과 사실 요약은 필수입니다.");
      return;
    }
    // Approval publishes a corporate offer, whose amounts are no support
    // money: the API stores none, so none is asked (review 2026-10-21 H2).
    if ([form.minAge, form.maxAge].some((value) => !isValidOptionalInteger(value))) {
      setError("나이는 정수 범위로 입력해주세요.");
      return;
    }
    setBusy(true);
    setError(null);
    const input: ReviewSourceCandidateInput = {
      decision: "approve",
      title: form.title.trim(),
      summary: form.summary.trim(),
      provider_name: form.providerName.trim() || null,
      benefit_category: form.benefitCategory,
      program_status: form.programStatus,
      application_start_at: optionalIsoDate(form.applicationStartAt),
      application_end_at: optionalIsoDate(form.applicationEndAt),
      deadline_at: optionalIsoDate(form.deadlineAt),
      min_age: optionalNumber(form.minAge),
      max_age: optionalNumber(form.maxAge),
      regions: listFromCsv(form.regions),
      tags: listFromCsv(form.tags),
      publish: form.publish,
    };
    try {
      await adminApi.reviewSourceCandidate(candidate.id, input);
      await onCompleted();
    } catch (requestError) {
      setError(errorMessage(requestError));
    } finally {
      setBusy(false);
    }
  };

  const reject = async () => {
    const reason = rejectReason.trim();
    if (reason.length < 2) {
      setError("제외 사유를 2자 이상 입력해주세요.");
      return;
    }
    if (!window.confirm("이 후보를 제외할까요?")) return;
    setBusy(true);
    setError(null);
    try {
      await adminApi.reviewSourceCandidate(candidate.id, {
        decision: "reject",
        reason,
      });
      await onCompleted();
    } catch (requestError) {
      setError(errorMessage(requestError));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="rounded-xl border border-teal-200 bg-teal-50/40 p-4">
      <div className="flex flex-wrap items-start justify-between gap-2">
        <div>
          <h3 className="text-sm font-bold text-gray-900">신규 혜택 검수</h3>
          <p className="mt-1 text-xs text-gray-500">
            공식 페이지와 대조한 사실만 남기고 게시 여부를 직접 결정하세요.
          </p>
        </div>
        {candidate.extracted_payload && (
          <span className="rounded-full bg-violet-100 px-2.5 py-1 text-xs font-semibold text-violet-700">
            구조화 초안{" "}
            {candidate.extraction_confidence == null
              ? ""
              : Math.round(candidate.extraction_confidence * 100) + "%"}
          </span>
        )}
      </div>

      {error && (
        <p className="mt-3 rounded-lg bg-red-50 px-3 py-2 text-xs text-red-700">
          {error}
        </p>
      )}

      <div className="mt-4 grid gap-3 sm:grid-cols-2">
        <label className="text-xs font-semibold text-gray-600 sm:col-span-2">
          제목
          <input
            value={form.title}
            onChange={(event) => setField("title", event.target.value)}
            maxLength={300}
            className="mt-1.5 w-full rounded-lg border border-gray-300 bg-white px-3 py-2 text-sm font-normal text-gray-900 outline-none focus:border-teal-500"
          />
        </label>
        <label className="text-xs font-semibold text-gray-600 sm:col-span-2">
          사실 요약
          <textarea
            value={form.summary}
            onChange={(event) => setField("summary", event.target.value)}
            maxLength={1000}
            rows={4}
            className="mt-1.5 w-full resize-y rounded-lg border border-gray-300 bg-white px-3 py-2 text-sm font-normal leading-6 text-gray-900 outline-none focus:border-teal-500"
          />
        </label>
        <label className="text-xs font-semibold text-gray-600">
          제공처
          <input
            value={form.providerName}
            onChange={(event) => setField("providerName", event.target.value)}
            maxLength={200}
            className="mt-1.5 w-full rounded-lg border border-gray-300 bg-white px-3 py-2 text-sm font-normal outline-none focus:border-teal-500"
          />
        </label>
        <label className="text-xs font-semibold text-gray-600">
          분류
          <select
            value={form.benefitCategory}
            onChange={(event) =>
              setField("benefitCategory", event.target.value)
            }
            className="mt-1.5 w-full rounded-lg border border-gray-300 bg-white px-3 py-2 text-sm font-normal outline-none focus:border-teal-500"
          >
            {[
              "discount",
              "student_discount",
              "membership",
              "subscription",
              "software",
              "telecom",
              "transport",
              "finance",
              "welfare",
              "scholarship",
              "recruitment",
              "internship",
              "education",
              "culture",
              "other",
            ].map((category) => (
              <option key={category} value={category}>
                {category}
              </option>
            ))}
          </select>
        </label>
        <label className="text-xs font-semibold text-gray-600">
          운영 상태
          <select
            value={form.programStatus}
            onChange={(event) => setField("programStatus", event.target.value)}
            className="mt-1.5 w-full rounded-lg border border-gray-300 bg-white px-3 py-2 text-sm font-normal outline-none focus:border-teal-500"
          >
            <option value="active">active</option>
            <option value="upcoming">upcoming</option>
            <option value="always_open">always_open</option>
            <option value="closed">closed</option>
          </select>
        </label>
        <label className="text-xs font-semibold text-gray-600">
          최종 마감
          <input
            type="datetime-local"
            value={form.deadlineAt}
            onChange={(event) => setField("deadlineAt", event.target.value)}
            className="mt-1.5 w-full rounded-lg border border-gray-300 bg-white px-3 py-2 text-sm font-normal outline-none focus:border-teal-500"
          />
        </label>
        <label className="text-xs font-semibold text-gray-600">
          신청 시작
          <input
            type="datetime-local"
            value={form.applicationStartAt}
            onChange={(event) =>
              setField("applicationStartAt", event.target.value)
            }
            className="mt-1.5 w-full rounded-lg border border-gray-300 bg-white px-3 py-2 text-sm font-normal outline-none focus:border-teal-500"
          />
        </label>
        <label className="text-xs font-semibold text-gray-600">
          신청 종료
          <input
            type="datetime-local"
            value={form.applicationEndAt}
            onChange={(event) =>
              setField("applicationEndAt", event.target.value)
            }
            className="mt-1.5 w-full rounded-lg border border-gray-300 bg-white px-3 py-2 text-sm font-normal outline-none focus:border-teal-500"
          />
        </label>
        <label className="text-xs font-semibold text-gray-600">
          최소 나이
          <input
            type="number"
            min={0}
            max={100}
            value={form.minAge}
            onChange={(event) => setField("minAge", event.target.value)}
            className="mt-1.5 w-full rounded-lg border border-gray-300 bg-white px-3 py-2 text-sm font-normal outline-none focus:border-teal-500"
          />
        </label>
        <label className="text-xs font-semibold text-gray-600">
          최대 나이
          <input
            type="number"
            min={0}
            max={100}
            value={form.maxAge}
            onChange={(event) => setField("maxAge", event.target.value)}
            className="mt-1.5 w-full rounded-lg border border-gray-300 bg-white px-3 py-2 text-sm font-normal outline-none focus:border-teal-500"
          />
        </label>
        <label className="text-xs font-semibold text-gray-600">
          지역 코드 (쉼표 구분)
          <input
            value={form.regions}
            onChange={(event) => setField("regions", event.target.value)}
            placeholder="busan, daegu"
            className="mt-1.5 w-full rounded-lg border border-gray-300 bg-white px-3 py-2 text-sm font-normal outline-none focus:border-teal-500"
          />
        </label>
        <label className="text-xs font-semibold text-gray-600">
          태그 (쉼표 구분)
          <input
            value={form.tags}
            onChange={(event) => setField("tags", event.target.value)}
            placeholder="청년, 할인"
            className="mt-1.5 w-full rounded-lg border border-gray-300 bg-white px-3 py-2 text-sm font-normal outline-none focus:border-teal-500"
          />
        </label>
      </div>

      <label className="mt-4 flex items-start gap-2 rounded-lg border border-gray-200 bg-white p-3 text-sm text-gray-700">
        <input
          type="checkbox"
          checked={form.publish}
          onChange={(event) => setField("publish", event.target.checked)}
          className="mt-0.5"
        />
        <span>
          검수 완료 즉시 게시
          <span className="mt-0.5 block text-xs text-gray-400">
            기본값은 게시 보류입니다. 공식 조건을 모두 확인한 경우에만
            선택하세요.
          </span>
        </span>
      </label>

      <div className="mt-4 flex flex-wrap items-end gap-3 border-t border-teal-100 pt-4">
        <button
          type="button"
          disabled={busy}
          onClick={() => void approve()}
          className="rounded-lg bg-teal-600 px-4 py-2 text-sm font-semibold text-white hover:bg-teal-700 disabled:opacity-50"
        >
          {busy ? "처리 중…" : "검수 승인"}
        </button>
        <label className="min-w-[220px] flex-1 text-xs font-semibold text-gray-600">
          제외 사유
          <input
            value={rejectReason}
            onChange={(event) => setRejectReason(event.target.value)}
            maxLength={1000}
            placeholder="청년 혜택이 아님, 공식 조건 확인 불가 등"
            className="mt-1.5 w-full rounded-lg border border-gray-300 bg-white px-3 py-2 text-sm font-normal outline-none focus:border-red-400"
          />
        </label>
        <button
          type="button"
          disabled={busy}
          onClick={() => void reject()}
          className="rounded-lg border border-red-300 bg-white px-4 py-2 text-sm font-semibold text-red-700 hover:bg-red-50 disabled:opacity-50"
        >
          후보 제외
        </button>
      </div>
    </div>
  );
}
