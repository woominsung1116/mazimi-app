import type { IncomeMedianBand, MaritalStatus } from "./optionalAnswers";
import { safeHttpsUrl } from "./safeUrl";

const API_BASE_URL =
  process.env.NEXT_PUBLIC_API_BASE_URL || "http://localhost:8080";

// ── Session ──
//
// The API token pair lives in httpOnly cookies that page scripts cannot read
// (set by the /auth/* server routes). Authenticated calls go to the
// same-origin /bff proxy, which attaches the access token as a Bearer header.
// A 401 triggers one refresh, then one retry.

const CSRF_HEADERS = { "X-Mazimi-Csrf": "1" } as const;
let refreshing: Promise<boolean> | null = null;

/**
 * Rotate the session once for every caller that hit a 401 at the same time;
 * across tabs a Web Lock serialises the rotations. Resolves false when the
 * session is over; rejects when the server could not tell (network, 503).
 */
export function refreshSession(): Promise<boolean> {
  const post = (headers: Record<string, string> = {}) =>
    fetch("/auth/refresh", { method: "POST", headers: { ...CSRF_HEADERS, ...headers }, credentials: "same-origin", cache: "no-store" });
  const rotate = async () => {
    const res = await post();
    if (res.status === 204) return true;
    if (res.status !== 401) throw new ApiError(res.status, res.statusText);
    // Refused. Clear the cookies only if the jar still holds the refused
    // token; 409 means another tab rotated meanwhile, so retry the call.
    const refused = res.headers.get("x-mazimi-session");
    if (!refused) return false;
    const cleared = await post({ "X-Mazimi-Clear-If": refused });
    // /auth/refresh answers a completed clear with 401: signed out.
    if (cleared.status === 409) return true;
    if (cleared.status === 401 || cleared.ok) return false;
    throw new ApiError(cleared.status, cleared.statusText);
  };
  if (!refreshing) {
    const run: Promise<boolean> = typeof navigator !== "undefined" && navigator.locks
      ? navigator.locks.request("mazimi-session-refresh", rotate).then(result => result)
      : rotate();
    refreshing = run.finally(() => { refreshing = null; });
  }
  return refreshing;
}

/** Revoke the refresh token at the API and clear the session cookies. */
export async function signOut(): Promise<void> {
  try {
    await fetch("/auth/logout", { method: "POST", headers: CSRF_HEADERS, credentials: "same-origin", cache: "no-store" });
  } finally {
    try {
      navigator.serviceWorker?.controller?.postMessage({ type: "clear-api-cache" });
    } catch {
      // No service worker; nothing cached to clear.
    }
  }
}

// ── API error class ──

export class ApiError extends Error {
  constructor(
    public readonly status: number,
    public readonly statusText: string,
    public readonly body?: unknown
  ) {
    super(`API error: ${status} ${statusText}`);
    this.name = "ApiError";
  }
}

// ── Core fetch wrapper ──

/** Send a signed-out visitor to the login page, then back to this page. */
function redirectToLogin() {
  if (typeof window === "undefined" || window.location.pathname === "/login") return;
  const back = `${window.location.pathname}${window.location.search}`;
  // Outside React (no router here); a full load also drops stale page state.
  // eslint-disable-next-line @next/next/no-location-assign-relative-destination
  window.location.assign(`/login?callbackUrl=${encodeURIComponent(back)}`);
}

async function request<T>(
  path: string,
  options?: RequestInit,
  authenticated = false,
  redirectWhenSignedOut = true
): Promise<T> {
  const headers: Record<string, string> = {
    "Content-Type": "application/json",
    ...(options?.headers as Record<string, string>),
  };

  let res: Response;
  if (authenticated) {
    const send = () => fetch(`/bff${path}`, {
      ...options,
      headers: { ...headers, ...CSRF_HEADERS },
      credentials: "same-origin",
      cache: "no-store",
    });
    res = await send();
    if (res.status === 401 && await refreshSession()) res = await send();
    // Still 401 after a refresh: the session is over. Pages need not each
    // handle it; the visitor goes to sign in and comes back here.
    if (res.status === 401 && redirectWhenSignedOut) redirectToLogin();
  } else {
    res = await fetch(`${API_BASE_URL}${path}`, {
      ...options,
      headers,
    });
  }

  if (!res.ok) {
    let body: unknown;
    try {
      body = await res.json();
    } catch {
      body = undefined;
    }
    throw new ApiError(res.status, res.statusText, body);
  }

  // 204 No Content — nothing to parse
  if (res.status === 204) {
    return undefined as unknown as T;
  }

  return res.json();
}

// Convenience wrappers to avoid passing the boolean everywhere
function publicRequest<T>(path: string, options?: RequestInit): Promise<T> {
  return request<T>(path, options, false);
}

function authRequest<T>(path: string, options?: RequestInit): Promise<T> {
  return request<T>(path, options, true);
}

/** Authenticated, but a 401 is an answer ("signed out"), not a redirect. */
function sessionProbe<T>(path: string, options?: RequestInit): Promise<T> {
  return request<T>(path, options, true, false);
}

// ---------------------------------------------------------------------------
// Types — aligned to the Rust models (snake_case, matching backend responses)
// ---------------------------------------------------------------------------

