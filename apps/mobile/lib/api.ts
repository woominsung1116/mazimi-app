// ---------------------------------------------------------------------------
// API client — connects to the Rust/Axum backend at API_BASE_URL.
//
// Auth token flow:
//   Call api.setToken(token) after login. All subsequent requests will
//   include `Authorization: Bearer <token>`. Call api.clearToken() on logout.
//
// All protected endpoints derive the user identity from the JWT on the server.
// No user_id is sent in request bodies or query params for authenticated routes.
// ---------------------------------------------------------------------------

import type { IncomeMedianBand, MaritalStatus } from "./optionalAnswers";
import { useAuthStore } from "../store/auth";
import type { BenefitGuidance, AssistantBriefing, AssistantBundle, AssistantScenario, AssistantSearch, AssistantTopic, BenefitComparison } from './assistantTypes';
import type { AgentConversation, AgentConversationSummary, AgentRun, AgentStatus, Decision, ProactiveSettings, StartedRun } from './agentTypes';

import { API_BASE_URL } from "./apiBaseUrl";
import { GENERIC_NOT_FOUND, errorCopy } from "./errorCopy";
import { displayLines, normalizeDisplayText } from "./displayText";

export { API_BASE_URL };

/** Read the current JWT synchronously from the Zustand auth store. */
function getToken(): string | null {
  return useAuthStore.getState().token;
}

// ---------------------------------------------------------------------------
// Core fetch wrapper
// ---------------------------------------------------------------------------

export class ApiRequestError extends Error {
  constructor(message: string, public status: number) { super(message); this.name='ApiRequestError'; }
}
/**
 * `skipRefresh`: a 401 is returned as-is instead of exchanging the refresh
 * token and retrying. Sign-out uses it so that an expired access token can
 * never mint a new session in the middle of ending one.
 */
export type RequestOptions = RequestInit & { skipRefresh?: boolean };
async function request<T>(path: string, options?: RequestOptions): Promise<T> {
  const { skipRefresh = false, ...init } = options ?? {};
  const token = getToken();
  const userId = useAuthStore.getState().user?.id;
  const headers: Record<string, string> = {
    "Content-Type": "application/json", ...(init.headers as Record<string, string>),
  };
  // A fetch that never gets a response ("Network request failed", "Failed to
  // fetch") becomes status 0 with Korean copy, like every other failure here.
  const send = async (credential: string | null) => {
    try {
      return await fetch(`${API_BASE_URL}${path}`, {
        ...init, headers: credential ? { ...headers, Authorization: `Bearer ${credential}` } : headers,
      });
    } catch (error) {
      if ((error as { name?: string } | null)?.name === "AbortError") throw error;
      throw new ApiRequestError(errorCopy({ status: 0 }), 0);
    }
  };
  let res = await send(token);
  if (res.status === 401 && token && skipRefresh) throw new ApiRequestError(errorCopy({ status: 401 }), 401);
  if (res.status === 401 && token) {
    const auth = useAuthStore.getState();
    // An old account's response must never refresh or log out a newer account.
    if (auth.user?.id !== userId || !auth.token) throw new ApiRequestError("로그인 상태가 변경됐어요.", 401);
    const refreshed = auth.token !== token || await auth.refreshAccessToken();
    const current = useAuthStore.getState();
    if (current.user?.id !== userId || !current.token) throw new ApiRequestError("로그인 상태가 변경됐어요.", 401);
    if (refreshed) res = await send(current.token);
    if (!refreshed || res.status === 401) {
      // Only an auth rejection ends the session. A 403/404/503 after a
      // successful refresh must retain its real status and the user's session.
      if (useAuthStore.getState().token === current.token) await current.logout();
      throw new ApiRequestError(errorCopy({ status: 401 }), 401);
    }
  }
  if (!res.ok) {
    let serverText = "";
    try { const body = await res.json(); if (typeof body?.error === "string") serverText = body.error; }
    catch { /* A gateway may return a non-JSON error body. */ }
    // Screens show error.message, so it must be user copy: Korean text the
    // server wrote for users is kept; English or internal text becomes generic copy.
    throw new ApiRequestError(/[가-힣]/.test(serverText) ? serverText : errorCopy({ status: res.status }, { notFound: GENERIC_NOT_FOUND }), res.status);
  }
  // 204 No Content (e.g. DELETE) has no body to parse.
  if (res.status === 204) return undefined as T;
  return res.json();
}

// ---------------------------------------------------------------------------
// Types — aligned to the Rust models in crates/core/src/models.rs
// ---------------------------------------------------------------------------

export interface ApplicationChannel {
  id: string; label: string; evidence: string;
}
export interface ApplicationWorkflow {
  version: number; channels: ApplicationChannel[]; needs_confirmation: boolean;
  no_application_required: boolean; method_text: string | null;
}
export interface ApplicationJourney {
  id: string; program_id: string; plan_revision: string; channel: string;
  outcome: 'checking' | 'unknown' | 'reported_applied';
  handoff_at: string; reported_at: string | null; receipt_reference: string | null;
  snapshot: { title: string; method: string | null; channel: ApplicationChannel;
    destination: { url: string; origin: string } | null; evidence_kind: 'user_reported' };
}

export interface ApplicationResource {
  id: string; kind: 'notice' | 'application' | 'template'; url: string; source_url: string; label: string;
  access: 'direct' | 'login_required'; file_format: string | null; byte_size: number | null; content_hash: string | null;
  verified_at: string; valid_until: string;
}
export interface ApplicationDestination {
  url: string; origin: string; platform: string;
  kind: 'information' | 'portal' | 'unverified_page' | 'application' | 'login_required';
  evidence: 'official_url' | 'reviewed_application' | 'link_resolver'; label: string; explanation: string;
}

