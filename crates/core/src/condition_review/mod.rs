//! Reviewed eligibility conditions (`program_condition_reviews`) and the
//! serve-time "조건 확인됨" predicate (plan "verified conditions" §1.1).
//!
//! A review records what reviewers concluded from the notice text they read:
//! the rules it states (rule_engine grammar), its application window, and
//! whether every condition became a rule (`complete`) or some remain
//! uncheckable (`incomplete`, with `remaining_checks`). Like
//! [`crate::applicant_review`], a review is bound to that text by
//! [`text_hash`]; when the source text or its codes change, the stored hash
//! no longer matches and the review stops applying (fail-safe: the row falls
//! back to "확인 필요").
//!
//! [`assess`] decides one recommendation row. It is `verified` only when all
//! of these hold:
//! 1. a current-hash `complete` review exists;
//! 2. a current applicant review says `individual`;
//! 3. the reviewed window is open now ([`window_state`]);
//! 4. the conjunction of the active eligibility rule (source codes), the
//!    reviewed rule and the structural checks is `Pass` with zero unknowns;
//! 5. the row is not `corporate_benefit` (unresolved scope and text-rule
//!    exclusions never reach [`assess`]: the list hides them first);
//! 6. the review is not a `conflict`.
//!
//! A complete current review hides a row only on reviewed facts: its rule
//! is contradicted (`HardRule`) or its window has closed (`NotOpen`).
//! Missing data never hides a row.
//!
//! A current review may also widen or drop a source-code rule
//! ([`overrides`]); such a row is never verified and lists the override's
//! check kind as a remaining check.

pub mod coverage;
pub mod overrides;
pub mod pilot;
pub mod reimport;
pub mod store;

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::applicant_review::{self, ApplicantClass, INDIRECT_NOTE, YOUTH_CENTER_FIELDS};
use crate::models::ExclusionReason;
use crate::rule_engine::{self, EligibilityProfile, RuleEvalResult, RuleNode, RuleVerdict};
use crate::structural::StructuralCheck;

/// 온통청년 code fields the reviewers read, hashed after the text fields.
/// A change of age band, region, job/school/special-field/major/marriage
/// codes, income condition or application-period code makes a review stale.
pub const YOUTH_CENTER_CODE_FIELDS: [&str; 14] = [
    "sprtTrgtMinAge",
    "sprtTrgtMaxAge",
    "zipCd",
    "jobCd",
    "schoolCd",
    "sbizCd",
    "plcyMajorCd",
    "mrgSttsCd",
    "earnCndSeCd",
    "earnMinAmt",
    "earnMaxAmt",
    "earnEtcCn",
    "aplyPrdSeCd",
    "aplyYmd",
];

/// 온통청년 application-period code: 상시 (always open).
pub const YOUTH_ROLLING_CODE: &str = "0057002";
/// 온통청년 application-period code: 마감 (closed).
pub const YOUTH_CLOSED_CODE: &str = "0057003";

/// A rolling window counts as open for this long after the official record
/// last said 상시 ([`store::confirm_window`]).
pub const ROLLING_CONFIRMATION_TTL_DAYS: i64 = 7;

const SEPARATOR: char = '\u{1f}';

/// Placeholders the curation pipeline put in place of contact data
/// (`segment.py` `mask`). Curators quote the masked text, so a quote may
/// contain them. Each stands only for a short run of the kind of text it
/// replaced ([`mask_char`], [`MASK_MAX_CHARS`]), never for a condition.
pub const MASK_TOKENS: [&str; 5] = [
    "[이메일 생략]",
    "[주민번호 생략]",
    "[전화번호 생략]",
    "[이름 생략]",
    "[구분자 생략]",
];
/// Longest original text each mask token may stand for (characters).
const MASK_MAX_CHARS: [usize; 5] = [100, 20, 30, 8, 40];

/// Whether `c` can be part of the text mask token `token` replaced.
fn mask_char(token: usize, c: char) -> bool {
    match token {
        0 => c.is_ascii_alphanumeric() || "._%+-@".contains(c),
        1 => c.is_ascii_digit() || " -".contains(c),
        2 => c.is_ascii_digit() || " -.()+~".contains(c),
        3 => ('가'..='힣').contains(&c) || " :：".contains(c),
        _ => c.is_ascii_alphabetic() || " _-</>".contains(c),
    }
}

/// The reviewed text fields of a source, `None` when condition reviews are
/// not supported for it yet (gov_benefits and local_welfare come with
/// their own golden hashes in a later phase).
pub fn text_fields(source_type: &str) -> Option<&'static [&'static str]> {
    match source_type {
        "youth_center" => Some(&YOUTH_CENTER_FIELDS),
        _ => None,
    }
}

