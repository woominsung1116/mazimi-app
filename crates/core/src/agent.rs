//! Contract of the AI benefit agent: tools, typed arguments, the plan a
//! planner may return, run and confirmation states, and the confirmation
//! hash. No I/O here; the API executes tools and the worker purges.
//!
//! A plan never carries a programme id, URL or SQL. Programmes are named by
//! [`ProgramRef`] tokens the server resolves (`context` = the screen's
//! programme, `selected` = ids the client selected, `saved` = the user's
//! saved programmes), so a planner cannot point a tool at data it invented.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub const TOOLSET_VERSION: &str = "agent-tools-v3";
/// Steps one plan may hold. The 신청 준비 journey is seven (check, checklist,
/// vault match, form card, signature card, official page card, alert).
pub const MAX_PLAN_STEPS: usize = 8;
pub const MAX_GOAL_CHARS: usize = 300;
pub const MAX_KEYWORDS: usize = 3;
pub const MAX_KEYWORD_CHARS: usize = 12;
pub const MAX_NOTICE_QUESTION_CHARS: usize = 200;
pub const MAX_TASK_CHARS: usize = 60;
pub const MAX_DOCUMENT_CHARS: usize = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolName {
    SearchBenefits,
    CheckEligibility,
    AnswerFromNotice,
    GetPreparation,
    Compare,
    Deadlines,
    OpenFormFill,
    Bookmark,
    SetAlert,
    ToggleChecklistItem,
    RecordApplicationStatus,
    ProfilePatch,
    CheckOfficialPage,
    DocumentIssueLink,
    AddToCalendar,
    /// The programme's required documents against the user's own document
    /// list (names and statuses the client keeps; files stay on the device).
    MatchVaultDocuments,
    /// Hand off to the device form helper's signature step. The server never
    /// sees a signature.
    RequestSignature,
    /// Hand off to the official application page the resolver verified.
    /// Nothing is submitted.
    OpenOfficialApplication,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    Read,
    ConfirmWrite,
}

impl ToolName {
    pub const ALL: [ToolName; 18] = [
        ToolName::SearchBenefits,
        ToolName::CheckEligibility,
        ToolName::AnswerFromNotice,
        ToolName::GetPreparation,
        ToolName::Compare,
        ToolName::Deadlines,
        ToolName::OpenFormFill,
        ToolName::Bookmark,
        ToolName::SetAlert,
        ToolName::ToggleChecklistItem,
        ToolName::RecordApplicationStatus,
        ToolName::ProfilePatch,
        ToolName::CheckOfficialPage,
        ToolName::DocumentIssueLink,
        ToolName::AddToCalendar,
        ToolName::MatchVaultDocuments,
        ToolName::RequestSignature,
        ToolName::OpenOfficialApplication,
    ];

    /// The 신청 준비 journey's steps. Planned by the server only: never in the
    /// model's schema or prompt.
    pub const JOURNEY: [ToolName; 3] = [
        ToolName::MatchVaultDocuments,
        ToolName::RequestSignature,
        ToolName::OpenOfficialApplication,
    ];

    /// The tools a model planner may be offered (all but [`Self::JOURNEY`]).
    pub fn model_tools() -> Vec<ToolName> {
        Self::ALL
            .into_iter()
            .filter(|t| !Self::JOURNEY.contains(t))
            .collect()
    }

    /// Tools added in `agent-tools-v3`, each behind its own server flag.
    pub const V3: [ToolName; 3] = [
        ToolName::CheckOfficialPage,
        ToolName::DocumentIssueLink,
        ToolName::AddToCalendar,
    ];

    /// The toolset the frozen evaluation sets were labelled against
    /// (`agent-tools-v1`). Coverage checks on those sets use this list:
    /// a tool added later has no label there by construction.
    pub const V1: [ToolName; 11] = [
        ToolName::SearchBenefits,
        ToolName::CheckEligibility,
        ToolName::AnswerFromNotice,
        ToolName::GetPreparation,
        ToolName::Compare,
        ToolName::Deadlines,
        ToolName::OpenFormFill,
        ToolName::Bookmark,
        ToolName::SetAlert,
        ToolName::ToggleChecklistItem,
        ToolName::RecordApplicationStatus,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::SearchBenefits => "search_benefits",
            Self::CheckEligibility => "check_eligibility",
            Self::AnswerFromNotice => "answer_from_notice",
            Self::GetPreparation => "get_preparation",
            Self::Compare => "compare",
            Self::Deadlines => "deadlines",
            Self::OpenFormFill => "open_form_fill",
            Self::Bookmark => "bookmark",
            Self::SetAlert => "set_alert",
            Self::ToggleChecklistItem => "toggle_checklist_item",
            Self::RecordApplicationStatus => "record_application_status",
            Self::ProfilePatch => "profile_patch",
            Self::CheckOfficialPage => "check_official_page",
            Self::DocumentIssueLink => "document_issue_link",
            Self::AddToCalendar => "add_to_calendar",
            Self::MatchVaultDocuments => "match_vault_documents",
            Self::RequestSignature => "request_signature",
            Self::OpenOfficialApplication => "open_official_application",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.as_str() == name)
    }

    /// Every state change needs a server-issued confirmation the user
    /// approved. This is the single source of that rule. `open_form_fill`
    /// changes no server state, but opening the form helper with a draft
    /// built from the user's data is the user's call, so it is a card too.
    pub fn permission(self) -> Permission {
        match self {
            Self::OpenFormFill
            | Self::Bookmark
            | Self::SetAlert
            | Self::ToggleChecklistItem
            | Self::RecordApplicationStatus
            | Self::ProfilePatch
            | Self::AddToCalendar
            // Handoffs: no server state changes, but leaving for the
            // signature step or the official site is the user's call.
            | Self::RequestSignature
            | Self::OpenOfficialApplication => Permission::ConfirmWrite,
            _ => Permission::Read,
        }
    }

    pub fn is_write(self) -> bool {
        self.permission() == Permission::ConfirmWrite
    }

    pub fn version(self) -> &'static str {
        "v1"
    }

    /// Timeline label shown while the step runs.
    pub fn label(self) -> &'static str {
        match self {
            Self::SearchBenefits => "조건에 맞는 혜택 찾기",
            Self::CheckEligibility => "내 조건 확인",
            Self::AnswerFromNotice => "공고 문장 찾기",
            Self::GetPreparation => "신청 준비 목록 확인",
            Self::Compare => "혜택 비교",
            Self::Deadlines => "마감 일정 정리",
            Self::OpenFormFill => "서류 작성 도우미 연결",
            Self::Bookmark => "관심 혜택 저장 제안",
            Self::SetAlert => "알림 설정 제안",
            Self::ToggleChecklistItem => "준비 항목 체크 제안",
            Self::RecordApplicationStatus => "신청 상태 기록 제안",
            Self::ProfilePatch => "프로필 수정 제안",
            Self::CheckOfficialPage => "공식 페이지 실시간 확인",
            Self::DocumentIssueLink => "서류 발급처 안내",
            Self::AddToCalendar => "캘린더 일정 추가 제안",
            Self::MatchVaultDocuments => "보관함 서류 대조",
            Self::RequestSignature => "서명 요청 (기기에서)",
            Self::OpenOfficialApplication => "공식 신청 페이지 열기 제안",
        }
    }
}