export interface ApplicationPreparation {
  revision: string;
  title: string;
  policy: NonNullable<ApiProgram["application_policy"]>;
  tasks: { id: string; title: string; detail: string; kind: string }[];
  completed_tasks: string[];
  progress_reset: boolean;
  selected_channel: string | null;
  /** HTTPS links quoted in the notice's application method. Pointers only, never submit authority. */
  official_links: { url: string; evidence: string }[];
}

/** Matches the `Program` struct returned by GET /api/v1/programs/:id */
/** One "label : value" line of 신청 방법 (Figma 34065:11159). */
export interface ApplicationProcedureLine { label: string; value: string | null; url: string | null; source: string }
/** A step of the notice's own procedure text, trimmed of its list mark. */
export interface ApplicationProcedureStep { title: string; detail: string | null; source: string }
export interface ApplicationProcedure {
  organization: string | null;
  lines: ApplicationProcedureLine[];
  intro: string | null;
  steps: ApplicationProcedureStep[];
  /** The notice numbered the steps or chained them with arrows. */
  ordered: boolean;
}
/** 신청 진행 단계 (Figma 34065:10997): 발견 · 자격 확인 · 서류 준비 · 신청. */
export interface ApplicationStage { stage: 'discover' | 'eligibility' | 'documents' | 'apply'; label: string; state: 'done' | 'current' | 'upcoming' }

export interface ApiProgram {
  id: string;
  company_name?: string | null;
  provider_kind?: string | null;
  benefit_category?: string | null;
  benefit_conditions?: string | null;
  /** `source` (v1 feedback server): "curated" (reviewed rows) or "official_text" (split from the notice's documents field). */
  required_documents?: { id: string; document_name: string; description: string | null; is_required: boolean | null; sort_order?: number; source?: "curated" | "official_text" }[];
  /** Detail only: the ※ notes of the documents text. Absent on older servers. */
  required_document_notes?: string[];
  /** Detail only: no document rows, and the notice states nothing is to be submitted ("해당없음"). Absent on older servers. */
  no_documents_required?: boolean;
  /** Provider axis (정책/장학금/기업혜택) and field axis (주거/생활/교육/복지/금융/취업). Absent on older servers; see constants/fields.ts. */
  provider_category?: string | null;
  provider_label?: string | null;
  provider_categories?: string[] | null;
  field_category?: string | null;
  field_label?: string | null;
  /** 기업혜택 chip code (mazimi_core::categories::Categories) of a row on the 기업혜택 tab, null elsewhere; served with the other category fields on list items, the detail page and recommendation items (absent on older servers). constants/categoryStyle.ts picks the glyph and colours from it. */
  corporate_category?: string | null;
  /** Server-cleaned summary (leading bullets removed, items joined by \n). Absent on older servers; use displaySummary(). */
  summary_display?: string | null;
  summary_lines?: string[] | null;
  /** Detail only: cleaned benefit_conditions. */
  benefit_conditions_display?: string | null;
  benefit_conditions_lines?: string[] | null;
  /** KST date of the deadline, "YYYY-MM-DD". */
  deadline_date?: string | null;
  /** Detail only: citations tagged by the check they support. */
  evidence?: CheckEvidence[];
  /** Detail only: 신청 방법 from the notice's own fields (server services/application_procedure.rs). */
  application_procedure?: ApplicationProcedure;
  /** Detail only: 신청 진행 단계 of a guest; a member's own come from getProgramProgress. */
  application_progress?: ApplicationStage[];
  application_policy?: { workflow: ApplicationWorkflow; mode: string; can_prepare: boolean; can_autofill: boolean; can_submit: boolean; reason: string; official_origin: string | null;
    destination: ApplicationDestination | null; notice_destination?: ApplicationDestination | null;
    resources?: { notice: ApplicationResource | null; application: ApplicationResource | null; templates: ApplicationResource[]; status: 'verified' | 'unverified' | 'recheck_required' } };
  program_type: string;       // "scholarship" | "support" | etc.
  source_type: string;
  source_id: string | null;
  title: string;
  /** `title` without the administrative prefix (server mazimi_core::title_display). Absent on older servers and /my/saved rows; use displayTitle(). */
  title_display?: string | null;
  summary: string | null;
  provider_name: string | null;
  official_url: string | null;
  program_status: string;     // "open" | "closed" | "always_open" | etc.
  application_start_at: string | null;
  application_end_at: string | null;
  benefit_amount_monthly: number | null;
  benefit_amount_semester: number | null;
  benefit_amount_once: number | null;
  region_scope: unknown | null;
  school_scope: unknown | null;
  tags: unknown | null;
  min_age: number | null;
  max_age: number | null;
  regions: string[] | null;
  deadline_at: string | null;
  /** Validity end of a corporate/campus offer; the third input of the effective deadline. */
  benefit_valid_until?: string | null;
  /** Earliest of deadline_at, application_end_at and benefit_valid_until (detail and saved list; list items too on the v1 feedback server). */
  effective_deadline?: string | null;
  is_active: boolean;
  last_synced_at: string | null;
  created_at: string;
  updated_at: string;
  /**
   * Free-text 신청방법 (how to apply). ~53% of 온통청년 policies carry this.
   * Often contains a "→"-delimited step chain (e.g. "시험응시 → 접수 →
   * 서류확인 → 선정 → 지급") — render as a stepper when present, otherwise
   * as plain text. Null when the source program has no such field.
   */
  application_method: string | null;
  /** Free-text 제출서류 (submission documents) — render as an itemized list. */
  submission_documents: string | null;
  /** Free-text 심사방법 (screening method) — render as a text section. */
  screening_method: string | null;
  /** youth_fit verdict (null/absent = not computed yet). A demoted row (not
   *  "fit") leaves the default list but stays searchable; show its label. */
  youth_fit?: "fit" | "off_target" | "business_only" | "child_only" | "disability_only" | "veteran_only" | null;
  /** Neutral card label of a demoted row, e.g. "청년 대상이 아닐 수 있음". */
  youth_fit_label?: string | null;
}