/// The reviewed code fields of a source (hashed after the text fields).
pub fn code_fields(source_type: &str) -> Option<&'static [&'static str]> {
    match source_type {
        "youth_center" => Some(&YOUTH_CENTER_CODE_FIELDS),
        _ => None,
    }
}

/// Every hashed field of a source, text fields first.
pub fn hashed_fields(source_type: &str) -> Option<Vec<&'static str>> {
    Some(
        text_fields(source_type)?
            .iter()
            .chain(code_fields(source_type)?.iter())
            .copied()
            .collect(),
    )
}

/// Lowercase hex SHA-256 of the hashed fields of `raw_payload` joined with a
/// unit separator (`segment.py` `text_hash` over text then code fields).
/// The importer, the admin route and the list all call this.
pub fn text_hash(source_type: &str, raw_payload: &Value) -> Option<String> {
    let joined = hashed_fields(source_type)?
        .into_iter()
        .map(|field| applicant_review::field_text(raw_payload, field))
        .collect::<Vec<_>>()
        .join(&SEPARATOR.to_string());
    Some(format!("{:x}", Sha256::digest(joined.as_bytes())))
}

/// SQL expression projecting only the hashed fields of `raw_payload` (with
/// table alias `alias`, e.g. `"p."`), so a list does not move whole notices.
/// Missing keys become JSON `null`, which [`text_hash`] reads as empty.
pub fn hashed_projection_sql(alias: &str) -> String {
    let pairs = hashed_fields("youth_center")
        .expect("youth_center is supported")
        .into_iter()
        .map(|field| format!("'{field}', {alias}raw_payload->'{field}'"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("CASE WHEN {alias}source_type = 'youth_center' THEN jsonb_build_object({pairs}) END")
}

/// Whether `quote` is a verbatim quote of the current text: a substring of
/// one reviewed text field (5-2000 characters), allowing [`MASK_TOKENS`] in
/// place of short runs of contact data; or a code segment
/// `code:<field>=<value>[;<field>=<value>…]` whose every pair equals the
/// current code value.
pub fn evidence_found(source_type: &str, raw_payload: &Value, quote: &str) -> bool {
    let quote = quote.trim();
    if let Some(code) = quote.strip_prefix("code:") {
        return code_quote_found(source_type, raw_payload, code);
    }
    let unmasked: usize = split_masked(quote)
        .iter()
        .map(|piece| piece.chars().count())
        .sum();
    if unmasked < applicant_review::MIN_EVIDENCE_CHARS
        || quote.chars().count() > applicant_review::MAX_EVIDENCE_CHARS
    {
        return false;
    }
    let Some(fields) = text_fields(source_type) else {
        return false;
    };
    fields.iter().any(|field| {
        let text = applicant_review::field_text(raw_payload, field);
        masked_quote_in(&text, quote)
    })
}

fn code_quote_found(source_type: &str, raw_payload: &Value, code: &str) -> bool {
    let Some(fields) = code_fields(source_type) else {
        return false;
    };
    let pairs: Vec<&str> = code.split(';').collect();
    !pairs.is_empty()
        && pairs.iter().all(|pair| {
            let Some((field, value)) = pair.split_once('=') else {
                return false;
            };
            fields.contains(&field)
                && applicant_review::field_text(raw_payload, field).trim() == value.trim()
        })
}

/// A quote cut at its mask tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Part<'a> {
    Text(&'a str),
    Mask(usize),
}

fn parts(quote: &str) -> Vec<Part<'_>> {
    let mut out = Vec::new();
    let mut rest = quote;
    loop {
        let next = MASK_TOKENS
            .iter()
            .enumerate()
            .filter_map(|(i, token)| rest.find(token).map(|at| (at, i, token.len())))
            .min();
        match next {
            Some((at, token, len)) => {
                if at > 0 {
                    out.push(Part::Text(&rest[..at]));
                }
                out.push(Part::Mask(token));
                rest = &rest[at + len..];
            }
            None => {
                if !rest.is_empty() {
                    out.push(Part::Text(rest));
                }
                return out;
            }
        }
    }
}

/// The unmasked text of a quote.
fn split_masked(quote: &str) -> Vec<&str> {
    parts(quote)
        .into_iter()
        .filter_map(|part| match part {
            Part::Text(text) => Some(text),
            Part::Mask(_) => None,
        })
        .collect()
}

/// `quote` occurs in `text` on one line, each run of mask tokens standing
/// for 1..=max characters that every one of those tokens could have
/// replaced (a mask never stands for ordinary words).
fn masked_quote_in(text: &str, quote: &str) -> bool {
    let parts = parts(quote);
    if !parts.iter().any(|p| matches!(p, Part::Mask(_))) {
        return text.contains(quote);
    }
    // [masks before][text][masks][text]…[masks after]
    let mut groups: Vec<(Vec<usize>, &str)> = Vec::new();
    let mut pending = Vec::new();
    for part in &parts {
        match part {
            Part::Mask(token) => pending.push(*token),
            Part::Text(piece) => groups.push((std::mem::take(&mut pending), piece)),
        }
    }
    let trailing = pending;
    let Some(((leading, first), rest)) = groups.split_first() else {
        return false;
    };
    // One token per gap, and a name only where the mask put one.
    let name_ok = |before: &str| before.ends_with("담당자 ") || before.ends_with("담당 ");
    if leading.len() > 1 || trailing.len() > 1 || leading.contains(&3) {
        return false;
    }
    let mut previous: &str = first;
    for (masks, piece) in rest {
        if masks.len() != 1 || (masks[0] == 3 && !name_ok(previous)) {
            return false;
        }
        previous = piece;
    }
    if trailing == [3] && !name_ok(previous) {
        return false;
    }
    let fits = |mask: usize, gap: &str| {
        let count = gap.chars().count();
        let digits = gap.chars().filter(char::is_ascii_digit).count();
        (1..=MASK_MAX_CHARS[mask]).contains(&count)
            && gap.chars().all(|c| c != '\n' && mask_char(mask, c))
            && match mask {
                0 => gap.contains('@'),
                1 => digits >= 13,
                2 => digits >= 7,
                4 => gap.to_ascii_lowercase().contains("untrusted"),
                _ => true,
            }
    };
    // A leading or trailing mask must border a character it could stand for.
    let borders = |masks: &[usize], c: Option<char>| {
        masks.is_empty() || c.is_some_and(|c| masks.iter().any(|&m| mask_char(m, c)))
    };
    text.match_indices(*first).any(|(start, _)| {
        if !borders(leading, text[..start].chars().next_back()) {
            return false;
        }
        let mut end = start + first.len();
        for (masks, piece) in rest {
            let mask = masks[0];
            let window: String = text[end..]
                .chars()
                .take(MASK_MAX_CHARS[mask] + piece.chars().count())
                .collect();
            let Some(offset) = window.find(*piece) else {
                return false;
            };
            if !fits(mask, &window[..offset]) {
                return false;
            }
            end += offset + piece.len();
        }
        borders(&trailing, text[end..].chars().next())
    })
}

// ── Vocabulary ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewStatus {
    /// Every condition of the text is a rule (or structural, window or
    /// applicant) and every quote checked out.
    Complete,
    /// At least one condition no profile field can answer yet
    /// (`remaining_checks`), or the coverage check failed.
    Incomplete,
    /// A source code and a reviewed rule disagree on the same field.
    Conflict,
}