/**
 * Server-selected navigation destination. A link is evidence of a page, never
 * proof of a working form or authority to submit on the user's behalf.
 */
export interface ApplicationDestination {
  url: string;
  origin: string;
  platform: string;
  /** `application` / `login_required`: a reviewed application page. */
  kind: "information" | "portal" | "unverified_page" | "application" | "login_required";
  evidence: "official_url" | "reviewed_application" | "link_resolver";
  label: string;
  explanation: string;
}

/** A reviewed official notice, application page or blank form (server `PublicResource`). */
export interface ApplicationResource {
  id: string;
  kind: "notice" | "application" | "template";
  url: string;
  source_url: string;
  label: string;
  access: "direct" | "login_required";
  file_format: string | null;
  byte_size: number | null;
  content_hash: string | null;
  verified_at: string;
  valid_until: string;
}

export interface ApplicationChannel {
  id: string;
  label: string;
  evidence: string;
}

export interface ApplicationWorkflow {
  version: number;
  channels: ApplicationChannel[];
  needs_confirmation: boolean;
  no_application_required: boolean;
  method_text: string | null;
}

/** Mirrors `services::application_policy::ApplicationPolicy`. */
export interface ApplicationPolicy {
  mode: string;
  can_prepare: boolean;
  can_autofill: boolean;
  can_submit: boolean;
  reason: string;
  official_origin: string | null;
  destination: ApplicationDestination | null;
  /** The notice page, kept apart from a reviewed application page. */
  notice_destination?: ApplicationDestination | null;
  workflow?: ApplicationWorkflow;
  resources?: {
    notice: ApplicationResource | null;
    application: ApplicationResource | null;
    templates: ApplicationResource[];
    status: "verified" | "unverified" | "recheck_required";
  };
}

/**
 * Validate a server destination on the client as well: the same public HTTPS
 * check as every external link (`safeHttpsUrl`), and the origin the server
 * reported.
 */
export function safeDestinationUrl(destination: ApplicationDestination | null | undefined): string | null {
  if (!destination) return null;
  const href = safeHttpsUrl(destination.url);
  if (!href) return null;
  return new URL(href).origin === destination.origin ? href : null;
}

/** A resource link still inside its review window, with a public HTTPS URL. */
export function safeResourceUrl(resource: ApplicationResource | null | undefined, now = Date.now()): string | null {
  if (!resource) return null;
  const validUntil = Date.parse(resource.valid_until);
  if (!Number.isFinite(validUntil) || validUntil <= now) return null;
  return safeHttpsUrl(resource.url);
}

/** "HWP · 원본 양식"; no empty part when the format is unknown. No verification date: that is internal. */
export function resourceMetaLine(resource: Pick<ApplicationResource, "file_format">): string {
  return [resource.file_format?.toUpperCase(), "원본 양식"].filter(Boolean).join(" · ");
}

/** The title to show: the server's title_display when present, else the stored title. */
export function displayTitle(program: { title: string; title_display?: string | null }): string {
  return program.title_display?.trim() || program.title;
}

/** Matches the `Program` struct returned by GET /api/v1/programs/{id} */
export interface ApiProgram {
  id: string;
  /** Present on the detail endpoint; list payloads may omit it. */
  application_policy?: ApplicationPolicy | null;
  program_type: string;       // "scholarship" | "support" | etc.
  source_type: string;
  source_id: string | null;
  title: string;
  /** `title` without the administrative prefix (server mazimi_core::title_display). Absent on older servers and /my/saved rows; use displayTitle(). */
  title_display?: string | null;
  summary: string | null;
  /** Server-cleaned summary (leading bullets removed, items joined by \n). Absent on older servers. */
  summary_display?: string | null;
  summary_lines?: string[] | null;
  provider_name: string | null;
  /** The brand behind a corporate offer (fallback when provider_name is empty). */
  company_name?: string | null;
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
  /** Earliest of deadline_at, application_end_at and benefit_valid_until (detail and saved list only). */
  effective_deadline?: string | null;
  is_active: boolean;
  last_synced_at: string | null;
  created_at: string;
  updated_at: string;
  /** youth_fit verdict (null/absent = not computed yet). A demoted row (not
   *  "fit") leaves the default list but stays searchable; show its label. */
  youth_fit?: "fit" | "off_target" | "business_only" | "child_only" | "disability_only" | "veteran_only" | null;
  /** Neutral card label of a demoted row, e.g. "청년 대상이 아닐 수 있음". */
  youth_fit_label?: string | null;
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
  items: ApiProgram[];
}

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
  /** See ApiProgram.title_display. */
  title_display?: string | null;
  program_type: string;
  match_score: number;
  benefit_amount_monthly: number | null;
  benefit_amount_semester: number | null;
  deadline: string | null;
  reasons: string[];
  missing_checks: string[];
  official_url: string | null;
}

/** Why a considered programme was left out of the list (server label included). */
export type ExclusionReason =
  | "not_open"
  | "age"
  | "region"
  | "school"
  | "enrollment"
  | "scope_unconfirmed"
  | "not_individual"
  | "target_group"
  | "hard_rule"
  | "rule_unavailable"
  /** Not recognisably youth-targeted: left out of the youth list only, still in "all". */
  | "outside_youth_scope";

