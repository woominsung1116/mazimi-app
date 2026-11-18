/// Rule engine for evaluating eligibility conditions against a UserProfile.
///
/// ## DSL format (stored as JSON in `eligibility_rules.rule_json`)
///
/// ```json
/// {
///   "all": [
///     { "field": "region_code", "op": "in", "value": ["busan", "daegu"], "weight": 1.0 },
///     { "field": "income_bracket", "op": "lte", "value": 5, "weight": 2.0 }
///   ]
/// }
/// ```
///
/// Supported top-level nodes: `all`, `any`, or a single condition object.
/// Conditions may omit `weight`; default is 1.0.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::cmp::Ordering;
use thiserror::Error;

use crate::models::{ProfileInput, Program, UserProfile};

// ── DSL types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleOp {
    Eq,
    Ne,
    In,
    NotIn,
    Between,
    Gte,
    Lte,
    Gt,
    Lt,
    Contains,
    IsTrue,
    IsFalse,
    IsNull,
    IsNotNull,
}

impl RuleOp {
    pub fn as_str(&self) -> &'static str {
        match self {
            RuleOp::Eq => "eq",
            RuleOp::Ne => "ne",
            RuleOp::In => "in",
            RuleOp::NotIn => "not_in",
            RuleOp::Between => "between",
            RuleOp::Gte => "gte",
            RuleOp::Lte => "lte",
            RuleOp::Gt => "gt",
            RuleOp::Lt => "lt",
            RuleOp::Contains => "contains",
            RuleOp::IsTrue => "is_true",
            RuleOp::IsFalse => "is_false",
            RuleOp::IsNull => "is_null",
            RuleOp::IsNotNull => "is_not_null",
        }
    }
}

/// A node in the eligibility rule tree.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RuleNode {
    /// All child rules must pass (logical AND).
    All { all: Vec<RuleNode> },
    /// At least one child rule must pass (logical OR).
    Any { any: Vec<RuleNode> },
    /// A leaf condition evaluated against a single profile field.
    Condition {
        field: String,
        op: RuleOp,
        #[serde(default)]
        value: Value,
        #[serde(default = "default_weight")]
        weight: f64,
        #[serde(default, skip_serializing_if = "Severity::is_hard")]
        severity: Severity,
    },
}

/// What a contradicted condition does. `Hard` (the default) fails it; a
/// `Label` condition names a check the user should confirm but never fails
/// or hides: a contradiction reads as `Unknown`. Used for source codes whose
/// provider flags are too loose to exclude anyone.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    #[default]
    Hard,
    Label,
}

impl Severity {
    fn is_hard(&self) -> bool {
        *self == Severity::Hard
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleVerdict {
    /// Every mandatory branch that has enough information is satisfied.
    Pass,
    /// At least one mandatory branch is explicitly contradicted.
    Fail,
    /// No contradiction is known, but more profile information is required.
    Unknown,
}

impl RuleVerdict {
    pub fn is_eligible(self) -> bool {
        !matches!(self, Self::Fail)
    }
}

#[derive(Debug, Error)]
pub enum RuleEngineError {
    #[error("invalid rule JSON: {0}")]
    InvalidJson(#[from] serde_json::Error),
    #[error("rule group '{0}' must contain at least one child")]
    EmptyGroup(&'static str),
    #[error("unsupported eligibility field: {0}")]
    UnsupportedField(String),
    #[error("weight for field '{0}' must be finite and non-negative")]
    InvalidWeight(String),
    #[error("operator '{operator}' has an invalid value for field '{field}'")]
    InvalidOperatorValue { field: String, operator: String },
}

fn default_weight() -> f64 {
    1.0
}

// ── Evaluation result types ────────────────────────────────────────────────

/// Per-condition evaluation detail, recorded for the explain trace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalDetail {
    pub field: String,
    pub op: String,
    pub expected: String,
    pub actual: Option<String>,
    pub weight: f64,
    /// `Some(true)` = matched, `Some(false)` = failed, `None` = unknown (field absent)
    pub passed: Option<bool>,
}

/// Full result of evaluating a rule tree against a user profile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleEvalResult {
    /// Three-state outcome of the rule tree. A hard filter excludes only Fail.
    pub verdict: RuleVerdict,
    pub matched_rules: Vec<EvalDetail>,
    pub failed_rules: Vec<EvalDetail>,
    /// Conditions where the profile field was `None` / absent — cannot determine pass/fail.
    pub unknown_rules: Vec<EvalDetail>,
    /// `sum(matched_weights) / sum(all_weights) * 100`, capped at 100.0
    pub match_percentage: f64,
    /// Human-readable Korean explanation strings for the recommendation card.
    pub explain_reasons: Vec<String>,
}

// ── Field extraction ───────────────────────────────────────────────────────

/// Profile values consumed by the rule engine. An unanswered field (`None`,
/// in a preview input or a saved profile alike) is unknown.
pub trait EligibilityProfile {
    fn eligibility_value(&self, field: &str) -> Option<Value>;

    /// Every value the field may legitimately take. Ambiguous evidence (만나이
    /// from a bare birth year) yields several candidates: a condition passes
    /// only when all of them pass and fails only when all of them fail.
    fn eligibility_candidates(&self, field: &str) -> Option<Vec<Value>> {
        self.eligibility_value(field).map(|value| vec![value])
    }

    /// The birth year and, when known, the date of birth: the evidence the
    /// `age` field is read from ([`ages`]). Required so a wrapper such as
    /// [`WithAgeBasis`] can never silently lose it.
    fn birth(&self) -> Option<(i32, Option<chrono::NaiveDate>)>;
}

/// `age` candidates of `birth` counted on every day of `basis`.
fn age_values(
    birth: Option<(i32, Option<chrono::NaiveDate>)>,
    basis: &AgeBasis,
) -> Option<Vec<Value>> {
    birth.map(|(year, date)| {
        let (youngest, oldest) = ages(year, date, basis);
        (youngest..=oldest).map(Value::from).collect()
    })
}

/// A profile whose `age` is counted on a programme's [`AgeBasis`] instead of
/// today alone. Every other field is the wrapped profile's.
pub struct WithAgeBasis<'a, P: EligibilityProfile + ?Sized> {
    profile: &'a P,
    basis: AgeBasis,
}

impl<'a, P: EligibilityProfile + ?Sized> WithAgeBasis<'a, P> {
    pub fn new(profile: &'a P, basis: AgeBasis) -> Self {
        Self { profile, basis }
    }

    /// The programme's basis at today's Korean date.
    pub fn for_program(profile: &'a P, program: &Program) -> Self {
        Self::new(profile, AgeBasis::for_program(program, today()))
    }
}

impl<P: EligibilityProfile + ?Sized> EligibilityProfile for WithAgeBasis<'_, P> {
    fn eligibility_value(&self, field: &str) -> Option<Value> {
        match field {
            "age" => single(self.eligibility_candidates(field)),
            other => self.profile.eligibility_value(other),
        }
    }

    fn eligibility_candidates(&self, field: &str) -> Option<Vec<Value>> {
        match field {
            "age" => age_values(self.profile.birth(), &self.basis),
            other => self.profile.eligibility_candidates(other),
        }
    }

    fn birth(&self) -> Option<(i32, Option<chrono::NaiveDate>)> {
        self.profile.birth()
    }
}

impl EligibilityProfile for UserProfile {
    fn eligibility_value(&self, field: &str) -> Option<Value> {
        match field {
            "birth_year" => self.birth_year.map(Value::from),
            "age" => single(self.eligibility_candidates(field)),
            "region_code" | "region" => self.region_code.as_ref().map(|v| Value::from(v.as_str())),
            "city_code" => self.city_code.as_ref().map(|v| Value::from(v.as_str())),
            "school_name" => self.school_name.as_ref().map(|v| Value::from(v.as_str())),
            "school_year" => self.school_year.map(Value::from),
            "school_type" => self.school_type.as_ref().map(|v| Value::from(v.as_str())),
            "enrollment_status" => self
                .enrollment_status
                .as_ref()
                .map(|v| Value::from(v.as_str())),
            "employment_status" => self
                .employment_status
                .as_ref()
                .map(|v| Value::from(v.as_str())),
            "major_group" => self.major_group.as_ref().map(|v| Value::from(v.as_str())),
            "income_bracket" => self.income_bracket.map(Value::from),
            "kosaf_support_bracket" => self.kosaf_support_bracket.map(Value::from),
            "housing_type" => self.housing_type.as_ref().map(|v| Value::from(v.as_str())),
            "household_size" => self.household_size.map(Value::from),
            "age_band" => self.age_band.as_ref().map(|v| Value::from(v.as_str())),
            "has_disability" => self.has_disability.map(Value::Bool),
            "is_multicultural_family" => self.is_multicultural_family.map(Value::Bool),
            "is_low_income_household" => self.is_low_income_household.map(Value::Bool),
            "veteran_family" => self.veteran_family.map(Value::Bool),
            "education_stage" | "major_category" | "income_median_pct" => {
                single(self.eligibility_candidates(field))
            }
            other => self.optional_answers().value(other),
        }
    }

    fn eligibility_candidates(&self, field: &str) -> Option<Vec<Value>> {
        match field {
            "income_median_pct" => self.optional_answers().income_median_candidates(),
            // Only with a basis ([`evaluate`], [`WithAgeBasis`]): a profile
            // alone cannot say on which day a notice counts age.
            "age" => None,
            "education_stage" => education_stages(
                self.school_type.as_deref(),
                self.enrollment_status.as_deref(),
                self.school_year,
            ),
            "major_category" => major_category(self.major_group.as_deref()),
            "school_type" => school_types(self.school_type.as_deref()),
            _ => self.eligibility_value(field).map(|value| vec![value]),
        }
    }

    fn birth(&self) -> Option<(i32, Option<chrono::NaiveDate>)> {
        self.birth_year.map(|year| (year, self.birth_date))
    }
}

impl EligibilityProfile for ProfileInput {
    fn eligibility_value(&self, field: &str) -> Option<Value> {
        match field {
            "birth_year" => Some(Value::from(self.birth_year)),
            "age" => single(self.eligibility_candidates(field)),
            "region_code" | "region" => Some(Value::from(self.region_code.as_str())),
            "city_code" => self.city_code.as_ref().map(|v| Value::from(v.as_str())),
            "school_name" => self.school_name.as_ref().map(|v| Value::from(v.as_str())),
            "school_year" => self.school_year.map(Value::from),
            "school_type" => self.school_type.as_ref().map(|v| Value::from(v.as_str())),
            "enrollment_status" => self
                .enrollment_status
                .as_ref()
                .map(|v| Value::from(v.as_str())),
            "employment_status" => self
                .employment_status
                .as_ref()
                .map(|v| Value::from(v.as_str())),
            "major_group" => self.major_group.as_ref().map(|v| Value::from(v.as_str())),
            "income_bracket" => self.income_bracket.map(Value::from),
            "kosaf_support_bracket" => self.kosaf_support_bracket.map(Value::from),
            "housing_type" => self.housing_type.as_ref().map(|v| Value::from(v.as_str())),
            "household_size" => self.household_size.map(Value::from),
            "age_band" => self.age_band.as_ref().map(|v| Value::from(v.as_str())),
            "has_disability" => self.has_disability.map(Value::Bool),
            "is_multicultural_family" => self.is_multicultural_family.map(Value::Bool),
            "is_low_income_household" => self.is_low_income_household.map(Value::Bool),
            "veteran_family" => self.veteran_family.map(Value::Bool),
            "education_stage" | "major_category" | "income_median_pct" => {
                single(self.eligibility_candidates(field))
            }
            other => self.optional_answers().value(other),
        }
    }

    fn eligibility_candidates(&self, field: &str) -> Option<Vec<Value>> {
        match field {
            "income_median_pct" => self.optional_answers().income_median_candidates(),
            // Only with a basis ([`evaluate`], [`WithAgeBasis`]): a profile
            // alone cannot say on which day a notice counts age.
            "age" => None,
            "education_stage" => education_stages(
                self.school_type.as_deref(),
                self.enrollment_status.as_deref(),
                self.school_year,
            ),
            "major_category" => major_category(self.major_group.as_deref()),
            "school_type" => school_types(self.school_type.as_deref()),
            _ => self.eligibility_value(field).map(|value| vec![value]),
        }
    }

    fn birth(&self) -> Option<(i32, Option<chrono::NaiveDate>)> {
        Some((self.birth_year, self.birth_date))
    }
}

impl UserProfile {
    fn optional_answers(&self) -> OptionalAnswers<'_> {
        OptionalAnswers {
            homeless_household: self.homeless_household,
            marital_status: self.marital_status.as_deref(),
            newlywed: self.newlywed,
            children_count: self.children_count,
            income_median_band: self.income_median_band.as_deref(),
            telecom_carrier: self.telecom_carrier.as_deref(),
            card_issuers: self.card_issuers.as_deref(),
            pay_apps: self.pay_apps.as_deref(),
        }
    }

    /// The holder answers [`crate::audience::membership`] reads.
    pub fn holdings(&self) -> crate::holder::Holdings {
        crate::holder::Holdings::from_answers(
            self.telecom_carrier.as_deref(),
            self.card_issuers.as_deref(),
            self.pay_apps.as_deref(),
        )
    }
}