impl ReviewStatus {
    pub const ALL: [Self; 3] = [Self::Complete, Self::Incomplete, Self::Conflict];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Incomplete => "incomplete",
            Self::Conflict => "conflict",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.as_str() == value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowKind {
    /// Both dates known (Asia/Seoul calendar dates, inclusive).
    Dated,
    /// Continuous intake. Open only while recently confirmed.
    Rolling,
    /// A yearly or periodic pattern without this year's dates.
    Recurring,
    /// A single event date or one-off session.
    Event,
    /// The source says intake is closed (온통청년 0057003).
    Closed,
    /// No usable window.
    Unknown,
}

/// A reviewed application window (`window_json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Window {
    pub kind: WindowKind,
    #[serde(default)]
    pub opens_at: Option<NaiveDate>,
    #[serde(default)]
    pub closes_at: Option<NaiveDate>,
    /// Curator pattern text ("매년 7~8월"); audit only, never served.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recurrence: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seg_id: Option<String>,
    /// `code` (structured source dates), `review` (curators) or
    /// `official_recheck` (the rolling-window confirmation job).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// The window as reviewed, kept when the confirmation job replaced it.
    /// A re-import of the same reviewed window keeps the confirmation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewed: Option<Box<Window>>,
}

impl Window {
    pub fn unknown() -> Self {
        Self {
            kind: WindowKind::Unknown,
            opens_at: None,
            closes_at: None,
            recurrence: None,
            evidence: None,
            seg_id: None,
            source: None,
            reviewed: None,
        }
    }
}