/** `youth` (default): youth-relevant rows. `all`: every row the user is not excluded from. */
export type RecommendScope = "youth" | "all";

/** Counts only: never the titles of excluded programmes. */
export interface FilterSummary {
  considered: number;
  included: number;
  included_with_unknowns: number;
  excluded: number;
  excluded_by_reason: { reason: ExclusionReason; label: string; count: number }[];
}

/** POST /api/v1/recommend/preview response */
export interface RecommendationResult {
  total_available: number;
  estimated_monthly: number;
  estimated_semester: number;
  items: RecommendationItem[];
  /** Absent on servers older than the grounded-explanations release. */
  filter_summary?: FilterSummary;
  /** The scope this list was built with. Absent on older servers. */
  scope?: RecommendScope;
  /** Items with verified = true (listed first). Absent on older servers. */
  verified_count?: number;
  /** Server flag: clients may show the 조건 확인됨 / 확인 필요 sections. */
  verified_section_enabled?: boolean;
}

/** POST /api/v1/recommend/preview request body — matches Rust ProfileInput */
export interface ProfileInput {
  /** Sent by onboarding saves; the server then requires one (null = none given). */
  nickname?: string | null;
  birth_year: number;
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
}

/** GET /api/v1/alerts response (user identity from JWT) */
export interface AlertListResponse {
  total: number;
  limit: number;
  offset: number;
  items: AlertItem[];
}

/** GET /api/v1/profile response (user identity from JWT) */
export interface UserProfileResponse {
  user_id: string;
  profile: UserProfile;
}

