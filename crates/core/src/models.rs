use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

// ── Users ──

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct User {
    pub id: Uuid,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub auth_provider: String,
    pub auth_provider_id: Option<String>,
    pub role: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// ── User Profiles ──

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct UserProfile {
    pub user_id: Uuid,
    pub birth_year: Option<i32>,
    pub region_code: Option<String>,
    pub city_code: Option<String>,
    pub school_name: Option<String>,
    pub school_year: Option<i32>,
    pub enrollment_status: Option<String>,
    pub employment_status: Option<String>,
    pub major_group: Option<String>,
    pub income_bracket: Option<i32>,
    pub kosaf_support_bracket: Option<i32>,
    pub housing_type: Option<String>,
    pub household_size: Option<i32>,
    /// Group flags, three-state at rest (migration 20261112000002): `None` =
    /// not answered, `Some(false)` = answered "no". Read from the `*_answer`
    /// columns; the old bool columns of the same name are kept for an older
    /// image only (expand/contract) and are never read here.
    #[sqlx(rename = "has_disability_answer")]
    pub has_disability: Option<bool>,
    #[sqlx(rename = "is_multicultural_family_answer")]
    pub is_multicultural_family: Option<bool>,
    #[sqlx(rename = "is_low_income_household_answer")]
    pub is_low_income_household: Option<bool>,
    #[sqlx(rename = "veteran_family_answer")]
    pub veteran_family: Option<bool>,
    pub preferred_categories: Vec<String>,
    pub school_type: Option<String>,
    pub age_band: Option<String>,
    pub profile_version: i32,
    pub updated_at: DateTime<Utc>,
    pub nickname: Option<String>,
    pub profile_image_url: Option<String>,
    /// Optional answers (migration 20261022000201). `None` = not answered.
    /// Sensitive: never logged.
    #[sqlx(default)]
    pub homeless_household: Option<bool>,
    #[sqlx(default)]
    pub marital_status: Option<String>,
    #[sqlx(default)]
    pub newlywed: Option<bool>,
    #[sqlx(default)]
    pub children_count: Option<i32>,
    #[sqlx(default)]
    pub income_median_band: Option<String>,
    /// Date of birth (migration 20261023000001). When set, `birth_year` is
    /// its year and age checks use the exact 만 나이. `None` = only the
    /// birth year is known. Personal data: never logged.
    #[sqlx(default)]
    #[serde(default)]
    pub birth_date: Option<NaiveDate>,
    /// Holder answers (migration 20261111000102, [`crate::holder`]): mobile
    /// carrier, card issuers held, pay apps used. `None` = not answered; an
    /// empty list is the answer "none". No client asks them yet.
    #[sqlx(default)]
    #[serde(default)]
    pub telecom_carrier: Option<String>,
    #[sqlx(default)]
    #[serde(default)]
    pub card_issuers: Option<Vec<String>>,
    #[sqlx(default)]
    #[serde(default)]
    pub pay_apps: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileInput {
    /// Birth year. May be omitted when `birth_date` is sent: the profile
    /// validation then derives it (0 = not sent until then).
    #[serde(default)]
    pub birth_year: i32,
    /// Date of birth, when the client asks for it. Takes precedence over
    /// `birth_year` for age checks (exact 만 나이).
    #[serde(default)]
    pub birth_date: Option<NaiveDate>,
    pub region_code: String,
    pub city_code: Option<String>,
    pub school_name: Option<String>,
    pub school_year: Option<i32>,
    pub enrollment_status: Option<String>,
    pub employment_status: Option<String>,
    pub major_group: Option<String>,
    pub income_bracket: Option<i32>,
    pub kosaf_support_bracket: Option<i32>,
    pub housing_type: Option<String>,
    pub household_size: Option<i32>,
    pub has_disability: Option<bool>,
    pub is_multicultural_family: Option<bool>,
    pub is_low_income_household: Option<bool>,
    pub veteran_family: Option<bool>,
    pub preferred_categories: Option<Vec<String>>,
    pub school_type: Option<String>,
    pub age_band: Option<String>,
    /// The user's own household owns no home (무주택). Optional.
    #[serde(default)]
    pub homeless_household: Option<bool>,
    /// `unmarried` | `married`. Optional.
    #[serde(default)]
    pub marital_status: Option<String>,
    /// Married within 7 years; only meaningful when `married`. Optional.
    #[serde(default)]
    pub newlywed: Option<bool>,
    /// Number of children. Optional.
    #[serde(default)]
    pub children_count: Option<i32>,
    /// 기준 중위소득 band ([`crate::rule_engine::INCOME_MEDIAN_BANDS`]) or
    /// `unknown` (모름). Optional; never computed for the user.
    #[serde(default)]
    pub income_median_band: Option<String>,
    /// Holder answers ([`crate::holder`]). Optional; a save that omits one
    /// keeps the stored answer (installed app builds do not know them).
    #[serde(default)]
    pub telecom_carrier: Option<String>,
    #[serde(default)]
    pub card_issuers: Option<Vec<String>>,
    #[serde(default)]
    pub pay_apps: Option<Vec<String>>,
}

// ── Eligibility Rules ──

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct EligibilityRule {
    pub id: Uuid,
    pub program_id: Uuid,
    /// DSL tree: `{ "all": [...] }` or `{ "any": [...] }` nodes
    pub rule_json: serde_json::Value,
    /// Quick hard-filter clauses evaluated before the full rule tree
    pub hard_filter_json: Option<serde_json::Value>,
    /// Template strings used to build human-readable match explanations
    pub explain_json: Option<serde_json::Value>,
    pub version: i32,
    pub created_by: Option<String>,
    pub reviewed_by: Option<String>,
    pub compiled_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// ── Program Documents ──

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct ProgramDocument {
    pub id: Uuid,
    pub program_id: Uuid,
    pub document_name: String,
    pub description: Option<String>,
    pub is_required: bool,
    pub sort_order: i32,
}

// ── Programs ──

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Program {
    pub id: Uuid,
    pub program_type: String,
    pub source_type: String,
    pub source_id: Option<String>,
    pub title: String,
    pub summary: Option<String>,
    pub provider_name: Option<String>,
    pub official_url: Option<String>,
    pub program_status: String,
    pub application_start_at: Option<DateTime<Utc>>,
    pub application_end_at: Option<DateTime<Utc>>,
    pub benefit_amount_monthly: Option<i32>,
    pub benefit_amount_semester: Option<i32>,
    pub benefit_amount_once: Option<i32>,
    pub region_scope: Option<serde_json::Value>,
    pub school_scope: Option<serde_json::Value>,
    pub tags: Option<serde_json::Value>,
    /// Provenance payloads are retained for service-side audit/debugging only.
    /// They may contain upstream fields or source text that must never be
    /// republished through a public or authenticated client API.
    #[serde(skip_serializing)]
    pub raw_payload: Option<serde_json::Value>,
    #[serde(skip_serializing)]
    pub normalized_payload: Option<serde_json::Value>,
    pub min_age: Option<i32>,
    pub max_age: Option<i32>,
    pub regions: Option<Vec<String>>,
    pub deadline_at: Option<DateTime<Utc>>,
    /// Corporate/campus offers expire by validity rather than by an
    /// application window. Serialized like the other two deadline columns so
    /// every client sees the same three inputs of [`Program::effective_deadline`].
    /// An explicit column list that omits it reads as NULL.
    #[sqlx(default)]
    pub benefit_valid_until: Option<DateTime<Utc>>,
    pub is_active: bool,
    pub last_synced_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    // ── Corporate benefit fields (nullable for non-corporate programs) ──
    pub company_name: Option<String>,
    pub company_logo_url: Option<String>,
    pub benefit_category: Option<String>,
    /// Who originates the benefit. This is orthogonal to program_type.
    pub provider_kind: Option<String>,
    // ── Application guide ──
    pub application_steps: Option<serde_json::Value>,
    /// 신청 방법 안내 텍스트 (원문, 온통청년 plcyAplyMthdCn 등)
    pub application_method: Option<String>,
    /// 제출 서류 안내 텍스트 (원문, 온통청년 sbmsnDcmntCn 등)
    pub submission_documents: Option<String>,
    /// 심사 방법 안내 텍스트 (원문, 온통청년 srngMthdCn 등)
    pub screening_method: Option<String>,
    /// 5-digit 시군구 codes the programme is limited to (`programs.region_districts`).
    /// `None` = not collected (no district check); `Some([])` = sido-wide.
    /// A query that does not select the column reads `None`.
    #[sqlx(default)]
    #[serde(default)]
    pub region_districts: Option<Vec<String>>,
    /// What the user must hold for an issuer-paid offer, verbatim from the
    /// issuer page (`programs.eligibility_condition`). Shown with the offer
    /// whenever it is set; a query that does not select it reads `None`.
    #[sqlx(default)]
    #[serde(default)]
    pub eligibility_condition: Option<String>,
    /// The holder requirement of an issuer-paid offer, `field:value`
    /// (the `current_programs.holder_requirement` column, [`crate::holder`]).
    /// `None` for every offer whose sponsor carries no holder key.
    #[sqlx(default)]
    #[serde(skip)]
    pub holder_requirement: Option<String>,
    /// The day the notice counts age on, when it states one
    /// (`programs.age_basis`, [`NoticeAgeBasis`]); `None` = not stated. A
    /// query that does not select it reads `None`.
    #[sqlx(default)]
    #[serde(skip)]
    pub age_basis: Option<String>,
}

/// The day a notice counts 만 나이 on, as the worker stores it in
/// `programs.age_basis` (migration 20261112000051): `window`, `year_start`,
/// `year_end` or a `YYYY-MM-DD` day. The one reader of that column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeAgeBasis {
    /// The application day (the notice says so; the same as not stated).
    Window,
    /// 1 January of the notice's year.
    YearStart,
    /// 31 December of the notice's year (연 나이).
    YearEnd,
    /// That day ("2026. 3. 2. 기준").
    On(NaiveDate),
}

impl NoticeAgeBasis {
    /// The stored form.
    pub fn as_stored(&self) -> String {
        match self {
            Self::Window => "window".into(),
            Self::YearStart => "year_start".into(),
            Self::YearEnd => "year_end".into(),
            Self::On(day) => day.format("%Y-%m-%d").to_string(),
        }
    }

    /// Reads a stored value; anything else is `None` (not stated).
    pub fn parse(stored: &str) -> Option<Self> {
        match stored.trim() {
            "window" => Some(Self::Window),
            "year_start" => Some(Self::YearStart),
            "year_end" => Some(Self::YearEnd),
            day => NaiveDate::parse_from_str(day, "%Y-%m-%d")
                .ok()
                .map(Self::On),
        }
    }
}

/// `programs.program_status` values that mean "accepting applications or
/// usable now". `upcoming`, `closed` and `review_required` are not.
pub const OPEN_PROGRAM_STATUSES: &[&str] = &["open", "active", "always_open"];

/// The groups a source's own codes limit a programme to (보조금24
/// `supportConditions` JA03xx/JA04xx, 온통청년 `sbizCd`), as canonical keys.
/// The domain of the `target_group` rule field: no profile answers it, so a
/// condition on it is always `Unknown` with "지원 대상" as the check. The
/// worker's code tables map every code into this list (tested there).
pub const TARGET_GROUPS: &[&str] = &[
    "prospective_parent",
    "pregnant",
    "childbirth_adoption",
    "farmer",
    "fisher",
    "livestock_farmer",
    "forester",
    "elementary_student",
    "middle_school_student",
    "high_school_student",
    "university_student",
    "worker",
    "job_seeker",
    "disabled",
    "veteran",
    "patient",
    "multicultural",
    "north_korean_defector",
    "single_parent_or_grandparent",
    "single_person",
    "multi_child",
    "homeless_household",
    "new_resident",
    "extended_family",
    "sme_worker",
    "women",
    "basic_livelihood_recipient",
    "soldier",
    "local_talent",
];

/// `gender` rule values. No profile answers the field.
pub const GENDERS: &[&str] = &["male", "female"];

impl Program {
    /// The moment the program stops being available: the earliest of the
    /// explicit deadline, the application window end and the benefit
    /// validity. Scoring, ordering and display read this instead of
    /// `deadline_at`, which collected programs never populate.
    pub fn effective_deadline(&self) -> Option<DateTime<Utc>> {
        [
            self.deadline_at,
            self.application_end_at,
            self.benefit_valid_until,
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// The Korean calendar day of [`Self::effective_deadline`]
    /// ([`crate::deadline_date`]).
    pub fn deadline_date(&self) -> Option<NaiveDate> {
        self.effective_deadline().map(crate::deadline_date)
    }

    /// Rust twin of [`crate::currently_available_sql`]: published, in an open
    /// status, started and before the effective deadline at `now`. The two
    /// must change together; `routes`, `services` and the worker use one or
    /// the other depending on whether the row is already loaded.
    /// The stated age basis ([`NoticeAgeBasis::parse`] of `age_basis`).
    pub fn stated_age_basis(&self) -> Option<NoticeAgeBasis> {
        self.age_basis.as_deref().and_then(NoticeAgeBasis::parse)
    }

    pub fn is_currently_available(&self, now: DateTime<Utc>) -> bool {
        self.is_active
            && OPEN_PROGRAM_STATUSES.contains(&self.program_status.as_str())
            && self.application_start_at.is_none_or(|start| start <= now)
            && self.effective_deadline().is_none_or(|end| end > now)
    }
}

// ── Brands ──

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Brand {
    pub id: Uuid,
    pub name: String,
    pub normalized_name: String,
    pub company_name: Option<String>,
    pub industry_major: Option<String>,
    pub industry_minor: Option<String>,
    pub primary_product: Option<String>,
    pub homepage_url: Option<String>,
    pub logo_url: Option<String>,
    pub source_type: String,
    pub source_id: String,
    pub discovery_priority: i32,
    pub is_active: bool,
    pub last_synced_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// ── Bookmarks ──

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Bookmark {
    pub id: Uuid,
    pub user_id: Uuid,
    pub program_id: Uuid,
    pub created_at: DateTime<Utc>,
}

// ── User Program States ──

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct UserProgramState {
    pub id: Uuid,
    pub user_id: Uuid,
    pub program_id: Uuid,
    pub state: String,
    pub memo: Option<String>,
    pub applied_at: Option<DateTime<Utc>>,
    pub result_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// ── Recommendation Result ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecommendationItem {
    /// Eligibility is separate from ranking; absent evidence remains unknown.
    #[serde(default)]
    pub eligibility_status: String,
    pub program_id: Uuid,
    pub title: String,
    /// [`crate::title_display::title_display`] of `title`, for display.
    #[serde(default)]
    pub title_display: String,
    pub program_type: String,
    /// The programme's summary cleaned for display ([`crate::display_text`]):
    /// the card body (Figma 34065:2930). `None` when the programme has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_display: Option<String>,
    /// Provider and company, the card subtitle (Figma 34065:6301).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_name: Option<String>,
    pub match_score: i32,
    pub benefit_amount_monthly: Option<i32>,
    pub benefit_amount_semester: Option<i32>,
    pub deadline: Option<String>,
    pub reasons: Vec<String>,
    pub missing_checks: Vec<String>,
    pub official_url: Option<String>,
    /// 신청 방법 안내 텍스트 (신청 가이드 카드용)
    pub application_method: Option<String>,
    /// 제출 서류 안내 텍스트 (신청 가이드 카드용)
    pub submission_documents: Option<String>,
    /// 심사 방법 안내 텍스트 (신청 가이드 카드용)
    pub screening_method: Option<String>,
    /// Citations behind the eligibility checks of this item (age, region,
    /// enrollment, hard rule). Omitted when nothing can be cited.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<CheckEvidence>,
    /// Whether an eligibility rule of the notice was evaluated for this row
    /// and every leaf of it passed (no unknown, no failed soft condition).
    /// `false` = no condition beyond age/region/school was confirmed; clients
    /// show "조건 미확인" and `missing_checks` carries
    /// [`UNVERIFIED_CONDITIONS_LABEL`].
    ///
    /// Deprecated for eligibility claims: clients key every claim off
    /// [`Self::verified`]. Kept for old clients.
    #[serde(default)]
    pub conditions_verified: bool,
    /// "조건 확인됨": a current, complete condition review, an individual
    /// applicant, an open window and every rule `pass` with no unknown
    /// ([`crate::condition_review::assess`]). The only field a client may
    /// turn into an eligibility claim ("충족", "받을 수 있어요").
    #[serde(default)]
    pub verified: bool,
    /// What is left to check before the row can be verified, each with a
    /// fixed label. Empty when `verified`. `missing_checks` repeats the
    /// labels for old clients.
    #[serde(default)]
    pub remaining_checks: Vec<crate::condition_review::RemainingCheck>,
    /// The reviewed application window; `None` without a current review.
    #[serde(default)]
    pub window: Option<crate::condition_review::WindowView>,
    /// Provider and field tabs ([`crate::categories`]).
    #[serde(flatten, default)]
    pub categories: crate::categories::Categories,
    /// How many of the checks evaluated for this row passed
    /// ([`ConditionCounts`]).
    #[serde(flatten, default)]
    pub conditions: ConditionCounts,
}

/// The checks the server evaluated for one recommendation row: structural
/// checks (age, region, school, enrollment, district), the income check, and
/// each distinct leaf of the hard filter, ranking rule and override
/// replacements (a leaf in both rules counts once, with its worse result).
/// `total = met + unknown + failed`. Conditions only written in the notice
/// text are not counted, so `met == total` is never an eligibility claim.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ConditionCounts {
    pub conditions_total: u32,
    pub conditions_met: u32,
    pub conditions_unknown: u32,
    pub conditions_failed: u32,
}

impl ConditionCounts {
    /// Count one evaluated check.
    pub fn add(&mut self, verdict: crate::rule_engine::RuleVerdict) {
        use crate::rule_engine::RuleVerdict;
        self.conditions_total += 1;
        match verdict {
            RuleVerdict::Pass => self.conditions_met += 1,
            RuleVerdict::Unknown => self.conditions_unknown += 1,
            RuleVerdict::Fail => self.conditions_failed += 1,
        }
    }
}

/// `missing_checks` entry of a row with no rule evidence.
pub const UNVERIFIED_CONDITIONS_LABEL: &str = "자격 조건 미확인 — 공고 확인 필요";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecommendationResult {
    pub total_available: usize,
    pub estimated_monthly: i64,
    pub estimated_semester: i64,
    pub items: Vec<RecommendationItem>,
    #[serde(default)]
    pub filter_summary: FilterSummary,
    /// The scope this list was built with.
    #[serde(default)]
    pub scope: RecommendScope,
    /// Items with `verified = true` (listed first).
    #[serde(default)]
    pub verified_count: usize,
    /// `VERIFIED_SECTION_ENABLED`: whether clients may show the "조건
    /// 확인됨 / 확인 필요" sections. The fields are computed either way.
    #[serde(default)]
    pub verified_section_enabled: bool,
}

// ── Grounded explanations ──

/// Where a cited fact comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    /// Verbatim text of the collected notice (for example 온통청년 제출서류).
    OfficialText,
    /// A structured upstream field the stored value still equals; there is
    /// no sentence to quote.
    StructuredSource,
    /// A span of the notice text an operator approved as the basis of an
    /// AI-proposed fact. The quote is the notice's own text.
    AiExtractedApproved,
}

/// One citation. `quote` is always a verbatim span of stored source text and
/// is omitted rather than paraphrased when there is none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    pub kind: EvidenceKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quote: Option<String>,
    pub source_label: String,
    /// Public https link to the notice; omitted when not safe to open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    /// When the cited source text or field was last collected or checked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub captured_at: Option<DateTime<Utc>>,
    /// Operator approval time (`ai_extracted_approved` only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_at: Option<DateTime<Utc>>,
    /// The item the citation is about when the container does not say it,
    /// for example a document name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
}

/// What a citation supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceCheck {
    Age,
    Region,
    Enrollment,
    HardRule,
    ApplicationWindow,
    Documents,
}

impl EvidenceCheck {
    /// Checks a recommendation item or an eligibility explanation cites.
    pub fn is_eligibility(self) -> bool {
        matches!(
            self,
            Self::Age | Self::Region | Self::Enrollment | Self::HardRule
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckEvidence {
    pub check: EvidenceCheck,
    #[serde(flatten)]
    pub evidence: Evidence,
}

/// Why a considered programme is not in a recommendation list. Each
/// programme is counted once: not open first, then the first failing
/// structural check in check order (age, region/scope, school, enrollment),
/// then the rule tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExclusionReason {
    NotOpen,
    Age,
    Region,
    School,
    Enrollment,
    ScopeUnconfirmed,
    HardRule,
    /// The programme's stored rule cannot be evaluated (data error), so it
    /// is hidden without claiming the user does not qualify.
    RuleUnavailable,
    /// The stored target text says no individual can apply (institutions,
    /// municipalities, corporations, businesses).
    NotIndividual,
    /// Reserved for a group the profile does not place the user in (seniors,
    /// disabled people, veterans, …).
    TargetGroup,
    /// Not recognisably youth-targeted: listed only in the "전체 혜택" mode.
    OutsideYouthScope,
    /// The coded 기준 중위소득 bound is below the user's whole band
    /// ([`crate::income`]).
    Income,
}

impl ExclusionReason {
    pub const ALL: [Self; 12] = [
        Self::NotOpen,
        Self::Age,
        Self::Region,
        Self::School,
        Self::Enrollment,
        Self::Income,
        Self::ScopeUnconfirmed,
        Self::NotIndividual,
        Self::TargetGroup,
        Self::HardRule,
        Self::RuleUnavailable,
        Self::OutsideYouthScope,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::NotOpen => "아직 신청 기간이 아니에요",
            Self::Age => "나이 조건이 달라요",
            Self::Region => "거주 지역 조건이 달라요",
            Self::School => "다른 학교 전용이거나 학교 확인이 필요해요",
            Self::Enrollment => "학적 조건이 달라요",
            Self::ScopeUnconfirmed => "공고의 대상 지역이 확인되지 않았어요",
            Self::HardRule => "공고의 필수 조건이 달라요",
            Self::RuleUnavailable => "공고 조건을 확인하고 있어요",
            Self::NotIndividual => "개인이 신청하는 혜택이 아니에요",
            Self::TargetGroup => "다른 대상(어르신·장애인 등) 전용이에요",
            Self::OutsideYouthScope => "청년 대상으로 확인되지 않아 전체 혜택에서만 보여요",
            Self::Income => "소득 기준이 달라요",
        }
    }
}

/// Which rows a personal recommendation list may show.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecommendScope {
    /// Youth-relevant rows only (the default personal list).
    #[default]
    Youth,
    /// Every row the user is not excluded from ("전체 혜택").
    All,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExclusionCount {
    pub reason: ExclusionReason,
    pub label: String,
    pub count: usize,
}

/// How a recommendation list was filtered: counts only, never the titles of
/// excluded programmes. `considered = included + excluded`, and the
/// per-reason counts sum to `excluded`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilterSummary {
    /// Published catalogue rows evaluated for this request.
    pub considered: usize,
    pub included: usize,
    /// Included, but at least one condition could not be confirmed.
    pub included_with_unknowns: usize,
    pub excluded: usize,
    /// Non-zero reasons only, in [`ExclusionReason::ALL`] order.
    pub excluded_by_reason: Vec<ExclusionCount>,
}

impl FilterSummary {
    /// Build from one decision per considered programme.
    pub fn tally(decisions: impl IntoIterator<Item = Result<bool, ExclusionReason>>) -> Self {
        let mut summary = Self::default();
        let mut counts = [0usize; ExclusionReason::ALL.len()];
        for decision in decisions {
            summary.considered += 1;
            match decision {
                Ok(confirmed) => {
                    summary.included += 1;
                    if !confirmed {
                        summary.included_with_unknowns += 1;
                    }
                }
                Err(reason) => {
                    summary.excluded += 1;
                    let index = ExclusionReason::ALL
                        .iter()
                        .position(|r| *r == reason)
                        .expect("every reason is listed");
                    counts[index] += 1;
                }
            }
        }
        summary.excluded_by_reason = ExclusionReason::ALL
            .iter()
            .zip(counts)
            .filter(|(_, count)| *count > 0)
            .map(|(reason, count)| ExclusionCount {
                reason: *reason,
                label: reason.label().to_owned(),
                count,
            })
            .collect();
        summary
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use uuid::Uuid;

    fn fixed_uuid() -> Uuid {
        Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap()
    }

    fn fixed_now() -> DateTime<Utc> {
        "2026-01-01T00:00:00Z".parse().unwrap()
    }

    // ── ProfileInput ──

    #[test]
    fn profile_input_required_fields() {
        let input = ProfileInput {
            birth_year: 2000,
            birth_date: None,
            region_code: "busan".to_string(),
            city_code: None,
            school_name: None,
            school_year: None,
            enrollment_status: None,
            employment_status: None,
            major_group: None,
            income_bracket: None,
            kosaf_support_bracket: None,
            housing_type: None,
            household_size: None,
            has_disability: None,
            is_multicultural_family: None,
            is_low_income_household: None,
            veteran_family: None,
            preferred_categories: None,
            school_type: None,
            age_band: None,
            homeless_household: None,
            marital_status: None,
            newlywed: None,
            children_count: None,
            income_median_band: None,
            telecom_carrier: None,
            card_issuers: None,
            pay_apps: None,
        };
        assert_eq!(input.birth_year, 2000);
        assert_eq!(input.region_code, "busan");
        assert!(input.income_bracket.is_none());
        assert!(input.has_disability.is_none());
    }

    #[test]
    fn profile_input_roundtrip_json() {
        let input = ProfileInput {
            birth_year: 1999,
            birth_date: None,
            region_code: "seoul".to_string(),
            city_code: Some("gangnam".to_string()),
            school_name: None,
            school_year: Some(2),
            enrollment_status: Some("enrolled".to_string()),
            employment_status: None,
            major_group: Some("engineering".to_string()),
            income_bracket: Some(3),
            kosaf_support_bracket: None,
            housing_type: Some("rental".to_string()),
            household_size: Some(2),
            has_disability: Some(false),
            is_multicultural_family: None,
            is_low_income_household: None,
            veteran_family: None,
            preferred_categories: Some(vec!["scholarship".to_string()]),
            school_type: Some("university".to_string()),
            age_band: None,
            homeless_household: None,
            marital_status: None,
            newlywed: None,
            children_count: None,
            income_median_band: None,
            telecom_carrier: None,
            card_issuers: None,
            pay_apps: None,
        };
        let json = serde_json::to_string(&input).expect("serialize");
        let decoded: ProfileInput = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded.birth_year, 1999);
        assert_eq!(decoded.region_code, "seoul");
        assert_eq!(decoded.city_code, Some("gangnam".to_string()));
        assert_eq!(decoded.income_bracket, Some(3));
    }

    // ── RecommendationItem ──

    #[test]
    fn recommendation_item_score_capped() {
        let item = RecommendationItem {
            eligibility_status: "unknown".into(),
            program_id: fixed_uuid(),
            title: "테스트 장학금".to_string(),
            title_display: String::new(),
            program_type: "scholarship".to_string(),
            summary_display: None,
            provider_name: None,
            company_name: None,
            match_score: 100,
            benefit_amount_monthly: Some(300_000),
            benefit_amount_semester: None,
            deadline: Some("2026-12-31".to_string()),
            reasons: vec!["부산 거주 조건에 맞아요.".to_string()],
            missing_checks: vec![],
            official_url: Some("https://example.com".to_string()),
            application_method: None,
            submission_documents: None,
            screening_method: None,
            evidence: vec![],
            conditions_verified: false,
            verified: false,
            remaining_checks: Vec::new(),
            window: None,
            categories: Default::default(),
            conditions: Default::default(),
        };
        assert!(item.match_score <= 100, "score must not exceed 100");
        assert_eq!(item.program_type, "scholarship");
    }

    #[test]
    fn recommendation_item_roundtrip_json() {
        let item = RecommendationItem {
            eligibility_status: "unknown".into(),
            program_id: fixed_uuid(),
            title: "청년 지원금".to_string(),
            title_display: String::new(),
            program_type: "support".to_string(),
            summary_display: None,
            provider_name: None,
            company_name: None,
            match_score: 75,
            benefit_amount_monthly: Some(200_000),
            benefit_amount_semester: None,
            deadline: None,
            reasons: vec!["현재 연령 조건에 맞아요.".to_string()],
            missing_checks: vec!["소득 기준 확인 필요".to_string()],
            official_url: None,
            application_method: None,
            submission_documents: None,
            screening_method: None,
            evidence: vec![],
            conditions_verified: false,
            verified: false,
            remaining_checks: Vec::new(),
            window: None,
            categories: Default::default(),
            conditions: Default::default(),
        };
        let json = serde_json::to_string(&item).expect("serialize");
        let decoded: RecommendationItem = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded.match_score, 75);
        assert_eq!(decoded.reasons.len(), 1);
        assert_eq!(decoded.missing_checks.len(), 1);
        assert!(decoded.deadline.is_none());
    }

    // ── RecommendationResult ──

    #[test]
    fn recommendation_result_estimated_totals() {
        let item1 = RecommendationItem {
            eligibility_status: "unknown".into(),
            program_id: fixed_uuid(),
            title: "A".to_string(),
            title_display: String::new(),
            program_type: "scholarship".to_string(),
            summary_display: None,
            provider_name: None,
            company_name: None,
            match_score: 80,
            benefit_amount_monthly: Some(200_000),
            benefit_amount_semester: None,
            deadline: None,
            reasons: vec![],
            missing_checks: vec![],
            official_url: None,
            application_method: None,
            submission_documents: None,
            screening_method: None,
            evidence: vec![],
            conditions_verified: false,
            verified: false,
            remaining_checks: Vec::new(),
            window: None,
            categories: Default::default(),
            conditions: Default::default(),
        };
        let item2 = RecommendationItem {
            eligibility_status: "unknown".into(),
            program_id: Uuid::parse_str("00000000-0000-0000-0000-000000000002").unwrap(),
            title: "B".to_string(),
            title_display: String::new(),
            program_type: "support".to_string(),
            summary_display: None,
            provider_name: None,
            company_name: None,
            match_score: 60,
            benefit_amount_monthly: Some(300_000),
            benefit_amount_semester: Some(1_000_000),
            deadline: None,
            reasons: vec![],
            missing_checks: vec![],
            official_url: None,
            application_method: None,
            submission_documents: None,
            screening_method: None,
            evidence: vec![],
            conditions_verified: false,
            verified: false,
            remaining_checks: Vec::new(),
            window: None,
            categories: Default::default(),
            conditions: Default::default(),
        };
        let result = RecommendationResult {
            total_available: 2,
            estimated_monthly: 500_000,
            estimated_semester: 1_000_000,
            items: vec![item1, item2],
            filter_summary: FilterSummary::default(),
            scope: RecommendScope::Youth,
            verified_count: 0,
            verified_section_enabled: false,
        };
        assert_eq!(result.total_available, 2);
        assert_eq!(result.estimated_monthly, 500_000);
        assert_eq!(result.estimated_semester, 1_000_000);
    }

    #[test]
    fn recommendation_result_empty() {
        let result = RecommendationResult {
            total_available: 0,
            estimated_monthly: 0,
            estimated_semester: 0,
            items: vec![],
            filter_summary: FilterSummary::default(),
            scope: RecommendScope::Youth,
            verified_count: 0,
            verified_section_enabled: false,
        };
        assert_eq!(result.total_available, 0);
        assert!(result.items.is_empty());
    }

    #[test]
    fn grounded_fields_are_additive() {
        // A response written before evidence and filter summaries existed
        // still decodes, and empty evidence is not serialized.
        let old = serde_json::json!({
            "total_available": 0, "estimated_monthly": 0, "estimated_semester": 0,
            "items": [{"eligibility_status": "unknown", "program_id": fixed_uuid(), "title": "A",
                "program_type": "policy", "match_score": 1, "benefit_amount_monthly": null,
                "benefit_amount_semester": null, "deadline": null, "reasons": [],
                "missing_checks": [], "official_url": null, "application_method": null,
                "submission_documents": null, "screening_method": null}]
        });
        let decoded: RecommendationResult = serde_json::from_value(old).unwrap();
        assert!(decoded.items[0].evidence.is_empty());
        let encoded = serde_json::to_value(&decoded.items[0]).unwrap();
        assert!(encoded.get("evidence").is_none());
    }

    #[test]
    fn filter_summary_reconciles_and_keeps_reason_order() {
        let summary = FilterSummary::tally([
            Ok(true),
            Ok(false),
            Err(ExclusionReason::HardRule),
            Err(ExclusionReason::Age),
            Err(ExclusionReason::Age),
        ]);
        assert_eq!(
            (
                summary.considered,
                summary.included,
                summary.included_with_unknowns,
                summary.excluded
            ),
            (5, 2, 1, 3)
        );
        let reasons: Vec<_> = summary
            .excluded_by_reason
            .iter()
            .map(|r| (r.reason, r.count))
            .collect();
        assert_eq!(
            reasons,
            [(ExclusionReason::Age, 2), (ExclusionReason::HardRule, 1)]
        );
        let json = serde_json::to_value(&summary).unwrap();
        assert_eq!(json["excluded_by_reason"][0]["reason"], "age");
    }

    #[test]
    fn evidence_serializes_kind_and_omits_missing_quote() {
        let item = CheckEvidence {
            check: EvidenceCheck::HardRule,
            evidence: Evidence {
                kind: EvidenceKind::StructuredSource,
                quote: None,
                source_label: "온통청년 공고 데이터".into(),
                source_url: None,
                captured_at: None,
                approved_at: None,
                subject: None,
            },
        };
        let json = serde_json::to_value(&item).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "check": "hard_rule", "kind": "structured_source", "source_label": "온통청년 공고 데이터"
            })
        );
    }

    // ── UserProfile ──

    #[test]
    fn user_profile_version_starts_at_one() {
        let profile = UserProfile {
            user_id: fixed_uuid(),
            birth_year: Some(2000),
            region_code: Some("busan".to_string()),
            city_code: None,
            school_name: None,
            school_year: None,
            enrollment_status: None,
            employment_status: None,
            major_group: None,
            income_bracket: None,
            kosaf_support_bracket: None,
            housing_type: None,
            household_size: None,
            has_disability: None,
            is_multicultural_family: None,
            is_low_income_household: None,
            veteran_family: None,
            preferred_categories: vec![],
            school_type: None,
            age_band: None,
            profile_version: 1,
            updated_at: fixed_now(),
            nickname: None,
            profile_image_url: None,
            homeless_household: None,
            marital_status: None,
            newlywed: None,
            children_count: None,
            income_median_band: None,
            birth_date: None,
            telecom_carrier: None,
            card_issuers: None,
            pay_apps: None,
        };
        assert_eq!(profile.profile_version, 1);
        assert_eq!(profile.region_code, Some("busan".to_string()));
        assert_eq!(profile.has_disability, None);
        assert!(profile.preferred_categories.is_empty());
    }

    // ── Bookmark ──

    #[test]
    fn bookmark_fields() {
        let now = fixed_now();
        let bookmark = Bookmark {
            id: fixed_uuid(),
            user_id: fixed_uuid(),
            program_id: Uuid::parse_str("00000000-0000-0000-0000-000000000002").unwrap(),
            created_at: now,
        };
        assert_eq!(bookmark.user_id, fixed_uuid());
    }
}
