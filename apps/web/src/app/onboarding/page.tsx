"use client";

import { useEffect, useState } from "react";
import { useRouter } from "next/navigation";
import OnboardingProgress from "@/components/OnboardingProgress";
import { useOnboardingStore } from "@/store/onboarding";
import { NICKNAME_MAX, validNickname } from "@/lib/nickname";
import { api } from "@/lib/api";
import { sharedSession } from "@/lib/session";

const REGIONS = ["부산", "대구"] as const;

const currentYear = new Date().getFullYear();
const BIRTH_YEARS = Array.from({ length: 11 }, (_, i) => currentYear - 29 + i);
// 1997 ~ 2007 range for 19-29

export default function OnboardingStep1() {
  const router = useRouter();
  const setStep1 = useOnboardingStore((s) => s.setStep1);

  const [nickname, setNickname] = useState(() => useOnboardingStore.getState().nickname);
  const [nicknameTouched, setNicknameTouched] = useState(false);
  // A signed-in member starts from the saved nickname (profile, else the account
  // name), so 이 조건 저장 does not ask for it again or replace it with "".
  // Whatever the person already typed wins over a late answer.
  useEffect(() => {
    let live = true;
    void (async () => {
      const session = await sharedSession();
      if (session.status !== "authenticated") return;
      const saved = await api.getProfile().then(r => r.profile?.nickname, () => null);
      const name = (saved || session.user.nickname || "").trim();
      if (live && name) setNickname(current => current || name.slice(0, NICKNAME_MAX));
    })();
    return () => { live = false; };
  }, []);
  const [region, setRegion] = useState("");
  const [birthYear, setBirthYear] = useState<number>(0);
  const [showResult, setShowResult] = useState(false);

  const nicknameOk = validNickname(nickname);
  const canProceed = nicknameOk && region !== "" && birthYear > 0;

  function handleNext() {
    if (!canProceed) return;
    setStep1(region, birthYear, nickname);
    setShowResult(true);
    setTimeout(() => {
      router.push("/onboarding/step2");
    }, 1500);
  }

  if (showResult) {
    return (
      <main className="flex min-h-screen flex-col items-center justify-center px-4">
        <div className="text-center space-y-4">
          <div className="text-4xl font-bold text-teal-600">
            주변 혜택 발견!
          </div>
          <p className="text-gray-500">맞춤 정보를 불러오고 있어요...</p>
        </div>
      </main>
    );
  }

  return (
    <main className="min-h-screen px-4 py-12">
      <div className="w-full max-w-md mx-auto">
        <OnboardingProgress currentStep={1} />

        <h1 className="text-2xl font-bold text-gray-900 mb-2">
          기본 정보를 알려주세요
        </h1>
        <p className="text-sm text-gray-500 mb-8">
          닉네임과 2가지만 답하면 바로 혜택을 찾아드려요
        </p>

        {/* Nickname (required, same rule as the app) */}
        <div className="mb-8">
          <label htmlFor="nickname" className="block text-sm font-semibold text-gray-700 mb-3">
            닉네임
          </label>
          <input
            id="nickname"
            type="text"
            value={nickname}
            maxLength={NICKNAME_MAX}
            autoComplete="nickname"
            onChange={(e) => setNickname(e.target.value)}
            onBlur={() => setNicknameTouched(true)}
            placeholder="닉네임을 입력해 주세요"
            aria-invalid={nicknameTouched && !nicknameOk}
            aria-describedby={nicknameTouched && !nicknameOk ? "nickname-error" : undefined}
            className={`w-full rounded-xl border-2 bg-white px-4 py-3.5 text-base text-gray-900 focus:outline-none transition-colors ${
              nicknameTouched && !nicknameOk ? "border-red-500 focus:border-red-500" : "border-gray-200 focus:border-teal-600"
            }`}
          />
          {nicknameTouched && !nicknameOk && (
            <p id="nickname-error" role="alert" className="mt-2 text-xs text-red-600">
              닉네임을 입력해 주세요. (1–{NICKNAME_MAX}자)
            </p>
          )}
        </div>

        {/* Region */}
        <div className="mb-8">
          <label className="block text-sm font-semibold text-gray-700 mb-3">
            지역
          </label>
          <div className="grid grid-cols-2 gap-3">
            {REGIONS.map((r) => (
              <button
                key={r}
                type="button"
                onClick={() => setRegion(r)}
                aria-label={`${r} 선택`}
                aria-pressed={region === r}
                className={`rounded-xl border-2 px-6 min-h-20 text-lg font-semibold transition-colors ${
                  region === r
                    ? "border-teal-600 bg-teal-50 text-teal-600"
                    : "border-gray-200 bg-white text-gray-700 hover:border-gray-300"
                }`}
              >
                {r}
              </button>
            ))}
          </div>
        </div>

        {/* Birth Year */}
        <div className="mb-10">
          <label className="block text-sm font-semibold text-gray-700 mb-3">
            출생연도
          </label>
          <select
            value={birthYear || ""}
            onChange={(e) => setBirthYear(Number(e.target.value))}
            className="w-full rounded-xl border-2 border-gray-200 bg-white px-4 py-3.5 text-base text-gray-900 focus:border-teal-600 focus:outline-none transition-colors appearance-none"
          >
            <option value="" disabled>
              출생연도를 선택하세요
            </option>
            {BIRTH_YEARS.map((y) => (
              <option key={y} value={y}>
                {y}년생
              </option>
            ))}
          </select>
        </div>

        {/* Next */}
        <button
          type="button"
          onClick={handleNext}
          disabled={!canProceed}
          aria-label="다음 단계로 이동"
          className={`w-full rounded-xl px-6 py-4 text-lg font-semibold transition-colors ${
            canProceed
              ? "bg-teal-600 text-white hover:bg-teal-700 shadow-sm"
              : "bg-gray-200 text-gray-400 cursor-not-allowed"
          }`}
        >
          다음
        </button>
      </div>
    </main>
  );
}