export interface UserProfile {
  user_id: string;
  /** The member's nickname (onboarding, or the Kakao name). */
  nickname?: string | null;
  birth_year: number | null;
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
  program_id: string;
  program_title: string;
  current_status: ApplicationStatus;
  /** One status for every client; both inputs are self-reported, never institution-verified. */
  display_status: ApplicationDisplayStatus;
  display_label: string;
  memo: string | null;
  applied_at: string | null;
  result_at: string | null;
  updated_at: string;
  history: StateHistoryEntry[];
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

/** Alert preference payload for POST /api/v1/alerts/preferences */
export interface AlertPreferences {
  program_id: string;
  enabled: boolean;
}

/** GET/PUT /api/v1/alerts/notification-preferences (user identity from JWT) */
export interface NotificationPreferences {
  deadline: boolean;
  new_program: boolean;
  profile_update: boolean;
  channels: { in_app: boolean; push: boolean; kakao: boolean; email: boolean };
}

// PROGRAM_TYPES and programTypeLabel live in ./displayLabels (shared with mobile).

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/** A won amount: `5만원` for whole 만원, otherwise `5,000원` / `15,000원`. */
export function formatWon(amount: number): string {
  if (amount >= 10000 && amount % 10000 === 0) return `${amount / 10000}만원`;
  return `${amount.toLocaleString("ko-KR")}원`;
}

/**
 * Whether the programme states a support amount to show. A corporate offer
 * never does: its discount is in the official sentence, not an amount.
 */
export function hasBenefitAmount(program: ApiProgram): boolean {
  if (program.program_type === "corporate_benefit") return false;
  return Boolean(
    program.benefit_amount_monthly || program.benefit_amount_semester || program.benefit_amount_once,
  );
}

/** Format a benefit amount into a human-readable Korean label. */
export function formatBenefit(program: ApiProgram): string {
  if (!hasBenefitAmount(program)) return "혜택 확인";
  if (program.benefit_amount_monthly) {
    return `월 ${formatWon(program.benefit_amount_monthly)}`;
  }
  if (program.benefit_amount_semester) {
    return `학기 ${formatWon(program.benefit_amount_semester)}`;
  }
  if (program.benefit_amount_once) {
    return `최대 ${formatWon(program.benefit_amount_once)}`;
  }
  return "혜택 확인";
}


// ---------------------------------------------------------------------------
// API methods
// All protected endpoints derive the user identity from the JWT on the server.
// No user_id is sent in request bodies or query params for authenticated routes.
// ---------------------------------------------------------------------------

/** `GET /api/v1/auth/me` — the signed-in account. */
export interface SessionUser {
  id: string;
  email: string | null;
  nickname: string | null;
  image: string | null;
  role: string;
  profile_complete: boolean;
}

export const api = {
  // ── Session ───────────────────────────────────────────────────────────────

  getMe: (signal?: AbortSignal): Promise<SessionUser> =>
    sessionProbe<SessionUser>("/api/v1/auth/me", { signal }),

  // ── Programs ──────────────────────────────────────────────────────────────

  /**
   * Signed-in viewers go through the BFF so the server can keep their own
   * youth_fit tiers (self-employed, likely parent, disability, veteran) in
   * the default list; a signed-out visitor gets the public list. `q`
   * searches every row, demoted ones last with their label. `signedOut`
   * (the page's shared /auth/me answer) skips the proxy, its 401 and the
   * refresh attempt for a visitor known to be signed out.
   */
  getPrograms: async (params?: {
    program_type?: string;
    region?: string;
    q?: string;
    per_page?: number;
  }, signal?: AbortSignal, signedOut = false): Promise<ProgramListResponse> => {
    const query = new URLSearchParams();
    if (params?.program_type) query.set("program_type", params.program_type);
    if (params?.region) query.set("region", params.region);
    if (params?.q?.trim()) query.set("q", params.q.trim());
    if (params?.per_page != null) query.set("per_page", String(params.per_page));
    const qs = query.toString();
    const path = `/api/v1/programs${qs ? `?${qs}` : ""}`;
    if (signedOut) return publicRequest<ProgramListResponse>(path, { signal });
    try {
      return await sessionProbe<ProgramListResponse>(path, { signal });
    } catch (error) {
      if (error instanceof ApiError && error.status === 401) {
        return publicRequest<ProgramListResponse>(path, { signal });
      }
      throw error;
    }
  },

  getProgram: (id: string): Promise<ApiProgram> =>
    publicRequest<ApiProgram>(`/api/v1/programs/${id}`),

  toggleBookmark: (programId: string): Promise<BookmarkResponse> =>
    authRequest<BookmarkResponse>(`/api/v1/programs/${programId}/bookmark`, {
      method: "POST",
    }),
  /** Idempotent: sets the bookmark and returns the state the server ended in. */
  setBookmark: (programId: string, bookmarked: boolean): Promise<BookmarkResponse> =>
    authRequest<BookmarkResponse>(`/api/v1/programs/${encodeURIComponent(programId)}/bookmark`, {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ bookmarked }),
    }),

  // ── Recommendations ───────────────────────────────────────────────────────

  getRecommendPreview: (profile: ProfileInput): Promise<RecommendationResult> =>
    publicRequest<RecommendationResult>("/api/v1/recommend/preview", {
      method: "POST",
      body: JSON.stringify(profile),
    }),

  // ── Dashboard ─────────────────────────────────────────────────────────────

  getDashboard: (): Promise<DashboardData> =>
    authRequest<DashboardData>("/api/v1/dashboard"),

  // ── Alerts ────────────────────────────────────────────────────────────────

  getAlerts: (
    opts?: { limit?: number; offset?: number }
  ): Promise<AlertListResponse> => {
    const q = new URLSearchParams();
    if (opts?.limit) q.set("limit", String(opts.limit));
    if (opts?.offset) q.set("offset", String(opts.offset));
    const qs = q.toString();
    return authRequest<AlertListResponse>(`/api/v1/alerts${qs ? `?${qs}` : ""}`);
  },

  upsertAlertPreference: (
    programId: string,
    enabled: boolean
  ): Promise<unknown> =>
    authRequest("/api/v1/alerts/preferences", {
      method: "POST",
      body: JSON.stringify({ program_id: programId, enabled }),
    }),

  getNotificationPreferences: (): Promise<NotificationPreferences> =>
    authRequest<NotificationPreferences>("/api/v1/alerts/notification-preferences"),

  saveNotificationPreferences: (
    preferences: NotificationPreferences
  ): Promise<NotificationPreferences> =>
    authRequest<NotificationPreferences>("/api/v1/alerts/notification-preferences", {
      method: "PUT",
      body: JSON.stringify(preferences),
    }),

  // ── Profile ───────────────────────────────────────────────────────────────

  getProfile: (): Promise<UserProfileResponse> =>
    authRequest<UserProfileResponse>("/api/v1/profile"),

  saveProfile: (profile: ProfileInput): Promise<UserProfileResponse> =>
    authRequest<UserProfileResponse>("/api/v1/profile", {
      method: "POST",
      body: JSON.stringify(profile),
    }),

  // ── Application status tracking ───────────────────────────────────────────

  getMyApplications: (): Promise<ApplicationListResponse> =>
    authRequest<ApplicationListResponse>("/api/v1/my/applications"),

  getApplicationStatus: async (
    programId: string
  ): Promise<ApplicationDetail | null> => {
    try {
      return await authRequest<ApplicationDetail>(
        `/api/v1/my/applications/${programId}`
      );
    } catch (err) {
      if (err instanceof ApiError && err.status === 404) {
        return null;
      }
      throw err;
    }
  },

  updateApplicationStatus: (
    programId: string,
    status: ApplicationStatus,
    memo?: string
  ): Promise<UpdateStatusResponse> =>
    authRequest<UpdateStatusResponse>(
      `/api/v1/my/applications/${programId}`,
      {
        method: "PUT",
        body: JSON.stringify({ status, memo: memo ?? null }),
      }
    ),

  // ── Saved / Bookmarks ────────────────────────────────────────────────────

  getSaved: (): Promise<{ items: SavedProgram[] }> =>
    authRequest<{ items: SavedProgram[] }>("/api/v1/my/saved"),
};

// ── Admin types ──

export interface AdminProgram {
  id: string;
  program_type: string;
  title: string;
  summary: string | null;
  provider_name: string | null;
  official_url: string | null;
  program_status: string;
  application_start_at: string | null;
  application_end_at: string | null;
  benefit_amount_monthly: number | null;
  benefit_amount_semester: number | null;
  benefit_amount_once: number | null;
  min_age: number | null;
  max_age: number | null;
  regions: string[] | null;
  deadline_at: string | null;
  benefit_valid_until?: string | null;
  is_active: boolean;
  created_at: string;
  updated_at: string;
}