impl ProfileInput {
    fn optional_answers(&self) -> OptionalAnswers<'_> {
        OptionalAnswers {
            homeless_household: self.homeless_household,
            marital_status: self.marital_status.as_deref(),
            newlywed: self.newlywed,
            children_count: self.children_count,
            income_median_band: self.income_median_band.as_deref(),
            telecom_carrier: self.telecom_carrier.as_deref(),
            card_issuers: self.card_issuers.as_deref(),
            pay_apps: self.pay_apps.as_deref(),
        }
    }

    /// The holder answers [`crate::audience::membership`] reads.
    pub fn holdings(&self) -> crate::holder::Holdings {
        crate::holder::Holdings::from_answers(
            self.telecom_carrier.as_deref(),
            self.card_issuers.as_deref(),
            self.pay_apps.as_deref(),
        )
    }
}

/// 기준 중위소득 bands the profile offers: `(code, lower, upper)` in percent.
/// A band covers `lower < x <= upper`; `gt180` has no upper bound. The user
/// picks one (or `unknown`, 모름); the band is never computed for them.
pub const INCOME_MEDIAN_BANDS: [(&str, f64, f64); 7] = [
    ("le50", 0.0, 50.0),
    ("le60", 50.0, 60.0),
    ("le100", 60.0, 100.0),
    ("le120", 100.0, 120.0),
    ("le150", 120.0, 150.0),
    ("le180", 150.0, 180.0),
    ("gt180", 180.0, 1000.0),
];

/// `unknown` (모름): answered, but it decides nothing.
pub const INCOME_MEDIAN_UNKNOWN: &str = "unknown";

/// `marital_status` answers.
pub const MARITAL_STATUSES: [&str; 2] = ["unmarried", "married"];

/// The optional answers (migration 20261022000201). An unanswered field is
/// `None`, so every condition on it is Unknown.
struct OptionalAnswers<'a> {
    homeless_household: Option<bool>,
    marital_status: Option<&'a str>,
    newlywed: Option<bool>,
    children_count: Option<i32>,
    income_median_band: Option<&'a str>,
    telecom_carrier: Option<&'a str>,
    card_issuers: Option<&'a [String]>,
    pay_apps: Option<&'a [String]>,
}

impl OptionalAnswers<'_> {
    fn value(&self, field: &str) -> Option<Value> {
        match field {
            "homeless_household" => self.homeless_household.map(Value::Bool),
            "marital_status" => self
                .marital_status
                .filter(|s| MARITAL_STATUSES.contains(s))
                .map(Value::from),
            // Unmarried is never newlywed; a married answer without the
            // follow-up stays unknown.
            "newlywed" => match self.marital_status {
                Some("unmarried") => Some(Value::Bool(false)),
                Some("married") => self.newlywed.map(Value::Bool),
                _ => None,
            },
            "children_count" => self
                .children_count
                .filter(|n| (0..=20).contains(n))
                .map(Value::from),
            "telecom_carrier" => self
                .telecom_carrier
                .filter(|c| crate::holder::TELECOM_CARRIERS.contains(c))
                .map(Value::from),
            "card_issuers" => self.card_issuers.map(|codes| Value::from(codes.to_vec())),
            "pay_apps" => self.pay_apps.map(|codes| Value::from(codes.to_vec())),
            _ => None,
        }
    }

    /// Both ends of the chosen band (`lower + 0.5`, `upper`): with integer
    /// thresholds a condition passes only when the whole band satisfies it
    /// and fails only when none of it does, otherwise it stays unknown.
    fn income_median_candidates(&self) -> Option<Vec<Value>> {
        let band = self.income_median_band?;
        let (_, lower, upper) = INCOME_MEDIAN_BANDS
            .iter()
            .find(|(code, _, _)| *code == band)?;
        Some(vec![Value::from(lower + 0.5), Value::from(*upper)])
    }
}

/// `education_stage` is derived from `school_type`, `enrollment_status` and
/// `school_year`; see [`crate::education`].
fn education_stages(
    school_type: Option<&str>,
    enrollment_status: Option<&str>,
    school_year: Option<i32>,
) -> Option<Vec<Value>> {
    crate::education::education_stage_candidates(school_type, enrollment_status, school_year)
        .map(|stages| stages.into_iter().map(Value::from).collect())
}

/// What a `school_type` answer may mean
/// ([`crate::education::school_type_candidates`]).
fn school_types(school_type: Option<&str>) -> Option<Vec<Value>> {
    school_type.map(|answer| {
        crate::education::school_type_candidates(answer)
            .into_iter()
            .map(Value::from)
            .collect()
    })
}

/// `major_category` is the 계열 a `major_group` answer names, if any.
fn major_category(major_group: Option<&str>) -> Option<Vec<Value>> {
    major_group
        .and_then(crate::education::canonical_major_category)
        .map(|category| vec![Value::from(category)])
}

/// A derived field's single value, for callers that read one value: none
/// when the answers allow several.
fn single(candidates: Option<Vec<Value>>) -> Option<Value> {
    match candidates?.as_slice() {
        [one] => Some(one.clone()),
        _ => None,
    }
}

fn current_year() -> i32 {
    crate::seoul_year(chrono::Utc::now())
}

/// With only a birth year, the Korean 만나이 is `연나이 - 1` until the birthday
/// and `연나이` after it. Both interpretations are returned, youngest first.
pub fn age_candidates(birth_year: i32) -> [i32; 2] {
    let calendar_age = current_year().saturating_sub(birth_year);
    [calendar_age.saturating_sub(1), calendar_age]
}

/// Three-state `min_age`/`max_age` gate for a bare birth year. Only a cohort
/// that is outside the range under both interpretations is excluded; the
/// boundary cohort stays visible as unknown so the user can confirm.
pub fn age_gate(birth_year: i32, min_age: Option<i32>, max_age: Option<i32>) -> RuleVerdict {
    age_gate_for(birth_year, None, min_age, max_age)
}

/// [`age_gate`] that uses the exact 만 나이 when the date of birth is known,
/// counted today ([`age_gate_on`] with [`AgeBasis::today`]).
pub fn age_gate_for(
    birth_year: i32,
    birth_date: Option<chrono::NaiveDate>,
    min_age: Option<i32>,
    max_age: Option<i32>,
) -> RuleVerdict {
    age_gate_on(birth_year, birth_date, min_age, max_age, &AgeBasis::today())
}

/// The three-state `min_age`/`max_age` gate over every 만 나이 the user may
/// be on a day of `basis` ([`ages`]): Pass only when all of them are inside
/// the band, Fail only when all are outside, else Unknown.
pub fn age_gate_on(
    birth_year: i32,
    birth_date: Option<chrono::NaiveDate>,
    min_age: Option<i32>,
    max_age: Option<i32>,
    basis: &AgeBasis,
) -> RuleVerdict {
    // A floor above the ceiling admits nobody: a data error, not a reason
    // to hide the programme from everyone.
    if let (Some(min), Some(max)) = (min_age, max_age) {
        if min > max {
            return RuleVerdict::Unknown;
        }
    }
    // Readings are every whole age from youngest to oldest: all are outside
    // the band only when the range lies wholly below or above it.
    let (youngest, oldest) = ages(birth_year, birth_date, basis);
    let below = |age: i32| min_age.is_some_and(|min| age < min);
    let above = |age: i32| max_age.is_some_and(|max| age > max);
    if below(oldest) || above(youngest) {
        RuleVerdict::Fail
    } else if below(youngest) || above(oldest) {
        RuleVerdict::Unknown
    } else {
        RuleVerdict::Pass
    }
}

/// 만 나이 on `today`: the full years since `birth_date` (민법 제158조). A
/// 29 February birthday falls on 1 March in a common year.
pub fn age_on(birth_date: chrono::NaiveDate, today: chrono::NaiveDate) -> i32 {
    use chrono::Datelike;
    let mut age = today.year() - birth_date.year();
    if (today.month(), today.day()) < (birth_date.month(), birth_date.day()) {
        age -= 1;
    }
    age
}

/// The youngest and oldest 만 나이 the evidence allows on the days of
/// `basis`: with a date of birth, the exact ages on its first and last day;
/// with a bare birth year, both readings of each day's year
/// ([`age_candidates`]). Every whole age between them is possible. The
/// birth year is ignored when a date is given.
pub fn ages(
    birth_year: i32,
    birth_date: Option<chrono::NaiveDate>,
    basis: &AgeBasis,
) -> (i32, i32) {
    use chrono::Datelike;
    let (first, last) = (basis.first(), basis.last());
    match birth_date {
        Some(date) => (age_on(date, first), age_on(date, last)),
        None => (
            first.year().saturating_sub(birth_year).saturating_sub(1),
            last.year().saturating_sub(birth_year),
        ),
    }
}

/// The longest window read as one notice cycle ([`AgeBasis::is_notice_cycle`]).
const NOTICE_CYCLE_DAYS: i64 = 366;

/// The days a programme may count 만 나이 on. Notices count it on their own
/// reference day (공고일, 접수 시작일, 마감일, …), which is not stored, so
/// every reading inside the window decides together: the age gate passes
/// only if the user is in the band on all of them and fails only if on none.
/// Never empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgeBasis {
    first: chrono::NaiveDate,
    last: chrono::NaiveDate,
}

impl AgeBasis {
    /// A single reference day.
    pub fn on(day: chrono::NaiveDate) -> Self {
        Self {
            first: day,
            last: day,
        }
    }

    /// Today's Korean date.
    pub fn today() -> Self {
        Self::on(today())
    }

    /// The reference days of `program` seen on `today`: today and, for a
    /// notice cycle ([`Self::is_notice_cycle`]), its opening and closing
    /// Korean days. The closing day is `application_end_at`, else
    /// `deadline_at` (`benefit_valid_until` is a validity, not a window).
    /// Anything else counts on the application day, today: a programme with
    /// no closing date, a window longer than a year, a sentinel end date. A
    /// user whose birthday falls outside these days has one age and is
    /// decided as on today.
    pub fn for_program(program: &Program, today: chrono::NaiveDate) -> Self {
        // A notice that states its day (`programs.age_basis`) counts on that
        // day alone; the year is the notice's (its closing day's, else
        // today's).
        let closing = program.application_end_at.or(program.deadline_at);
        let year = || {
            use chrono::Datelike;
            closing.map_or(today, crate::deadline_date).year()
        };
        let stated = |month, day| chrono::NaiveDate::from_ymd_opt(year(), month, day);
        match program.stated_age_basis() {
            Some(crate::models::NoticeAgeBasis::YearStart) => {
                if let Some(day) = stated(1, 1) {
                    return Self::on(day);
                }
            }
            Some(crate::models::NoticeAgeBasis::YearEnd) => {
                if let Some(day) = stated(12, 31) {
                    return Self::on(day);
                }
            }
            Some(crate::models::NoticeAgeBasis::On(day)) => return Self::on(day),
            Some(crate::models::NoticeAgeBasis::Window) | None => {}
        }
        let mut basis = Self::on(today);
        if let Some(end) = program.application_end_at.or(program.deadline_at) {
            basis.include(crate::deadline_date(end));
            if let Some(start) = program.application_start_at {
                basis.include(crate::seoul_date(start));
            }
        }
        if basis.is_notice_cycle() {
            basis
        } else {
            Self::on(today)
        }
    }

    /// [`Self::for_program`] on today's Korean date.
    pub fn of(program: &Program) -> Self {
        Self::for_program(program, today())
    }

    /// Whether the days span at most a year: one notice cycle, whose
    /// unknown reference day the basis covers. A longer span says nothing
    /// about that day, and never reaches back more than a year.
    pub fn is_notice_cycle(&self) -> bool {
        (self.last - self.first).num_days() <= NOTICE_CYCLE_DAYS
    }

    fn include(&mut self, day: chrono::NaiveDate) {
        self.first = self.first.min(day);
        self.last = self.last.max(day);
    }

    pub fn first(&self) -> chrono::NaiveDate {
        self.first
    }

    pub fn last(&self) -> chrono::NaiveDate {
        self.last
    }
}

/// Today's date in Korea.
fn today() -> chrono::NaiveDate {
    crate::seoul_date(chrono::Utc::now())
}

/// The value domain of a rule field.
#[derive(Debug, Clone, Copy)]
pub enum FieldType {
    /// A number; reviews accept integers in `min..=max`.
    Int(i64, i64),
    /// One of these strings.
    Enum(&'static [&'static str]),
    /// A string the check accepts.
    Text(fn(&str) -> bool),
    Bool,
}

/// One field the rule engine reads.
#[derive(Debug, Clone, Copy)]
pub struct FieldSpec {
    pub name: &'static str,
    /// Korean label; user-facing text never shows `name`.
    pub label: &'static str,
    pub ty: FieldType,
}

const fn field(name: &'static str, label: &'static str, ty: FieldType) -> FieldSpec {
    FieldSpec { name, label, ty }
}

fn city_code_text(v: &str) -> bool {
    v.len() == 5 && v.bytes().all(|b| b.is_ascii_digit())
}

fn short_text(v: &str) -> bool {
    (1..=200).contains(&v.chars().count())
}