/// Conditions no profile field answers yet (contract `check_kinds`), with
/// their fixed copy. No free text reaches users.
pub const CHECK_KINDS: [(&str, &str); 19] = [
    ("delegated_notice", "세부 자격은 공고문에서 확인이 필요해요"),
    ("gender", "성별 조건 확인이 필요해요"),
    ("residence_duration", "거주 기간 조건 확인이 필요해요"),
    (
        "household_income_amount",
        "가구·부모·부부 소득 금액 조건 확인이 필요해요",
    ),
    ("assets", "재산·자동차 기준 확인이 필요해요"),
    (
        "duplicate_benefit",
        "다른 지원과 중복으로 받을 수 없는 조건 확인이 필요해요",
    ),
    ("prior_participation", "과거 참여·수혜 이력 확인이 필요해요"),
    (
        "employer_or_job_detail",
        "근무처·고용 형태 조건 확인이 필요해요",
    ),
    ("business_detail", "사업장·업종 조건 확인이 필요해요"),
    ("housing_detail", "주택·임대차 조건 확인이 필요해요"),
    ("family_detail", "자녀·가족 조건 확인이 필요해요"),
    ("school_detail", "학교·학년·입학 조건 확인이 필요해요"),
    ("academic_merit", "성적·추천 조건 확인이 필요해요"),
    (
        "certification_or_training",
        "자격증·교육 이수 조건 확인이 필요해요",
    ),
    ("military_service", "병역 조건 확인이 필요해요"),
    ("debt_or_credit", "채무·신용 조건 확인이 필요해요"),
    ("isolation", "고립·은둔 여부 확인이 필요해요"),
    ("target_group_other", "특정 대상 해당 여부 확인이 필요해요"),
    ("other", "추가 자격 조건 확인이 필요해요"),
];

pub fn is_check_kind(kind: &str) -> bool {
    CHECK_KINDS.iter().any(|(k, _)| *k == kind)
}

/// Fixed labels of the remaining-check kinds [`assess`] emits besides
/// [`CHECK_KINDS`].
pub mod labels {
    /// No condition review yet. Distinct from the list's
    /// "자격 조건 미확인" (no notice rule at all): a row may have a notice
    /// rule and still be unreviewed.
    pub const CONDITIONS_UNREVIEWED: &str = "공고의 자격 조건 검토가 필요해요";
    pub const CONDITIONS_CHANGED: &str = "공고 내용이 바뀌어 조건을 다시 확인해야 해요";
    pub const CONFLICT: &str = "공고의 조건 표기가 서로 달라 확인이 필요해요";
    pub const RULE_UNAVAILABLE: &str = "공고 조건을 확인하고 있어요";
    pub const APPLICANT: &str = "개인이 직접 신청하는 혜택인지 확인이 필요해요";
    pub const WINDOW: &str = "신청 기간 확인이 필요해요";
    pub const RULE_MISMATCH: &str = "공고의 일부 조건이 입력하신 정보와 달라 확인이 필요해요";
    pub const PROVIDER_TERMS: &str = "제공처 조건 확인이 필요해요";
    /// A row reserved for a group the profile cannot confirm
    /// ([`crate::audience::membership`]).
    pub const TARGET: &str = "지원 대상 확인이 필요해요";
}

/// One thing left to check before a row can be "조건 확인됨". `label` is a
/// fixed template for `kind`; `question_id` names the profile field that
/// answers it, when one does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemainingCheck {
    pub kind: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question_id: Option<String>,
}

impl RemainingCheck {
    fn new(kind: &str, label: &str) -> Self {
        Self {
            kind: kind.to_owned(),
            label: label.to_owned(),
            question_id: None,
        }
    }

    fn check_kind(kind: &str) -> Self {
        let label = CHECK_KINDS
            .iter()
            .find(|(k, _)| *k == kind)
            .map_or(CHECK_KINDS[18].1, |(_, label)| label);
        Self::new(kind, label)
    }

    fn profile(field: &str) -> Self {
        if !rule_engine::profile_answers(field) {
            let (kind, label) = unanswered_check(field)
                .expect("every field no profile answers names a check (tested)");
            return Self::new(kind, label);
        }
        let field = if field == "region" {
            "region_code"
        } else {
            field
        };
        Self {
            kind: "profile".to_owned(),
            label: profile_label(field),
            question_id: Some(field.to_owned()),
        }
    }
}

/// Fixed copy for a profile field the user has not answered. The list's
/// `missing_checks` uses the same strings. An optional answer names what to
/// enter ([`rule_engine::optional_answer_prompt`]).
pub fn profile_label(field: &str) -> String {
    if let Some((_, label)) = unanswered_check(field) {
        return label.to_owned();
    }
    match field {
        "homeless_household" | "marital_status" | "newlywed" | "children_count"
        | "income_median_pct" => rule_engine::optional_answer_prompt(field).to_string(),
        "income_bracket" | "kosaf_support_bracket" => "소득 기준 확인 필요".to_string(),
        "birth_year" => "생년 정보 확인 필요".to_string(),
        "city_code" => "시·군·구 확인 필요".to_string(),
        other => format!("{} 확인 필요", rule_engine::field_label(other)),
    }
}

