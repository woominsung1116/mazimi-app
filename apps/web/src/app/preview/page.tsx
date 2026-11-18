"use client";

import { useEffect, useState } from "react";
import { useRouter } from "next/navigation";
import Link from "next/link";
import { useOnboardingStore } from "@/store/onboarding";
import { useAuthSession } from "@/lib/session";
import {
  api,
  ApiError,
  type ExclusionReason,
  type FilterSummary,
  type RecommendationResult,
  type RecommendationItem,
  type UserProfile,
  displayTitle,
  formatWon,
} from "@/lib/api";
import { programTypeLabel } from "@/lib/displayLabels";
import { employmentLabel, enrollmentLabel } from "@/lib/profileCodes";
import { safeHttpsUrl } from "@/lib/safeUrl";
import { DISTRICT_REGION_LABELS, DISTRICTS, type DistrictRegion } from "@/lib/districts.generated";
import { previewProfileForSave } from "@/lib/profileSave";
import { ANY_OPTIONAL_QUESTION_ENABLED, askedAnswers } from "@/lib/optionalAnswers";
import { remainingLabels, verifiedSections } from "@/lib/verifiedSections";

/** 시·군·구 options of the onboarding sido ("부산" or "busan"). */
function districtsOf(region: string) {
  const key = (Object.keys(DISTRICT_REGION_LABELS) as DistrictRegion[]).find(
    (code) => code === region || DISTRICT_REGION_LABELS[code] === region,
  );
  return key ? DISTRICTS[key] : [];
}