/** A user's own checklist entry (GET/POST /api/v1/my/document-items). */
export interface DocumentItem {
  id: string;
  program_id: string | null;
  name: string;
  status: "todo" | "done";
  created_at: string;
  updated_at: string;
}
/** Name rule of the document-items API: 1–60 characters after trimming, no control characters. */
export function validDocumentItemName(name: string): boolean {
  const trimmed = name.trim();
  // eslint-disable-next-line no-control-regex
  return trimmed.length >= 1 && [...trimmed].length <= 60 && !/[\u0000-\u001f\u007f-\u009f]/.test(trimmed);
}

/** How long a missing GET /recommendations route is remembered (same as the category probe's staleTime). */
export const RECOMMENDATIONS_CAPABILITY_TTL_MS = 30 * 60_000;
let recommendationsRouteMissingUntil = 0;

/**
 * Reads a members-only action decides on (saved list, alert preference, saved
 * profile) never hang: Android's fetch has no timeout of its own. After
 * READ_TIMEOUT_MS the request is aborted and fails like a dropped connection
 * (status 0), so the screen shows its error and retry instead of a spinner.
 */
export const READ_TIMEOUT_MS = 15_000;
export async function boundedRead<T>(read: (signal: AbortSignal) => Promise<T>, ms = READ_TIMEOUT_MS): Promise<T> {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), ms);
  try { return await read(controller.signal); }
  catch (error) {
    if (controller.signal.aborted) throw new ApiRequestError(errorCopy({ status: 0 }), 0);
    throw error;
  }
  finally { clearTimeout(timer); }
}

/** The title to show: the server's title_display when present, else the stored title. */
export function displayTitle(program: { title: string; title_display?: string | null }): string {
  return program.title_display?.trim() || program.title;
}
/** Server-cleaned summary when present, else the raw summary cleaned on the client; "" without either (F4.5/F5.6). */
export function displaySummary(program: { summary?: string | null; summary_display?: string | null }): string {
  return program.summary_display?.trim() || normalizeDisplayText(program.summary).display || "";
}
/** Summary items for bullets (detail, 혜택 요약). */
export const summaryLinesOf = (p: Pick<ApiProgram, "summary" | "summary_display" | "summary_lines" | "program_type">) =>
  // A corporate offer shows its official passage verbatim ("공식 안내 원문"), never split or cleaned.
  p.program_type === "corporate_benefit" ? (p.summary?.trim() ? [p.summary.trim()] : []) : displayLines(p.summary_lines, p.summary_display, p.summary);
/** Benefit-condition items for bullets (detail only). */
export const benefitConditionLinesOf = (p: Pick<ApiProgram, "benefit_conditions" | "benefit_conditions_display" | "benefit_conditions_lines">) => displayLines(p.benefit_conditions_lines, p.benefit_conditions_display, p.benefit_conditions);
/** One line for a card: the cleaned items joined with " · ". */
export function summaryLine(program: { summary?: string | null; summary_display?: string | null }): string {
  return displaySummary(program).split("\n").map(line => line.trim()).filter(Boolean).join(" · ");
}

/** One row of GET /api/v1/my/saved. Saved rows stay listed after they close. */
export interface SavedProgram extends ApiProgram {
  /** Still in the public catalogue; false = 마감·종료 (the detail page would 404). */
  available: boolean;
  effective_deadline: string | null;
}

/** GET /api/v1/programs response envelope */
export interface ProgramListResponse {
  total: number;
  school_total?: number;
  common_total?: number;
  /** 대학 tab: true when no school is recognised (common offers only). */
  school_required?: boolean;
  page: number;
  per_page: number;
  has_next: boolean;
  items: ApiProgram[];
}

// ---------------------------------------------------------------------------
// Application URL resolution
// ---------------------------------------------------------------------------

/**
 * Only the server-selected application destination is opened, and only after
 * the client re-checks it: HTTPS, no credentials, default port, a public
 * dotted host name (no IP literal, no localhost/internal suffix) and the
 * origin the server declared. The raw `official_url` is never a fallback.
 */
export function resolveApplyUrl(program: Pick<ApiProgram, "application_policy">): string | null {
  const destination = program.application_policy?.destination;
  if (!destination?.url) return null;
  try {
    const url = new URL(destination.url);
    const host = url.hostname.toLowerCase();
    const publicHost = host.includes(".") && !host.endsWith(".") && !host.includes(":") && !host.startsWith("[")
      && !/^[\d.]+$/.test(host)
      && host !== "localhost" && !/\.(localhost|local|internal|home\.arpa)$/.test(host);
    return url.protocol === "https:" && !url.username && !url.password && url.port === ""
      && publicHost && url.origin === destination.origin
      ? url.href : null;
  } catch { return null; }
}

/** A citation. `quote` is verbatim notice text and is absent when there is
 * no sentence to quote (structured fields). Mirrors Rust `models::Evidence`. */
export interface Evidence {
  kind: 'official_text' | 'structured_source' | 'ai_extracted_approved';
  quote?: string;
  source_label: string;
  source_url?: string;
  captured_at?: string;
  approved_at?: string;
  subject?: string;
}
export type EvidenceCheck = 'age' | 'region' | 'enrollment' | 'hard_rule' | 'application_window' | 'documents';
export interface CheckEvidence extends Evidence { check: EvidenceCheck }

/** `youth` (default): youth-relevant rows. `all`: the "전체 혜택" mode. */
export type RecommendScope = "youth" | "all";

/** One thing left to check before a row is "조건 확인됨"; `label` is fixed server copy. */
export interface RemainingCheck {
  kind: string;
  label: string;
  /** Profile field that answers it, when one does. */
  question_id?: string;
}

/** Reviewed application window of a recommendation row. */
export interface ReviewedWindow {
  kind: "dated" | "rolling" | "recurring" | "event" | "closed" | "unknown";
  opens_at: string | null;
  closes_at: string | null;
  /** When the official record last confirmed a rolling window. */
  confirmed_at: string | null;
  open_now: boolean;
}