/// The check a field no profile answers stands for
/// ([`rule_engine::NO_PROFILE_ANSWER`]), as (kind, label): a coded group is
/// "who the row is for", the same fact as a capping tier or an audience
/// check ([`labels::TARGET`]); a coded gender is the gender check kind. One
/// label per fact, so the list and the card name it once, and no question
/// is asked for it.
fn unanswered_check(field: &str) -> Option<(&'static str, &'static str)> {
    match field {
        "target_group" => Some(("target", labels::TARGET)),
        "gender" => CHECK_KINDS
            .iter()
            .find(|(kind, _)| *kind == "gender")
            .copied(),
        _ => None,
    }
}

/// The served form of a reviewed window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowView {
    pub kind: WindowKind,
    #[serde(default)]
    pub opens_at: Option<NaiveDate>,
    #[serde(default)]
    pub closes_at: Option<NaiveDate>,
    /// When the official record last confirmed a rolling window.
    #[serde(default)]
    pub confirmed_at: Option<DateTime<Utc>>,
    /// The window passes the "open now" clause of the predicate.
    pub open_now: bool,
}

// ── Window ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowState {
    /// Open now on reviewed facts.
    Open,
    /// Closed, or its dates have passed: a reviewed fact that hides the row.
    NotOpen,
    /// Not known to be open (recurring, unknown, not yet open, unconfirmed
    /// rolling): listed as 확인 필요.
    Unverified,
}

/// The reviewed window at `now` (Asia/Seoul calendar dates, inclusive).
/// `rolling` is open only within [`ROLLING_CONFIRMATION_TTL_DAYS`] of the
/// last official confirmation; `recurring`, `event` and `unknown` are never
/// open-now verified.
pub fn window_state(
    window: &Window,
    confirmed_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> WindowState {
    let today = crate::seoul_date(now);
    match window.kind {
        WindowKind::Closed => WindowState::NotOpen,
        WindowKind::Dated => match (window.opens_at, window.closes_at) {
            (_, Some(closes)) if closes < today => WindowState::NotOpen,
            (Some(opens), Some(closes)) if opens <= today && today <= closes => WindowState::Open,
            _ => WindowState::Unverified,
        },
        WindowKind::Event => match window.closes_at.or(window.opens_at) {
            Some(date) if date < today => WindowState::NotOpen,
            _ => WindowState::Unverified,
        },
        WindowKind::Rolling => match confirmed_at {
            // A confirmation from the future (clock skew) is not trusted.
            Some(at)
                if at <= now + chrono::Duration::minutes(5)
                    && now - at <= chrono::Duration::days(ROLLING_CONFIRMATION_TTL_DAYS) =>
            {
                WindowState::Open
            }
            _ => WindowState::Unverified,
        },
        WindowKind::Recurring | WindowKind::Unknown => WindowState::Unverified,
    }
}

// ── Serve predicate ────────────────────────────────────────────────────────

/// A stored review as the list reads it.
#[derive(Debug, Clone)]
pub struct ServeReview {
    pub status: ReviewStatus,
    /// The reviewed leaves (`origin: review`, each with a verbatim quote);
    /// `None` when the review adds none. Only these hide a row.
    pub rule: Option<RuleNode>,
    /// The source-code leaves the reviewers relied on (`origin:
    /// source_codes`). They must pass for `verified`, but a `Fail` never
    /// hides: hiding on codes is the job of the compiled `eligibility_rules`
    /// hard filter, whose mapping may be fixed after a review was written.
    pub code_rule: Option<RuleNode>,
    /// The stored `rule_json` failed to parse: never verified, never hides.
    pub rule_error: bool,
    pub window: Window,
    pub remaining_checks: Vec<String>,
    pub window_confirmed_at: Option<DateTime<Utc>>,
    /// The stored `text_hash` equals the current text's.
    pub current: bool,
    /// Reviewed overrides of source-code rules ([`overrides`]); applied by
    /// the list only while `current`.
    pub code_overrides: Vec<overrides::CodeOverride>,
}

impl ServeReview {
    /// Build from stored columns. A malformed rule or window never raises:
    /// it is recorded so the row is not verified.
    pub fn from_columns(
        status: &str,
        rule_json: Option<&Value>,
        window_json: &Value,
        remaining_checks: Vec<String>,
        window_confirmed_at: Option<DateTime<Utc>>,
        current: bool,
    ) -> Self {
        let (rule, code_rule, rule_error) = match rule_json {
            None | Some(Value::Null) => (None, None, false),
            Some(value) => match rule_engine::parse_rule_json(value) {
                Ok(_) => {
                    let (code, reviewed): (Vec<&Value>, Vec<&Value>) =
                        rule_engine::top_level_conditions(Some(value))
                            .into_iter()
                            .partition(|c| rule_engine::is_source_condition(c));
                    let tree = |nodes: Vec<&Value>| {
                        (!nodes.is_empty())
                            .then(|| {
                                rule_engine::parse_rule_json(&serde_json::json!({ "all": nodes }))
                            })
                            .transpose()
                    };
                    match (tree(reviewed), tree(code)) {
                        (Ok(rule), Ok(code_rule)) => (rule, code_rule, false),
                        _ => (None, None, true),
                    }
                }
                Err(_) => (None, None, true),
            },
        };
        let window: Option<Window> = serde_json::from_value(window_json.clone()).ok();
        let (status, rule_error) = match (ReviewStatus::parse(status), window.is_some()) {
            (Some(status), true) => (status, rule_error),
            _ => (ReviewStatus::Incomplete, true),
        };
        Self {
            status,
            rule,
            code_rule,
            rule_error,
            window: window.unwrap_or_else(Window::unknown),
            remaining_checks,
            window_confirmed_at,
            current,
            code_overrides: Vec::new(),
        }
    }

    /// With the stored `code_overrides`. A malformed value is never applied
    /// (the code rules hide as before) and the row is not verified.
    pub fn with_code_overrides(mut self, value: Option<&Value>) -> Self {
        match overrides::parse_stored(value) {
            Ok(list) => self.code_overrides = list,
            Err(_) => {
                self.code_overrides = Vec::new();
                self.rule_error = true;
            }
        }
        self
    }
}

/// Everything [`assess`] reads about one listed row.
pub struct ServeFacts<'a> {
    /// The row's condition review, current or stale.
    pub review: Option<&'a ServeReview>,
    /// The class of the row's applicant review, only when it is current.
    pub applicant: Option<ApplicantClass>,
    pub program_type: &'a str,
    /// Structural checks of the row (none of them `Fail`: the list already
    /// hid those).
    pub structural: &'a [StructuralCheck],
    /// Evaluations of the active `eligibility_rules` version (hard filter
    /// and ranking rule), source-code conditions included.
    pub source_rules: &'a [&'a RuleEvalResult],
    /// The active rule version exists but could not be evaluated.
    pub source_rule_error: bool,
    /// Check kinds of the review's applied code overrides
    /// ([`overrides::applicable`]). Any one withholds `verified`.
    pub override_checks: &'a [&'a str],
    /// Targets of the applied overrides the profile is listed through
    /// ([`overrides::admitting`]): the code rule would hide it. Each adds an
    /// [`overrides::EXCEPTION_KIND`] check.
    pub override_exceptions: &'a [&'a str],
    /// The days the row counts 만 나이 on ([`rule_engine::AgeBasis::for_program`]).
    pub age_basis: &'a rule_engine::AgeBasis,
    /// Checks the list decides outside the rules above (the coded income
    /// condition, [`crate::income::check`]): any one not `Pass` withholds
    /// `verified` and is a remaining check (audit M2).
    pub list_checks: &'a [ListCheck<'a>],
    pub now: DateTime<Utc>,
}