export default function PreviewPage() {
  const router = useRouter();
  const { nickname, region, birthYear, enrollmentStatus, employmentStatus, statusAnswered, schoolName, incomeBracket, optional } =
    useOnboardingStore();

  // Optional 시·군·구 of the onboarding sido, sent as city_code. A sido with
  // a single 시·군·구 (세종) has nothing to choose and always sends it.
  const [cityCode, setCityCode] = useState("");
  const districts = districtsOf(region);
  const sentCity = districts.length === 1 ? districts[0][0] : cityCode;
  // The shown list and the city_code it was fetched with.
  const [shown, setShown] = useState<{ result: RecommendationResult; cityCode: string } | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [attempt, setAttempt] = useState(0);
  const session = useAuthSession();
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);

  async function saveAndContinue() {
    setSaving(true);
    setSaveError(null);
    try {
      // POST /profile replaces the whole row. Start from the saved profile so
      // answers given elsewhere (mobile, settings: age_band, a 시·군·구) are
      // kept; only a 404 means there is nothing to keep. Any other read error
      // aborts the save.
      let current: UserProfile | null = null;
      try {
        current = (await api.getProfile()).profile;
      } catch (err) {
        if (!(err instanceof ApiError && err.status === 404)) throw err;
      }
      await api.saveProfile(
        previewProfileForSave(
          current,
          {
            nickname,
            region_code: region,
            birth_year: birthYear,
            city_code: sentCity,
            enrollment_status: enrollmentStatus,
            employment_status: employmentStatus,
            school_name: schoolName,
            income_bracket: incomeBracket,
            ...(ANY_OPTIONAL_QUESTION_ENABLED && { optional }),
          },
          districts.map(([code]) => code),
        ),
      );
      router.push("/dashboard");
    } catch {
      setSaveError("조건을 저장하지 못했어요. 다시 시도해 주세요.");
      setSaving(false);
    }
  }

  useEffect(() => {
    if (!region || !birthYear || !statusAnswered) {
      router.replace("/onboarding");
      return;
    }

    // A later 시·군·구 choice wins over a slower earlier response.
    let current = true;
    // Codes are canonical (lib/profileCodes); labels are only for display.
    api
      .getRecommendPreview({
        region_code: region,
        city_code: sentCity || null,
        birth_year: birthYear,
        enrollment_status: enrollmentStatus || null,
        employment_status: employmentStatus || null,
        school_name: schoolName || undefined,
        income_bracket: incomeBracket,
        ...askedAnswers(optional),
      })
      .then((res) => {
        if (!current) return;
        setShown({ result: res, cityCode: sentCity });
        setLoading(false);
      })
      .catch((err) => {
        if (!current) return;
        setError(err.message);
        setLoading(false);
      });
    return () => {
      current = false;
    };
  }, [region, sentCity, attempt, birthYear, enrollmentStatus, employmentStatus, statusAnswered, schoolName, incomeBracket, optional, router]);

  const reload = () => {
    setLoading(true);
    setError(null);
  };
  const statusLabel = enrollmentLabel(enrollmentStatus) || employmentLabel(employmentStatus);

  if (!shown && loading) {
    return (
      <main className="flex min-h-screen items-center justify-center px-4">
        <div className="text-center space-y-3">
          <div className="h-8 w-8 mx-auto border-2 border-teal-600 border-t-transparent rounded-full animate-spin" />
          <p className="text-sm text-gray-500">맞춤 추천을 분석하고 있어요...</p>
        </div>
      </main>
    );
  }

  if (!shown && error) {
    return (
      <main className="flex min-h-screen items-center justify-center px-4">
        <div className="text-center space-y-4 max-w-md">
          <p className="text-gray-700">추천 결과를 불러오지 못했어요</p>
          <p className="text-sm text-gray-500">{error}</p>
          <Link
            href="/onboarding"
            className="inline-block rounded-xl bg-teal-600 px-6 py-3 text-sm font-semibold text-white hover:bg-teal-700 transition-colors"
          >
            다시 시도하기
          </Link>
        </div>
      </main>
    );
  }

  if (!shown) return null;
  const data = shown.result;
  // 조건 확인됨 / 확인 필요 only while the server flag is on; otherwise one list as before.
  const sections = verifiedSections(data, data.items);
  const shownDistrict = districts.length > 1 ? districts.find(([code]) => code === shown.cityCode)?.[1] : undefined;

  return (
    <main className="min-h-screen px-4 py-8 pb-28">
      <div className="w-full max-w-md mx-auto">
        {/* Summary Card */}
        <div className="rounded-xl bg-teal-600 p-5 text-white mb-6">
          <p data-testid="preview-summary" aria-live="polite" className="text-sm opacity-80 mb-3">
            {shownDistrict ? `${region} ${shownDistrict}` : region} / {statusLabel} 기준 추천 결과
          </p>
          <div className="flex justify-between items-end">
            <div>
              <p className="text-sm opacity-80">내 조건으로 찾은 후보</p>
              <p className="text-3xl font-bold">{data.total_available}건</p>
            </div>
            <div className="text-right">
              <p className="text-sm opacity-80">자격·중복 수혜 확인 필요</p>
              <p className="text-xs mt-2">지원액을 합산하지 않아요</p>
            </div>
          </div>
        </div>

        {districts.length > 1 && (
          <div className="mb-6">
            <label htmlFor="city-code" className="block text-sm font-medium text-gray-700 mb-2">
              시·군·구 (선택)
            </label>
            <select
              id="city-code"
              value={cityCode}
              aria-describedby="city-code-hint"
              onChange={(e) => {
                setCityCode(e.target.value);
                reload();
              }}
              className="w-full rounded-xl border border-gray-200 bg-white px-4 py-3 text-sm text-gray-900 focus:border-teal-600 focus:outline-none"
            >
              <option value="">선택 안 함</option>
              {districts.map(([code, name]) => (
                <option key={code} value={code}>
                  {name}
                </option>
              ))}
            </select>
            <p id="city-code-hint" className="mt-1 text-xs text-gray-500">
              선택하면 시·군·구 단위 혜택도 확인할 수 있어요.
            </p>
          </div>
        )}

        {loading && (
          <p role="status" className="mb-4 text-sm text-gray-500">
            조건에 맞는 결과를 불러오고 있어요...
          </p>
        )}
        {error && (
          <div role="alert" className="mb-4 flex items-center justify-between gap-3 rounded-xl bg-amber-50 p-3 text-sm text-amber-800">
            <span>결과를 새로 불러오지 못해 이전 결과를 보여드려요.</span>
            <button
              type="button"
              onClick={() => {
                setAttempt((n) => n + 1);
                reload();
              }}
              className="shrink-0 font-semibold underline"
            >
              다시 시도
            </button>
          </div>
        )}

        {data.filter_summary && <FilterSummaryCard summary={data.filter_summary} />}

        {/* Recommendations */}
        <h2 className="text-xl font-bold text-gray-900 mb-4">
          맞춤 추천 혜택
        </h2>

        {data.items.length === 0 ? (
          <div className="rounded-xl bg-white p-6 text-center shadow-sm border border-gray-100">
            <p className="text-gray-500">
              현재 조건에 맞는 추천 결과가 없어요
            </p>
          </div>
        ) : sections ? (
          <div className="space-y-6">
            {([
              ["조건 확인됨", sections.verified],
              ["확인 필요", sections.needsCheck],
            ] as const).map(([title, items]) =>
              items.length > 0 ? (
                <section key={title} aria-label={title} className="space-y-3">
                  <h3 className="text-base font-semibold text-gray-900">
                    {title} <span className="text-teal-700">{items.length}건</span>
                  </h3>
                  {items.map((item) => (
                    <RecommendationCard key={item.program_id} item={item} sectionEnabled />
                  ))}
                </section>
              ) : null,
            )}
          </div>
        ) : (
          <div className="space-y-3">
            {data.items.map((item) => (
              <RecommendationCard key={item.program_id} item={item} />
            ))}
          </div>
        )}

        {/* Bottom CTA. A signed-in visitor saves these conditions as the
            profile (this ends onboarding); a guest browses the catalogue. */}
        {/* Sits above the fixed BottomNav (h-16), which would cover it. */}
        <div className="fixed bottom-16 left-0 right-0 z-30 bg-white border-t border-gray-100 px-4 py-4">
          <div className="max-w-md mx-auto">
            {saveError && (
              <p role="alert" className="mb-2 text-center text-sm text-red-700">
                {saveError}
              </p>
            )}
            {session.status === "authenticated" ? (
              <button
                type="button"
                disabled={saving}
                onClick={saveAndContinue}
                className="block w-full text-center rounded-xl bg-teal-600 px-6 py-3.5 text-base font-semibold text-white hover:bg-teal-700 disabled:opacity-60 transition-colors"
              >
                {saving ? "저장하는 중..." : "이 조건 저장하고 내 대시보드 보기"}
              </button>
            ) : (
              <Link
                href="/programs"
                className="block w-full text-center rounded-xl bg-teal-600 px-6 py-3.5 text-base font-semibold text-white hover:bg-teal-700 transition-colors"
              >
                전체 프로그램 둘러보기
              </Link>
            )}
          </div>
        </div>
      </div>
    </main>
  );
}

