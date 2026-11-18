"use client";

import {
  PROFILE_CONDITION_QUESTIONS_ENABLED,
  INCOME_QUESTION_ENABLED,
  INCOME_MEDIAN_BANDS,
  MEDIAN_INCOME_TABLE_URL,
  OPTIONAL_COPY as T,
  type OptionalAnswers,
} from "@/lib/optionalAnswers";

function Choice({ selected, onClick, children }: { selected: boolean; onClick: () => void; children: React.ReactNode }) {
  return (
    <button
      type="button"
      aria-pressed={selected}
      onClick={onClick}
      className={`rounded-xl border-2 px-3 py-2.5 text-sm font-semibold transition-all ${
        selected ? "border-teal-600 bg-teal-50 text-teal-600" : "border-gray-200 bg-white text-gray-700 hover:border-gray-300"
      }`}
    >
      {children}
    </button>
  );
}

function Title({ text, onSkip }: { text: string; onSkip: () => void }) {
  return (
    <div className="flex items-center justify-between mb-2">
      <span className="text-sm font-semibold text-gray-700">
        {text}
        <span className="ml-1 text-xs font-normal text-gray-400">({T.optional})</span>
      </span>
      <button type="button" onClick={onSkip} className="text-xs text-teal-600 hover:text-teal-700 font-medium">
        {T.skip}
      </button>
    </div>
  );
}

/**
 * 무주택 / 혼인·자녀 (`household`, PROFILE_CONDITION_QUESTIONS_ENABLED) and 소득 구간
 * (`income`, INCOME_QUESTION_ENABLED): each optional, each skippable. Nothing when both are off.
 */
export default function OptionalConditions({
  value,
  onChange,
  household = PROFILE_CONDITION_QUESTIONS_ENABLED,
  income = INCOME_QUESTION_ENABLED,
}: {
  value: OptionalAnswers;
  onChange: (next: OptionalAnswers) => void;
  household?: boolean;
  income?: boolean;
}) {
  if (!household && !income) return null;
  const set = (patch: Partial<OptionalAnswers>) => onChange({ ...value, ...patch });
  return (
    <div className="mb-10 space-y-8">
      <p className="text-xs text-gray-500">{household ? T.intro : T.incomeIntro}</p>
      {household && (
        <>
      <div>
        <Title text={T.homelessTitle} onSkip={() => set({ homeless_household: null })} />
        <p className="text-xs text-gray-500 mb-3">{T.homelessHelp}</p>
        <div className="grid grid-cols-2 gap-3">
          <Choice selected={value.homeless_household === true} onClick={() => set({ homeless_household: true })}>
            {T.homelessYes}
          </Choice>
          <Choice selected={value.homeless_household === false} onClick={() => set({ homeless_household: false })}>
            {T.homelessNo}
          </Choice>
        </div>
      </div>

      <div>
        <Title
          text={T.maritalTitle}
          onSkip={() => set({ marital_status: null, newlywed: null, children_count: null })}
        />
        <p className="text-xs text-gray-500 mb-3">{T.maritalHelp}</p>
        <div className="grid grid-cols-2 gap-3">
          <Choice
            selected={value.marital_status === "unmarried"}
            onClick={() => set({ marital_status: "unmarried", newlywed: null })}
          >
            {T.unmarried}
          </Choice>
          <Choice selected={value.marital_status === "married"} onClick={() => set({ marital_status: "married" })}>
            {T.married}
          </Choice>
        </div>
        {value.marital_status === "married" && (
          <label className="mt-3 flex items-center gap-2 text-sm text-gray-700">
            <input
              type="checkbox"
              checked={value.newlywed === true}
              onChange={(e) => set({ newlywed: e.target.checked })}
              className="h-4 w-4 accent-teal-600"
            />
            {T.newlyweds}
          </label>
        )}
        <label className="mt-4 block text-xs font-semibold text-gray-600" htmlFor="children-count">
          {T.childrenTitle}
        </label>
        <select
          id="children-count"
          value={value.children_count ?? ""}
          onChange={(e) => set({ children_count: e.target.value === "" ? null : Number(e.target.value) })}
          className="mt-1 w-full rounded-xl border-2 border-gray-200 bg-white px-3 py-2.5 text-sm text-gray-900 focus:border-teal-600 focus:outline-none"
        >
          <option value="">선택 안 함</option>
          {[0, 1, 2, 3, 4, 5].map((n) => (
            <option key={n} value={n}>
              {n === 5 ? "5명 이상" : `${n}명`}
            </option>
          ))}
        </select>
      </div>
        </>
      )}

      {income && (
      <div>
        <Title text={T.incomeTitle} onSkip={() => set({ income_median_band: null })} />
        <p className="text-xs text-gray-500 mb-1">{T.incomeHelp}</p>
        <a
          href={MEDIAN_INCOME_TABLE_URL}
          target="_blank"
          rel="noopener noreferrer"
          className="mb-3 inline-block text-xs font-medium text-teal-600 underline"
        >
          {T.incomeLink}
        </a>
        <div className="grid grid-cols-2 gap-2">
          {INCOME_MEDIAN_BANDS.map((band) => (
            <Choice
              key={band.value}
              selected={value.income_median_band === band.value}
              onClick={() => set({ income_median_band: band.value })}
            >
              {band.label}
            </Choice>
          ))}
        </div>
      </div>
      )}
    </div>
  );
}