/// A programme named by a server-resolved token, never by id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgramRef {
    /// The programme the conversation or message was opened from.
    Context,
    /// Ids the user selected on screen (compare), 2..=4.
    Selected,
    /// The user's saved, prepared or subscribed programmes, deadline first.
    Saved,
    /// The on-topic programmes an earlier `search_benefits` step of the same
    /// run found, in its order. Server-only: never offered to the model, and
    /// a step on it with no search before it in the plan finds nothing.
    Found,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplicationStatus {
    Interested,
    Planning,
    Applying,
    Applied,
    Waiting,
    Received,
    Abandoned,
}

impl ApplicationStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Interested => "interested",
            Self::Planning => "planning",
            Self::Applying => "applying",
            Self::Applied => "applied",
            Self::Waiting => "waiting",
            Self::Received => "received",
            Self::Abandoned => "abandoned",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Interested => "관심",
            Self::Planning => "신청 예정",
            Self::Applying => "신청 준비 중",
            Self::Applied => "신청 완료(직접 기록)",
            Self::Waiting => "결과 대기",
            Self::Received => "수혜 완료",
            Self::Abandoned => "진행 중단",
        }
    }
}

/// Profile fields the agent may propose to change. Closed on purpose: no
/// birth year (age gate), free text (school name, nickname) or
/// special-category flags (disability, multicultural, low-income household,
/// veteran family); those stay on the profile screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileField {
    RegionCode,
    EnrollmentStatus,
    EmploymentStatus,
    SchoolYear,
    /// 소득분위 (`income_bracket`).
    IncomeBracket,
    /// 학자금 지원구간 (`kosaf_support_bracket`): its own column, never
    /// written through 소득분위 (client audit R-H1).
    KosafSupportBracket,
    HouseholdSize,
}

impl ProfileField {
    pub const ALL: [ProfileField; 7] = [
        ProfileField::RegionCode,
        ProfileField::EnrollmentStatus,
        ProfileField::EmploymentStatus,
        ProfileField::SchoolYear,
        ProfileField::IncomeBracket,
        ProfileField::KosafSupportBracket,
        ProfileField::HouseholdSize,
    ];

    /// The `user_profiles` column, a constant.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RegionCode => "region_code",
            Self::EnrollmentStatus => "enrollment_status",
            Self::EmploymentStatus => "employment_status",
            Self::SchoolYear => "school_year",
            Self::IncomeBracket => "income_bracket",
            Self::KosafSupportBracket => "kosaf_support_bracket",
            Self::HouseholdSize => "household_size",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::RegionCode => "거주 지역",
            Self::EnrollmentStatus => "학적 상태",
            Self::EmploymentStatus => "취업 상태",
            Self::SchoolYear => "학년",
            Self::IncomeBracket => "소득분위",
            Self::KosafSupportBracket => "학자금 지원구간",
            Self::HouseholdSize => "가구원 수",
        }
    }

    fn number_range(self) -> Option<std::ops::RangeInclusive<i32>> {
        match self {
            Self::SchoolYear => Some(1..=6),
            Self::IncomeBracket | Self::KosafSupportBracket => Some(0..=10),
            Self::HouseholdSize => Some(1..=20),
            _ => None,
        }
    }

    /// Whether `value` is a canonical value of this field: a code from the
    /// field's closed list, or a decimal integer in its range.
    pub fn accepts(self, value: &str) -> bool {
        match self {
            Self::RegionCode => REGION_CODES.contains(&value),
            Self::EnrollmentStatus => ENROLLMENT_CODES.contains(&value),
            Self::EmploymentStatus => EMPLOYMENT_CODES.contains(&value),
            _ => self.number(value).is_some(),
        }
    }

    /// The integer of a numeric field's value, inside its range.
    pub fn number(self, value: &str) -> Option<i32> {
        let range = self.number_range()?;
        // Canonical decimal only: no sign, no leading zero, at most 2 digits.
        if value.is_empty()
            || value.len() > 2
            || (value.len() > 1 && value.starts_with('0'))
            || !value.bytes().all(|b| b.is_ascii_digit())
        {
            return None;
        }
        value.parse::<i32>().ok().filter(|n| range.contains(n))
    }

    /// Korean label of a stored or proposed value; `None` = not entered.
    pub fn value_label(self, value: Option<&str>) -> String {
        let Some(value) = value.filter(|v| !v.trim().is_empty()) else {
            return "입력 안 됨".into();
        };
        match self {
            Self::RegionCode => crate::region_label(value).into(),
            Self::EnrollmentStatus => crate::enrollment_status_label(value).into(),
            Self::EmploymentStatus => crate::employment_status_label(value).into(),
            Self::SchoolYear => format!("{value}학년"),
            Self::IncomeBracket => format!("{value}분위"),
            Self::KosafSupportBracket => format!("{value}구간"),
            Self::HouseholdSize => format!("{value}명"),
        }
    }
}

pub const EMPLOYMENT_CODES: &[&str] = &[
    "employed",
    "new_hire",
    "unemployed",
    "preparing",
    "self_employed",
    "student",
    "other",
];

pub const REGION_CODES: &[&str] = &[
    "seoul",
    "busan",
    "daegu",
    "incheon",
    "gwangju",
    "daejeon",
    "ulsan",
    "sejong",
    "gyeonggi",
    "gangwon",
    "chungbuk",
    "chungnam",
    "jeonbuk",
    "jeonnam",
    "gyeongbuk",
    "gyeongnam",
    "jeju",
];
pub const ENROLLMENT_CODES: &[&str] = &["enrolled", "on_leave", "graduated", "research_student"];
pub const PROGRAM_TYPES: &[&str] = &[
    "scholarship",
    "policy",
    "campus_benefit",
    "corporate_benefit",
];

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchArgs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enrollment_status: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub program_types: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
    /// Field tabs the goal names ([`crate::categories::FIELDS`] codes), read
    /// from the goal by the server. A row is on topic when its field is one
    /// of them or its title names one ([`crate::categories::fields_named`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<String>,
    /// Only rows whose deadline (a Korean calendar day) is at most this
    /// many days from today (0..=[`MAX_DEADLINE_DAYS`]), deadline first: a
    /// search goal that asks for closing-soon rows ("이번 달 마감 장학금").
    /// Read from the goal by the server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_within_days: Option<u16>,
    /// The first day of that period, in days from today (default 0):
    /// "다음 달 마감" starts on the 1st of next month.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_from_days: Option<u16>,
    /// The goal named the period ("이번 주", "다음 달") rather than a length
    /// ("3일 안"); only with a window. Read from the goal by the server.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub deadline_named: bool,
    /// Only the row that closes first (a superlative on the deadline: "마감
    /// 제일 빠른 거", QA B5): rows with a deadline, deadline first, one row.
    /// Read from the goal by the server.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub earliest: bool,
    /// Only rows whose stored amount for [`Self::amount_period`] is at
    /// least this many won ("월 30만원 이상"); rows without a stored amount
    /// are listed apart. Read from the goal by the server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amount_min: Option<i32>,
    /// One of [`AMOUNT_PERIODS`], only with `amount_min`; `None` = any
    /// stored period.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amount_period: Option<String>,
    /// Regions the goal excludes ("부산 말고"): a search that would run
    /// under one of them asks which region instead. Read from the goal by
    /// the server.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excluded_regions: Vec<String>,
}

/// The largest amount bound a search takes (won), read by the parser and
/// the plan validator alike.
pub const MAX_AMOUNT_WON: i32 = 100_000_000;

/// An amount bound a search can take: 1..=[`MAX_AMOUNT_WON`].
pub fn valid_amount(won: i32) -> bool {
    (1..=MAX_AMOUNT_WON).contains(&won)
}

