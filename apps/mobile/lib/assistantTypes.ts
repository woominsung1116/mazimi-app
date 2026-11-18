import type { ApplicationPreparation, Evidence, RecommendationItem } from './api';

export type AssistantTopic = 'summary' | 'eligibility' | 'documents' | 'deadline' | 'steps' | 'alerts' | 'unsupported';
export type EligibilityStatus = 'pass' | 'fail' | 'unknown';
export interface BenefitGuidance {
  version: number; program_id: string; title: string;
  amounts: { monthly: number | null; semester: number | null; once: number | null };
  mode: 'rules' | 'ai_assisted' | 'fallback'; ai_available: boolean; notice: string;
  eligibility: {
    status: EligibilityStatus; summary: string;
    checks: { label: string; status: EligibilityStatus; detail: string; evidence?: Evidence[] }[];
    missing_fields: string[]; profile_version: number | null;
  };
  preparation: ApplicationPreparation; remaining_task_ids: string[];
  alert_enabled: boolean; deadline_at: string | null;
  source: { official_url: string | null; last_synced_at: string | null; program_updated_at: string; checked_at: string; note: string };
  /** `evidence` and `source_checked_at` (공고 원문 확인일) only on sections that restate the notice. */
  sections: { topic: AssistantTopic; title: string; body: string; basis: string; evidence?: Evidence[]; source_checked_at?: string }[];
  /** Only on POST (a question): quoted passages, verdict, not-found. */
  answer?: GroundedAnswer;
}
export interface AssistantBriefing {
  scope: 'saved_programs_deadline_first'; generated_at: string;
  items: { program_id: string; title: string; deadline_at: string | null; reason: string; next_task: string; remaining_count: number; progress_reset: boolean }[];
}

export type AssistantScenario = 'returning_student' | 'independent_living' | 'job_search' | 'daily_savings';
export const assistantScenarios: { id: AssistantScenario; label: string }[] = [
  { id: 'returning_student', label: '학교생활·복학' }, { id: 'independent_living', label: '독립·주거' },
  { id: 'job_search', label: '취업 준비' }, { id: 'daily_savings', label: '일상 절약' },
];
export interface AssistantBundle {
  version: number; scenario: AssistantScenario; profile_version: number | null;
  required_profile_fields: string[];
  groups: { title: string; basis: string; total_candidates: number; items: RecommendationItem[] }[];
}
export interface BenefitComparison {
  version: number; programs: BenefitGuidance[]; amounts_additive: false;
  pairs: { program_a_id: string; program_a_title: string; program_b_id: string; program_b_title: string; status: 'unknown'; reason: string }[];
  shared_documents: { name: string; uses: { program_id: string; document_id: string; required: boolean | null; evidence: string; source: string }[] }[];
  compatibility_notice: string; documents_notice: string;
}

export const eligibilityLabels: Record<EligibilityStatus, string> = {
  pass: '일부 조건 맞음', fail: '조건 불일치', unknown: '확인 필요',
};
export const profileFieldLabels: Record<string, string> = {
  birth_year: '출생연도', age: '출생연도', region_code: '거주 지역', region: '거주 지역',
  school_name: '학교', enrollment_status: '학적 상태', income_bracket: '소득 구간',
  kosaf_support_bracket: '학자금 지원구간', employment_status: '고용 상태', school_year: '학년',
  housing_type: '주거 형태', household_size: '가구원 수', major_group: '전공 계열', school_type: '학교 유형',
  city_code: '거주 시·군·구', education_stage: '학력', major_category: '전공 계열', age_band: '연령대',
  has_disability: '장애 여부', is_multicultural_family: '다문화가정 여부', is_low_income_household: '저소득 가구 여부', veteran_family: '보훈 가족 여부',
  homeless_household: '무주택 여부', marital_status: '혼인 여부', newlywed: '혼인 여부', children_count: '자녀 수', income_median_pct: '기준 중위소득 구간',
  telecom_carrier: '통신사', card_issuers: '보유 카드', pay_apps: '간편결제',
};

/** Distinct Korean labels of profile field keys; an unlisted key reads as 추가 프로필 조건, never the key. */
export function profileFieldList(fields: readonly string[]): string {
  return [...new Set(fields.map(field => profileFieldLabels[field] ?? '추가 프로필 조건'))].join(' · ');
}

/**
 * Client re-validation before handing a server URL to the OS: https only, no
 * credentials, default port, and a public dotted hostname (no IP literal,
 * localhost or internal suffix). Returns null when the link must not open.
 */
export function safeOfficialUrl(value: string | null | undefined): string | null {
  if (!value) return null;
  try {
    const url = new URL(value);
    const host = url.hostname.toLowerCase();
    const ipLiteral = /^\d{1,3}(\.\d{1,3}){3}$/.test(host) || host.includes(':') || host.startsWith('[');
    const internal = host === 'localhost' || /\.(localhost|local|internal)$/.test(host);
    return url.protocol === 'https:' && !url.username && !url.password && (!url.port || url.port === '443')
      && host.includes('.') && !host.endsWith('.') && !ipLiteral && !internal
      ? url.toString() : null;
  } catch { return null; }
}

/** Char offsets (Unicode code points, not UTF-16 units) into `text`. */
export interface PassageHighlight { start: number; end: number }
export interface GroundedPassage {
  id: string; text: string; highlights: PassageHighlight[];
  section: 'summary' | 'benefit' | 'conditions' | 'method' | 'documents' | 'screening' | 'period' | 'other';
  kind: 'official_text' | 'approved_quote' | 'record';
  source_label: string; captured_at: string | null; approved_at?: string;
}
export interface GroundedVerdict {
  check: 'overall' | 'age' | 'region' | 'school' | 'enrollment' | 'income';
  /** For the saved profile. */
  status: EligibilityStatus; registered: boolean; summary: string;
  /** The value the question named (e.g. 휴학) checked against the registered condition. */
  asked_value?: string; asked_status?: EligibilityStatus; passage_id?: string;
}
/** Grounded answer of POST /programs/{id}/assistant: quotes only, never generated. */
export interface GroundedAnswer {
  topics: AssistantTopic[]; follow_up_topic?: AssistantTopic; verdict?: GroundedVerdict;
  passages: GroundedPassage[]; not_found: boolean; unsupported: boolean; notice: string;
}
/** One line of POST /programs/{id}/assistant/answer-stream. */
export type AnswerStreamEvent =
  | { type: 'start' }
  | { type: 'delta'; text: string }
  | { type: 'final'; valid: boolean; text: string; violation?: string; error?: string };

export interface AssistantSearch {
  version: number;
  intent: { region: string | null; region_label: string | null; enrollment_status: string | null; enrollment_label: string | null; program_types: string[]; keywords: string[] };
  unsupported: boolean; intent_empty: boolean; required_profile_fields: string[];
  total_candidates: number;
  /** youth_fit_label: a demoted row the question asked for (listed last). */
  results: (RecommendationItem & { matched_keywords: string[]; youth_fit_label?: string | null })[];
  not_found: boolean; notice: string;
}