/// One check the list decided itself, with the fixed copy it shows.
#[derive(Debug, Clone, Copy)]
pub struct ListCheck<'a> {
    pub verdict: RuleVerdict,
    pub kind: &'a str,
    pub label: &'a str,
    /// The profile field that answers it, when one does.
    pub question_id: Option<&'a str>,
}

/// Why a row is not verified (explain trace; not served).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Blocker {
    NoReview,
    StaleReview,
    Incomplete,
    Conflict,
    RuleUnavailable,
    ApplicantMissing,
    ApplicantNotIndividual(ApplicantClass),
    WindowNotOpen,
    WindowUnverified,
    RuleFail,
    /// Unknown leaves of the conjunction, by field.
    RuleUnknown(Vec<String>),
    StructuralUnknown,
    CorporateBenefit,
    /// A code rule was widened or dropped by the review.
    CodeOverride,
    /// The row is reserved for a group the profile cannot confirm.
    TargetUnconfirmed,
    /// A check the list decided itself ([`ServeFacts::list_checks`]) is
    /// not a pass, by kind.
    ListCheckOpen(String),
}

#[derive(Debug, Clone)]
pub struct Assessment {
    pub verified: bool,
    /// Hide the row on a reviewed fact.
    pub hide: Option<ExclusionReason>,
    /// Ordered: review state, code-rule exceptions, applicant, window, profile questions,
    /// uncheckable kinds, provider terms. Empty when `verified`.
    pub remaining_checks: Vec<RemainingCheck>,
    pub window: Option<WindowView>,
    pub blockers: Vec<Blocker>,
    /// The reviewed rule's evaluation (explain trace: passed, failed and
    /// unknown leaves).
    pub reviewed_eval: Option<RuleEvalResult>,
}