/** Reasons that are a mismatch with the user's conditions. Not-open and
 * unconfirmed-scope rows are excluded for other reasons, so the headline
 * only says "조건이 맞지 않아" when nothing else is in the count. */
const CONDITION_REASONS = new Set<ExclusionReason>([
  "age",
  "region",
  "school",
  "enrollment",
  "hard_rule",
  "not_individual",
  "target_group",
]);

/** "총 N개 중 M개 제외" with the reasons behind a native disclosure. Counts
 * only; the server never sends the titles of excluded programmes.
 * `outside_youth_scope` rows are not an exclusion: they were only left out of
 * the youth list, so they get their own note pointing at the full catalogue. */
function FilterSummaryCard({ summary }: { summary: FilterSummary }) {
  if (summary.considered === 0) return null;
  const outside = summary.excluded_by_reason.find((r) => r.reason === "outside_youth_scope")?.count ?? 0;
  const reasons = summary.excluded_by_reason.filter((r) => r.reason !== "outside_youth_scope");
  const excluded = summary.excluded - outside;
  const onlyConditions = reasons.every((r) => CONDITION_REASONS.has(r.reason));
  const headline =
    excluded === 0
      ? `총 ${summary.considered.toLocaleString()}개를 모두 살펴봤어요`
      : `총 ${summary.considered.toLocaleString()}개 중 ${excluded.toLocaleString()}개는 ${
          onlyConditions ? "조건이 맞지 않아" : "조건이 맞지 않거나 지금은 안내할 수 없어"
        } 제외했어요`;
  return (
    <section aria-label="추천 필터 요약" className="rounded-xl border border-gray-100 bg-white p-4 mb-6 shadow-sm">
      <p className="text-sm font-semibold text-gray-900">{headline}</p>
      {summary.included_with_unknowns > 0 && (
        <p className="mt-1 text-xs text-gray-500">
          남은 {summary.included.toLocaleString()}개 중 {summary.included_with_unknowns.toLocaleString()}개는 확인할 조건이
          남아 있어요
        </p>
      )}
      {outside > 0 && (
        <p className="mt-1 text-xs text-gray-500">
          청년 대상으로 확인되지 않은 {outside.toLocaleString()}개는{" "}
          <Link href="/programs" className="text-teal-700 underline hover:text-teal-800">
            전체 프로그램
          </Link>
          에서 볼 수 있어요
        </p>
      )}
      {reasons.length > 0 && (
        <details className="mt-3 group">
          <summary className="cursor-pointer text-sm text-teal-700 hover:text-teal-800">제외한 이유 보기</summary>
          <ul className="mt-2 space-y-1">
            {reasons.map((r) => (
              <li key={r.reason} className="flex justify-between text-sm text-gray-600">
                <span>{r.label}</span>
                <span className="font-medium text-gray-900">{r.count.toLocaleString()}개</span>
              </li>
            ))}
          </ul>
          <p className="mt-2 text-xs text-gray-400">
            한 혜택은 먼저 확인된 이유 하나로만 세요. 공고 조건은 기관에서 최종 확인해 주세요.
          </p>
        </details>
      )}
    </section>
  );
}