/// The periods a stored amount has: `programs.benefit_amount_monthly`,
/// `_semester`, `_once`.
pub const AMOUNT_PERIODS: [&str; 3] = ["monthly", "semester", "once"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProgramArgs {
    pub program: ProgramRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoticeArgs {
    pub program: ProgramRef,
    pub question: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompareArgs {
    pub programs: ProgramRef,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeadlineArgs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub within_days: Option<u16>,
    /// The first day of the period, as [`SearchArgs::deadline_from_days`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_days: Option<u16>,
}

/// The farthest deadline day a period reaches: next month seen from the
/// 1st of a 31-day month ends 61 days away.
pub const MAX_DEADLINE_DAYS: u16 = 61;

/// A deadline period of day offsets: the end within the limit, a start only
/// with an end and not after it.
fn valid_period(from: Option<u16>, to: Option<u16>) -> bool {
    match (from, to) {
        (None, None) => true,
        (from, Some(to)) => to <= MAX_DEADLINE_DAYS && from.is_none_or(|f| f <= to),
        (Some(_), None) => false,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BookmarkArgs {
    pub program: ProgramRef,
    pub bookmarked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AlertArgs {
    pub program: ProgramRef,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChecklistArgs {
    pub program: ProgramRef,
    /// A checklist task id or a fragment of its title, resolved against
    /// the current plan at execution; unresolved means no proposal.
    pub task: String,
    pub done: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatusArgs {
    pub program: ProgramRef,
    pub status: ApplicationStatus,
}

/// One field of the user's own profile. No user or programme: the server
/// applies it to the JWT user only. `value` is canonical
/// ([`ProfileField::accepts`]); the server reads it from the goal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfilePatchArgs {
    pub field: ProfileField,
    pub value: String,
}

/// A required document to find the issuing site for: named in the goal
/// (`document`), or every document task of the programme's checklist.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentArgs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub program: Option<ProgramRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document: Option<String>,
}

/// Which date of a programme a calendar event marks. The server reads the
/// date from its data (deadline, start) or a notice sentence (announcement,
/// interview); the planner never writes a date.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CalendarDate {
    #[default]
    Deadline,
    ApplicationStart,
    Announcement,
    Interview,
}

impl CalendarDate {
    pub fn label(self) -> &'static str {
        match self {
            Self::Deadline => "신청 마감",
            Self::ApplicationStart => "신청 시작",
            Self::Announcement => "결과 발표",
            Self::Interview => "면접",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CalendarArgs {
    pub program: ProgramRef,
    #[serde(default)]
    pub date: CalendarDate,
    /// Days before the date ("마감 하루 전"), 0..=14; read from the goal.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub days_before: u8,
}

fn is_zero(n: &u8) -> bool {
    *n == 0
}

/// Longest offset a calendar event may have before its date.
pub const MAX_DAYS_BEFORE: u8 = 14;

/// One planned tool call: `{"tool": "...", "args": {...}}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "tool",
    content = "args",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum ToolCall {
    SearchBenefits(SearchArgs),
    CheckEligibility(ProgramArgs),
    AnswerFromNotice(NoticeArgs),
    GetPreparation(ProgramArgs),
    Compare(CompareArgs),
    Deadlines(DeadlineArgs),
    OpenFormFill(ProgramArgs),
    Bookmark(BookmarkArgs),
    SetAlert(AlertArgs),
    ToggleChecklistItem(ChecklistArgs),
    RecordApplicationStatus(StatusArgs),
    ProfilePatch(ProfilePatchArgs),
    CheckOfficialPage(ProgramArgs),
    DocumentIssueLink(DocumentArgs),
    AddToCalendar(CalendarArgs),
    MatchVaultDocuments(ProgramArgs),
    RequestSignature(ProgramArgs),
    OpenOfficialApplication(ProgramArgs),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlanError {
    #[error("plan is not valid JSON for the tool schema")]
    InvalidJson,
    #[error("plan has too many steps")]
    TooManySteps,
    #[error("argument out of range: {0}")]
    InvalidArgument(&'static str),
    #[error("duplicate step")]
    Duplicate,
}

fn clean_text(value: &str, max: usize) -> bool {
    let value = value.trim();
    !value.is_empty()
        && value.chars().count() <= max
        && !value
            .chars()
            .any(|c| c.is_control() || c == '<' || c == '>')
        && !looks_like_reference(value)
}

/// URLs, e-mail addresses and ids are never planner output.
fn looks_like_reference(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.contains("://")
        || lower.contains("www.")
        || lower.contains('@')
        || lower.contains(".com")
        || lower.contains(".kr")
        || looks_like_uuid(&lower)
}

fn looks_like_uuid(value: &str) -> bool {
    let hex: String = value.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    hex.len() >= 16
        && value
            .chars()
            .filter(|c| c.is_ascii_hexdigit() || *c == '-')
            .count()
            >= 20
}

impl ToolCall {
    pub fn name(&self) -> ToolName {
        match self {
            Self::SearchBenefits(_) => ToolName::SearchBenefits,
            Self::CheckEligibility(_) => ToolName::CheckEligibility,
            Self::AnswerFromNotice(_) => ToolName::AnswerFromNotice,
            Self::GetPreparation(_) => ToolName::GetPreparation,
            Self::Compare(_) => ToolName::Compare,
            Self::Deadlines(_) => ToolName::Deadlines,
            Self::OpenFormFill(_) => ToolName::OpenFormFill,
            Self::Bookmark(_) => ToolName::Bookmark,
            Self::SetAlert(_) => ToolName::SetAlert,
            Self::ToggleChecklistItem(_) => ToolName::ToggleChecklistItem,
            Self::RecordApplicationStatus(_) => ToolName::RecordApplicationStatus,
            Self::ProfilePatch(_) => ToolName::ProfilePatch,
            Self::CheckOfficialPage(_) => ToolName::CheckOfficialPage,
            Self::DocumentIssueLink(_) => ToolName::DocumentIssueLink,
            Self::AddToCalendar(_) => ToolName::AddToCalendar,
            Self::MatchVaultDocuments(_) => ToolName::MatchVaultDocuments,
            Self::RequestSignature(_) => ToolName::RequestSignature,
            Self::OpenOfficialApplication(_) => ToolName::OpenOfficialApplication,
        }
    }

    pub fn program(&self) -> Option<ProgramRef> {
        match self {
            Self::CheckEligibility(a) | Self::GetPreparation(a) | Self::OpenFormFill(a) => {
                Some(a.program)
            }
            Self::AnswerFromNotice(a) => Some(a.program),
            Self::Compare(a) => Some(a.programs),
            Self::Bookmark(a) => Some(a.program),
            Self::SetAlert(a) => Some(a.program),
            Self::ToggleChecklistItem(a) => Some(a.program),
            Self::RecordApplicationStatus(a) => Some(a.program),
            Self::CheckOfficialPage(a) => Some(a.program),
            Self::DocumentIssueLink(a) => a.program,
            Self::AddToCalendar(a) => Some(a.program),
            Self::MatchVaultDocuments(a)
            | Self::RequestSignature(a)
            | Self::OpenOfficialApplication(a) => Some(a.program),
            Self::SearchBenefits(_) | Self::Deadlines(_) | Self::ProfilePatch(_) => None,
        }
    }

    /// Domain validation after deserialization: enums, bounds, and which
    /// programme tokens a tool accepts.
    pub fn validate(&self) -> Result<(), PlanError> {
        use PlanError::InvalidArgument as Bad;
        match self {
            Self::SearchBenefits(a) => {
                if a.region
                    .as_deref()
                    .is_some_and(|r| !REGION_CODES.contains(&r))
                {
                    return Err(Bad("region"));
                }
                if a.enrollment_status
                    .as_deref()
                    .is_some_and(|s| !ENROLLMENT_CODES.contains(&s))
                {
                    return Err(Bad("enrollment_status"));
                }
                if a.program_types.len() > PROGRAM_TYPES.len()
                    || a.program_types
                        .iter()
                        .any(|t| !PROGRAM_TYPES.contains(&t.as_str()))
                {
                    return Err(Bad("program_types"));
                }
                if a.fields.len() > crate::categories::FIELDS.len()
                    || a.fields
                        .iter()
                        .any(|f| crate::categories::parse_field(f) != Some(f.as_str()))
                {
                    return Err(Bad("fields"));
                }
                if !valid_period(a.deadline_from_days, a.deadline_within_days)
                    || (a.deadline_named && a.deadline_within_days.is_none())
                {
                    return Err(Bad("deadline_within_days"));
                }
                let amount_ok = match (a.amount_min, a.amount_period.as_deref()) {
                    (None, None) => true,
                    (Some(min), period) => {
                        valid_amount(min) && period.is_none_or(|p| AMOUNT_PERIODS.contains(&p))
                    }
                    _ => false,
                };
                if !amount_ok {
                    return Err(Bad("amount_min"));
                }
                if a.excluded_regions.len() > 17
                    || a.excluded_regions
                        .iter()
                        .any(|r| crate::canonical_region_code(r) != Some(r.as_str()))
                {
                    return Err(Bad("excluded_regions"));
                }
                if a.keywords.len() > MAX_KEYWORDS
                    || a.keywords.iter().any(|k| {
                        !clean_text(k, MAX_KEYWORD_CHARS) || !k.chars().all(|c| c.is_alphanumeric())
                    })
                {
                    return Err(Bad("keywords"));
                }
            }
            Self::AnswerFromNotice(a) => {
                if a.program != ProgramRef::Context {
                    return Err(Bad("program"));
                }
                if !clean_text(&a.question, MAX_NOTICE_QUESTION_CHARS) {
                    return Err(Bad("question"));
                }
            }
            Self::Compare(a) => {
                if a.programs == ProgramRef::Context {
                    return Err(Bad("programs"));
                }
            }
            Self::Deadlines(a) => {
                if !valid_period(a.from_days, a.within_days) {
                    return Err(Bad("within_days"));
                }
            }
            Self::ToggleChecklistItem(a) => {
                if a.program != ProgramRef::Context {
                    return Err(Bad("program"));
                }
                if !clean_text(&a.task, MAX_TASK_CHARS) {
                    return Err(Bad("task"));
                }
            }
            Self::RecordApplicationStatus(a) => {
                if a.program != ProgramRef::Context {
                    return Err(Bad("program"));
                }
            }
            Self::ProfilePatch(a) => {
                if !a.field.accepts(&a.value) {
                    return Err(Bad("value"));
                }
            }
            Self::DocumentIssueLink(a) => {
                if a.program.is_none() && a.document.is_none() {
                    return Err(Bad("document"));
                }
                if a.program == Some(ProgramRef::Selected) {
                    return Err(Bad("program"));
                }
                if a.document.as_deref().is_some_and(|d| {
                    !clean_text(d, MAX_DOCUMENT_CHARS)
                        || !d.chars().all(|c| c.is_alphanumeric() || c == ' ')
                }) {
                    return Err(Bad("document"));
                }
            }
            Self::CheckOfficialPage(a) => {
                if a.program == ProgramRef::Selected {
                    return Err(Bad("program"));
                }
            }
            Self::AddToCalendar(a) => {
                if a.program == ProgramRef::Selected {
                    return Err(Bad("program"));
                }
                if a.days_before > MAX_DAYS_BEFORE {
                    return Err(Bad("days_before"));
                }
            }
            Self::CheckEligibility(_)
            | Self::GetPreparation(_)
            | Self::OpenFormFill(_)
            | Self::Bookmark(_)
            | Self::SetAlert(_) => {}
            // One programme each: the one on screen or one the run found.
            Self::MatchVaultDocuments(a)
            | Self::RequestSignature(a)
            | Self::OpenOfficialApplication(a) => {
                if !matches!(a.program, ProgramRef::Context | ProgramRef::Found) {
                    return Err(Bad("program"));
                }
            }
        }
        Ok(())
    }
}

/// What a planner returns: `{"steps": [...]}`. An empty plan is a refusal.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub steps: Vec<ToolCall>,
}

impl Plan {
    pub fn new(steps: Vec<ToolCall>) -> Self {
        Self { steps }
    }

    pub fn validate(&self) -> Result<(), PlanError> {
        if self.steps.len() > MAX_PLAN_STEPS {
            return Err(PlanError::TooManySteps);
        }
        for (i, step) in self.steps.iter().enumerate() {
            step.validate()?;
            if self.steps[..i].contains(step) {
                return Err(PlanError::Duplicate);
            }
        }
        Ok(())
    }

    /// Parse untrusted planner output: strict schema (unknown tools,
    /// unknown or missing fields fail), then domain validation. Markdown
    /// code fences around the JSON are tolerated; nothing else is.
    pub fn parse(raw: &str) -> Result<Self, PlanError> {
        let trimmed = raw.trim();
        let body = trimmed
            .strip_prefix("```json")
            .or_else(|| trimmed.strip_prefix("```"))
            .and_then(|rest| rest.strip_suffix("```"))
            .unwrap_or(trimmed)
            .trim();
        if body.len() > 8 * 1024 {
            return Err(PlanError::InvalidJson);
        }
        let plan: Plan = serde_json::from_str(body).map_err(|_| PlanError::InvalidJson)?;
        plan.validate()?;
        Ok(plan)
    }

    pub fn writes(&self) -> impl Iterator<Item = &ToolCall> {
        self.steps.iter().filter(|s| s.name().is_write())
    }
}

impl SearchArgs {
    /// Canonical form: programme types in [`PROGRAM_TYPES`] order without
    /// repeats, keywords trimmed and unique. Two plans that mean the same
    /// search compare equal after this.
    pub fn canonicalize(&mut self) {
        self.program_types
            .retain(|t| PROGRAM_TYPES.contains(&t.as_str()));
        self.program_types.sort_by_key(|t| {
            PROGRAM_TYPES
                .iter()
                .position(|known| known == t)
                .unwrap_or(usize::MAX)
        });
        self.program_types.dedup();
        let order = |f: &String| {
            crate::categories::FIELDS
                .iter()
                .position(|(code, _)| code == f)
                .unwrap_or(usize::MAX)
        };
        self.fields.retain(|f| order(f) != usize::MAX);
        self.fields.sort_by_key(order);
        self.fields.dedup();
        let mut seen = Vec::new();
        for keyword in std::mem::take(&mut self.keywords) {
            let keyword = keyword.trim().to_owned();
            if !keyword.is_empty() && !seen.contains(&keyword) {
                seen.push(keyword);
            }
        }
        self.keywords = seen;
    }
}

impl Plan {
    /// Canonical argument order and no repeated steps. Never adds a step.
    pub fn canonicalize(&mut self) {
        let mut steps: Vec<ToolCall> = Vec::with_capacity(self.steps.len());
        for mut step in std::mem::take(&mut self.steps) {
            if let ToolCall::SearchBenefits(args) = &mut step {
                args.canonicalize();
            }
            if !steps.contains(&step) {
                steps.push(step);
            }
        }
        self.steps = steps;
    }
}

/// Steps a model plan may hold. Fixed plans are at most three steps; the
/// tighter bound keeps a model from padding a plan.
pub const MAX_MODEL_PLAN_STEPS: usize = 4;

/// What the model decided before any step: plan, refuse (out of scope,
/// other people's data, instructions inside the goal, chit-chat), or ask
/// for a programme on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlannerDecision {
    Plan,
    Refuse,
    NeedsProgram,
}

/// Model planner output: `{"decision": ..., "steps": [{"<tool>": {args}}]}`.
/// The tool is the step's only key so a decoder that writes object keys in
/// sorted order (Ollama's schema grammar does) still picks the tool before
/// its arguments; `{"tool","args"}` would make the model commit to
/// arguments first. `steps` are normalized to `{"tool","args"}` raw JSON
/// here: the server fills the arguments it already knows and validates them
/// as [`ToolCall`]s afterwards. The v1 shape (`{"steps":[{"tool","args"}]}`
/// without `decision`; empty = refuse) is still read.
#[derive(Debug, Clone, PartialEq)]
pub struct PlannerReply {
    pub decision: PlannerDecision,
    pub steps: Vec<Value>,
}

impl PlannerReply {
    /// Parse untrusted model output: one JSON object with only `decision`
    /// and `steps`, each step naming one registered tool. Code fences are
    /// tolerated. Argument checks come later ([`Plan::validate`] after the
    /// server fills its arguments).
    pub fn parse(raw: &str) -> Result<Self, PlanError> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            #[serde(default)]
            decision: Option<PlannerDecision>,
            #[serde(default)]
            steps: Vec<WireStep>,
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Tagged {
            tool: ToolName,
            #[serde(default)]
            args: serde_json::Map<String, Value>,
        }
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum WireStep {
            Tagged(Tagged),
            Keyed(std::collections::HashMap<ToolName, serde_json::Map<String, Value>>),
        }
        impl WireStep {
            fn into_parts(self) -> Option<(ToolName, serde_json::Map<String, Value>)> {
                match self {
                    Self::Tagged(t) => Some((t.tool, t.args)),
                    Self::Keyed(map) if map.len() == 1 => map.into_iter().next(),
                    Self::Keyed(_) => None,
                }
            }
        }
        let trimmed = raw.trim();
        let body = trimmed
            .strip_prefix("```json")
            .or_else(|| trimmed.strip_prefix("```"))
            .and_then(|rest| rest.strip_suffix("```"))
            .unwrap_or(trimmed)
            .trim();
        if body.len() > 8 * 1024 {
            return Err(PlanError::InvalidJson);
        }
        let wire: Wire = serde_json::from_str(body).map_err(|_| PlanError::InvalidJson)?;
        let decision = wire.decision.unwrap_or(if wire.steps.is_empty() {
            PlannerDecision::Refuse
        } else {
            PlannerDecision::Plan
        });
        let steps = wire
            .steps
            .into_iter()
            .map(|s| {
                s.into_parts()
                    .map(|(tool, args)| serde_json::json!({"tool": tool, "args": Value::Object(args)}))
                    .ok_or(PlanError::InvalidJson)
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self { decision, steps })
    }
}

/// Model-facing arguments per tool. Arguments the server knows are not
/// asked for: the notice question is the goal, `compare` follows the
/// selection, checklist, status and notice steps always target the
/// programme on screen, and search filters and the deadline window are
/// read from the goal's words (the model's guesses hid matching benefits,
/// and every output token costs time on a CPU host). Search keeps optional
/// keywords, which the server cleans.
fn model_args_schema(tool: ToolName) -> Value {
    use serde_json::json;
    let program = json!({"type":"string","enum":["context","saved"]});
    let object = |properties: Value, required: &[&str]| json!({"type":"object","additionalProperties":false,"properties":properties,"required":required});
    match tool {
        ToolName::SearchBenefits => object(
            json!({
                "keywords":{"type":"array","maxItems":MAX_KEYWORDS,"items":{"type":"string","maxLength":MAX_KEYWORD_CHARS}}
            }),
            &[],
        ),
        ToolName::CheckEligibility
        | ToolName::GetPreparation
        | ToolName::OpenFormFill
        | ToolName::CheckOfficialPage => object(json!({"program":program}), &["program"]),
        ToolName::AnswerFromNotice | ToolName::Compare | ToolName::Deadlines => {
            object(json!({}), &[])
        }
        ToolName::Bookmark => object(
            json!({"program":program,"bookmarked":{"type":"boolean"}}),
            &["program", "bookmarked"],
        ),
        ToolName::SetAlert => object(
            json!({"program":program,"enabled":{"type":"boolean"}}),
            &["program", "enabled"],
        ),
        ToolName::ToggleChecklistItem => object(
            json!({"task":{"type":"string","minLength":1,"maxLength":MAX_TASK_CHARS},"done":{"type":"boolean"}}),
            &["task", "done"],
        ),
        ToolName::RecordApplicationStatus => object(
            json!({"status":{"type":"string","enum":["interested","planning","applying","applied","waiting","received","abandoned"]}}),
            &["status"],
        ),
        // Field and value are read from the goal by the server.
        ToolName::ProfilePatch => object(json!({}), &[]),
        // The document (and the programme on screen) come from the goal.
        ToolName::DocumentIssueLink => object(json!({}), &[]),
        // Which date (deadline, start, announcement, interview) the server
        // reads from the goal's words.
        ToolName::AddToCalendar => object(json!({"program":program}), &["program"]),
        // Server-planned only; never in a model schema.
        ToolName::MatchVaultDocuments
        | ToolName::RequestSignature
        | ToolName::OpenOfficialApplication => object(json!({"program":program}), &["program"]),
    }
}

/// JSON schema handed to a structured-output model (Ollama `format`): the
/// decision (sorts before `steps`), then steps as a union of one-key
/// objects `{"<tool>": {args}}`, each with its own closed argument object.
/// It narrows decoding; [`PlannerReply::parse`] and [`Plan::validate`]
/// stay the authority.
pub fn planner_json_schema() -> Value {
    planner_json_schema_for(&ToolName::model_tools())
}

/// [`planner_json_schema`] limited to `tools` (the deployment's offer).
pub fn planner_json_schema_for(tools: &[ToolName]) -> Value {
    let steps: Vec<Value> = tools
        .iter()
        .map(|tool| {
            serde_json::json!({
                "type":"object","additionalProperties":false,"required":[tool.as_str()],
                "properties":{tool.as_str():model_args_schema(*tool)}
            })
        })
        .collect();
    serde_json::json!({
        "type":"object","additionalProperties":false,"required":["decision","steps"],
        "properties":{
            "decision":{"type":"string","enum":["plan","refuse","needs_program"]},
            "steps":{"type":"array","maxItems":MAX_MODEL_PLAN_STEPS,"items":{"anyOf":steps}}
        }
    })
}

/// Run states (spec §6). `failed`, `timed_out`, `cancelled` and
/// `completed` are terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Received,
    Planning,
    ToolRunning,
    AwaitingConfirmation,
    Executing,
    ValidatingResponse,
    Completed,
    Failed,
    TimedOut,
    Cancelled,
}

impl RunStatus {
    pub const ALL: [RunStatus; 10] = [
        RunStatus::Received,
        RunStatus::Planning,
        RunStatus::ToolRunning,
        RunStatus::AwaitingConfirmation,
        RunStatus::Executing,
        RunStatus::ValidatingResponse,
        RunStatus::Completed,
        RunStatus::Failed,
        RunStatus::TimedOut,
        RunStatus::Cancelled,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Received => "received",
            Self::Planning => "planning",
            Self::ToolRunning => "tool_running",
            Self::AwaitingConfirmation => "awaiting_confirmation",
            Self::Executing => "executing",
            Self::ValidatingResponse => "validating_response",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::TimedOut => "timed_out",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.as_str() == value)
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::TimedOut | Self::Cancelled
        )
    }

    /// The only legal edges. Planning may complete directly (refusal or
    /// clarification); a run with nothing to confirm skips the CONFIRM
    /// states; every non-terminal state may fail, time out or be cancelled.
    pub fn can_transition(self, to: RunStatus) -> bool {
        use RunStatus::*;
        if self.is_terminal() {
            return false;
        }
        if matches!(to, Failed | TimedOut | Cancelled) {
            return true;
        }
        matches!(
            (self, to),
            (Received, Planning)
                | (Planning, ToolRunning)
                | (Planning, ValidatingResponse)
                | (ToolRunning, AwaitingConfirmation)
                | (ToolRunning, ValidatingResponse)
                | (AwaitingConfirmation, Executing)
                | (AwaitingConfirmation, Completed)
                | (Executing, AwaitingConfirmation)
                | (Executing, Completed)
                | (ValidatingResponse, AwaitingConfirmation)
                | (ValidatingResponse, Completed)
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfirmationStatus {
    Pending,
    Approved,
    Rejected,
    Expired,
    Executed,
    Failed,
    Stale,
}

impl ConfirmationStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
            Self::Expired => "expired",
            Self::Executed => "executed",
            Self::Failed => "failed",
            Self::Stale => "stale",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        [
            Self::Pending,
            Self::Approved,
            Self::Rejected,
            Self::Expired,
            Self::Executed,
            Self::Failed,
            Self::Stale,
        ]
        .into_iter()
        .find(|s| s.as_str() == value)
    }
}

/// Everything a confirmation is bound to. The hash is shown to the client
/// with the card and must come back with the approval; the server also
/// recomputes it from the stored row before executing.
pub struct ConfirmationBinding<'a> {
    pub user_id: uuid::Uuid,
    pub run_id: uuid::Uuid,
    pub tool_call_id: uuid::Uuid,
    pub tool: ToolName,
    pub program_id: uuid::Uuid,
    pub arguments: &'a Value,
    pub precondition: &'a Value,
    pub expires_at: chrono::DateTime<chrono::Utc>,
}

pub fn confirmation_hash(binding: &ConfirmationBinding) -> String {
    // serde_json maps are ordered by key, so this is canonical.
    let material = serde_json::json!({
        "v": 1,
        "user": binding.user_id,
        "run": binding.run_id,
        "tool_call": binding.tool_call_id,
        "tool": binding.tool.as_str(),
        "program": binding.program_id,
        "args": binding.arguments,
        "pre": binding.precondition,
        "exp": binding.expires_at.timestamp(),
    });
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&material).expect("JSON value serializes"))
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn strict_schema_rejects_unknown_tools_fields_and_values() {
        let ok = r#"{"steps":[{"tool":"search_benefits","args":{"region":"busan","program_types":["scholarship"]}},{"tool":"deadlines","args":{}}]}"#;
        assert_eq!(Plan::parse(ok).unwrap().steps.len(), 2);
        let fenced = format!("```json\n{ok}\n```");
        assert!(Plan::parse(&fenced).is_ok());
        for bad in [
            r#"{"steps":[{"tool":"execute_sql","args":{"sql":"DROP TABLE users"}}]}"#,
            r#"{"steps":[{"tool":"bookmark","args":{"program":"context","bookmarked":true,"user_id":"x"}}]}"#,
            r#"{"steps":[{"tool":"bookmark","args":{"program":"6f1c2b3a-0000-4000-8000-000000000000","bookmarked":true}}]}"#,
            r#"{"steps":[{"tool":"bookmark","args":{"program":"context"}}]}"#,
            r#"{"steps":[],"answer":"invented"}"#,
            r#"{"steps":[{"tool":"deadlines","args":{},"confirm":false}]}"#,
            r#"not json"#,
            r#"{"steps":[{"tool":"set_alert","args":{"program":"context","enabled":"yes"}}]}"#,
        ] {
            assert_eq!(Plan::parse(bad), Err(PlanError::InvalidJson), "{bad}");
        }
        for (bad, why) in [
            (
                r#"{"steps":[{"tool":"search_benefits","args":{"region":"atlantis"}}]}"#,
                "region",
            ),
            (
                r#"{"steps":[{"tool":"search_benefits","args":{"keywords":["https://evil.example"]}}]}"#,
                "keywords",
            ),
            (
                r#"{"steps":[{"tool":"search_benefits","args":{"keywords":["a","b","c","d"]}}]}"#,
                "keywords",
            ),
            (
                r#"{"steps":[{"tool":"answer_from_notice","args":{"program":"saved","question":"서류"}}]}"#,
                "program",
            ),
            (
                r#"{"steps":[{"tool":"answer_from_notice","args":{"program":"context","question":"메일 a@b.kr 로 보내"}}]}"#,
                "question",
            ),
            (
                r#"{"steps":[{"tool":"deadlines","args":{"within_days":365}}]}"#,
                "within_days",
            ),
            (
                r#"{"steps":[{"tool":"compare","args":{"programs":"context"}}]}"#,
                "programs",
            ),
        ] {
            assert_eq!(
                Plan::parse(bad),
                Err(PlanError::InvalidArgument(why)),
                "{bad}"
            );
        }
        let seven = format!(
            r#"{{"steps":[{}]}}"#,
            (1..=MAX_PLAN_STEPS + 1)
                .map(|d| format!(r#"{{"tool":"deadlines","args":{{"within_days":{d}}}}}"#))
                .collect::<Vec<_>>()
                .join(",")
        );
        assert_eq!(Plan::parse(&seven), Err(PlanError::TooManySteps));
        let dup = r#"{"steps":[{"tool":"deadlines","args":{}},{"tool":"deadlines","args":{}}]}"#;
        assert_eq!(Plan::parse(dup), Err(PlanError::Duplicate));
    }

    #[test]
    fn writes_are_exactly_the_confirm_write_tools() {
        let writes: Vec<_> = ToolName::ALL.into_iter().filter(|t| t.is_write()).collect();
        assert_eq!(
            writes,
            [
                ToolName::OpenFormFill,
                ToolName::Bookmark,
                ToolName::SetAlert,
                ToolName::ToggleChecklistItem,
                ToolName::RecordApplicationStatus,
                ToolName::ProfilePatch,
                ToolName::AddToCalendar,
                ToolName::RequestSignature,
                ToolName::OpenOfficialApplication,
            ]
        );
        assert!(ToolName::V1.iter().all(|t| ToolName::ALL.contains(t)));
        for tool in ToolName::ALL {
            assert_eq!(ToolName::parse(tool.as_str()), Some(tool));
            let serialized = serde_json::to_value(tool).unwrap();
            assert_eq!(serialized, tool.as_str());
        }
    }

    #[test]
    fn profile_patch_accepts_only_canonical_values_of_whitelisted_fields() {
        let ok = r#"{"steps":[{"tool":"profile_patch","args":{"field":"region_code","value":"busan"}}]}"#;
        assert_eq!(
            Plan::parse(ok).unwrap().steps[0].name(),
            ToolName::ProfilePatch
        );
        assert_eq!(
            Plan::parse(ok).unwrap().steps[0].program(),
            None,
            "names no programme"
        );
        for (field, value) in [
            ("enrollment_status", "on_leave"),
            ("employment_status", "preparing"),
            ("school_year", "3"),
            ("income_bracket", "0"),
            ("income_bracket", "10"),
            ("household_size", "4"),
        ] {
            let raw = format!(
                r#"{{"steps":[{{"tool":"profile_patch","args":{{"field":"{field}","value":"{value}"}}}}]}}"#
            );
            assert!(Plan::parse(&raw).is_ok(), "{raw}");
        }
        for (field, value) in [
            ("region_code", "부산"),
            ("region_code", "atlantis"),
            ("enrollment_status", "휴학"),
            ("school_year", "7"),
            ("school_year", "03"),
            ("income_bracket", "11"),
            ("income_bracket", "-1"),
            ("household_size", "0"),
            ("household_size", "1e1"),
        ] {
            let raw = format!(
                r#"{{"steps":[{{"tool":"profile_patch","args":{{"field":"{field}","value":"{value}"}}}}]}}"#
            );
            assert_eq!(
                Plan::parse(&raw),
                Err(PlanError::InvalidArgument("value")),
                "{raw}"
            );
        }
        for raw in [
            // Not on the list: age gate, free text, special categories.
            r#"{"steps":[{"tool":"profile_patch","args":{"field":"birth_year","value":"2000"}}]}"#,
            r#"{"steps":[{"tool":"profile_patch","args":{"field":"school_name","value":"x"}}]}"#,
            r#"{"steps":[{"tool":"profile_patch","args":{"field":"has_disability","value":"true"}}]}"#,
            // No user, no programme.
            r#"{"steps":[{"tool":"profile_patch","args":{"field":"region_code","value":"busan","user_id":"x"}}]}"#,
            r#"{"steps":[{"tool":"profile_patch","args":{"field":"region_code","value":"busan","program":"context"}}]}"#,
        ] {
            assert_eq!(Plan::parse(raw), Err(PlanError::InvalidJson), "{raw}");
        }
        assert_eq!(ProfileField::RegionCode.value_label(Some("busan")), "부산");
        assert_eq!(ProfileField::SchoolYear.value_label(None), "입력 안 됨");
        for field in ProfileField::ALL {
            assert!(!field.as_str().is_empty() && !field.label().is_empty());
        }
    }

    #[test]
    fn run_state_machine_allows_only_listed_edges() {
        use RunStatus::*;
        let allowed = [
            (Received, Planning),
            (Planning, ToolRunning),
            (Planning, ValidatingResponse),
            (ToolRunning, AwaitingConfirmation),
            (ToolRunning, ValidatingResponse),
            (AwaitingConfirmation, Executing),
            (AwaitingConfirmation, Completed),
            (Executing, AwaitingConfirmation),
            (Executing, Completed),
            (ValidatingResponse, AwaitingConfirmation),
            (ValidatingResponse, Completed),
        ];
        for from in RunStatus::ALL {
            for to in RunStatus::ALL {
                let expected = !from.is_terminal()
                    && (matches!(to, Failed | TimedOut | Cancelled)
                        || allowed.contains(&(from, to)));
                assert_eq!(from.can_transition(to), expected, "{from:?} -> {to:?}");
            }
            assert_eq!(RunStatus::parse(from.as_str()), Some(from));
        }
        assert!(!Completed.can_transition(Cancelled), "terminal is final");
        assert!(
            !Received.can_transition(Executing),
            "no write before a plan"
        );
        assert!(
            !ToolRunning.can_transition(Executing),
            "no execution without awaiting a confirmation first"
        );
    }

    #[test]
    fn confirmation_hash_binds_every_field() {
        let user = uuid::Uuid::new_v4();
        let run = uuid::Uuid::new_v4();
        let call = uuid::Uuid::new_v4();
        let program = uuid::Uuid::new_v4();
        let args = json!({"enabled":true});
        let pre = json!({"enabled":false});
        let expires = chrono::Utc::now();
        let base = ConfirmationBinding {
            user_id: user,
            run_id: run,
            tool_call_id: call,
            tool: ToolName::SetAlert,
            program_id: program,
            arguments: &args,
            precondition: &pre,
            expires_at: expires,
        };
        let hash = confirmation_hash(&base);
        assert_eq!(hash.len(), 64);
        assert_eq!(hash, confirmation_hash(&base), "deterministic");
        let other_args = json!({"enabled":false});
        let other_pre = json!({"enabled":true});
        let variants = [
            ConfirmationBinding {
                user_id: uuid::Uuid::new_v4(),
                ..base_clone(&base)
            },
            ConfirmationBinding {
                run_id: uuid::Uuid::new_v4(),
                ..base_clone(&base)
            },
            ConfirmationBinding {
                tool_call_id: uuid::Uuid::new_v4(),
                ..base_clone(&base)
            },
            ConfirmationBinding {
                tool: ToolName::Bookmark,
                ..base_clone(&base)
            },
            ConfirmationBinding {
                program_id: uuid::Uuid::new_v4(),
                ..base_clone(&base)
            },
            ConfirmationBinding {
                arguments: &other_args,
                ..base_clone(&base)
            },
            ConfirmationBinding {
                precondition: &other_pre,
                ..base_clone(&base)
            },
            ConfirmationBinding {
                expires_at: expires + chrono::Duration::seconds(1),
                ..base_clone(&base)
            },
        ];
        for variant in variants {
            assert_ne!(confirmation_hash(&variant), hash);
        }
    }

    fn base_clone<'a>(b: &ConfirmationBinding<'a>) -> ConfirmationBinding<'a> {
        ConfirmationBinding {
            user_id: b.user_id,
            run_id: b.run_id,
            tool_call_id: b.tool_call_id,
            tool: b.tool,
            program_id: b.program_id,
            arguments: b.arguments,
            precondition: b.precondition,
            expires_at: b.expires_at,
        }
    }

    #[test]
    fn planner_schema_is_a_closed_union_discriminated_by_tool() {
        let schema = planner_json_schema();
        assert_eq!(
            schema["required"],
            json!(["decision", "steps"]),
            "decision before steps"
        );
        assert_eq!(
            schema["properties"]["decision"]["enum"],
            json!(["plan", "refuse", "needs_program"])
        );
        assert_eq!(
            schema["properties"]["steps"]["maxItems"],
            MAX_MODEL_PLAN_STEPS
        );
        let variants = schema["properties"]["steps"]["items"]["anyOf"]
            .as_array()
            .unwrap();
        assert_eq!(variants.len(), ToolName::model_tools().len());
        assert!(!schema.to_string().contains("match_vault_documents"));
        for (variant, tool) in variants.iter().zip(ToolName::model_tools()) {
            // One key, the tool: chosen before any argument even when the
            // decoder sorts keys.
            assert_eq!(variant["required"], json!([tool.as_str()]));
            assert_eq!(variant["properties"].as_object().unwrap().len(), 1);
            assert_eq!(variant["additionalProperties"], false);
            let args = &variant["properties"][tool.as_str()];
            assert_eq!(args["additionalProperties"], false, "{tool:?}");
            let keys: Vec<&str> = args["properties"]
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            // Server-filled arguments are never asked of the model.
            for server_owned in [
                "question",
                "programs",
                "user_id",
                "id",
                "url",
                "region",
                "enrollment_status",
                "program_types",
                "within_days",
            ] {
                assert!(!keys.contains(&server_owned), "{tool:?} {server_owned}");
            }
            for required in args["required"].as_array().unwrap() {
                assert!(keys.contains(&required.as_str().unwrap()), "{tool:?}");
            }
        }
        // Programme tokens the model may choose: never `selected` (the
        // server derives it from the screen) and never an id.
        assert_eq!(
            variants[1]["properties"]["check_eligibility"]["properties"]["program"]["enum"],
            json!(["context", "saved"])
        );
    }

    #[test]
    fn planner_reply_accepts_decision_envelope_and_v1_shape_only() {
        let reply = PlannerReply::parse(
            r#"{"decision":"plan","steps":[{"tool":"compare","args":{}},{"tool":"deadlines"}]}"#,
        )
        .unwrap();
        assert_eq!(reply.decision, PlannerDecision::Plan);
        assert_eq!(reply.steps[1], json!({"tool":"deadlines","args":{}}));
        let keyed = PlannerReply::parse(
            r#"{"decision":"plan","steps":[{"set_alert":{"enabled":true,"program":"context"}},{"compare":{}}]}"#,
        )
        .unwrap();
        assert_eq!(
            keyed.steps,
            vec![
                json!({"tool":"set_alert","args":{"enabled":true,"program":"context"}}),
                json!({"tool":"compare","args":{}}),
            ]
        );
        assert_eq!(
            PlannerReply::parse(r#"{"decision":"refuse","steps":[]}"#)
                .unwrap()
                .decision,
            PlannerDecision::Refuse
        );
        assert_eq!(
            PlannerReply::parse(r#"{"steps":[]}"#).unwrap().decision,
            PlannerDecision::Refuse,
            "v1 empty plan"
        );
        assert_eq!(
            PlannerReply::parse(r#"{"steps":[{"tool":"deadlines","args":{}}]}"#)
                .unwrap()
                .decision,
            PlannerDecision::Plan
        );
        for bad in [
            r#"{"decision":"execute","steps":[]}"#,
            r#"{"decision":"plan","steps":[{"tool":"execute_sql","args":{}}]}"#,
            r#"{"decision":"plan","steps":[],"answer":"x"}"#,
            r#"{"decision":"plan","steps":[{"tool":"deadlines","args":{},"confirm":false}]}"#,
            r#"{"decision":"plan","steps":[{"tool":"deadlines","args":{}}"#,
            r#"{"decision":"plan","steps":[{"execute_sql":{}}]}"#,
            r#"{"decision":"plan","steps":[{"compare":{},"deadlines":{}}]}"#,
            r#"{"decision":"plan","steps":[{"deadlines":7}]}"#,
            "not json",
        ] {
            assert_eq!(
                PlannerReply::parse(bad),
                Err(PlanError::InvalidJson),
                "{bad}"
            );
        }
    }

    #[test]
    fn canonical_plans_ignore_type_order_and_repeats() {
        let mut plan = Plan::new(vec![
            ToolCall::SearchBenefits(SearchArgs {
                program_types: vec![
                    "corporate_benefit".into(),
                    "campus_benefit".into(),
                    "corporate_benefit".into(),
                ],
                keywords: vec![" 월세 ".into(), "월세".into()],
                ..SearchArgs::default()
            }),
            ToolCall::Deadlines(DeadlineArgs::default()),
            ToolCall::Deadlines(DeadlineArgs::default()),
        ]);
        plan.canonicalize();
        assert_eq!(
            plan.steps,
            vec![
                ToolCall::SearchBenefits(SearchArgs {
                    program_types: vec!["campus_benefit".into(), "corporate_benefit".into()],
                    keywords: vec!["월세".into()],
                    ..SearchArgs::default()
                }),
                ToolCall::Deadlines(DeadlineArgs::default()),
            ]
        );
        assert!(plan.validate().is_ok());
    }

    /// agent-tools-v3: the three new tools take tokens and closed values
    /// only. No URL, date or free-form id from a planner.
    #[test]
    fn v3_tools_take_tokens_and_closed_values_only() {
        for ok in [
            r#"{"steps":[{"tool":"check_official_page","args":{"program":"context"}}]}"#,
            r#"{"steps":[{"tool":"document_issue_link","args":{"document":"주민등록등본"}}]}"#,
            r#"{"steps":[{"tool":"document_issue_link","args":{"program":"context"}}]}"#,
            r#"{"steps":[{"tool":"add_to_calendar","args":{"program":"context"}}]}"#,
            r#"{"steps":[{"tool":"add_to_calendar","args":{"program":"saved","date":"interview"}}]}"#,
        ] {
            assert!(Plan::parse(ok).is_ok(), "{ok}");
        }
        let default_date =
            Plan::parse(r#"{"steps":[{"tool":"add_to_calendar","args":{"program":"context"}}]}"#)
                .unwrap();
        assert_eq!(
            default_date.steps[0],
            ToolCall::AddToCalendar(CalendarArgs {
                program: ProgramRef::Context,
                date: CalendarDate::Deadline,
                days_before: 0
            })
        );
        assert_eq!(
            Plan::parse(r#"{"steps":[{"tool":"add_to_calendar","args":{"program":"context","days_before":1}}]}"#)
                .unwrap()
                .steps[0],
            ToolCall::AddToCalendar(CalendarArgs {
                program: ProgramRef::Context,
                date: CalendarDate::Deadline,
                days_before: 1
            })
        );
        for raw in [
            r#"{"steps":[{"tool":"check_official_page","args":{"program":"context","url":"https://evil.example"}}]}"#,
            r#"{"steps":[{"tool":"add_to_calendar","args":{"program":"context","date":"2026-11-27"}}]}"#,
            r#"{"steps":[{"tool":"add_to_calendar","args":{"program":"context","title":"x"}}]}"#,
        ] {
            assert_eq!(Plan::parse(raw), Err(PlanError::InvalidJson), "{raw}");
        }
        for (raw, why) in [
            (
                r#"{"steps":[{"tool":"document_issue_link","args":{}}]}"#,
                "document",
            ),
            (
                r#"{"steps":[{"tool":"document_issue_link","args":{"document":"https://gov.kr"}}]}"#,
                "document",
            ),
            (
                r#"{"steps":[{"tool":"document_issue_link","args":{"document":"<등본>"}}]}"#,
                "document",
            ),
            (
                r#"{"steps":[{"tool":"check_official_page","args":{"program":"selected"}}]}"#,
                "program",
            ),
            (
                r#"{"steps":[{"tool":"add_to_calendar","args":{"program":"context","days_before":15}}]}"#,
                "days_before",
            ),
        ] {
            assert_eq!(
                Plan::parse(raw),
                Err(PlanError::InvalidArgument(why)),
                "{raw}"
            );
        }
        assert_eq!(
            ToolName::AddToCalendar.permission(),
            Permission::ConfirmWrite
        );
        assert_eq!(ToolName::CheckOfficialPage.permission(), Permission::Read);
        assert_eq!(ToolName::DocumentIssueLink.permission(), Permission::Read);
        // Chat-based value filling was removed by user decision: no such tool.
        assert_eq!(ToolName::parse("fill_form_fields"), None);
        assert_eq!(
            Plan::parse(r#"{"steps":[{"tool":"fill_form_fields","args":{"program":"context"}}]}"#),
            Err(PlanError::InvalidJson)
        );
        assert!(ToolName::V3.iter().all(|t| !ToolName::V1.contains(t)));
    }
}