export interface AdminStats {
  total_programs: number;
  active_programs: number;
  total_users: number;
  monitored_programs: number;
  source_reviews_pending: number;
  overdue_source_monitors: number;
  open_operational_events: number;
  open_data_quality_issues: number;
}

export type OperationalEventStatus =
  | "open"
  | "acknowledged"
  | "resolved";
export type OperationalEventSeverity =
  | "info"
  | "warning"
  | "error"
  | "critical";

export interface OperationalEvent {
  id: string;
  fingerprint: string;
  severity: OperationalEventSeverity;
  component: string;
  event_type: string;
  subject_type: string | null;
  subject_id: string | null;
  message: string;
  details: Record<string, unknown>;
  status: OperationalEventStatus;
  occurrence_count: number;
  first_occurred_at: string;
  last_occurred_at: string;
  acknowledged_at: string | null;
  resolved_at: string | null;
  resolution_note: string | null;
  delivery_attempt_count: number;
  delivered_at: string | null;
  last_delivery_error: string | null;
}

export type DataQualityIssueStatus = "open" | "resolved" | "ignored";

export interface DataQualityIssue {
  id: string;
  program_id: string;
  program_title: string;
  issue_code: string;
  original_value: unknown;
  status: DataQualityIssueStatus;
  resolution_note: string | null;
  first_detected_at: string;
  last_detected_at: string;
  resolved_at: string | null;
}

export interface ProgramLifecycleItem {
  id: string;
  title: string;
  provider_name: string | null;
  official_url: string | null;
  program_status: string;
  is_active: boolean;
  effective_end_at: string;
}

export interface DuplicateProgramGroup {
  match_type: "official_url" | "provider_title";
  match_key: string;
  item_count: number;
  active_count: number;
  program_ids: string[];
  titles: string[];
  providers: string[];
}

export interface SourceReviewItem {
  program_id: string;
  title: string;
  provider_name: string | null;
  source_health: string;
  source_review_due_at: string | null;
  open_hold_count: number;
}

export interface ProgramMaintenanceResponse {
  daily_jobs: { name: string; status: string; last_attempt_at: string | null; last_success_at: string | null; stale: boolean; details: Record<string, unknown> }[];
  limit: number;
  expired: { total: number; items: ProgramLifecycleItem[] };
  expiring: { total: number; items: ProgramLifecycleItem[] };
  duplicates: { total: number; items: DuplicateProgramGroup[] };
  source_reviews: { total: number; items: SourceReviewItem[] };
}

export interface AdminQueueResponse<T> {
  total: number;
  limit: number;
  offset: number;
  items: T[];
}

export type SourceCandidateStatus =
  | "queued"
  | "fetched"
  | "extracted"
  | "review"
  | "approved"
  | "change_review"
  | "unreachable_review"
  | "verification_due"
  | "rejected"
  | "error";

export interface MonitoredSourceProgram {
  program_id: string;
  title: string;
  summary: string | null;
  provider_name: string | null;
  benefit_category: string | null;
  is_active: boolean;
  program_status: string;
  source_health: string;
  verification_state: string;
  verified_at: string | null;
  review_due_at: string | null;
  change_detected_at: string | null;
  last_error: string | null;
}

export interface SourceCandidateListItem {
  id: string;
  url: string;
  domain: string;
  organization_name: string;
  discovered_via: string;
  candidate_kind: string;
  discovery_depth: number;
  discovery_query: string | null;
  page_title: string | null;
  status: SourceCandidateStatus;
  extraction_confidence: number | null;
  program_id: string | null;
  previous_content_hash: string | null;
  attempt_count: number;
  last_error: string | null;
  next_check_at: string | null;
  last_successful_check_at: string | null;
  consecutive_failures: number;
  brand_ids: string[];
  monitored_program_ids: string[];
  monitor_states: string[];
  monitored_programs: MonitoredSourceProgram[];
  discovered_at: string;
  last_checked_at: string | null;
}

export interface SourceCandidateDetail extends SourceCandidateListItem {
  parent_candidate_id: string | null;
  review_text: string | null;
  extracted_payload: Partial<ReviewSourceCandidateInput> & {
    confidence?: number;
  } | null;
  extraction_confidence: number | null;
  http_status: number | null;
  fetched_bytes: number | null;
  content_hash: string | null;
}

export interface SourceCandidateListResponse {
  items: SourceCandidateListItem[];
  total: number;
  page: number;
  per_page: number;
  has_next: boolean;
}

export type SourceReviewResolution =
  | "confirmed_unchanged"
  | "program_updated"
  | "retired";

export interface SourceReviewOutcome {
  program_id: string;
  candidate_id: string;
  resolution: SourceReviewResolution;
  source_health: string;
  is_active: boolean;
  program_status: string;
  candidate_status: string;
  next_review_at: string | null;
}

export interface ReviewSourceCandidateInput {
  decision: "approve" | "reject";
  reason?: string | null;
  title?: string | null;
  summary?: string | null;
  provider_name?: string | null;
  benefit_category?: string | null;
  program_status?: string | null;
  application_start_at?: string | null;
  application_end_at?: string | null;
  deadline_at?: string | null;
  benefit_amount_monthly?: number | null;
  benefit_amount_semester?: number | null;
  benefit_amount_once?: number | null;
  min_age?: number | null;
  max_age?: number | null;
  regions?: string[];
  tags?: string[];
  publish?: boolean;
}