/// Every field the rule engine reads, with its label and domain: the one
/// vocabulary of [`parse_rule_json`], [`field_label`] and the condition
/// review importer ([`crate::condition_review::store::is_review_field`]).
/// Both [`EligibilityProfile`] impls answer every name (tested).
pub const FIELDS: &[FieldSpec] = &[
    field("birth_year", "생년", FieldType::Int(1900, 2030)),
    field("age", "생년월일", FieldType::Int(0, 120)),
    field(
        "region_code",
        "거주 지역",
        FieldType::Enum(crate::agent::REGION_CODES),
    ),
    field(
        "region",
        "거주 지역",
        FieldType::Enum(crate::agent::REGION_CODES),
    ),
    field(
        "city_code",
        "거주 시·군·구",
        FieldType::Text(city_code_text),
    ),
    field("school_name", "학교", FieldType::Text(short_text)),
    field("school_year", "학년", FieldType::Int(1, 6)),
    field(
        "school_type",
        "학교 유형",
        FieldType::Enum(&["high_school", "college", "university", "graduate"]),
    ),
    field(
        "enrollment_status",
        "재학 상태",
        FieldType::Enum(crate::agent::ENROLLMENT_CODES),
    ),
    field(
        "employment_status",
        "취업 상태",
        FieldType::Enum(crate::agent::EMPLOYMENT_CODES),
    ),
    field(
        "major_group",
        "전공 계열",
        FieldType::Enum(&[
            "humanities",
            "social_science",
            "business",
            "natural_science",
            "engineering",
            "arts_sports",
            "agriculture",
            "medical",
            "education",
            "other",
        ]),
    ),
    field("income_bracket", "소득 구간", FieldType::Int(0, 10)),
    field(
        "kosaf_support_bracket",
        "학자금 지원 구간",
        FieldType::Int(0, 10),
    ),
    field(
        "housing_type",
        "주거 형태",
        FieldType::Enum(&[
            "parents",
            "monthly_rent",
            "jeonse",
            "own",
            "dormitory",
            "other",
        ]),
    ),
    field("household_size", "가구원 수", FieldType::Int(1, 20)),
    field("age_band", "연령대", FieldType::Text(short_text)),
    field("has_disability", "장애 여부", FieldType::Bool),
    field(
        "is_multicultural_family",
        "다문화가정 여부",
        FieldType::Bool,
    ),
    field(
        "is_low_income_household",
        "저소득 가구 여부",
        FieldType::Bool,
    ),
    field("veteran_family", "보훈 가족 여부", FieldType::Bool),
    // Derived from school type, enrollment and year (`crate::education`);
    // the domains are that module's, so the two never drift.
    field(
        "education_stage",
        "학력",
        FieldType::Enum(crate::education::EDUCATION_STAGES),
    ),
    field(
        "major_category",
        "전공 계열",
        FieldType::Enum(crate::education::MAJOR_CATEGORIES),
    ),
    field("homeless_household", "무주택 여부", FieldType::Bool),
    field(
        "marital_status",
        "혼인 여부",
        FieldType::Enum(&MARITAL_STATUSES),
    ),
    field("newlywed", "혼인 여부", FieldType::Bool),
    // Source-coded restrictions no profile answers ([`NO_PROFILE_ANSWER`]):
    // a condition on them is always Unknown and names the check.
    field("gender", "성별", FieldType::Enum(crate::models::GENDERS)),
    field(
        "target_group",
        "지원 대상",
        FieldType::Enum(crate::models::TARGET_GROUPS),
    ),
    field("children_count", "자녀 수", FieldType::Int(0, 20)),
    field(
        "income_median_pct",
        "기준 중위소득 구간",
        FieldType::Int(0, 1000),
    ),
    // Holder answers ([`crate::holder`]): `card_issuers` and `pay_apps`
    // hold several codes of their domain; a leaf tests one with `contains`.
    field(
        "telecom_carrier",
        "통신사",
        FieldType::Enum(crate::holder::TELECOM_CARRIERS),
    ),
    field(
        "card_issuers",
        "보유 카드",
        FieldType::Enum(crate::holder::CARD_ISSUERS),
    ),
    field(
        "pay_apps",
        "간편결제",
        FieldType::Enum(crate::holder::PAY_APPS),
    ),
];

/// Fields of [`FIELDS`] that no profile answers: restrictions a source
/// publishes as codes (gender, 군인, 농업인, 중소기업 재직 …) with no profile
/// question. Neither [`EligibilityProfile`] impl answers them (tested).
pub const NO_PROFILE_ANSWER: &[&str] = &["gender", "target_group"];

/// The profile fields an evaluation's unknown leaves ask for: the field of
/// every unknown leaf the profile left unanswered (`actual` is `None`),
/// except those no profile answers ([`NO_PROFILE_ANSWER`]). A leaf left
/// undecided by an answer the profile holds ("대학 (전문대 포함)" on a
/// 전문대-only row, a band across a bound) stays a check with its label
/// and is never a question: answering it again cannot change the result
/// (verify V-M1). The one reader of unknown leaves as questions: the list,
/// the card and the agent's question all go through it.
pub fn fields_to_ask(unknown: &[EvalDetail]) -> impl Iterator<Item = &str> {
    unknown
        .iter()
        .filter(|detail| detail.actual.is_none())
        .map(|detail| detail.field.as_str())
        .filter(|field| profile_answers(field))
}

/// Whether a profile question can settle `field` ([`NO_PROFILE_ANSWER`]).
pub fn profile_answers(field: &str) -> bool {
    !NO_PROFILE_ANSWER.contains(&field)
}

/// The spec of a supported field.
pub fn field_spec(field: &str) -> Option<&'static FieldSpec> {
    FIELDS.iter().find(|spec| spec.name == field)
}

/// The domain of a supported field.
pub fn field_type(field: &str) -> Option<FieldType> {
    field_spec(field).map(|spec| spec.ty)
}

/// Korean label for a rule field. User-facing text must never show the raw
/// field name; the API's missing-check labels share this table.
pub fn field_label(field: &str) -> &'static str {
    field_spec(field).map_or("추가 자격", |spec| spec.label)
}

// ── Operator evaluation ────────────────────────────────────────────────────

/// Equality of two scalar values; numbers compare as numbers (2 == 2.0).
fn same(a: &Value, b: &Value) -> bool {
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) => x == y,
        _ => a == b,
    }
}

/// Returns `true` if `actual` satisfies `op` against `expected`.
fn eval_op(actual: &Value, op: &RuleOp, expected: &Value) -> bool {
    let listed = || {
        expected
            .as_array()
            .is_some_and(|items| items.iter().any(|item| same(actual, item)))
    };
    match op {
        RuleOp::Eq => same(actual, expected),
        RuleOp::Ne => !same(actual, expected),
        RuleOp::In => listed(),
        RuleOp::NotIn => expected.is_array() && !listed(),
        RuleOp::Between => {
            if let Some(arr) = expected.as_array() {
                if arr.len() == 2 {
                    return matches!(
                        (
                            compare_values(actual, &arr[0]),
                            compare_values(actual, &arr[1])
                        ),
                        (
                            Some(Ordering::Greater | Ordering::Equal),
                            Some(Ordering::Less | Ordering::Equal)
                        )
                    );
                }
            }
            false
        }
        RuleOp::Gte => matches!(
            compare_values(actual, expected),
            Some(Ordering::Greater | Ordering::Equal)
        ),
        RuleOp::Lte => matches!(
            compare_values(actual, expected),
            Some(Ordering::Less | Ordering::Equal)
        ),
        RuleOp::Gt => compare_values(actual, expected) == Some(Ordering::Greater),
        RuleOp::Lt => compare_values(actual, expected) == Some(Ordering::Less),
        RuleOp::Contains => match (actual, expected) {
            (Value::String(s), Value::String(sub)) => s.contains(sub.as_str()),
            // A set answer (`card_issuers`) contains one code.
            (Value::Array(items), Value::String(code)) => items
                .iter()
                .any(|item| item.as_str() == Some(code.as_str())),
            (Value::Array(arr), v) => arr.contains(v),
            _ => false,
        },
        RuleOp::IsTrue => actual == &Value::Bool(true),
        RuleOp::IsFalse => actual == &Value::Bool(false),
        RuleOp::IsNull => actual.is_null(),
        RuleOp::IsNotNull => !actual.is_null(),
    }
}

/// Numeric or string comparison. Incomparable values never satisfy an ordered
/// operator; this avoids the old `i32::MAX` fallback accidentally passing GTE.
fn compare_values(a: &Value, b: &Value) -> Option<Ordering> {
    match (a.as_f64(), b.as_f64()) {
        (Some(av), Some(bv)) => av.partial_cmp(&bv),
        _ => match (a.as_str(), b.as_str()) {
            (Some(as_), Some(bs)) => Some(as_.cmp(bs)),
            _ => None,
        },
    }
}

// ── Korean explain template ───────────────────────────────────────────────

fn explain_unknown(field: &str) -> String {
    match field {
        "income_bracket" | "kosaf_support_bracket" => {
            "소득 구간 정보가 필요합니다 (입력 시 더 정확한 추천이 가능해요)".to_string()
        }
        "enrollment_status" => "재학 여부를 입력하면 더 정확한 추천이 가능해요".to_string(),
        "employment_status" => "취업 상태를 입력하면 더 정확한 추천이 가능해요".to_string(),
        "region_code" | "region" => "거주 지역 정보가 필요합니다".to_string(),
        "birth_year" => "생년 정보가 필요합니다".to_string(),
        "school_type" => "학교 유형을 입력하면 더 정확한 추천이 가능해요".to_string(),
        "major_group" | "major_category" => {
            "전공 계열을 입력하면 더 정확한 추천이 가능해요".to_string()
        }
        "education_stage" => {
            "학교 유형·학적 상태·학년을 입력하면 학력 조건을 확인할 수 있어요".to_string()
        }
        "housing_type" => "주거 형태를 입력하면 더 정확한 추천이 가능해요".to_string(),
        "age" => "생년월일을 확인하면 연령 조건을 정확히 판단할 수 있어요".to_string(),
        "homeless_household" | "marital_status" | "newlywed" | "children_count"
        | "income_median_pct" => optional_answer_prompt(field).to_string(),
        other => format!("{} 정보가 필요합니다", field_label(other)),
    }
}

/// What to enter to settle a condition on an optional answer. Shared with the
/// list's remaining-check labels.
pub fn optional_answer_prompt(field: &str) -> &'static str {
    match field {
        "homeless_household" => "무주택 여부를 입력하면 확인할 수 있어요",
        "marital_status" => "혼인 여부를 입력하면 확인할 수 있어요",
        "newlywed" => "혼인 여부와 신혼(7년 이내) 여부를 입력하면 확인할 수 있어요",
        "children_count" => "자녀 수를 입력하면 확인할 수 있어요",
        "income_median_pct" => "기준 중위소득 구간을 입력하면 확인할 수 있어요",
        _ => "추가 정보를 입력하면 확인할 수 있어요",
    }
}

// ── Core evaluation logic ─────────────────────────────────────────────────

/// Internal accumulator passed through the recursive walk.
struct EvalAccumulator {
    matched: Vec<EvalDetail>,
    failed: Vec<EvalDetail>,
    unknown: Vec<EvalDetail>,
}

impl EvalAccumulator {
    fn new() -> Self {
        Self {
            matched: Vec::new(),
            failed: Vec::new(),
            unknown: Vec::new(),
        }
    }
}

/// Recursively walk `node`, recording every leaf condition into `acc` and
/// returning the three-state outcome of the complete subtree. All leaves are
/// evaluated so ranking and explanation data remain deterministic.
fn walk<P: EligibilityProfile>(
    profile: &P,
    node: &RuleNode,
    acc: &mut EvalAccumulator,
) -> RuleVerdict {
    match node {
        RuleNode::All { all } => {
            let results: Vec<RuleVerdict> =
                all.iter().map(|child| walk(profile, child, acc)).collect();
            if results.contains(&RuleVerdict::Fail) {
                RuleVerdict::Fail
            } else if results.contains(&RuleVerdict::Unknown) {
                RuleVerdict::Unknown
            } else {
                RuleVerdict::Pass
            }
        }
        RuleNode::Any { any } => {
            // The alternatives' unknowns are read into their own trace: a
            // group that passes asks nothing about an alternative it did not
            // need (audit L1). Matched and failed leaves stay for the score.
            let mut group = EvalAccumulator::new();
            let results: Vec<RuleVerdict> = any
                .iter()
                .map(|child| walk(profile, child, &mut group))
                .collect();
            let verdict = if results.contains(&RuleVerdict::Pass) {
                RuleVerdict::Pass
            } else if results.contains(&RuleVerdict::Unknown) {
                RuleVerdict::Unknown
            } else {
                RuleVerdict::Fail
            };
            acc.matched.append(&mut group.matched);
            acc.failed.append(&mut group.failed);
            if verdict != RuleVerdict::Pass {
                acc.unknown.append(&mut group.unknown);
            }
            verdict
        }
        RuleNode::Condition {
            field,
            op,
            value: expected,
            weight,
            severity,
        } => {
            let candidates = profile
                .eligibility_candidates(field)
                .filter(|values| !values.is_empty());
            let actual = candidates.as_deref().map(|values| match values {
                [single] => single.to_string(),
                _ => values
                    .iter()
                    .map(Value::to_string)
                    .collect::<Vec<_>>()
                    .join("~"),
            });
            let mut detail = EvalDetail {
                field: field.clone(),
                op: op.as_str().to_string(),
                expected: expected.to_string(),
                actual,
                weight: *weight,
                passed: None,
            };

            let Some(candidates) = candidates else {
                // Field not present on profile — unknown
                acc.unknown.push(detail);
                return RuleVerdict::Unknown;
            };
            let passing = candidates
                .iter()
                .filter(|value| eval_op(value, op, expected))
                .count();
            if passing == candidates.len() {
                detail.passed = Some(true);
                acc.matched.push(detail);
                RuleVerdict::Pass
            } else if passing == 0 && *severity == Severity::Hard {
                detail.passed = Some(false);
                acc.failed.push(detail);
                RuleVerdict::Fail
            } else {
                // Some readings of the evidence pass and some fail (만나이 from
                // a bare birth year): ask the user, never decide for them.
                acc.unknown.push(detail);
                RuleVerdict::Unknown
            }
        }
    }
}