/** Single item inside RecommendationResult.items */
export interface RecommendationItem {
  eligibility_status?: "pass" | "unknown";
  /** true = a notice rule was evaluated and every condition passed; false = none was, or one is unknown ("조건 미확인"). Deprecated: claims key off `verified`. Absent on older servers. */
  conditions_verified?: boolean;
  /** "조건 확인됨". The only field any eligibility claim in the UI may rest on. Absent on older servers. */
  verified?: boolean;
  /** What is left to check; empty when verified. Absent on older servers. */
  remaining_checks?: RemainingCheck[];
  /** Reviewed window; null without a current review. Absent on older servers. */
  window?: ReviewedWindow | null;
  program_id: string;
  title: string;
  /** `title` without the administrative prefix (server mazimi_core::title_display). Absent on older servers and /my/saved rows; use displayTitle(). */
  title_display?: string | null;
  program_type: string;
  match_score: number;
  benefit_amount_monthly: number | null;
  benefit_amount_semester: number | null;
  deadline: string | null;
  reasons: string[];
  missing_checks: string[];
  official_url: string | null;
  /** Eligibility citations; absent when none. */
  evidence?: CheckEvidence[];
  /** Checks the server evaluated: total = met + unknown + failed. Never an eligibility claim. Absent on older servers. */
  conditions_total?: number;
  conditions_met?: number;
  conditions_unknown?: number;
  conditions_failed?: number;
  provider_category?: string | null;
  provider_label?: string | null;
  provider_categories?: string[] | null;
  field_category?: string | null;
  field_label?: string | null;
  /** 기업혜택 chip code (mazimi_core::categories::Categories) of a row on the 기업혜택 tab, null elsewhere; served with the other category fields on list items, the detail page and recommendation items (absent on older servers). constants/categoryStyle.ts picks the glyph and colours from it. */
  corporate_category?: string | null;
  /** Card body (34065:2930): the programme summary, cleaned and capped by the server; absent when the programme has none. */
  summary?: string | null;
  summary_display?: string | null;
  provider_name?: string | null;
  company_name?: string | null;
}

/** POST /api/v1/recommend/preview response */
export interface RecommendationResult {
  total_available: number;
  estimated_monthly: number;
  estimated_semester: number;
  items: RecommendationItem[];
  scope?: RecommendScope;
  /** Items with verified = true (listed first). Absent on older servers. */
  verified_count?: number;
  /** Server flag: clients may show the 조건 확인됨 / 확인 필요 sections. */
  verified_section_enabled?: boolean;
}

/** POST /api/v1/recommend/preview request body — matches Rust ProfileInput */
export interface ProfileInput {
  nickname?: string | null;
  birth_year: number;
  /** "YYYY-MM-DD" (api-contract §1). Servers before it ignore the key. */
  birth_date?: string | null;
  region_code: string;
  city_code?: string | null;
  school_name?: string | null;
  school_year?: number | null;
  enrollment_status?: string | null;
  employment_status?: string | null;
  major_group?: string | null;
  income_bracket?: number | null;
  kosaf_support_bracket?: number | null;
  housing_type?: string | null;
  household_size?: number | null;
  has_disability?: boolean | null;
  is_multicultural_family?: boolean | null;
  is_low_income_household?: boolean | null;
  veteran_family?: boolean | null;
  preferred_categories?: string[] | null;
  school_type?: string | null;
  age_band?: string | null;
  /** Optional answers (lib/optionalAnswers). A save that omits a key keeps the stored answer; null clears it. */
  homeless_household?: boolean | null;
  marital_status?: MaritalStatus | null;
  newlywed?: boolean | null;
  children_count?: number | null;
  income_median_band?: IncomeMedianBand | null;
}

/** GET /api/v1/dashboard response (user identity from JWT) */
export interface DashboardData {
  bookmarked_count: number;
  applying_count: number;
  upcoming_deadlines: ApiProgram[];
  estimated_monthly: number;
  estimated_semester: number;
  todo_items: string[];
}

/** Single alert item from GET /api/v1/alerts */
export interface NotificationPreferences {
  deadline: boolean;
  new_program: boolean;
  profile_update: boolean;
  channels: { in_app: boolean; push: boolean; kakao: boolean; email: boolean };
}

export interface AlertItem {
  id: string;
  user_id: string;
  program_id: string;
  alert_type: string;
  alert_date: string;
  program_title: string;
  official_url: string | null;
  deadline_at: string | null;
  application_end_at: string | null;
  read_at: string | null;
}

/** GET /api/v1/alerts response (user identity from JWT) */
export interface AlertListResponse {
  total: number;
  limit: number;
  offset: number;
  unread: number;
  items: AlertItem[];
}

/** GET /api/v1/profile response (user identity from JWT) */
export interface UserProfileResponse {
  user_id: string;
  profile: UserProfile;
}

export interface UserProfile {
  user_id: string;
  birth_year: number | null;
  /** Absent on servers before api-contract §1. */
  birth_date?: string | null;
  region_code: string | null;
  city_code: string | null;
  school_name: string | null;
  school_year: number | null;
  enrollment_status: string | null;
  employment_status: string | null;
  major_group: string | null;
  income_bracket: number | null;
  kosaf_support_bracket: number | null;
  housing_type: string | null;
  household_size: number | null;
  /** Group flags: null = not answered, false = answered "no". */
  has_disability: boolean | null;
  is_multicultural_family: boolean | null;
  is_low_income_household: boolean | null;
  veteran_family: boolean | null;
  preferred_categories: string[];
  school_type: string | null;
  age_band: string | null;
  homeless_household?: boolean | null;
  marital_status?: MaritalStatus | null;
  newlywed?: boolean | null;
  children_count?: number | null;
  income_median_band?: IncomeMedianBand | null;
  profile_version: number;
  updated_at: string;
  nickname: string | null;
  profile_image_url: string | null;
}