/** Inline recommendation card for the preview page. `sectionEnabled`: the
 * card sits in a 조건 확인됨 / 확인 필요 section; a 확인 필요 row lists its
 * remaining checks. */
function RecommendationCard({ item, sectionEnabled = false }: { item: RecommendationItem; sectionEnabled?: boolean }) {
  const officialHref = safeHttpsUrl(item.official_url);
  const checks = sectionEnabled ? remainingLabels(item, true) : item.missing_checks;
  const scoreLabel =
    item.match_score >= 90
      ? "매우 높음"
      : item.match_score >= 70
        ? "높음"
        : item.match_score >= 50
          ? "보통"
          : "낮음";

  const scoreColor =
    item.match_score >= 90
      ? "bg-green-100 text-green-700"
      : item.match_score >= 70
        ? "bg-teal-100 text-teal-700"
        : item.match_score >= 50
          ? "bg-yellow-100 text-yellow-700"
          : "bg-gray-100 text-gray-600";

  function formatBenefitAmount(): string {
    // A corporate offer carries no support amount.
    if (item.program_type === "corporate_benefit") return "혜택 확인";
    if (item.benefit_amount_monthly) {
      return `월 ${formatWon(item.benefit_amount_monthly)}`;
    }
    if (item.benefit_amount_semester) {
      return `학기 ${formatWon(item.benefit_amount_semester)}`;
    }
    return "혜택 확인";
  }

  return (
    <div className="rounded-xl bg-white shadow-sm border border-gray-100 p-4">
      <div className="flex items-center gap-2 mb-2">
        <span className="text-xs font-medium text-gray-500 bg-gray-50 px-2 py-0.5 rounded">
          {programTypeLabel(item.program_type)}
        </span>
        <span
          className={`inline-flex items-center rounded-full px-2 py-0.5 text-xs font-medium ${scoreColor}`}
        >
          적합도 {scoreLabel}
        </span>
        {sectionEnabled && item.verified === true && (
          <span className="inline-flex items-center rounded-full bg-teal-50 px-2 py-0.5 text-xs font-medium text-teal-700">
            조건 확인됨
          </span>
        )}
      </div>
      <h3 className="font-semibold text-gray-900">{displayTitle(item)}</h3>
      <p className="text-sm font-medium text-teal-600 mt-1">{formatBenefitAmount()}</p>

      {item.reasons.length > 0 && (
        <div className="mt-2">
          <ul className="space-y-1">
            {item.reasons.map((reason, i) => (
              <li key={i} className="flex items-start gap-2 text-sm text-gray-600">
                <svg
                  className="h-4 w-4 text-green-500 mt-0.5 shrink-0"
                  fill="none"
                  stroke="currentColor"
                  viewBox="0 0 24 24"
                  aria-hidden="true"
                >
                  <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M5 13l4 4L19 7" />
                </svg>
                {reason}
              </li>
            ))}
          </ul>
        </div>
      )}

      {checks.length > 0 && (
        <div className="mt-2">
          <ul className="space-y-1">
            {checks.map((check, i) => (
              <li key={i} className="flex items-start gap-2 text-sm text-gray-500">
                <svg
                  className="h-4 w-4 text-amber-500 mt-0.5 shrink-0"
                  fill="none"
                  stroke="currentColor"
                  viewBox="0 0 24 24"
                  aria-hidden="true"
                >
                  <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M12 9v2m0 4h.01M12 3a9 9 0 100 18 9 9 0 000-18z" />
                </svg>
                {check}
              </li>
            ))}
          </ul>
        </div>
      )}

      {/* A link to read the official page, never an "apply, you qualify" call:
          the fit badge is a ranking score, not eligibility. */}
      {officialHref && (
        <a
          href={officialHref}
          target="_blank"
          rel="noopener noreferrer"
          aria-label={`${displayTitle(item)} 공식 사이트 보기 (새 창)`}
          className="inline-flex items-center gap-1.5 mt-3 rounded-lg border border-gray-200 px-4 py-2 text-sm font-medium text-gray-700 hover:bg-gray-50 transition-colors"
        >
          공식 사이트 보기
          <svg className="h-4 w-4" fill="none" stroke="currentColor" viewBox="0 0 24 24" aria-hidden="true">
            <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M10 6H6a2 2 0 00-2 2v10a2 2 0 002 2h10a2 2 0 002-2v-4M14 4h6m0 0v6m0-6L10 14" />
          </svg>
        </a>
      )}
    </div>
  );
}