// ── Public API ────────────────────────────────────────────────────────────

/// Evaluate a rule tree against a user profile.
///
/// Every leaf condition is categorised into matched / failed / unknown.
/// `match_percentage` = sum(matched weights) / sum(all weights) × 100.
///
/// `age` is counted on the days of `basis` (a programme's
/// [`AgeBasis::for_program`]); the profile alone never answers it.
pub fn evaluate<P: EligibilityProfile + ?Sized>(
    profile: &P,
    rules: &RuleNode,
    basis: &AgeBasis,
) -> RuleEvalResult {
    let profile = WithAgeBasis::new(profile, basis.clone());
    let mut acc = EvalAccumulator::new();
    let verdict = walk(&profile, rules, &mut acc);

    let total_weight: f64 = acc
        .matched
        .iter()
        .chain(acc.failed.iter())
        .chain(acc.unknown.iter())
        .map(|d| d.weight)
        .sum();

    let matched_weight: f64 = acc.matched.iter().map(|d| d.weight).sum();

    let match_percentage = if total_weight > 0.0 {
        (matched_weight / total_weight * 100.0).min(100.0)
    } else {
        0.0
    };

    // Build explain strings
    let mut explain_reasons: Vec<String> = Vec::new();

    for detail in &acc.matched {
        explain_reasons.push({
            match detail.field.as_str() {
                "region_code" | "region" => format!(
                    "{}{} 맞음",
                    detail
                        .actual
                        .as_deref()
                        .map(crate::region_label)
                        .unwrap_or("기타"),
                    crate::RESIDENCE_REASON_STEM
                ),
                "income_bracket" | "kosaf_support_bracket" => format!(
                    "소득 구간 {} 조건 맞음",
                    detail.actual.as_deref().unwrap_or("-")
                ),
                "enrollment_status" => format!(
                    "{} ({})",
                    crate::ENROLLMENT_REASON_STEM,
                    detail
                        .actual
                        .as_deref()
                        .map(crate::enrollment_status_label)
                        .unwrap_or("기타")
                ),
                "employment_status" => format!(
                    "취업 상태 조건 맞음 ({})",
                    detail
                        .actual
                        .as_deref()
                        .map(crate::employment_status_label)
                        .unwrap_or("기타")
                ),
                "has_disability" => "장애인 해당 조건 맞음".to_string(),
                "homeless_household" => "무주택 조건 맞음".to_string(),
                "marital_status" | "newlywed" => "혼인 조건 맞음".to_string(),
                "children_count" => "자녀 수 조건 맞음".to_string(),
                "income_median_pct" => "기준 중위소득 조건 맞음".to_string(),
                "is_multicultural_family" => "다문화가정 조건 맞음".to_string(),
                "is_low_income_household" => "저소득 가구 조건 맞음".to_string(),
                "veteran_family" => "보훈 가족 조건 맞음".to_string(),
                "birth_year" => format!(
                    "연령 조건 맞음 (출생연도 {})",
                    detail.actual.as_deref().unwrap_or("-")
                ),
                "age" => format!(
                    "연령 조건 맞음 (만 {}세)",
                    detail.actual.as_deref().unwrap_or("-")
                ),
                "school_name" => format!(
                    "학교 조건 맞음 ({})",
                    detail
                        .actual
                        .as_deref()
                        .map(|value| value.trim_matches('"'))
                        .unwrap_or("-")
                ),
                // Codes are shown by label; an unrecognised value adds nothing.
                // The first candidate is the stored answer itself.
                "school_type" => match detail
                    .actual
                    .as_deref()
                    .and_then(|actual| actual.split('~').next())
                    .and_then(crate::education::canonical_school_type)
                {
                    Some(code) => format!(
                        "학교 유형({}) 조건 맞음",
                        crate::education::school_type_label(code)
                    ),
                    None => "학교 유형 조건 맞음".to_string(),
                },
                "major_group" => match detail
                    .actual
                    .as_deref()
                    .and_then(crate::education::canonical_major_category)
                {
                    Some(code) => format!(
                        "전공 계열({}) 조건 맞음",
                        crate::education::major_category_label(code)
                    ),
                    None => "전공 계열 조건 맞음".to_string(),
                },
                "education_stage" => "학력 조건 맞음".to_string(),
                "major_category" => format!(
                    "전공 계열 조건 맞음 ({})",
                    detail
                        .actual
                        .as_deref()
                        .map(crate::education::major_category_label)
                        .unwrap_or("-")
                ),
                other => format!("{} 조건 맞음", field_label(other)),
            }
        });
    }

    for detail in &acc.unknown {
        explain_reasons.push(match (&detail.actual, detail.field.as_str()) {
            // Answered, but the answer alone cannot settle it (several
            // readings, or a label condition the answer contradicts).
            (Some(_), field) if field != "age" => {
                format!("{} 조건은 공고에서 확인해 주세요", field_label(field))
            }
            (_, field) => explain_unknown(field),
        });
    }

    RuleEvalResult {
        verdict,
        matched_rules: acc.matched,
        failed_rules: acc.failed,
        unknown_rules: acc.unknown,
        match_percentage,
        explain_reasons,
    }
}

// ── Source-code conditions ──────────────────────────────────────────────────

/// `eligibility_rules.created_by` of a version the ingestion writes from a
/// source's structured codes.
pub const SOURCE_RULE_AUTHOR: &str = "ingestion:source_codes";

/// Marks a top-level rule condition as compiled from a source's structured
/// codes, so the next sync can replace exactly its own conditions. The rule
/// engine ignores unknown keys on a condition.
pub const SOURCE_ORIGIN_KEY: &str = "origin";
pub const SOURCE_ORIGIN: &str = "source_codes";

/// Top-level conditions of a rule tree: the members of a plain `all`, else
/// the tree itself; nothing for NULL.
pub fn top_level_conditions(tree: Option<&Value>) -> Vec<&Value> {
    match tree {
        None | Some(Value::Null) => Vec::new(),
        Some(tree) => match tree.as_object() {
            Some(object) if object.len() == 1 && object.get("all").is_some_and(Value::is_array) => {
                object["all"].as_array().into_iter().flatten().collect()
            }
            _ => vec![tree],
        },
    }
}

/// Whether a top-level condition was compiled from source codes.
pub fn is_source_condition(condition: &Value) -> bool {
    condition.get(SOURCE_ORIGIN_KEY).and_then(Value::as_str) == Some(SOURCE_ORIGIN)
}

/// Whether a rule version carries no condition beyond the source's own
/// codes: it checks nothing a reviewer read in the notice.
pub fn source_codes_only(
    rule_json: &Value,
    hard_filter_json: Option<&Value>,
    created_by: Option<&str>,
) -> bool {
    let conditions: Vec<&Value> = top_level_conditions(Some(rule_json))
        .into_iter()
        .chain(top_level_conditions(hard_filter_json))
        .collect();
    if conditions.is_empty() {
        return created_by == Some(SOURCE_RULE_AUTHOR);
    }
    conditions.into_iter().all(source_only)
}

/// A condition marked as source codes, or an `all`/`any` group whose leaves
/// all are (a group carries no `origin` of its own). An empty group is not.
/// Other keys of a group are ignored, as [`RuleNode`] parsing ignores them.
fn source_only(condition: &Value) -> bool {
    if is_source_condition(condition) {
        return true;
    }
    let group = condition.as_object().and_then(|object| {
        (object.get("all").and_then(Value::as_array))
            .or_else(|| object.get("any").and_then(Value::as_array))
    });
    group.is_some_and(|children| !children.is_empty() && children.iter().all(source_only))
}

/// Parse and validate a rule tree before it can influence publication or
/// recommendations. Configuration mistakes fail closed instead of looking
/// like missing user data.
pub fn parse_rule_json(value: &Value) -> Result<RuleNode, RuleEngineError> {
    let node: RuleNode = serde_json::from_value(value.clone())?;
    validate_rule_node(&node)?;
    Ok(node)
}

fn validate_rule_node(node: &RuleNode) -> Result<(), RuleEngineError> {
    match node {
        RuleNode::All { all } => {
            if all.is_empty() {
                return Err(RuleEngineError::EmptyGroup("all"));
            }
            for child in all {
                validate_rule_node(child)?;
            }
        }
        RuleNode::Any { any } => {
            if any.is_empty() {
                return Err(RuleEngineError::EmptyGroup("any"));
            }
            for child in any {
                validate_rule_node(child)?;
            }
        }
        RuleNode::Condition {
            field,
            op,
            value,
            weight,
            ..
        } => {
            let Some(ty) = field_type(field) else {
                return Err(RuleEngineError::UnsupportedField(field.clone()));
            };
            if !weight.is_finite() || *weight < 0.0 {
                return Err(RuleEngineError::InvalidWeight(field.clone()));
            }
            // A scalar of the field's JSON type: a number for an Int field,
            // a string for an Enum/Text field, a boolean for a Bool field.
            // The domain itself (range, enum) is the review importer's check.
            let typed = |v: &Value| match ty {
                FieldType::Int(..) => v.as_f64().is_some_and(f64::is_finite),
                FieldType::Enum(_) | FieldType::Text(_) => v.is_string(),
                FieldType::Bool => v.is_boolean(),
            };
            let all_typed = |v: &Value| v.as_array().is_some_and(|items| items.iter().all(typed));
            let numeric = matches!(ty, FieldType::Int(..));
            let valid_value = match op {
                RuleOp::In | RuleOp::NotIn => {
                    value.as_array().is_some_and(|items| !items.is_empty()) && all_typed(value)
                }
                RuleOp::Between => {
                    numeric
                        && value.as_array().is_some_and(|items| items.len() == 2)
                        && all_typed(value)
                }
                RuleOp::Gte | RuleOp::Lte | RuleOp::Gt | RuleOp::Lt => numeric && typed(value),
                RuleOp::Contains => match ty {
                    FieldType::Int(..) | FieldType::Bool => false,
                    _ => value.is_string(),
                },
                RuleOp::Eq | RuleOp::Ne => typed(value),
                RuleOp::IsTrue | RuleOp::IsFalse => matches!(ty, FieldType::Bool),
                RuleOp::IsNull | RuleOp::IsNotNull => true,
            };
            if !valid_value {
                return Err(RuleEngineError::InvalidOperatorValue {
                    field: field.clone(),
                    operator: op.as_str().to_string(),
                });
            }
        }
    }
    Ok(())
}

// ── Scoring formula ───────────────────────────────────────────────────────

/// Compute the final recommendation score from the rule evaluation result and the program.
///
/// Formula (spec §9-2):
/// ```text
/// score =
///     적합도     × 0.40  (rule match_percentage → 0–40 pts)
///   + 혜택 크기  × 0.20  (benefit score → 0–20 pts)
///   + 마감 임박  × 0.15  (deadline urgency → 0–15 pts)
///   + 지역 우선순위 × 0.10 (region priority → 0–10 pts)
///   + 유저 선호  × 0.10  (placeholder: always 5 pts until preference engine is built)
///   - 서류 복잡도 × 0.05 (placeholder: always 0 until document scoring is built)
/// ```
///
/// Returns a score in [0.0, 100.0].
pub fn compute_final_score(rule_result: &RuleEvalResult, program: &Program) -> f64 {
    let region_targeted = program.regions.as_ref().is_some_and(|r| !r.is_empty());
    compute_final_score_with(
        rule_result,
        program,
        RankSignals {
            region_confirmed: region_targeted,
            relevance: 5.0,
        },
        chrono::Utc::now(),
    )
}

/// Per-user ranking inputs [`compute_final_score_with`] takes in place of the
/// region and preference placeholders.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RankSignals {
    /// The region priority (+10) applies: the programme's district matched
    /// the user's, or it is confirmed sido-wide for the user's sido. A
    /// sido-level match alone is not enough while districts collapse to sidos.
    pub region_confirmed: bool,
    /// 0–10: youth-target match, persona category, district match.
    pub relevance: f64,
}