/** POST /api/v1/programs/{id}/bookmark response */
export interface BookmarkResponse {
  bookmarked: boolean;
}

// Application status values as used by the backend
export type ApplicationStatus =
  | "interested"
  | "planning"
  | "applying"
  | "applied"
  | "waiting"
  | "received"
  | "abandoned";

/** Single history entry inside ApplicationDetail */
export interface StateHistoryEntry {
  state: ApplicationStatus;
  memo: string | null;
  changed_at: string;
}

/** Server-derived status for display: the 7 user-recorded states plus an unconfirmed journey. */
export type ApplicationDisplayStatus = ApplicationStatus | "submission_unconfirmed";

/** GET /api/v1/my/applications/{program_id} (user identity from JWT) */
export interface ApplicationDetail {
  journey?: ApplicationJourney | null;
  submission_check?: ApplicationJourney["outcome"] | null;
  /** One status for every client; both inputs are self-reported, never institution-verified. */
  display_status: ApplicationDisplayStatus;
  display_label: string;
  snapshot?: { captured_at: string; capture_reason: string; program: {
    title: string; summary: string | null; conditions?: string | null; application_method?: string | null;
    required_documents?: { document_name: string; is_required: boolean }[];
  } } | null;
  program_id: string;
  program_title: string;
  current_status: ApplicationStatus;
  memo: string | null;
  applied_at: string | null;
  result_at: string | null;
  updated_at: string;
  history: StateHistoryEntry[];
  /** List only: the programme's provider and cleaned summary (신청 이력 card 34068:3827 / 3831). */
  provider_name?: string | null;
  summary_display?: string | null;
}

/** GET /api/v1/my/applications (user identity from JWT) */
export interface ApplicationListResponse {
  total: number;
  items: ApplicationDetail[];
}