// Backend list response shape for GET /api/v1/admin/programs
export interface AdminProgramListResponse {
  total: number;
  limit: number;
  offset: number;
  items: AdminProgram[];
}

export interface CreateProgramInput {
  program_type: string;
  title: string;
  summary?: string;
  provider_name?: string;
  official_url?: string;
  benefit_amount_monthly?: number;
  benefit_amount_semester?: number;
  benefit_amount_once?: number;
  min_age?: number;
  max_age?: number;
  regions?: string[];
  deadline_at?: string;
  benefit_valid_until?: string;
  application_start_at?: string;
  application_end_at?: string;
  is_active?: boolean;
}

export interface UpdateProgramInput {
  program_type?: string;
  title?: string;
  summary?: string;
  provider_name?: string;
  official_url?: string;
  benefit_amount_monthly?: number;
  benefit_amount_semester?: number;
  benefit_amount_once?: number;
  min_age?: number;
  max_age?: number;
  regions?: string[];
  deadline_at?: string;
  benefit_valid_until?: string;
  application_start_at?: string;
  application_end_at?: string;
  is_active?: boolean;
}

/** Admin read-only view of AI agent runs: metadata only, no goals, messages or user ids. */
export interface AssistantRunAudit {
  items: {
    id: string; status: string; planner: string | null; goal_kind: string | null; model: string | null;
    latency_ms: number | null; error_code: string | null; created_at: string;
    tools: { seq: number; tool: string; permission: string; status: string; latency_ms: number | null; error_code: string | null }[];
    confirmations: number; executed: number;
  }[];
  last_7_days: { status: string; count: number }[];
}