/// [`compute_final_score`] with the user's region confirmation and relevance
/// instead of "any region-targeted row" and the constant preference.
pub fn compute_final_score_with(
    rule_result: &RuleEvalResult,
    program: &Program,
    signals: RankSignals,
    now: chrono::DateTime<chrono::Utc>,
) -> f64 {
    // ① 적합도 (0–40)
    let eligibility_score = rule_result.match_percentage * 0.40;

    // ② 혜택 크기 (0–20)
    //    Normalise to a monthly equivalent for comparison.
    //    A corporate offer pays no support money (crate::pays_support).
    let paid = crate::pays_support(&program.program_type);
    let amount = |value: Option<i32>| if paid { value.unwrap_or(0) as f64 } else { 0.0 };
    let monthly = amount(program.benefit_amount_monthly);
    let semester = amount(program.benefit_amount_semester) / 6.0;
    let once = amount(program.benefit_amount_once) / 12.0;
    let max_monthly_equiv = monthly.max(semester).max(once);
    let benefit_score: f64 = if max_monthly_equiv >= 500_000.0 {
        20.0
    } else if max_monthly_equiv >= 300_000.0 {
        17.0
    } else if max_monthly_equiv >= 100_000.0 {
        13.0
    } else if max_monthly_equiv > 0.0 {
        8.0
    } else {
        0.0
    };

    // ③ 마감 임박 (0–15)
    let deadline_score: f64 = match program.effective_deadline() {
        None => 10.0, // always open — stable, moderately urgent
        Some(deadline) => {
            let days_left = crate::days_until(deadline, now);
            if days_left < 0 {
                0.0 // expired
            } else if days_left <= 7 {
                15.0 // very urgent
            } else if days_left <= 30 {
                12.0
            } else if days_left <= 60 {
                9.0
            } else {
                6.0
            }
        }
    };

    // ④ 지역 우선순위 (0–10)
    //    Only a confirmed match is "more targeted"; an unconfirmed regional
    //    row ranks like a nationwide one.
    let region_score: f64 = if signals.region_confirmed { 10.0 } else { 5.0 };

    // ⑤ 유저 관련도 (0–10)
    let preference_score: f64 = signals.relevance.clamp(0.0, 10.0);

    // ⑥ 서류 복잡도 (-5–0) — placeholder until document scoring is built
    //    Currently 0 (no penalty). Will be negative once doc count is evaluated.
    let document_penalty: f64 = 0.0;

    let total =
        eligibility_score + benefit_score + deadline_score + region_score + preference_score
            - document_penalty;

    total.clamp(0.0, 100.0)
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use serde_json::json;
    use uuid::Uuid;

    /// Audit L1: an `any` group that passes leaves no question about the
    /// alternatives it did not need.
    #[test]
    fn a_passed_any_group_leaves_its_other_alternatives_unasked() {
        let mut profile = make_profile("busan", None, None);
        profile.employment_status = Some("unemployed".into());
        let rule = parse_rule_json(&json!({"all": [{"any": [
            {"field": "employment_status", "op": "in", "value": ["unemployed"]},
            {"field": "enrollment_status", "op": "eq", "value": "enrolled"}
        ]}]}))
        .unwrap();
        let e = evaluate(&profile, &rule, &AgeBasis::today());
        assert_eq!(e.verdict, RuleVerdict::Pass);
        assert!(e.unknown_rules.is_empty(), "{:?}", e.unknown_rules);
        assert_eq!(e.matched_rules.len(), 1);
        // An undecided group still names what would settle it.
        profile.employment_status = Some("employed".into());
        let e = evaluate(&profile, &rule, &AgeBasis::today());
        assert_eq!(e.verdict, RuleVerdict::Unknown);
        assert_eq!(e.unknown_rules.len(), 1);
    }

    fn make_profile(region: &str, income: Option<i32>, enrollment: Option<&str>) -> UserProfile {
        UserProfile {
            user_id: Uuid::new_v4(),
            birth_year: Some(2000),
            region_code: Some(region.to_string()),
            city_code: None,
            school_name: None,
            school_year: None,
            enrollment_status: enrollment.map(|s| s.to_string()),
            employment_status: None,
            major_group: None,
            income_bracket: income,
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
            updated_at: Utc::now(),
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
        }
    }

    fn make_program() -> Program {
        Program {
            id: Uuid::new_v4(),
            program_type: "scholarship".to_string(),
            source_type: "manual".to_string(),
            source_id: None,
            title: "테스트 장학금".to_string(),
            summary: None,
            provider_name: None,
            official_url: None,
            program_status: "open".to_string(),
            application_start_at: None,
            application_end_at: None,
            benefit_amount_monthly: Some(300_000),
            benefit_amount_semester: None,
            benefit_amount_once: None,
            region_scope: None,
            school_scope: None,
            tags: None,
            raw_payload: None,
            normalized_payload: None,
            min_age: None,
            max_age: None,
            regions: Some(vec!["busan".to_string()]),
            deadline_at: None,
            benefit_valid_until: None,
            is_active: true,
            last_synced_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            company_name: None,
            company_logo_url: None,
            benefit_category: None,
            provider_kind: None,
            application_steps: None,
            application_method: None,
            submission_documents: None,
            screening_method: None,
            region_districts: None,
            eligibility_condition: None,
            holder_requirement: None,
            age_basis: None,
        }
    }

    fn ymd(y: i32, m: u32, d: u32) -> chrono::NaiveDate {
        chrono::NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    #[test]
    fn age_on_counts_full_years_and_moves_a_leap_birthday_to_march() {
        assert_eq!(age_on(ymd(2001, 3, 15), ymd(2026, 3, 14)), 24);
        assert_eq!(age_on(ymd(2001, 3, 15), ymd(2026, 3, 15)), 25);
        assert_eq!(age_on(ymd(2004, 2, 29), ymd(2025, 2, 28)), 20);
        assert_eq!(age_on(ymd(2004, 2, 29), ymd(2025, 3, 1)), 21);
        assert_eq!(age_on(ymd(2004, 2, 29), ymd(2028, 2, 29)), 24);
    }

    #[test]
    fn a_date_of_birth_decides_the_boundary_year_a_birth_year_cannot() {
        let today = crate::seoul_date(chrono::Utc::now());
        use chrono::Datelike;
        // Born in the boundary year of a 19–34 band: 만 34 or 35 by year.
        let year = today.year() - 35;
        assert_eq!(age_gate(year, Some(19), Some(34)), RuleVerdict::Unknown);
        // Birthday already passed this year → 35 → outside.
        let passed = ymd(year, 1, 1);
        // Birthday still ahead (31 Dec, unless today is 31 Dec) → 34 → inside.
        let ahead = ymd(year, 12, 31);
        assert_eq!(
            age_gate_for(year, Some(passed), Some(19), Some(34)),
            RuleVerdict::Fail
        );
        if (today.month(), today.day()) != (12, 31) {
            assert_eq!(
                age_gate_for(year, Some(ahead), Some(19), Some(34)),
                RuleVerdict::Pass
            );
        }
        // The rule engine's `age` field reads the same single value.
        let mut input = make_input();
        input.birth_year = year;
        input.birth_date = Some(passed);
        let rule = parse_rule_json(&json!({"field": "age", "op": "lte", "value": 34})).unwrap();
        assert_eq!(
            evaluate(&input, &rule, &AgeBasis::today()).verdict,
            RuleVerdict::Fail
        );
        input.birth_date = None;
        assert_eq!(
            evaluate(&input, &rule, &AgeBasis::today()).verdict,
            RuleVerdict::Unknown
        );
        let mut saved = make_profile("busan", None, None);
        saved.birth_year = Some(year);
        saved.birth_date = Some(passed);
        assert_eq!(
            evaluate(&saved, &rule, &AgeBasis::today()).verdict,
            RuleVerdict::Fail
        );
        assert_eq!(ages(year, Some(passed), &AgeBasis::today()), (35, 35));
    }

    fn make_input() -> ProfileInput {
        ProfileInput {
            birth_year: 2000,
            birth_date: None,
            region_code: "busan".to_string(),
            city_code: None,
            school_name: None,
            school_year: None,
            enrollment_status: Some("enrolled".to_string()),
            employment_status: None,
            major_group: None,
            income_bracket: Some(3),
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
        }
    }

    #[test]
    fn all_conditions_matched() {
        let profile = make_profile("busan", Some(3), Some("enrolled"));
        let rule: RuleNode = serde_json::from_value(json!({
            "all": [
                { "field": "region_code", "op": "in", "value": ["busan", "daegu"], "weight": 2.0 },
                { "field": "income_bracket", "op": "lte", "value": 5, "weight": 1.0 },
                { "field": "enrollment_status", "op": "in", "value": ["enrolled", "on_leave"], "weight": 1.0 }
            ]
        }))
        .unwrap();

        let result = evaluate(&profile, &rule, &AgeBasis::today());
        assert_eq!(result.verdict, RuleVerdict::Pass);
        assert_eq!(result.matched_rules.len(), 3);
        assert_eq!(result.failed_rules.len(), 0);
        assert_eq!(result.unknown_rules.len(), 0);
        assert!((result.match_percentage - 100.0).abs() < 0.01);
    }

    #[test]
    fn unknown_field_recorded() {
        let profile = make_profile("busan", None, None);
        let rule: RuleNode = serde_json::from_value(json!({
            "all": [
                { "field": "income_bracket", "op": "lte", "value": 5, "weight": 1.0 }
            ]
        }))
        .unwrap();

        let result = evaluate(&profile, &rule, &AgeBasis::today());
        assert_eq!(result.verdict, RuleVerdict::Unknown);
        assert_eq!(result.unknown_rules.len(), 1);
        assert_eq!(result.matched_rules.len(), 0);
        assert!((result.match_percentage - 0.0).abs() < 0.01);
        assert!(result.explain_reasons[0].contains("소득 구간"));
    }

    #[test]
    fn any_passes_if_one_matches() {
        let profile = make_profile("busan", Some(3), None);
        let rule: RuleNode = serde_json::from_value(json!({
            "any": [
                { "field": "region_code", "op": "eq", "value": "busan", "weight": 1.0 },
                { "field": "region_code", "op": "eq", "value": "daegu", "weight": 1.0 }
            ]
        }))
        .unwrap();

        let result = evaluate(&profile, &rule, &AgeBasis::today());
        assert_eq!(result.verdict, RuleVerdict::Pass);
        assert_eq!(result.matched_rules.len(), 1);
        assert_eq!(result.failed_rules.len(), 1);
        assert!((result.match_percentage - 50.0).abs() < 0.01);
    }

    #[test]
    fn between_op_works() {
        let profile = make_profile("seoul", Some(3), None);
        let rule: RuleNode = serde_json::from_value(json!({
            "all": [
                { "field": "income_bracket", "op": "between", "value": [1, 5], "weight": 1.0 }
            ]
        }))
        .unwrap();

        let result = evaluate(&profile, &rule, &AgeBasis::today());
        assert_eq!(result.matched_rules.len(), 1);
    }

    #[test]
    fn compute_final_score_range() {
        let profile = make_profile("busan", Some(2), Some("enrolled"));
        let rule: RuleNode = serde_json::from_value(json!({
            "all": [
                { "field": "region_code", "op": "in", "value": ["busan"], "weight": 1.0 },
                { "field": "enrollment_status", "op": "eq", "value": "enrolled", "weight": 1.0 }
            ]
        }))
        .unwrap();
        let eval = evaluate(&profile, &rule, &AgeBasis::today());
        let program = make_program();
        let score = compute_final_score(&eval, &program);
        assert!((0.0..=100.0).contains(&score), "score={score}");
    }

    #[test]
    fn parse_rule_json_roundtrip() {
        let raw = json!({
            "all": [
                { "field": "region_code", "op": "in", "value": ["busan"], "weight": 1.0 }
            ]
        });
        let node = parse_rule_json(&raw).expect("parse");
        let re_serialised = serde_json::to_value(&node).expect("serialise");
        assert!(re_serialised.get("all").is_some());
    }

    #[test]
    fn all_fails_when_one_mandatory_condition_is_contradicted() {
        let input = make_input();
        let rule = parse_rule_json(&json!({
            "all": [
                { "field": "enrollment_status", "op": "eq", "value": "enrolled" },
                { "field": "income_bracket", "op": "lte", "value": 2 }
            ]
        }))
        .unwrap();

        let result = evaluate(&input, &rule, &AgeBasis::today());

        assert_eq!(result.verdict, RuleVerdict::Fail);
        assert_eq!(result.matched_rules.len(), 1);
        assert_eq!(result.failed_rules.len(), 1);
    }

    #[test]
    fn all_is_unknown_when_known_conditions_pass() {
        let mut input = make_input();
        input.income_bracket = None;
        let rule = parse_rule_json(&json!({
            "all": [
                { "field": "enrollment_status", "op": "eq", "value": "enrolled" },
                { "field": "income_bracket", "op": "lte", "value": 5 }
            ]
        }))
        .unwrap();

        assert_eq!(
            evaluate(&input, &rule, &AgeBasis::today()).verdict,
            RuleVerdict::Unknown
        );
    }

    #[test]
    fn any_is_unknown_only_when_no_branch_has_passed() {
        let mut input = make_input();
        input.income_bracket = None;
        let rule = parse_rule_json(&json!({
            "any": [
                { "field": "region_code", "op": "eq", "value": "seoul" },
                { "field": "income_bracket", "op": "lte", "value": 5 }
            ]
        }))
        .unwrap();

        assert_eq!(
            evaluate(&input, &rule, &AgeBasis::today()).verdict,
            RuleVerdict::Unknown
        );

        input.region_code = "seoul".to_string();
        assert_eq!(
            evaluate(&input, &rule, &AgeBasis::today()).verdict,
            RuleVerdict::Pass
        );
    }

    #[test]
    fn omitted_optional_boolean_is_not_treated_as_false() {
        let mut input = make_input();
        let rule = parse_rule_json(&json!({
            "field": "has_disability",
            "op": "is_true"
        }))
        .unwrap();

        assert_eq!(
            evaluate(&input, &rule, &AgeBasis::today()).verdict,
            RuleVerdict::Unknown
        );

        input.has_disability = Some(false);
        assert_eq!(
            evaluate(&input, &rule, &AgeBasis::today()).verdict,
            RuleVerdict::Fail
        );
    }

    #[test]
    fn invalid_rule_configuration_is_rejected() {
        assert!(matches!(
            parse_rule_json(&json!({
                "field": "made_up_field",
                "op": "eq",
                "value": "anything"
            })),
            Err(RuleEngineError::UnsupportedField(_))
        ));
        assert!(matches!(
            parse_rule_json(&json!({ "all": [] })),
            Err(RuleEngineError::EmptyGroup("all"))
        ));
        // Audit M3: a value whose type is not the field's never parses, and
        // ordered operators need a numeric field.
        for rule in [
            json!({"field": "children_count", "op": "ne", "value": "2"}),
            json!({"field": "region_code", "op": "not_in", "value": [26]}),
            json!({"field": "age", "op": "gte", "value": "19"}),
            json!({"field": "region_code", "op": "lte", "value": "zzz"}),
            json!({"field": "has_disability", "op": "eq", "value": "yes"}),
            json!({"field": "age", "op": "between", "value": [19, "34"]}),
        ] {
            assert!(
                matches!(
                    parse_rule_json(&rule),
                    Err(RuleEngineError::InvalidOperatorValue { .. })
                ),
                "{rule}"
            );
        }
        // is_true / is_false only on a boolean field (verifier gap 11).
        for op in ["is_true", "is_false"] {
            assert!(
                matches!(
                    parse_rule_json(&json!({"field": "region_code", "op": op})),
                    Err(RuleEngineError::InvalidOperatorValue { .. })
                ),
                "{op}"
            );
        }
        assert!(parse_rule_json(&json!({"field": "has_disability", "op": "is_true"})).is_ok());
        // A number compares as a number: 2.0 is 2.
        let mut input = make_input();
        input.children_count = Some(2);
        let two =
            parse_rule_json(&json!({"field": "children_count", "op": "eq", "value": 2.0})).unwrap();
        assert_eq!(
            evaluate(&input, &two, &AgeBasis::today()).verdict,
            RuleVerdict::Pass
        );
        let listed =
            parse_rule_json(&json!({"field": "children_count", "op": "in", "value": [1.0, 2.0]}))
                .unwrap();
        assert_eq!(
            evaluate(&input, &listed, &AgeBasis::today()).verdict,
            RuleVerdict::Pass
        );
        let not_two =
            parse_rule_json(&json!({"field": "children_count", "op": "ne", "value": 2.0})).unwrap();
        assert_eq!(
            evaluate(&input, &not_two, &AgeBasis::today()).verdict,
            RuleVerdict::Fail
        );
    }

    #[test]
    fn incomparable_ordered_values_never_parse() {
        // Was: parsed, then failed everyone. Since audit M3 an ordered
        // operator on a text field is a configuration error.
        assert!(parse_rule_json(&json!({
            "field": "region_code",
            "op": "gte",
            "value": 1
        }))
        .is_err());
    }

    /// A notice open 2026-10-25 (00:00 KST) to 2026-11-30 (23:59:59 KST).
    pub(crate) fn notice_window(program: &mut Program) {
        use chrono::TimeZone;
        program.application_start_at = Some(Utc.with_ymd_and_hms(2026, 10, 24, 15, 0, 0).unwrap());
        program.application_end_at = Some(Utc.with_ymd_and_hms(2026, 11, 30, 14, 59, 59).unwrap());
    }

    /// Review R-M1: a notice that states the day it counts age on
    /// (`programs.age_basis`) is decided on that one day; unstated keeps
    /// the window. Probe of the review: window 2026-11-01..11-30, band 19–34.
    #[test]
    fn a_stated_age_basis_collapses_the_basis_to_that_day() {
        use chrono::{Datelike, TimeZone};
        let today = ymd(2026, 11, 10);
        let mut program = make_program();
        program.application_start_at = Some(Utc.with_ymd_and_hms(2026, 10, 31, 15, 0, 0).unwrap());
        program.application_end_at = Some(Utc.with_ymd_and_hms(2026, 11, 30, 14, 59, 59).unwrap());
        let gate = |program: &Program, born: chrono::NaiveDate| {
            let basis = AgeBasis::for_program(program, today);
            age_gate_on(born.year(), Some(born), Some(19), Some(34), &basis)
        };
        program.age_basis = Some("year_end".into());
        assert_eq!(
            AgeBasis::for_program(&program, today),
            AgeBasis::on(ymd(2026, 12, 31))
        );
        // 35 on 31 Dec: outside a 연 나이 band, although 34 in the window.
        assert_eq!(gate(&program, ymd(1991, 12, 15)), RuleVerdict::Fail);
        program.age_basis = Some("year_start".into());
        assert_eq!(
            AgeBasis::for_program(&program, today),
            AgeBasis::on(ymd(2026, 1, 1))
        );
        assert_eq!(gate(&program, ymd(1991, 3, 1)), RuleVerdict::Pass);
        program.age_basis = Some("2026-03-02".into());
        assert_eq!(
            AgeBasis::for_program(&program, today),
            AgeBasis::on(ymd(2026, 3, 2))
        );
        for unstated in [
            None,
            Some("window".to_string()),
            Some("garbage".to_string()),
        ] {
            program.age_basis = unstated;
            let basis = AgeBasis::for_program(&program, today);
            assert_eq!(
                (basis.first(), basis.last()),
                (ymd(2026, 11, 1), ymd(2026, 11, 30))
            );
        }
    }

    /// Audit H1: notices count 만 나이 on their own day (공고일, 접수일,
    /// 마감일), which is not stored. A birthday inside the window leaves
    /// the boundary unknown instead of deciding it on today's date; a
    /// birthday outside it decides exactly as before. The clock is injected.
    #[test]
    fn a_birthday_inside_the_window_is_unknown() {
        let today = ymd(2026, 11, 10);
        let mut program = make_program();
        notice_window(&mut program);
        let basis = AgeBasis::for_program(&program, today);
        use chrono::Datelike;
        let gate = |born: chrono::NaiveDate| {
            age_gate_on(born.year(), Some(born), Some(19), Some(34), &basis)
        };
        // 34 on 10-25, 35 today: was Fail on today's date.
        assert_eq!(gate(ymd(1991, 10, 28)), RuleVerdict::Unknown);
        // 34 today, 35 on 11-30: was Pass on today's date.
        assert_eq!(gate(ymd(1991, 11, 20)), RuleVerdict::Unknown);
        // Birthday outside the window: the same answer as on today alone.
        assert_eq!(gate(ymd(1991, 12, 15)), RuleVerdict::Pass);
        assert_eq!(gate(ymd(1991, 10, 1)), RuleVerdict::Fail);
        assert_eq!(gate(ymd(1990, 6, 1)), RuleVerdict::Fail);
        for born in [ymd(1991, 12, 15), ymd(1991, 10, 1), ymd(2000, 3, 3)] {
            assert_eq!(
                gate(born),
                age_gate_on(
                    born.year(),
                    Some(born),
                    Some(19),
                    Some(34),
                    &AgeBasis::on(today)
                ),
                "{born}"
            );
        }
        // A bare birth year stays unknown at the boundary.
        assert_eq!(
            age_gate_on(1991, None, Some(19), Some(34), &basis),
            RuleVerdict::Unknown
        );
        // The rule engine's `age` field reads the same readings.
        let rule = parse_rule_json(&json!({"field": "age", "op": "lte", "value": 34})).unwrap();
        let mut input = make_input();
        input.birth_year = 1991;
        input.birth_date = Some(ymd(1991, 10, 28));
        assert_eq!(
            evaluate(&input, &rule, &basis).verdict,
            RuleVerdict::Unknown
        );
        input.birth_date = Some(ymd(1991, 12, 15));
        assert_eq!(evaluate(&input, &rule, &basis).verdict, RuleVerdict::Pass);
        // An open-ended programme counts on the application day: today.
        let mut rolling = make_program();
        notice_window(&mut rolling);
        rolling.application_end_at = None;
        assert_eq!(AgeBasis::for_program(&rolling, today), AgeBasis::on(today));
    }

    /// Review R-H1/R-M6: the window is a notice cycle only when it spans
    /// at most a year; a long or sentinel window counts on today. The
    /// closing day is `application_end_at`, else `deadline_at`.
    #[test]
    fn a_long_or_sentinel_window_counts_on_today() {
        use chrono::TimeZone;
        let today = ymd(2026, 11, 11);
        let at = |y, m, d| Utc.with_ymd_and_hms(y, m, d, 3, 0, 0).unwrap();
        let mut sentinel = make_program();
        sentinel.application_end_at = Some(at(2099, 12, 31));
        assert_eq!(AgeBasis::for_program(&sentinel, today), AgeBasis::on(today));
        let basis = AgeBasis::for_program(&sentinel, today);
        assert_eq!(
            age_gate_on(1996, Some(ymd(1996, 5, 5)), Some(19), Some(34), &basis),
            RuleVerdict::Pass
        );
        let mut long = make_program();
        long.application_start_at = Some(at(2020, 1, 1));
        long.application_end_at = Some(at(2026, 12, 31));
        let basis = AgeBasis::for_program(&long, today);
        assert_eq!(basis, AgeBasis::on(today));
        assert_eq!(
            age_gate_on(1988, Some(ymd(1988, 6, 1)), None, Some(34), &basis),
            RuleVerdict::Fail
        );
        let mut deadline_only = make_program();
        deadline_only.deadline_at = Some(at(2026, 11, 30));
        let basis = AgeBasis::for_program(&deadline_only, today);
        assert_eq!((basis.first(), basis.last()), (today, ymd(2026, 11, 30)));
        // Readings are a pair, never a list sized by the dates.
        assert_eq!(ages(1996, None, &AgeBasis::on(today)), (29, 30));
    }

    /// Review R-L2: the age basis cannot be skipped. A bare profile does not
    /// answer `age`; only [`evaluate`] with a basis does.
    #[test]
    fn a_bare_profile_does_not_answer_age() {
        let input = make_input();
        assert_eq!(input.eligibility_candidates("age"), None);
        // Verify gap 5: the saved profile is bare too.
        let saved = make_profile("busan", None, None);
        assert!(saved.birth_year.is_some());
        assert_eq!(saved.eligibility_candidates("age"), None);
        let rule = parse_rule_json(&json!({"field": "age", "op": "lte", "value": 34})).unwrap();
        assert_eq!(
            evaluate(&input, &rule, &AgeBasis::on(ymd(2026, 11, 11))).verdict,
            RuleVerdict::Pass
        );
    }

    /// Audit L7: a band whose floor is above its ceiling is a data error;
    /// it must not hide the programme from everyone.
    #[test]
    fn an_inverted_band_is_unknown() {
        let year = current_year();
        assert_eq!(
            age_gate(year - 25, Some(35), Some(19)),
            RuleVerdict::Unknown
        );
        assert_eq!(
            age_gate_for(year - 25, Some(ymd(year - 25, 1, 1)), Some(35), Some(19)),
            RuleVerdict::Unknown
        );
        // An equal floor and ceiling is a one-year band, not inverted.
        assert_eq!(age_gate(year - 40, Some(25), Some(25)), RuleVerdict::Fail);
    }

    /// Audit M7: "졸업(예정)자" notices are often coded 대학 졸업 only; a
    /// final-year student may be graduating and is not hidden by that code,
    /// while an earlier year still is.
    #[test]
    fn a_final_year_student_is_not_hidden_by_a_graduate_code() {
        let admitted = crate::education::admitted_education_stages(&["univ_graduate", "postgrad"]);
        let rule = parse_rule_json(&json!({
            "field": "education_stage", "op": "in", "value": admitted
        }))
        .unwrap();
        let mut input = make_input();
        input.school_type = Some("university".into());
        input.enrollment_status = Some("enrolled".into());
        input.school_year = Some(4);
        assert_eq!(
            evaluate(&input, &rule, &AgeBasis::today()).verdict,
            RuleVerdict::Unknown
        );
        input.school_year = Some(2);
        assert_eq!(
            evaluate(&input, &rule, &AgeBasis::today()).verdict,
            RuleVerdict::Fail
        );
        input.enrollment_status = Some("graduated".into());
        input.school_year = None;
        assert_eq!(
            evaluate(&input, &rule, &AgeBasis::today()).verdict,
            RuleVerdict::Pass
        );
    }

    /// Audit M4: both profile impls answer every field of [`FIELDS`] when
    /// the profile holds it.
    #[test]
    fn every_field_is_answered_by_both_profiles() {
        let mut input = make_input();
        input.city_code = Some("26410".into());
        input.school_name = Some("부산대학교".into());
        input.school_year = Some(2);
        input.school_type = Some("university".into());
        input.employment_status = Some("unemployed".into());
        input.major_group = Some("engineering".into());
        input.kosaf_support_bracket = Some(3);
        input.housing_type = Some("monthly_rent".into());
        input.household_size = Some(1);
        input.age_band = Some("20s".into());
        input.has_disability = Some(false);
        input.is_multicultural_family = Some(false);
        input.is_low_income_household = Some(false);
        input.veteran_family = Some(false);
        input.homeless_household = Some(true);
        input.marital_status = Some("married".into());
        input.newlywed = Some(true);
        input.children_count = Some(1);
        input.income_median_band = Some("le60".into());
        input.telecom_carrier = Some("skt".into());
        input.card_issuers = Some(vec!["kb".into()]);
        input.pay_apps = Some(Vec::new());
        let mut saved = make_profile("busan", Some(2), Some("enrolled"));
        saved.telecom_carrier = Some("skt".into());
        saved.card_issuers = Some(vec!["kb".into()]);
        saved.pay_apps = Some(Vec::new());
        saved.birth_year = Some(2000);
        saved.city_code = input.city_code.clone();
        saved.school_name = input.school_name.clone();
        saved.school_year = input.school_year;
        saved.school_type = input.school_type.clone();
        saved.enrollment_status = Some("enrolled".into());
        saved.employment_status = input.employment_status.clone();
        saved.major_group = input.major_group.clone();
        saved.kosaf_support_bracket = Some(3);
        saved.housing_type = input.housing_type.clone();
        saved.household_size = Some(1);
        saved.age_band = input.age_band.clone();
        saved.has_disability = Some(true);
        saved.is_multicultural_family = Some(false);
        saved.is_low_income_household = Some(true);
        saved.veteran_family = Some(false);
        saved.homeless_household = Some(true);
        saved.marital_status = Some("married".into());
        saved.newlywed = Some(true);
        saved.children_count = Some(1);
        saved.income_median_band = Some("le60".into());
        // `age` needs a basis (the bare profiles do not answer it).
        let input = WithAgeBasis::new(&input, AgeBasis::today());
        let saved = WithAgeBasis::new(&saved, AgeBasis::today());
        for spec in FIELDS {
            if NO_PROFILE_ANSWER.contains(&spec.name) {
                assert!(input.eligibility_candidates(spec.name).is_none());
                assert!(saved.eligibility_candidates(spec.name).is_none());
                continue;
            }
            assert!(
                input.eligibility_candidates(spec.name).is_some(),
                "input {}",
                spec.name
            );
            assert!(
                saved.eligibility_candidates(spec.name).is_some(),
                "saved {}",
                spec.name
            );
        }
    }

    /// Audit L4: an out-of-range birth year saturates instead of overflowing.
    #[test]
    fn an_extreme_birth_year_does_not_overflow() {
        let [younger, older] = age_candidates(i32::MIN);
        assert!(younger <= older);
        assert_eq!(age_gate(i32::MIN, Some(19), Some(34)), RuleVerdict::Fail);
        let [younger, older] = age_candidates(i32::MAX);
        assert!(younger <= older);
        assert_eq!(age_gate(i32::MAX, Some(19), Some(34)), RuleVerdict::Fail);
    }

    #[test]
    fn age_gate_excludes_only_when_both_full_age_readings_are_outside() {
        let year = current_year();
        // 연나이 34 → 만 33~34: inside a 19~34 band under both readings.
        assert_eq!(age_gate(year - 34, Some(19), Some(34)), RuleVerdict::Pass);
        // 연나이 35 → 만 34~35: still eligible until the birthday, so ask.
        assert_eq!(
            age_gate(year - 35, Some(19), Some(34)),
            RuleVerdict::Unknown
        );
        // 연나이 36 → 만 35~36: too old under both readings.
        assert_eq!(age_gate(year - 36, Some(19), Some(34)), RuleVerdict::Fail);
        // Lower boundary: 연나이 19 → 만 18~19 is ambiguous, 18 → 17~18 is out.
        assert_eq!(age_gate(year - 20, Some(19), None), RuleVerdict::Pass);
        assert_eq!(age_gate(year - 19, Some(19), None), RuleVerdict::Unknown);
        assert_eq!(age_gate(year - 18, Some(19), None), RuleVerdict::Fail);
        assert_eq!(age_gate(year - 50, None, None), RuleVerdict::Pass);
    }

    #[test]
    fn age_rule_field_is_unknown_for_the_boundary_cohort() {
        let year = current_year();
        let band = parse_rule_json(&json!({
            "field": "age", "op": "between", "value": [19, 34]
        }))
        .unwrap();
        let mut input = make_input();
        input.birth_year = year - 34;
        assert_eq!(
            evaluate(&input, &band, &AgeBasis::today()).verdict,
            RuleVerdict::Pass
        );
        input.birth_year = year - 35;
        let boundary = evaluate(&input, &band, &AgeBasis::today());
        assert_eq!(boundary.verdict, RuleVerdict::Unknown);
        assert_eq!(boundary.unknown_rules.len(), 1);
        assert!(boundary.explain_reasons[0].contains("생년월일"));
        input.birth_year = year - 36;
        assert_eq!(
            evaluate(&input, &band, &AgeBasis::today()).verdict,
            RuleVerdict::Fail
        );

        // A stored profile shares the interpretation.
        let lower = parse_rule_json(&json!({ "field": "age", "op": "gte", "value": 19 })).unwrap();
        let mut profile = make_profile("busan", None, None);
        profile.birth_year = Some(year - 19);
        assert_eq!(
            evaluate(&profile, &lower, &AgeBasis::today()).verdict,
            RuleVerdict::Unknown
        );
        profile.birth_year = Some(year - 18);
        assert_eq!(
            evaluate(&profile, &lower, &AgeBasis::today()).verdict,
            RuleVerdict::Fail
        );
        profile.birth_year = None;
        assert_eq!(
            evaluate(&profile, &lower, &AgeBasis::today()).verdict,
            RuleVerdict::Unknown
        );
    }

    #[test]
    fn explain_strings_use_korean_labels_instead_of_field_names_or_codes() {
        let mut input = make_input();
        input.employment_status = Some("employed".to_string());
        input.school_name = Some("부산대학교".to_string());
        let rule = parse_rule_json(&json!({
            "all": [
                { "field": "enrollment_status", "op": "in", "value": ["enrolled", "재학"] },
                { "field": "employment_status", "op": "eq", "value": "employed" },
                { "field": "school_name", "op": "in", "value": ["부산대학교"] },
                { "field": "has_disability", "op": "is_true" },
                { "field": "veteran_family", "op": "is_true" },
                { "field": "household_size", "op": "lte", "value": 4 }
            ]
        }))
        .unwrap();

        let joined = evaluate(&input, &rule, &AgeBasis::today())
            .explain_reasons
            .join("\n");
        for leaked in [
            "enrolled",
            "employed",
            "has_disability",
            "veteran_family",
            "household_size",
            "school_name",
        ] {
            assert!(!joined.contains(leaked), "{leaked} leaked into:\n{joined}");
        }
        assert!(joined.contains("재학 상태 조건 맞음 (재학)"), "{joined}");
        assert!(joined.contains("취업 상태 조건 맞음 (재직)"), "{joined}");
        assert!(joined.contains("학교 조건 맞음 (부산대학교)"), "{joined}");
        assert!(joined.contains("장애 여부 정보가 필요합니다"), "{joined}");
        assert!(
            joined.contains("보훈 가족 여부 정보가 필요합니다"),
            "{joined}"
        );
        assert!(joined.contains("가구원 수 정보가 필요합니다"), "{joined}");
    }

    #[test]
    fn school_type_and_major_reasons_show_labels_not_codes() {
        let mut input = make_input();
        input.school_type = Some("university".to_string());
        input.major_group = Some("engineering".to_string());
        let rule = parse_rule_json(&json!({
            "all": [
                { "field": "school_type", "op": "in", "value": ["university", "college"] },
                { "field": "major_group", "op": "in", "value": ["engineering"] }
            ]
        }))
        .unwrap();
        let joined = evaluate(&input, &rule, &AgeBasis::today())
            .explain_reasons
            .join("\n");
        assert!(joined.contains("학교 유형(대학교) 조건 맞음"), "{joined}");
        assert!(joined.contains("전공 계열(공학계열) 조건 맞음"), "{joined}");
        assert!(!joined.contains("university") && !joined.contains("engineering"));
    }

    #[test]
    fn deadline_urgency_uses_the_earliest_expiration_field() {
        let eval = evaluate(
            &make_input(),
            &parse_rule_json(&json!({ "field": "region_code", "op": "eq", "value": "busan" }))
                .unwrap(),
            &AgeBasis::today(),
        );
        let soon = Utc::now() + chrono::Duration::days(3);
        let far = Utc::now() + chrono::Duration::days(90);
        let undated = make_program();
        let mut explicit = make_program();
        explicit.deadline_at = Some(soon);
        let mut collected = make_program();
        collected.application_end_at = Some(soon);
        let mut validity = make_program();
        validity.benefit_valid_until = Some(soon);
        validity.deadline_at = Some(far);

        assert_eq!(validity.effective_deadline(), Some(soon));
        let explicit_score = compute_final_score(&eval, &explicit);
        assert_eq!(compute_final_score(&eval, &collected), explicit_score);
        assert_eq!(compute_final_score(&eval, &validity), explicit_score);
        assert!(explicit_score > compute_final_score(&eval, &undated));
    }

    #[test]
    fn empty_evidence_never_outscores_a_fully_passing_rule() {
        let passing = evaluate(
            &make_input(),
            &parse_rule_json(&json!({ "field": "region_code", "op": "eq", "value": "busan" }))
                .unwrap(),
            &AgeBasis::today(),
        );
        let none = RuleEvalResult {
            verdict: RuleVerdict::Unknown,
            matched_rules: vec![],
            failed_rules: vec![],
            unknown_rules: vec![],
            match_percentage: 0.0,
            explain_reasons: vec![],
        };
        // Best possible rule-less program vs. worst possible fully passing one.
        let mut rich = make_program();
        rich.benefit_amount_monthly = Some(1_000_000);
        rich.deadline_at = Some(Utc::now() + chrono::Duration::days(1));
        let mut poor = make_program();
        poor.benefit_amount_monthly = None;
        poor.regions = Some(vec![]);
        poor.deadline_at = Some(Utc::now() + chrono::Duration::days(365));

        assert!(compute_final_score(&none, &rich) < compute_final_score(&passing, &poor));
    }

    #[test]
    fn a_corporate_amount_adds_no_benefit_score() {
        let none = RuleEvalResult {
            verdict: RuleVerdict::Unknown,
            matched_rules: vec![],
            failed_rules: vec![],
            unknown_rules: vec![],
            match_percentage: 0.0,
            explain_reasons: vec![],
        };
        let mut corporate = make_program();
        corporate.program_type = "corporate_benefit".into();
        corporate.benefit_amount_monthly = Some(1_000_000);
        let mut plain = corporate.clone();
        plain.benefit_amount_monthly = None;
        assert_eq!(
            compute_final_score(&none, &corporate),
            compute_final_score(&none, &plain)
        );
        let mut support = corporate.clone();
        support.program_type = "support".into();
        assert!(compute_final_score(&none, &support) > compute_final_score(&none, &plain));
    }

    #[test]
    fn unconfirmed_region_gets_no_bonus_and_relevance_orders_rows() {
        let none = RuleEvalResult {
            verdict: RuleVerdict::Unknown,
            matched_rules: vec![],
            failed_rules: vec![],
            unknown_rules: vec![],
            match_percentage: 0.0,
            explain_reasons: vec![],
        };
        let mut regional = make_program();
        regional.regions = Some(vec!["busan".into()]);
        let nationwide = make_program();
        let plain = RankSignals {
            region_confirmed: false,
            relevance: 0.0,
        };
        assert_eq!(
            compute_final_score_with(&none, &regional, plain, chrono::Utc::now()),
            compute_final_score_with(&none, &nationwide, plain, chrono::Utc::now()),
            "a sido-level match alone no longer outranks nationwide rows"
        );
        let confirmed = RankSignals {
            region_confirmed: true,
            ..plain
        };
        assert_eq!(
            compute_final_score_with(&none, &regional, confirmed, chrono::Utc::now()),
            compute_final_score_with(&none, &regional, plain, chrono::Utc::now()) + 5.0
        );
        let relevant = RankSignals {
            relevance: 8.0,
            ..plain
        };
        assert_eq!(
            compute_final_score_with(&none, &nationwide, relevant, chrono::Utc::now()),
            compute_final_score_with(&none, &nationwide, plain, chrono::Utc::now()) + 8.0
        );
        // The legacy entry point keeps its behaviour.
        assert_eq!(
            compute_final_score(&none, &regional),
            compute_final_score_with(&none, &regional, confirmed, chrono::Utc::now()) + 5.0
        );
    }

    /// A `"severity": "label"` condition names a check but never hides: a
    /// contradiction reads as Unknown, a match still passes.
    #[test]
    fn label_conditions_never_fail() {
        let rule = parse_rule_json(&json!({"all": [
            {"field": "employment_status", "op": "in", "value": ["unemployed"], "severity": "label"}
        ]}))
        .unwrap();
        let mut input = make_input();
        input.employment_status = Some("employed".into());
        let result = evaluate(&input, &rule, &AgeBasis::today());
        assert_eq!(result.verdict, RuleVerdict::Unknown);
        assert_eq!(result.unknown_rules.len(), 1);
        assert!(result.failed_rules.is_empty());
        assert!(
            result.explain_reasons[0].contains("공고"),
            "{:?}",
            result.explain_reasons
        );
        input.employment_status = Some("unemployed".into());
        assert_eq!(
            evaluate(&input, &rule, &AgeBasis::today()).verdict,
            RuleVerdict::Pass
        );
        input.employment_status = None;
        assert_eq!(
            evaluate(&input, &rule, &AgeBasis::today()).verdict,
            RuleVerdict::Unknown
        );
        // Without the attribute the same condition still hides.
        let hard = parse_rule_json(
            &json!({"field": "employment_status", "op": "in", "value": ["unemployed"]}),
        )
        .unwrap();
        input.employment_status = Some("employed".into());
        assert_eq!(
            evaluate(&input, &hard, &AgeBasis::today()).verdict,
            RuleVerdict::Fail
        );
        // An any-of labels stays non-hiding.
        let any = parse_rule_json(&json!({"any": [
            {"field": "has_disability", "op": "is_true", "severity": "label"},
            {"field": "employment_status", "op": "in", "value": ["unemployed"], "severity": "label"}
        ]}))
        .unwrap();
        input.has_disability = Some(false);
        assert_eq!(
            evaluate(&input, &any, &AgeBasis::today()).verdict,
            RuleVerdict::Unknown
        );
        assert!(parse_rule_json(
            &json!({"field": "age", "op": "gte", "value": 19, "severity": "maybe"})
        )
        .is_err());
    }

    /// The saved profile stores an unanswered flag as `false`, and no client
    /// asks for these flags yet: a stored `false` is not an answer.
    #[test]
    fn stored_default_false_flags_do_not_fail_is_true_rules() {
        let stored = make_profile("busan", None, None);
        for field in [
            "has_disability",
            "is_multicultural_family",
            "is_low_income_household",
            "veteran_family",
        ] {
            let rule = parse_rule_json(&json!({"field": field, "op": "is_true"})).unwrap();
            assert_eq!(
                evaluate(&stored, &rule, &AgeBasis::today()).verdict,
                RuleVerdict::Unknown,
                "{field}"
            );
        }
        let mut disabled = make_profile("busan", None, None);
        disabled.has_disability = Some(true);
        let rule = parse_rule_json(&json!({"field": "has_disability", "op": "is_true"})).unwrap();
        assert_eq!(
            evaluate(&disabled, &rule, &AgeBasis::today()).verdict,
            RuleVerdict::Pass
        );
    }

    /// `education_stage` and `major_category` are read from existing answers
    /// on both profile types; several readings only ever give Unknown.
    #[test]
    fn derived_education_fields_read_existing_answers() {
        let rule = parse_rule_json(&json!({"all": [
            {"field": "education_stage", "op": "in", "value": ["univ_expected", "univ_graduate"]},
            {"field": "major_category", "op": "in", "value": ["engineering"]}
        ]}))
        .unwrap();
        let mut stored = make_profile("busan", None, Some("graduated"));
        stored.school_type = Some("university".into());
        stored.major_group = Some("engineering".into());
        assert_eq!(
            evaluate(&stored, &rule, &AgeBasis::today()).verdict,
            RuleVerdict::Pass
        );
        assert_eq!(
            stored.eligibility_value("education_stage"),
            Some(json!("univ_graduate"))
        );
        stored.major_group = Some("humanities".into());
        assert_eq!(
            evaluate(&stored, &rule, &AgeBasis::today()).verdict,
            RuleVerdict::Fail
        );

        let mut input = make_input();
        input.school_type = Some("university".into());
        input.major_group = Some("engineering".into());
        // A student with no year may be graduating: unknown, not hidden.
        assert_eq!(
            evaluate(&input, &rule, &AgeBasis::today()).verdict,
            RuleVerdict::Unknown
        );
        assert_eq!(input.eligibility_value("education_stage"), None);
        input.school_year = Some(2);
        assert_eq!(
            evaluate(&input, &rule, &AgeBasis::today()).verdict,
            RuleVerdict::Fail
        );
        input.school_type = None;
        input.enrollment_status = None;
        input.major_group = None;
        let result = evaluate(&input, &rule, &AgeBasis::today());
        assert_eq!(result.verdict, RuleVerdict::Unknown);
        assert_eq!(result.unknown_rules.len(), 2);
        assert_eq!(field_label("education_stage"), "학력");
    }

    /// Review 2026-10-21 (Low): a root `any` group of source-code leaves
    /// without its own `origin` checks nothing a reviewer read; a group is a
    /// source condition when all its leaves are.
    #[test]
    fn a_group_of_source_leaves_is_source_codes_only() {
        let src = |value: &str| {
            json!({"field": "region_code", "op": "eq", "value": value,
                   "origin": "source_codes"})
        };
        let reviewed = json!({"field": "income_bracket", "op": "lte", "value": 5});
        let any_root = json!({"any": [src("busan"), src("seoul")]});
        assert!(source_codes_only(&any_root, Some(&any_root), None));
        let nested =
            json!({"all": [src("busan"), {"any": [src("seoul"), {"all": [src("ulsan")]}]}]});
        assert!(source_codes_only(&nested, None, None));
        let mixed = json!({"any": [src("busan"), reviewed.clone()]});
        assert!(!source_codes_only(&mixed, None, None));
        let mixed_nested =
            json!({"all": [src("busan"), {"any": [src("seoul"), reviewed.clone()]}]});
        assert!(!source_codes_only(&mixed_nested, Some(&any_root), None));
        assert!(!source_codes_only(&reviewed, Some(&any_root), None));
        // An empty group carries no source leaf: not a source condition.
        assert!(!source_codes_only(&json!({"any": []}), None, None));
        // A group with extra keys parses as the same group (serde ignores
        // them), so it is judged by its leaves too.
        let annotated = json!({"any": [src("busan"), src("seoul")], "note": "시도 코드"});
        assert!(source_codes_only(&annotated, None, None));
        let annotated_nested =
            json!({"all": [src("busan"), {"all": [src("ulsan")], "label": "x"}], "id": 1});
        assert!(source_codes_only(&annotated_nested, None, None));
        let annotated_mixed = json!({"any": [src("busan"), reviewed.clone()], "note": "x"});
        assert!(!source_codes_only(&annotated_mixed, None, None));
        // Unchanged: plain `all` roots and the author fallback.
        assert!(source_codes_only(
            &json!({"all": [src("busan")]}),
            None,
            None
        ));
        assert!(source_codes_only(
            &Value::Null,
            None,
            Some(SOURCE_RULE_AUTHOR)
        ));
    }

    fn answers(extra: Value) -> ProfileInput {
        let mut base = json!({"birth_year": 2000, "region_code": "busan"});
        base.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        serde_json::from_value(base).unwrap()
    }

    fn verdict(profile: &ProfileInput, rule: Value) -> RuleVerdict {
        evaluate(
            profile,
            &parse_rule_json(&rule).unwrap(),
            &AgeBasis::today(),
        )
        .verdict
    }

    #[test]
    fn unanswered_optional_answers_are_unknown_never_pass() {
        let blank = answers(json!({}));
        for rule in [
            json!({"field": "homeless_household", "op": "is_true"}),
            json!({"field": "marital_status", "op": "eq", "value": "married"}),
            json!({"field": "newlywed", "op": "is_true"}),
            json!({"field": "children_count", "op": "gte", "value": 2}),
            json!({"field": "income_median_pct", "op": "lte", "value": 100}),
        ] {
            assert_eq!(
                verdict(&blank, rule.clone()),
                RuleVerdict::Unknown,
                "{rule}"
            );
        }
        // 모름 is an answer that decides nothing.
        let unknown_band = answers(json!({"income_median_band": "unknown"}));
        assert_eq!(
            verdict(
                &unknown_band,
                json!({"field": "income_median_pct", "op": "lte", "value": 180})
            ),
            RuleVerdict::Unknown
        );
        // A married answer without the 신혼 follow-up leaves newlywed open.
        let married = answers(json!({"marital_status": "married"}));
        assert_eq!(
            verdict(&married, json!({"field": "newlywed", "op": "is_true"})),
            RuleVerdict::Unknown
        );
    }

    #[test]
    fn optional_answers_pass_and_fail_on_what_was_answered() {
        let homeless = answers(json!({"homeless_household": true}));
        let owner = answers(json!({"homeless_household": false}));
        let rule = json!({"field": "homeless_household", "op": "is_true"});
        assert_eq!(verdict(&homeless, rule.clone()), RuleVerdict::Pass);
        assert_eq!(verdict(&owner, rule), RuleVerdict::Fail);

        let single = answers(json!({"marital_status": "unmarried"}));
        let newlywed = answers(json!({"marital_status": "married", "newlywed": true}));
        let long_married = answers(json!({"marital_status": "married", "newlywed": false}));
        let rule = json!({"field": "newlywed", "op": "is_true"});
        assert_eq!(verdict(&newlywed, rule.clone()), RuleVerdict::Pass);
        assert_eq!(verdict(&single, rule.clone()), RuleVerdict::Fail);
        assert_eq!(verdict(&long_married, rule), RuleVerdict::Fail);
        // A stray newlywed answer on an unmarried profile is not newlywed.
        let stray = answers(json!({"marital_status": "unmarried", "newlywed": true}));
        assert_eq!(
            verdict(&stray, json!({"field": "newlywed", "op": "is_true"})),
            RuleVerdict::Fail
        );

        let two = answers(json!({"children_count": 2}));
        assert_eq!(
            verdict(
                &two,
                json!({"field": "children_count", "op": "gte", "value": 2})
            ),
            RuleVerdict::Pass
        );
        assert_eq!(
            verdict(
                &two,
                json!({"field": "children_count", "op": "gte", "value": 3})
            ),
            RuleVerdict::Fail
        );
    }

    #[test]
    fn an_income_band_decides_only_what_the_whole_band_decides() {
        let band = |b: &str| answers(json!({"income_median_band": b}));
        let lte = |p: i64| json!({"field": "income_median_pct", "op": "lte", "value": p});
        let lt = |p: i64| json!({"field": "income_median_pct", "op": "lt", "value": p});
        // (50, 60] against ≤ 60 / ≤ 100: inside. Against ≤ 55: the band
        // straddles it. Against ≤ 50: entirely above.
        assert_eq!(verdict(&band("le60"), lte(60)), RuleVerdict::Pass);
        assert_eq!(verdict(&band("le60"), lte(100)), RuleVerdict::Pass);
        assert_eq!(verdict(&band("le60"), lte(55)), RuleVerdict::Unknown);
        assert_eq!(verdict(&band("le60"), lte(50)), RuleVerdict::Fail);
        // "60% 미만": 60% itself is in the band, so it cannot pass.
        assert_eq!(verdict(&band("le60"), lt(60)), RuleVerdict::Unknown);
        assert_eq!(verdict(&band("le50"), lt(60)), RuleVerdict::Pass);
        assert_eq!(verdict(&band("le120"), lte(100)), RuleVerdict::Fail);
        assert_eq!(verdict(&band("gt180"), lte(180)), RuleVerdict::Fail);
        assert_eq!(verdict(&band("le180"), lte(180)), RuleVerdict::Pass);
        // Every band and the stored profile read the same way.
        for (code, _, _) in INCOME_MEDIAN_BANDS {
            assert!(band(code)
                .eligibility_candidates("income_median_pct")
                .is_some());
        }
        let mut saved = make_profile("busan", None, None);
        saved.income_median_band = Some("le100".into());
        assert_eq!(
            evaluate(
                &saved,
                &parse_rule_json(&lte(100)).unwrap(),
                &AgeBasis::today()
            )
            .verdict,
            RuleVerdict::Pass
        );
        // An unrecognised stored band is unanswered.
        assert_eq!(verdict(&band("le70"), lte(180)), RuleVerdict::Unknown);
    }

    #[test]
    fn optional_answers_have_korean_labels_and_prompts() {
        for field in [
            "homeless_household",
            "marital_status",
            "newlywed",
            "children_count",
            "income_median_pct",
        ] {
            assert_ne!(field_label(field), "추가 자격", "{field}");
            assert!(optional_answer_prompt(field).ends_with("입력하면 확인할 수 있어요"));
            assert!(field_spec(field).is_some());
        }
        let eval = evaluate(
            &answers(json!({})),
            &parse_rule_json(&json!({"field": "homeless_household", "op": "is_true"})).unwrap(),
            &AgeBasis::today(),
        );
        assert_eq!(
            eval.explain_reasons,
            vec!["무주택 여부를 입력하면 확인할 수 있어요".to_string()]
        );
    }
}