/** PUT /api/v1/my/applications/{program_id} response (user identity from JWT) */
export interface UpdateStatusResponse {
  program_id: string;
  status: ApplicationStatus;
  memo: string | null;
  applied_at: string | null;
  result_at: string | null;
  updated_at: string;
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/** A won amount: `5만원` for whole 만원, otherwise `5,000원` / `15,000원`. */
export function formatWon(amount: number): string {
  if (amount >= 10000 && amount % 10000 === 0) return `${amount / 10000}만원`;
  return `${amount.toLocaleString("ko-KR")}원`;
}

/** Mirror of mazimi_core::pays_support: a corporate offer's amounts are no support money. */
export function paysSupport(programType: string): boolean {
  return programType !== "corporate_benefit";
}

/**
 * Whether the programme states a support amount to show. A corporate offer
 * never does: its discount is in the official sentence, not an amount.
 */
export function hasBenefitAmount(program: ApiProgram): boolean {
  if (!paysSupport(program.program_type)) return false;
  return Boolean(
    program.benefit_amount_monthly || program.benefit_amount_semester || program.benefit_amount_once,
  );
}

/** Format a benefit amount into a human-readable Korean label. */
/** Support amount label, or "" without one (callers hide the row; never a placeholder like "혜택 확인"). */
export function formatBenefit(program: ApiProgram): string {
  if (!hasBenefitAmount(program)) return "";
  if (program.benefit_amount_monthly) {
    return `월 ${formatWon(program.benefit_amount_monthly)}`;
  }
  if (program.benefit_amount_semester) {
    return `학기 ${formatWon(program.benefit_amount_semester)}`;
  }
  if (program.benefit_amount_once) {
    return `최대 ${formatWon(program.benefit_amount_once)}`;
  }
  return "";
}

/** Program-type labels live in the module shared with web; re-exported for existing imports. */
export { programTypeLabel } from "../constants/displayLabels";


// ---------------------------------------------------------------------------
// Auth helpers
// ---------------------------------------------------------------------------

/**
 * Returns the current user's ID from the Zustand auth store.
 * Throws if no user is authenticated.
 *
 * NOTE: Most API methods no longer require the caller to pass a user ID —
 * the server extracts identity from the JWT Bearer token automatically.
 * Use this only when you genuinely need the local user UUID (e.g. building
 * a cache key or logging).
 */
export function getCurrentUserId(): string {
  const userId = useAuthStore.getState().user?.id;
  if (!userId) {
    throw new Error("getCurrentUserId: no authenticated user");
  }
  return userId;
}

// ---------------------------------------------------------------------------
// API methods
// ---------------------------------------------------------------------------

export interface CorporateCategoryCounts {
  items: { code: string; label: string; count: number }[];
  /** Visible programmes of the 기업혜택 tab. */
  total: number;
}

export const api = {
  deleteAccount: (): Promise<{ deleted: boolean }> => request("/api/v1/auth/me", { method: "DELETE", body: JSON.stringify({ confirmation: "회원 탈퇴" }) }),
  /** Revokes the rotating refresh token, which is the credential itself. Never refreshes: ending a session must not mint one. */
  /** Revokes the refresh token; with `pushToken`, also removes this device's push row. */
  logout: (refreshToken: string, pushToken?: string | null): Promise<unknown> => request("/api/v1/auth/logout", { method: "POST", body: JSON.stringify(pushToken ? { refresh_token: refreshToken, push_token: pushToken } : { refresh_token: refreshToken }), skipRefresh: true }),
  // ── Programs ──────────────────────────────────────────────────────────────

  getPrograms: (params?: {
    program_type?: string;
    region?: string;
    q?: string;
    status?: string;
    sort?: string;
    /** 기업혜택 chip code from getCorporateCategories; limits the list to the corporate tab. */
    corporate_category?: string;
    /** v1 feedback server: provider axis (policy|scholarship|corporate) and field axis. */
    provider?: string;
    field?: string;
    /** v1 feedback server: only deadlines D-0..D-N (KST). Older servers ignore it. */
    closing_within?: number;
    /** 탐색 소득 구간: a 기준 중위소득 band code ("0-50" … "200+"); only rows that flag it. */
    income_band?: string;
    page?: number;
    per_page?: number;
  }): Promise<ProgramListResponse> => {
    const query = new URLSearchParams();
    for (const [key, value] of Object.entries(params ?? {})) {
      if (value !== undefined && value !== "") query.set(key, String(value));
    }
    const qs = query.toString();
    // QueryClient handles brief in-memory reuse. Do not restore an old
    // persistent catalogue after a server-side expiry or source suspension.
    return request<ProgramListResponse>(`/api/v1/programs${qs ? `?${qs}` : ""}`);
  },

  getProgram: (id: string): Promise<ApiProgram> =>
    request<ApiProgram>(`/api/v1/programs/${encodeURIComponent(id)}`),
  /** The signed-in user's 신청 진행 단계 for one programme (private, never cached by the server). */
  getProgramProgress: (id: string): Promise<{ stages: ApplicationStage[] }> =>
    request<{ stages: ApplicationStage[] }>(`/api/v1/programs/${encodeURIComponent(id)}/progress`),
  /** Mazimi-hosted PDF converted from a published HWP/HWPX form; verify against the template's content_hash. */
  formPdfUrl: (programId: string, resourceId: string): string =>
    `${API_BASE_URL}/api/v1/programs/${encodeURIComponent(programId)}/application-resources/${encodeURIComponent(resourceId)}/form-pdf`,
  /** 서류 작성 도우미 input boxes for a published template; null when none is defined. Validate with parseTemplate. */
  getFormTemplate: async (programId: string, resourceId: string): Promise<unknown | null> => {
    try {
      // form-date-parts-v1: this build reads a box's date `part`; without it the server strips part boxes' links.
      return await request<unknown>(`/api/v1/programs/${encodeURIComponent(programId)}/application-resources/${encodeURIComponent(resourceId)}/form-template`, { headers: { 'X-Mazimi-Client-Caps': 'form-date-parts-v1' } });
    } catch (e) {
      if (e instanceof ApiRequestError && e.status === 404) return null;
      throw e;
    }
  },

  getUniversities: (): Promise<{ items: { id: string; name: string; region: string; aliases: string[] }[] }> => request("/api/v1/campus/universities"),
  getCampusPrograms: (school: string, enrollmentStatus: string, q = "", page = 1): Promise<ProgramListResponse> =>
    request(`/api/v1/campus/programs?${new URLSearchParams({ school, enrollment_status: enrollmentStatus, q, page: String(page) })}`),

  getPreparation: (id: string): Promise<ApplicationPreparation> => request(`/api/v1/programs/${encodeURIComponent(id)}/preparation`),
  getBenefitGuidance: (id: string, signal?: AbortSignal): Promise<BenefitGuidance> => request(`/api/v1/programs/${encodeURIComponent(id)}/assistant`, { signal }),
  getAssistantBriefing: (signal?: AbortSignal): Promise<AssistantBriefing> => request('/api/v1/assistant/briefing', { signal }),
  getAssistantBundle: (scenario: AssistantScenario, signal?: AbortSignal): Promise<AssistantBundle> =>
    request(`/api/v1/assistant/bundles?scenario=${encodeURIComponent(scenario)}`, { signal }),
  compareBenefits: (programIds: string[], signal?: AbortSignal): Promise<BenefitComparison> =>
    request('/api/v1/assistant/compare', { method: 'POST', signal, body: JSON.stringify({ program_ids: programIds }) }),
  /** Grounded answer (quotes, verdict, not-found). `previousTopic` is the last
   * answer's follow_up_topic, kept on the device only. */
  askBenefitAssistant: async (id: string, question: string, allowAiProcessing: boolean, previousTopic?: AssistantTopic): Promise<BenefitGuidance> => {
    const controller = new AbortController();
    const timeout = setTimeout(() => controller.abort(), 50_000);
    try {
      return await request(`/api/v1/programs/${encodeURIComponent(id)}/assistant`, {
        method: 'POST', signal: controller.signal,
        body: JSON.stringify({ question, allow_ai_processing: allowAiProcessing, previous_topic: previousTopic }),
      });
    } catch (error) {
      if (controller.signal.aborted) throw new Error('질문 처리 시간이 길어지고 있어요. 기본 안내를 이용하거나 잠시 후 다시 시도해 주세요.');
      throw error;
    } finally { clearTimeout(timeout); }
  },
  // ── AI benefit agent (docs/mazimi-ai-agent.md). user_id comes from the JWT. ──
  getAgentStatus: (signal?: AbortSignal): Promise<AgentStatus> => request('/api/v1/assistant/agent', { signal }),
  listAgentConversations: (signal?: AbortSignal): Promise<{ items: AgentConversationSummary[]; retention_days: number }> =>
    request('/api/v1/assistant/conversations', { signal }),
  createAgentConversation: (contextProgramId?: string): Promise<{ id: string }> =>
    request('/api/v1/assistant/conversations', { method: 'POST', body: JSON.stringify(contextProgramId ? { context_program_id: contextProgramId } : {}) }),
  getAgentConversation: (id: string, signal?: AbortSignal): Promise<AgentConversation> =>
    request(`/api/v1/assistant/conversations/${encodeURIComponent(id)}`, { signal }),
  deleteAgentConversation: (id: string): Promise<{ deleted: boolean }> =>
    request(`/api/v1/assistant/conversations/${encodeURIComponent(id)}`, { method: 'DELETE' }),
  /** Starts a run (202). The goal is the only free text; programme ids are what the user selected. */
  sendAgentGoal: (conversationId: string, goal: string, idempotencyKey: string, programId?: string, programIds?: string[]): Promise<StartedRun> =>
    request(`/api/v1/assistant/conversations/${encodeURIComponent(conversationId)}/messages`, {
      // journey-v1: this build opens the 신청 준비 handoff cards.
      method: 'POST', headers: { 'Idempotency-Key': idempotencyKey, 'X-Mazimi-Client-Caps': 'journey-v1' },
      body: JSON.stringify({ goal, ...(programId ? { program_id: programId } : {}), ...(programIds?.length ? { program_ids: programIds } : {}) }),
    }),
  getAgentRun: (id: string, signal?: AbortSignal): Promise<AgentRun> => request(`/api/v1/assistant/runs/${encodeURIComponent(id)}`, { signal }),
  cancelAgentRun: (id: string): Promise<{ run_id: string; status: string }> =>
    request(`/api/v1/assistant/runs/${encodeURIComponent(id)}/cancel`, { method: 'POST' }),
  /** Sends only the hash shown on the card; the server re-validates before executing once. */
  approveAgentConfirmation: (id: string, payloadHash: string, idempotencyKey: string): Promise<Decision> =>
    request(`/api/v1/assistant/confirmations/${encodeURIComponent(id)}/approve`, {
      method: 'POST', headers: { 'Idempotency-Key': idempotencyKey }, body: JSON.stringify({ payload_hash: payloadHash }),
    }),
  rejectAgentConfirmation: (id: string): Promise<Decision> =>
    request(`/api/v1/assistant/confirmations/${encodeURIComponent(id)}/reject`, { method: 'POST', body: '{}' }),
  getAgentProactive: (): Promise<{ settings: ProactiveSettings; quiet_hours: string; note: string }> => request('/api/v1/assistant/agent/proactive'),
  saveAgentProactive: (settings: ProactiveSettings): Promise<{ settings: ProactiveSettings }> =>
    request('/api/v1/assistant/agent/proactive', { method: 'PUT', body: JSON.stringify(settings) }),
  /** Cross-programme search from one question. Rules only, no profile change. */
  searchAssistant: (question: string, signal?: AbortSignal): Promise<AssistantSearch> =>
    request('/api/v1/assistant/search', { method: 'POST', signal, body: JSON.stringify({ question }) }),
  savePreparation: (id: string, revision: string, completed_tasks: string[], selected_channel?: string): Promise<ApplicationPreparation> =>
    request(`/api/v1/programs/${encodeURIComponent(id)}/preparation`, { method: "PUT", body: JSON.stringify({ revision, completed_tasks, selected_channel }) }),
  getApplicationJourney: (id: string): Promise<{ journey: ApplicationJourney | null }> => request(`/api/v1/my/application-journeys/${encodeURIComponent(id)}`),
  startApplicationJourney: (id: string, revision: string, channel: string): Promise<ApplicationJourney> =>
    request(`/api/v1/programs/${encodeURIComponent(id)}/handoff`, { method: "POST", body: JSON.stringify({ revision, channel }) }),
  reportApplicationJourney: (id: string, journey_id: string, outcome: 'unknown' | 'reported_applied', confirmed = false, receipt_reference?: string): Promise<ApplicationJourney> =>
    request(`/api/v1/my/application-journeys/${encodeURIComponent(id)}`, { method: "PUT", body: JSON.stringify({ journey_id, outcome, confirmed, receipt_reference }) }),

  toggleBookmark: (programId: string): Promise<BookmarkResponse> =>
    request<BookmarkResponse>(`/api/v1/programs/${encodeURIComponent(programId)}/bookmark`, {
      method: "POST",
    }),
  /** Idempotent: sets the bookmark and returns the state the server ended in. */
  setBookmark: (programId: string, bookmarked: boolean): Promise<BookmarkResponse> =>
    request<BookmarkResponse>(`/api/v1/programs/${encodeURIComponent(programId)}/bookmark`, {
      method: "PUT",
      body: JSON.stringify({ bookmarked }),
    }),

  /** 기업혜택 chips: visible programmes per category (mazimi_core::corporate_categories) under the list's q/status/region; empty ones left out. */
  getCorporateCategories: (filters: { q?: string; status?: string; region?: string; income_band?: string } = {}): Promise<CorporateCategoryCounts> => {
    const query = new URLSearchParams();
    for (const [key, value] of Object.entries(filters)) if (value) query.set(key, value);
    const qs = query.toString();
    return request<CorporateCategoryCounts>(`/api/v1/programs/corporate-categories${qs ? `?${qs}` : ""}`);
  },

  // ── Recommendations ───────────────────────────────────────────────────────

  /**
   * Conditions in the body, no login needed. A server with the anonymous preview
   * off answers 401 login_required even to a member: that is not an expired
   * session, so skipRefresh keeps it from refreshing or logging the member out.
   */
  getRecommendPreview: (profile: ProfileInput, scope: RecommendScope = "youth"): Promise<RecommendationResult> =>
    request<RecommendationResult>(`/api/v1/recommend/preview?scope=${scope === "all" ? "all" : "youth"}`, {
      method: "POST",
      body: JSON.stringify(profile),
      skipRefresh: true,
    }),
  /**
   * A member's recommendations from the saved profile (GET /api/v1/recommendations).
   * A server without the route (404/405) gets the preview with `profile` instead,
   * and the missing route is remembered for RECOMMENDATIONS_CAPABILITY_TTL_MS, so
   * later fetches go straight to the preview instead of a 404 first (M3).
   * 409 profile_required surfaces as an ApiRequestError with status 409.
   */
  getRecommendations: async (profile: ProfileInput, scope: RecommendScope = "youth"): Promise<RecommendationResult> => {
    if (Date.now() < recommendationsRouteMissingUntil) return api.getRecommendPreview(profile, scope);
    try {
      return await request<RecommendationResult>(`/api/v1/recommendations?scope=${scope === "all" ? "all" : "youth"}`);
    } catch (error) {
      if (error instanceof ApiRequestError && (error.status === 404 || error.status === 405)) {
        recommendationsRouteMissingUntil = Date.now() + RECOMMENDATIONS_CAPABILITY_TTL_MS;
        return api.getRecommendPreview(profile, scope);
      }
      throw error;
    }
  },

  // ── Dashboard ─────────────────────────────────────────────────────────────

  getDashboard: (): Promise<DashboardData> =>
    request<DashboardData>("/api/v1/dashboard"),

  // ── Alerts ────────────────────────────────────────────────────────────────

  getAlerts: (
    opts?: { limit?: number; offset?: number }
  ): Promise<AlertListResponse> => {
    const q = new URLSearchParams();
    if (opts?.limit) q.set("limit", String(opts.limit));
    if (opts?.offset) q.set("offset", String(opts.offset));
    const qs = q.toString();
    return request<AlertListResponse>(`/api/v1/alerts${qs ? `?${qs}` : ""}`);
  },

  upsertAlertPreference: (
    programId: string,
    enabled: boolean
  ): Promise<unknown> =>
    request("/api/v1/alerts/preferences", {
      method: "POST",
      body: JSON.stringify({ program_id: programId, enabled }),
    }),

  getAlertPreference: (programId: string): Promise<{ program_id: string; enabled: boolean }> =>
    boundedRead(signal => request(`/api/v1/alerts/preferences/${encodeURIComponent(programId)}`, { signal })),

  markAlertsRead: (): Promise<{ updated: number }> =>
    request("/api/v1/alerts/read-all", { method: "POST" }),

  getNotificationPreferences: (): Promise<NotificationPreferences> => request("/api/v1/alerts/notification-preferences"),
  saveNotificationPreferences: (preferences: NotificationPreferences): Promise<NotificationPreferences> => request("/api/v1/alerts/notification-preferences", { method: "PUT", body: JSON.stringify(preferences) }),

  // ── Profile ───────────────────────────────────────────────────────────────

  getProfile: (): Promise<UserProfileResponse> => boundedRead(signal => request<UserProfileResponse>("/api/v1/profile", { signal })),

  saveProfile: (profile: ProfileInput): Promise<UserProfileResponse> =>
    request<UserProfileResponse>("/api/v1/profile", {
      method: "POST",
      body: JSON.stringify(profile),
    }),

  // ── Application status tracking ───────────────────────────────────────────

  /**
   * Returns all programs the authenticated user has a status record for.
   * GET /api/v1/my/applications
   */
  getMyApplications: (): Promise<ApplicationListResponse> =>
    request<ApplicationListResponse>("/api/v1/my/applications"),

  /**
   * Returns status + full history for a single program.
   * Returns null when the user has never set a status for this program
   * (backend returns 404 in that case).
   * GET /api/v1/my/applications/{programId}
   */
  getApplicationStatus: async (
    programId: string
  ): Promise<ApplicationDetail | null> => {
    try {
      return await request<ApplicationDetail>(
        `/api/v1/my/applications/${encodeURIComponent(programId)}`
      );
    } catch (err) {
      if (err instanceof ApiRequestError && err.status === 404) {
        return null;
      }
      throw err;
    }
  },

  /**
   * Upserts the application status for one program.
   * PUT /api/v1/my/applications/{programId}
   * Body: { status, memo? }
   */
  updateApplicationStatus: (
    programId: string,
    status: ApplicationStatus,
    memo?: string
  ): Promise<UpdateStatusResponse> =>
    request<UpdateStatusResponse>(
      `/api/v1/my/applications/${encodeURIComponent(programId)}`,
      {
        method: "PUT",
        body: JSON.stringify({ status, memo: memo ?? null }),
      }
    ),

  // ── Saved / Bookmarks ────────────────────────────────────────────────────

  getSaved: (): Promise<{ items: SavedProgram[] }> =>
    boundedRead(signal => request<{ items: SavedProgram[] }>("/api/v1/my/saved", { signal })),

  // ── My document checklist items (names and status only; files stay on the device) ──

  /**
   * Whether the server has the document-items route. Asked without a token: an
   * existing route answers 401, a missing one 404/405. A network error throws (unknown).
   */
  hasDocumentItemsRoute: async (): Promise<boolean> => {
    const response = await fetch(`${API_BASE_URL}/api/v1/my/document-items`, { method: "GET" });
    return response.status !== 404 && response.status !== 405;
  },
  getDocumentItems: (programId?: string): Promise<{ items: DocumentItem[] }> =>
    request(`/api/v1/my/document-items${programId ? `?program_id=${encodeURIComponent(programId)}` : ""}`),
  addDocumentItem: (name: string, programId: string | null): Promise<DocumentItem> =>
    request("/api/v1/my/document-items", { method: "POST", body: JSON.stringify({ name, program_id: programId }) }),
  updateDocumentItem: (id: string, patch: { name?: string; status?: DocumentItem["status"] }): Promise<DocumentItem> =>
    request(`/api/v1/my/document-items/${encodeURIComponent(id)}`, { method: "PUT", body: JSON.stringify(patch) }),
  deleteDocumentItem: (id: string): Promise<void> =>
    request(`/api/v1/my/document-items/${encodeURIComponent(id)}`, { method: "DELETE" }),

  // ── Push notifications ─────────────────────────────────────────────────────

  /**
   * Registers an Expo push token for the authenticated user.
   * POST /api/v1/push/register
   * Body: { token, platform }  — user identity comes from JWT, not the body.
   */
  registerPushToken: (token: string, platform: string): Promise<unknown> =>
    request("/api/v1/push/register", {
      method: "POST",
      body: JSON.stringify({ token, platform }),
    }),

  /**
   * Removes this device's push token, or every token of the user when omitted.
   * DELETE /api/v1/push/register
   * Body: { token } — user identity comes from JWT, not the body.
   */
  unregisterPushToken: ({ token, ...options }: { token?: string } & Pick<RequestOptions, "skipRefresh"> = {}): Promise<unknown> =>
    request("/api/v1/push/register", {
      method: "DELETE",
      ...(token && { body: JSON.stringify({ token }) }),
      ...options,
    }),
};