/// The §1.1 predicate for one row and profile at `now`.
pub fn assess<P: EligibilityProfile>(profile: &P, facts: &ServeFacts<'_>) -> Assessment {
    let mut blockers = Vec::new();
    let mut hide = None;
    let mut unknown_fields: Vec<String> = Vec::new();
    // The unknown fields a question can settle ([`rule_engine::fields_to_ask`]).
    let mut askable: Vec<String> = Vec::new();
    let usable = facts.review.filter(|r| r.current);
    match facts.review {
        None => blockers.push(Blocker::NoReview),
        Some(r) if !r.current => blockers.push(Blocker::StaleReview),
        Some(_) => {}
    }

    let mut reviewed_eval = None;
    let mut window = None;
    if let Some(review) = usable {
        match review.status {
            ReviewStatus::Complete => {}
            ReviewStatus::Incomplete => blockers.push(Blocker::Incomplete),
            ReviewStatus::Conflict => blockers.push(Blocker::Conflict),
        }
        if review.rule_error {
            blockers.push(Blocker::RuleUnavailable);
        }
        let trusted = review.status == ReviewStatus::Complete && !review.rule_error;
        let state = window_state(&review.window, review.window_confirmed_at, facts.now);
        window = Some(WindowView {
            kind: review.window.kind,
            opens_at: review.window.opens_at,
            closes_at: review.window.closes_at,
            confirmed_at: (review.window.kind == WindowKind::Rolling)
                .then_some(review.window_confirmed_at)
                .flatten(),
            open_now: state == WindowState::Open,
        });
        match state {
            WindowState::Open => {}
            WindowState::NotOpen => {
                blockers.push(Blocker::WindowNotOpen);
                if trusted {
                    hide = Some(ExclusionReason::NotOpen);
                }
            }
            WindowState::Unverified => blockers.push(Blocker::WindowUnverified),
        }
        if let Some(node) = &review.rule {
            let eval = rule_engine::evaluate(profile, node, facts.age_basis);
            match eval.verdict {
                RuleVerdict::Fail => {
                    blockers.push(Blocker::RuleFail);
                    if trusted {
                        hide = hide.or(Some(ExclusionReason::HardRule));
                    }
                }
                RuleVerdict::Unknown | RuleVerdict::Pass => {
                    unknown_fields.extend(eval.unknown_rules.iter().map(|d| d.field.clone()));
                    askable
                        .extend(rule_engine::fields_to_ask(&eval.unknown_rules).map(str::to_owned));
                }
            }
            reviewed_eval = Some(eval);
        }
        if let Some(node) = &review.code_rule {
            let eval = rule_engine::evaluate(profile, node, facts.age_basis);
            if eval.verdict == RuleVerdict::Fail {
                blockers.push(Blocker::RuleFail);
            }
            unknown_fields.extend(eval.unknown_rules.iter().map(|d| d.field.clone()));
            askable.extend(rule_engine::fields_to_ask(&eval.unknown_rules).map(str::to_owned));
        }
    }

    match facts.applicant {
        Some(ApplicantClass::Individual) => {}
        Some(class) => blockers.push(Blocker::ApplicantNotIndividual(class)),
        None => blockers.push(Blocker::ApplicantMissing),
    }

    if facts.source_rule_error {
        blockers.push(Blocker::RuleUnavailable);
    }
    let mut source_fail = false;
    for eval in facts.source_rules {
        if eval.verdict == RuleVerdict::Fail {
            source_fail = true;
        }
        unknown_fields.extend(eval.unknown_rules.iter().map(|d| d.field.clone()));
        askable.extend(rule_engine::fields_to_ask(&eval.unknown_rules).map(str::to_owned));
    }
    if source_fail {
        blockers.push(Blocker::RuleFail);
    }
    let mut structural_unknown = false;
    for check in facts.structural {
        if check.status != RuleVerdict::Pass {
            structural_unknown = true;
            // Who the row is reserved for, with no answer to ask for.
            if check.kind == crate::structural::CheckKind::Audience && check.missing_field.is_none()
            {
                blockers.push(Blocker::TargetUnconfirmed);
            }
            if let Some(field) = check.missing_field {
                let field = if field == "birth_year" {
                    "age".to_owned()
                } else {
                    field.to_owned()
                };
                // A structural check names a field only when it is unanswered.
                askable.push(field.clone());
                unknown_fields.push(field);
            }
        }
    }
    if structural_unknown {
        blockers.push(Blocker::StructuralUnknown);
    }
    dedupe(&mut unknown_fields);
    if !unknown_fields.is_empty() {
        blockers.push(Blocker::RuleUnknown(unknown_fields.clone()));
    }
    if facts.program_type == "corporate_benefit" {
        blockers.push(Blocker::CorporateBenefit);
    }
    if !facts.override_checks.is_empty() {
        blockers.push(Blocker::CodeOverride);
    }
    for check in facts.list_checks {
        if check.verdict != RuleVerdict::Pass {
            blockers.push(Blocker::ListCheckOpen(check.kind.to_owned()));
        }
    }

    let verified = usable.is_some() && blockers.is_empty();
    let remaining_checks = if verified {
        Vec::new()
    } else {
        remaining(
            facts,
            usable,
            &blockers,
            &unknown_fields,
            &askable,
            source_fail,
        )
    };
    Assessment {
        verified,
        hide,
        remaining_checks,
        window,
        blockers,
        reviewed_eval,
    }
}