export const adminApi = {
  listApplicationResources: (id: string) => authRequest<ApplicationResourceReviewList>(`/api/v1/admin/programs/${id}/application-resources`),
  queueApplicationResource: (id: string, input: {kind: string;url: string;source_url: string;label: string}) =>
    authRequest<AdminApplicationResource>(`/api/v1/admin/programs/${id}/application-resources`, {method:'POST',body:JSON.stringify(input)}),
  reviewApplicationResource: (id: string, input: {decision:string;expected_revision:number;program_fingerprint:string;access:string;same_program_confirmed:boolean;current_intake_confirmed:boolean;destination_confirmed:boolean;note:string;validity_hours:number}) =>
    authRequest<AdminApplicationResource>(`/api/v1/admin/application-resources/${id}`, {method:'PUT',body:JSON.stringify(input)}),
  getFormTemplate: (resourceId: string) => authRequest<AdminFormTemplateView>(`/api/v1/admin/application-resources/${resourceId}/form-template`),
  saveFormTemplate: (resourceId: string, template: FormTemplateDefinition, expectedRevision: number | null) =>
    authRequest<{revision: number}>(`/api/v1/admin/application-resources/${resourceId}/form-template`, {method:'PUT',body:JSON.stringify({template,expected_revision:expectedRevision})}),
  deleteFormTemplate: (resourceId: string) => authRequest<void>(`/api/v1/admin/application-resources/${resourceId}/form-template`, {method:'DELETE'}),
  getAiExtraction: (id: string) => authRequest<AiExtractionReview>(`/api/v1/admin/programs/${id}/ai-extraction`),
  reviewAiExtraction: (id: string, input: {decisions: AiExtractionDecision[]; note: string}) =>
    authRequest<{program_id: string; eligibility_rule_version: number | null}>(`/api/v1/admin/programs/${id}/ai-extraction/review`, {method:'POST',body:JSON.stringify(input)}),
  listAiExtractionQueue: (status: AiExtractionQueueFilter) =>
    authRequest<{items: AiExtractionQueueItem[]}>(`/api/v1/admin/ai-extraction/candidates?status=${encodeURIComponent(status)}&limit=200`),
  listAssistantRuns: (limit = 100) =>
    authRequest<AssistantRunAudit>(`/api/v1/admin/assistant/runs?limit=${limit}`),
  listAiExtractionAutoRuns: () => authRequest<{runs: AiExtractionAutoRun[]}>("/api/v1/admin/ai-extraction/auto-runs"),
  confirmAiExtractionAudit: (candidateId: string, note: string) =>
    authRequest<{candidate_id: string}>(`/api/v1/admin/ai-extraction/candidates/${candidateId}/audit`, {method:'POST',body:JSON.stringify({note})}),
  revertAiExtraction: (input: AiExtractionRevertInput) =>
    authRequest<AiExtractionRevertReport>("/api/v1/admin/ai-extraction/revert", {method:'POST',body:JSON.stringify(input)}),
  getStats: () => authRequest<AdminStats>("/api/v1/admin/stats"),

  triggerSync: () =>
    authRequest<{ status: string; message: string }>("/api/v1/admin/sync", {
      method: "POST",
    }),

  listPrograms: (params?: {
    limit?: number;
    offset?: number;
  }): Promise<AdminProgramListResponse> => {
    const query = new URLSearchParams();
    if (params?.limit != null) query.set("limit", String(params.limit));
    if (params?.offset != null) query.set("offset", String(params.offset));
    const qs = query.toString();
    return authRequest<AdminProgramListResponse>(
      `/api/v1/admin/programs${qs ? `?${qs}` : ""}`
    );
  },

  // Admin route: no visibility filter, so inactive and expired programs load.
  getProgram: (id: string) =>
    authRequest<AdminProgram>(`/api/v1/admin/programs/${encodeURIComponent(id)}`),

  createProgram: (input: CreateProgramInput) =>
    authRequest<AdminProgram>("/api/v1/admin/programs", {
      method: "POST",
      body: JSON.stringify(input),
    }),

  updateProgram: (id: string, input: UpdateProgramInput) =>
    authRequest<AdminProgram>(`/api/v1/admin/programs/${id}`, {
      method: "PUT",
      body: JSON.stringify(input),
    }),

  togglePublish: (id: string) =>
    authRequest<{ id: string; is_active: boolean }>(
      `/api/v1/admin/programs/${id}/publish`,
      { method: "POST" }
    ),

  listSourceCandidates: (params?: {
    status?: SourceCandidateStatus;
    page?: number;
    per_page?: number;
  }): Promise<SourceCandidateListResponse> => {
    const query = new URLSearchParams();
    if (params?.status) query.set("status", params.status);
    if (params?.page != null) query.set("page", String(params.page));
    if (params?.per_page != null) {
      query.set("per_page", String(params.per_page));
    }
    const qs = query.toString();
    return authRequest<SourceCandidateListResponse>(
      `/api/v1/admin/benefit-candidates${qs ? `?${qs}` : ""}`
    );
  },

  getSourceCandidate: (id: string): Promise<SourceCandidateDetail> =>
    authRequest<SourceCandidateDetail>(
      `/api/v1/admin/benefit-candidates/${id}`
    ),

  reviewSourceCandidate: (
    id: string,
    input: ReviewSourceCandidateInput
  ): Promise<{
    id: string;
    status: string;
    program_id?: string;
    published?: boolean;
  }> =>
    authRequest(
      `/api/v1/admin/benefit-candidates/${id}/review`,
      {
        method: "PUT",
        body: JSON.stringify(input),
      }
    ),

  reviewProgramSource: (
    programId: string,
    input: {
      candidate_id: string;
      resolution: SourceReviewResolution;
      note?: string | null;
    }
  ): Promise<SourceReviewOutcome> =>
    authRequest<SourceReviewOutcome>(
      `/api/v1/admin/programs/${programId}/source-verification`,
      {
        method: "PUT",
        body: JSON.stringify(input),
      }
    ),

  listOperationalEvents: (params?: {
    status?: OperationalEventStatus;
    severity?: OperationalEventSeverity;
    limit?: number;
    offset?: number;
  }): Promise<AdminQueueResponse<OperationalEvent>> => {
    const query = new URLSearchParams();
    if (params?.status) query.set("status", params.status);
    if (params?.severity) query.set("severity", params.severity);
    if (params?.limit != null) query.set("limit", String(params.limit));
    if (params?.offset != null) query.set("offset", String(params.offset));
    const qs = query.toString();
    return authRequest(
      `/api/v1/admin/operational-events${qs ? `?${qs}` : ""}`
    );
  },

  reviewOperationalEvent: (
    id: string,
    input: {
      action: "acknowledge" | "resolve" | "reopen";
      note?: string | null;
    }
  ): Promise<OperationalEvent> =>
    authRequest<OperationalEvent>(`/api/v1/admin/operational-events/${id}`, {
      method: "PUT",
      body: JSON.stringify(input),
    }),

  listDataQualityIssues: (params?: {
    status?: DataQualityIssueStatus;
    limit?: number;
    offset?: number;
  }): Promise<AdminQueueResponse<DataQualityIssue>> => {
    const query = new URLSearchParams();
    if (params?.status) query.set("status", params.status);
    if (params?.limit != null) query.set("limit", String(params.limit));
    if (params?.offset != null) query.set("offset", String(params.offset));
    const qs = query.toString();
    return authRequest(
      `/api/v1/admin/data-quality-issues${qs ? `?${qs}` : ""}`
    );
  },

  getProgramMaintenance: (limit = 50): Promise<ProgramMaintenanceResponse> =>
    authRequest<ProgramMaintenanceResponse>(
      `/api/v1/admin/program-maintenance?limit=${encodeURIComponent(limit)}`
    ),

  reviewDataQualityIssue: (
    id: string,
    input: {
      action: "recheck" | "ignore" | "reopen";
      note?: string | null;
    }
  ): Promise<{ id: string; status: DataQualityIssueStatus }> =>
    authRequest(`/api/v1/admin/data-quality-issues/${id}`, {
      method: "PUT",
      body: JSON.stringify(input),
    }),
};

export interface AdminApplicationResource {
  id: string; kind: 'notice'|'application'|'template'; url: string; source_url: string; label: string;
  status: string; access: string; file_format: string|null; byte_size: number|null; content_hash: string|null;
  check_error: string|null; checked_at: string|null; verified_at: string|null; valid_until: string|null; revision: number;
  program_fingerprint: string;
}
export interface ApplicationResourceReviewList {
  program_title: string; program_fingerprint: string; items: AdminApplicationResource[];
}