fn dedupe(values: &mut Vec<String>) {
    let mut seen = std::collections::HashSet::new();
    values.retain(|v| seen.insert(v.clone()));
}

fn remaining(
    facts: &ServeFacts<'_>,
    usable: Option<&ServeReview>,
    blockers: &[Blocker],
    unknown_fields: &[String],
    askable: &[String],
    source_fail: bool,
) -> Vec<RemainingCheck> {
    let mut out = Vec::new();
    let has = |b: &Blocker| blockers.contains(b);
    if has(&Blocker::NoReview) {
        out.push(RemainingCheck::new(
            "conditions_unreviewed",
            labels::CONDITIONS_UNREVIEWED,
        ));
    }
    if has(&Blocker::StaleReview) {
        out.push(RemainingCheck::new(
            "conditions_changed",
            labels::CONDITIONS_CHANGED,
        ));
    }
    if has(&Blocker::Conflict) {
        out.push(RemainingCheck::new("conflict", labels::CONFLICT));
    }
    if has(&Blocker::RuleUnavailable) {
        out.push(RemainingCheck::new(
            "rule_unavailable",
            labels::RULE_UNAVAILABLE,
        ));
    }
    // Listed only on the text's exception to a code rule: said before
    // anything else about the applicant.
    for target in facts.override_exceptions {
        let label = overrides::exception_label(target);
        if !out.iter().any(|c| c.label == label) {
            out.push(RemainingCheck::new(overrides::EXCEPTION_KIND, label));
        }
    }
    // Applicant and window are asked for only once a review exists; without
    // one, "conditions_unreviewed" covers them. A reviewed indirect row
    // always carries its note.
    match facts.applicant {
        Some(ApplicantClass::Indirect) => {
            out.push(RemainingCheck::new("applicant_indirect", INDIRECT_NOTE))
        }
        Some(ApplicantClass::Individual) => {}
        _ if usable.is_some() => out.push(RemainingCheck::new("applicant", labels::APPLICANT)),
        _ => {}
    }
    if has(&Blocker::TargetUnconfirmed) {
        out.push(RemainingCheck::new("target", labels::TARGET));
    }
    if usable.is_some() && (has(&Blocker::WindowUnverified) || has(&Blocker::WindowNotOpen)) {
        out.push(RemainingCheck::new("window", labels::WINDOW));
    }
    for field in unknown_fields {
        let mut check = RemainingCheck::profile(field);
        // An answered field stays a check without a question (verify V-M1).
        if !askable.contains(field) {
            check.question_id = None;
        }
        if !out.iter().any(|c| c.label == check.label) {
            out.push(check);
        }
    }
    for check in facts.list_checks {
        if check.verdict != RuleVerdict::Pass && !out.iter().any(|c| c.label == check.label) {
            out.push(RemainingCheck {
                kind: check.kind.to_owned(),
                label: check.label.to_owned(),
                question_id: check.question_id.map(str::to_owned),
            });
        }
    }
    if source_fail || (usable.is_some() && has(&Blocker::RuleFail)) {
        out.push(RemainingCheck::new("rule_mismatch", labels::RULE_MISMATCH));
    }
    for kind in facts.override_checks {
        if !out.iter().any(|c| c.kind == *kind) {
            out.push(RemainingCheck::check_kind(kind));
        }
    }
    if let Some(review) = usable.filter(|r| r.status == ReviewStatus::Incomplete) {
        let before = out.len();
        for kind in &review.remaining_checks {
            if !out.iter().any(|c| &c.kind == kind) {
                out.push(RemainingCheck::check_kind(kind));
            }
        }
        if out.len() == before && review.remaining_checks.is_empty() {
            out.push(RemainingCheck::check_kind("other"));
        }
    }
    if has(&Blocker::CorporateBenefit) {
        out.push(RemainingCheck::new(
            "provider_terms",
            labels::PROVIDER_TERMS,
        ));
    }
    out
}

#[cfg(test)]
mod overrides_tests;
#[cfg(test)]
mod tests;