/** 서류 작성 도우미 input boxes for a verified PDF template. Positions only; never user answers.
 * Mirrors crates/core/src/form_templates.rs. Rect = fractions of the page crop box, origin top-left. */
export type FormFieldType = 'text'|'multiline'|'date'|'checkbox'|'signature'|'stamp';
export interface FormTemplateField {
  id: string; page: number; rect: {x: number; y: number; w: number; h: number}; type: FormFieldType; label: string;
  mapping: string|null; required: boolean; format?: string; font_size?: number; acro_name?: string;
  /** FormTemplateGroup id (checkboxes only). */
  group?: string;
}
/** Checkbox options answering one question: single = one tick (radio), multi = any number. */
export interface FormTemplateGroup { id: string; mode: 'single'|'multi'; label: string }
export interface FormTemplateDefinition { version: 1; content_hash: string; page_count: number; source: 'admin'|'acroform'; fields: FormTemplateField[]; groups?: FormTemplateGroup[] }
export interface AdminFormTemplateView {
  resource: {id: string; kind: string; file_format: string|null; content_hash: string|null; label: string; url: string};
  template: FormTemplateDefinition|null; revision: number|null; updated_at: string|null; stale: boolean;
}

/** A-09 AI extraction review (admin only). Offsets are Unicode code-point
 * offsets into `source_text`; slice with `Array.from`, not UTF-16 indices. */
export interface AiExtractionEvidence {
  quote: string; start: number; end: number; source_hash: string; offset_source: "model" | "server_anchored";
}
export interface AiExtractionChange { target: string; current: unknown; proposed: unknown; overwrites: boolean }
export interface AiExtractionPlan { changes: AiExtractionChange[]; notes: string[]; blockers: string[] }
/** The rule parser's independent reading of the evidence quote. `hold` is
 * set when it agreed but the fact was not applied automatically. */
export interface AiExtractionAutoCheck {
  verdict: "agree" | "disagree" | "no_parse"; parser_version: string;
  rule: string | null; parsed: unknown; reason: string | null; hold?: string;
}
/** Provenance shared by the program panel and the cross-program queue. */
export interface AiExtractionAutoFields {
  auto_check: AiExtractionAutoCheck | null; applied_by: "reviewer" | "auto_verified" | null;
  auto_rule: string | null; auto_parser_version: string | null; auto_run_id: string | null;
  audit_sample: boolean; audit_reviewed_at: string | null;
}
export type AiExtractionStatus = "pending" | "flagged" | "invalid" | "approved" | "rejected" | "superseded" | "reverted";
export interface AiExtractionCandidate extends AiExtractionAutoFields {
  id: string; cache_id: string; source_hash: string; stale: boolean;
  fact_type: string; fact_key: string; value: Record<string, unknown>;
  evidence: AiExtractionEvidence | null; confidence: "HIGH" | "MEDIUM" | "LOW" | null;
  status: AiExtractionStatus;
  validation: { errors: string[]; flags: string[] };
  model: string; prompt_version: string; reviewer_id: string | null; reviewed_at: string | null;
  review_note: string | null; applied_changes: unknown; created_at: string; plan: AiExtractionPlan | null;
  reverted_at: string | null; revert_note: string | null;
}
/** `pending` is the exception queue (not auto-applied); `audit` is the
 * random sample of auto-applied facts still awaiting a human. */
export type AiExtractionQueueFilter = "pending" | "flagged" | "auto_applied" | "audit" | "reverted";
export interface AiExtractionQueueItem extends AiExtractionAutoFields {
  id: string; program_id: string; program_title: string; fact_type: string; fact_key: string;
  value: Record<string, unknown>; evidence: AiExtractionEvidence | null;
  confidence: "HIGH" | "MEDIUM" | "LOW" | null; status: AiExtractionStatus;
  validation: { errors: string[]; flags: string[] }; model: string;
  reviewed_at: string | null; created_at: string;
}
export interface AiExtractionAutoRun {
  id: string; trigger: "nightly" | "queue" | "manual"; status: string; model: string;
  started_at: string; finished_at: string | null; auto_verified: number; auto_applied: number;
  auto_held: number; auto_cap_skips: number; audit_sampled: number;
  still_applied: number; reverted: number; audit_open: number;
}
export interface AiExtractionRevertInput {
  run_id?: string; parser_version?: string; rule?: string; candidate_id?: string;
  note: string; dry_run?: boolean;
}
export interface AiExtractionRevertReport {
  matched: number; reverted: string[]; dry_run: boolean; truncated: boolean;
  skipped: { candidate_id: string; reason: string }[];
  kept: { candidate_id: string; target: string; reason: string }[];
}
export interface AiExtractionSource {
  id: string; source_hash: string; source_text: string; provider: string; model: string;
  prompt_version: string; created_at: string; stale: boolean;
}
export interface AiExtractionReview {
  program: { id: string; title: string }; current_source_hash: string;
  sources: AiExtractionSource[]; candidates: AiExtractionCandidate[];
}
export interface AiExtractionDecision { candidate_id: string; decision: "approve" | "reject"; confirm_overwrite: boolean }
