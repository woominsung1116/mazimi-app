//! Server-side validation. The model is never trusted: every fact must quote
//! the stored source text exactly, stay inside domain ranges and canonical
//! codes, and pass a textual merge guard (the quote itself must mention what
//! the fact claims). Only `HIGH` facts that pass everything enter the default
//! review queue; valid lower-confidence facts are stored as `flagged`; the
//! rest are stored as `invalid` for audit and evaluation and can never be
//! approved.
use super::facts::*;
use super::source::{ExtractionSource, RuleCoverage};
use chrono::{Datelike, NaiveDate};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::LazyLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactState {
    /// HIGH confidence, evidence-valid, guard-passed: default review queue.
    Pending,
    /// Valid but MEDIUM/LOW confidence or a reviewer warning. Approvable,
    /// hidden from the default queue.
    Flagged,
    /// Failed validation. Stored for audit and eval; never approvable.
    Invalid,
}

impl FactState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Flagged => "flagged",
            Self::Invalid => "invalid",
        }
    }
}

/// Error codes are prefixed by class: `evidence:`, `range:`, `code:`,
/// `guard:`, `format:`. Flags are reviewer warnings on otherwise valid facts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Validation {
    pub errors: Vec<String>,
    pub flags: Vec<String>,
}

impl Validation {
    pub fn guard_rejected(&self) -> bool {
        self.errors.iter().any(|e| e.starts_with("guard:"))
    }
    pub fn evidence_rejected(&self) -> bool {
        self.errors.iter().any(|e| e.starts_with("evidence:"))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProcessedFact {
    pub key: String,
    pub fact_type: FactType,
    /// Normalized [`FactValue`] JSON when it parsed, the raw model JSON
    /// otherwise.
    pub value: Value,
    pub evidence: Option<Evidence>,
    pub confidence: Option<Confidence>,
    pub state: FactState,
    pub validation: Validation,
}

impl ProcessedFact {
    pub fn fact_value(&self) -> Option<FactValue> {
        serde_json::from_value(self.value.clone()).ok()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ProcessCounters {
    pub excluded_by_rules: usize,
    pub rejected_by_guard: usize,
    pub invalid_evidence: usize,
    pub invalid: usize,
    pub flagged: usize,
    pub pending: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Processed {
    pub facts: Vec<ProcessedFact>,
    pub counters: ProcessCounters,
}

// ── Evidence ──────────────────────────────────────────────────────────────

/// True when `quote` is exactly the char span `[start, end)` of `source`.
pub fn span_matches(source: &str, start: usize, end: usize, quote: &str) -> bool {
    end >= start
        && end - start == quote.chars().count()
        && source
            .chars()
            .skip(start)
            .take(end - start)
            .eq(quote.chars())
        && source.chars().count() >= end
}

/// Resolve the model's quote to char offsets. The model's own offset is used
/// when it points exactly at the quote; otherwise a quote that occurs exactly
/// once is located by the server. Missing or ambiguous quotes are rejected:
/// nothing is ever matched fuzzily.
pub fn anchor(
    source: &str,
    quote: &str,
    hint: Option<i64>,
) -> Result<(usize, usize, OffsetSource), String> {
    let length = quote.chars().count();
    if !(MIN_QUOTE_CHARS..=MAX_QUOTE_CHARS).contains(&length) || quote.trim().is_empty() {
        return Err(format!(
            "evidence: quote must be {MIN_QUOTE_CHARS}-{MAX_QUOTE_CHARS} characters"
        ));
    }
    if let Some(start) = hint.and_then(|h| usize::try_from(h).ok()) {
        if span_matches(source, start, start + length, quote) {
            return Ok((start, start + length, OffsetSource::Model));
        }
    }
    // Overlapping occurrences count too ("가가" occurs twice in "가가가"), so
    // the search restarts one character after each hit.
    let first = source.find(quote);
    let second = first.and_then(|byte| {
        let next = byte + source[byte..].chars().next().map_or(1, char::len_utf8);
        source[next..].find(quote)
    });
    match (first, second) {
        (None, _) => Err("evidence: quote is not an exact substring of the source text".into()),
        (Some(byte), None) => {
            let start = source[..byte].chars().count();
            Ok((start, start + length, OffsetSource::ServerAnchored))
        }
        (Some(_), Some(_)) => {
            Err("evidence: quote occurs more than once and the offset does not match".into())
        }
    }
}

fn check_evidence(evidence: &Evidence, source: &ExtractionSource, errors: &mut Vec<String>) {
    let length = evidence.quote.chars().count();
    if evidence.source_hash != source.hash {
        errors.push("evidence: source hash does not match the stored source text".into());
    }
    if !(MIN_QUOTE_CHARS..=MAX_QUOTE_CHARS).contains(&length) {
        errors.push("evidence: quote length out of range".into());
    }
    if !span_matches(&source.text, evidence.start, evidence.end, &evidence.quote) {
        errors.push("evidence: quote is not the source text at the given offsets".into());
    }
}

// ── Guards ────────────────────────────────────────────────────────────────

fn compact(value: &str) -> String {
    value.chars().filter(|c| !c.is_whitespace()).collect()
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

static NUMBER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\d+").expect("valid"));
static AGE_PHRASE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\d+세|\d+살|연령|나이|년생|출생").expect("valid"));
static MAN_AGE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"만\d").expect("valid"));
/// Exclusive age bounds on the compacted quote: `39세미만`, `18세초과`.
static AGE_BELOW: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(\d+)세미만").expect("valid"));
static AGE_ABOVE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(\d+)세초과").expect("valid"));
/// Residence cues: a region named in a title or provider is not a
/// residence requirement.
const RESIDENCE_CUES: &[&str] = &["거주", "주소", "소재", "관내", "출신", "주민", "지역"];
static DATE_SEPARATED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(\d{4})\s*(?:[.\-/]|년)\s*(\d{1,2})\s*(?:[.\-/]|월)\s*(\d{1,2})").expect("valid")
});
static DATE_COMPACT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|\D)(\d{4})(\d{2})(\d{2})(?:\D|$)").expect("valid"));

fn numbers_in(value: &str) -> Vec<i64> {
    NUMBER
        .find_iter(value)
        .filter_map(|m| m.as_str().parse().ok())
        .collect()
}

pub(crate) fn dates_in(value: &str) -> Vec<NaiveDate> {
    let mut dates = Vec::new();
    for regex in [&*DATE_SEPARATED, &*DATE_COMPACT] {
        for captures in regex.captures_iter(value) {
            let parse = |i: usize| captures.get(i).and_then(|m| m.as_str().parse::<u32>().ok());
            if let (Some(y), Some(m), Some(d)) = (parse(1), parse(2), parse(3)) {
                if let Some(date) = NaiveDate::from_ymd_opt(y as i32, m, d) {
                    if !dates.contains(&date) {
                        dates.push(date);
                    }
                }
            }
        }
    }
    dates
}

/// Phrases that contain 소득 but are not an income requirement.
const INCOME_IRRELEVANT: &[&str] = &[
    "소득공제",
    "소득세",
    "금융소득종합과세",
    "소득이체",
    "소득금액증명",
    "원천징수",
];
const INCOME_TOKENS: &[&str] = &["소득", "중위", "분위", "지원구간", "연봉", "총급여"];

fn mentions_income(quote: &str) -> bool {
    let mut text = compact(quote);
    for phrase in INCOME_IRRELEVANT {
        text = text.replace(phrase, "");
    }
    contains_any(&text, INCOME_TOKENS)
}

fn region_aliases(code: &str) -> Vec<&'static str> {
    let mut aliases = vec![mazimi_core::region_label(code)];
    aliases.extend(match code {
        "chungbuk" => &["충청북도"][..],
        "chungnam" => &["충청남도"][..],
        "jeonbuk" => &["전라북도"][..],
        "jeonnam" => &["전라남도"][..],
        "gyeongbuk" => &["경상북도"][..],
        "gyeongnam" => &["경상남도"][..],
        _ => &[][..],
    });
    aliases
}

fn enrollment_keywords(code: &str) -> &'static [&'static str] {
    match code {
        "enrolled" => &["재학"],
        "on_leave" => &["휴학"],
        "graduated" => &["졸업"],
        "research_student" => &["연구생", "수료"],
        _ => &[],
    }
}

fn employment_keywords(code: &str) -> &'static [&'static str] {
    match code {
        "employed" => &["재직", "근로자", "직장", "근무"],
        "new_hire" => &["신규", "입사", "채용"],
        "unemployed" => &["미취업", "실업", "무직"],
        "preparing" => &["취업준비", "구직", "취준", "미취업"],
        "self_employed" => &["자영업", "프리랜서", "사업자", "창업"],
        "student" => &["학생"],
        _ => &[],
    }
}

/// The quote a status keyword is matched against. `student` must not be
/// satisfied by the 학생 inside 재학생/휴학생/졸업생.
fn status_text(quote: &str, status: &str) -> String {
    if status == "student" {
        ["재학생", "휴학생", "졸업생", "연구생"]
            .iter()
            .fold(quote.to_string(), |text, word| text.replace(word, ""))
    } else {
        quote.to_string()
    }
}

fn plain(value: &str) -> bool {
    !value.contains('<') && !value.contains('>')
}

// ── Value + guard validation ─────────────────────────────────────────────

/// Full validation of a stored fact against the stored source. The worker
/// runs it before a candidate is stored and the admin API runs it again,
/// against the program's current source, before anything is applied.
pub fn validate_fact(
    value: &FactValue,
    evidence: &Evidence,
    source: &ExtractionSource,
) -> Validation {
    let mut v = Validation::default();
    check_evidence(evidence, source, &mut v.errors);
    let quote = compact(&evidence.quote);
    let numbers = numbers_in(&quote);
    match value {
        FactValue::Age {
            min_age,
            max_age,
            basis,
        } => {
            if min_age.is_none() && max_age.is_none() {
                v.errors.push("range: age needs min_age or max_age".into());
            }
            if [min_age, max_age]
                .into_iter()
                .flatten()
                .any(|age| !(0..=100).contains(age))
                || min_age.zip(*max_age).is_some_and(|(min, max)| min > max)
            {
                v.errors.push("range: min_age/max_age out of range".into());
            }
            if !AGE_PHRASE.is_match(&quote) {
                v.errors.push("guard: quote has no age phrase".into());
            }
            let captured = |regex: &Regex| -> Vec<i64> {
                regex
                    .captures_iter(&quote)
                    .filter_map(|c| c[1].parse().ok())
                    .collect()
            };
            let (below, above) = (captured(&AGE_BELOW), captured(&AGE_ABOVE));
            // The columns are inclusive: "39세 미만" is max_age 38 and
            // "18세 초과" is min_age 19.
            for (age, is_max) in [(min_age, false), (max_age, true)] {
                let Some(age) = age.map(i64::from) else {
                    continue;
                };
                let (exclusive, shifted, word) = if is_max {
                    (&below, age + 1, "미만")
                } else {
                    (&above, age - 1, "초과")
                };
                if exclusive.contains(&age) {
                    v.errors.push(format!(
                        "guard: '{age}세 {word}' is exclusive; the inclusive bound is {}",
                        if is_max { age - 1 } else { age + 1 }
                    ));
                } else if !numbers.contains(&age) && !exclusive.contains(&shifted) {
                    v.errors
                        .push(format!("guard: age {age} does not appear in the quote"));
                }
            }
            if *basis != AgeBasis::ManAge {
                v.flags
                    .push("age basis is not 만나이; programs.min_age/max_age mean 만나이".into());
            } else if !MAN_AGE.is_match(&quote) {
                v.flags
                    .push("만나이 claimed but the quote has no '만 N' form".into());
            }
        }
        FactValue::Region { codes } => {
            if codes.is_empty() {
                v.errors
                    .push("range: region needs at least one code".into());
            }
            for code in codes {
                if !REGION_CODES.contains(&code.as_str()) {
                    v.errors.push(format!("code: unknown region {code}"));
                } else if !contains_any(&quote, &region_aliases(code)) {
                    v.errors
                        .push(format!("guard: region {code} is not named in the quote"));
                } else if code == "gwangju" && quote.contains("광주시") {
                    // 광주시 is a Gyeonggi municipality, not 광주광역시.
                    v.errors
                        .push("guard: 광주시 (Gyeonggi) is not the gwangju province".into());
                }
            }
            if !contains_any(&quote, RESIDENCE_CUES) {
                v.flags
                    .push("region quote has no residence cue (거주/주소/소재 …); confirm it is a requirement".into());
            }
        }
        FactValue::Enrollment { statuses } | FactValue::Employment { statuses } => {
            let enrollment = matches!(value, FactValue::Enrollment { .. });
            if statuses.is_empty() {
                v.errors
                    .push("range: at least one status is required".into());
            }
            for status in statuses {
                let (known, keywords) = if enrollment {
                    (
                        ENROLLMENT_CODES.contains(&status.as_str()),
                        enrollment_keywords(status),
                    )
                } else {
                    (
                        EMPLOYMENT_CODES.contains(&status.as_str()),
                        employment_keywords(status),
                    )
                };
                if !known {
                    v.errors.push(format!("code: unknown status {status}"));
                } else if !contains_any(&status_text(&quote, status), keywords) {
                    v.errors
                        .push(format!("guard: status {status} has no textual basis"));
                }
            }
        }
        FactValue::Income {
            criterion,
            comparator,
            value: amount,
        } => {
            let (range, token) = match criterion {
                IncomeCriterion::MedianIncomePercent => (1..=300, "중위"),
                IncomeCriterion::IncomeDecile => (0..=10, "분위"),
                IncomeCriterion::KosafBracket => (0..=10, "구간"),
            };
            if !range.contains(amount) {
                v.errors.push("range: income value out of range".into());
            }
            if !mentions_income(&evidence.quote) {
                v.errors
                    .push("guard: quote has no income requirement".into());
            }
            if !quote.contains(token) {
                v.errors.push(format!(
                    "guard: income criterion needs '{token}' in the quote"
                ));
            }
            if !numbers.contains(&i64::from(*amount)) {
                v.errors.push(format!(
                    "guard: income value {amount} does not appear in the quote"
                ));
            }
            let word = match comparator {
                Comparator::Lte => Some("이하"),
                Comparator::Lt => Some("미만"),
                Comparator::Gte => Some("이상"),
                Comparator::Gt => Some("초과"),
                Comparator::Eq => None,
            };
            if let Some(word) = word.filter(|w| !quote.contains(w)) {
                v.errors.push(format!(
                    "guard: comparator {} needs '{word}' in the quote",
                    comparator.as_str()
                ));
            }
            if matches!(comparator, Comparator::Gt | Comparator::Gte) {
                v.flags
                    .push("income lower bound: confirm it is an eligibility condition".into());
            }
        }
        FactValue::Document {
            name,
            required: _,
            notes,
        } => {
            let count = name.chars().count();
            if !(1..=100).contains(&count) || !plain(name) {
                v.errors
                    .push("range: document name must be 1-100 plain characters".into());
            }
            if notes
                .as_deref()
                .is_some_and(|n| n.chars().count() > 300 || !plain(n))
            {
                v.errors
                    .push("range: document notes must be at most 300 plain characters".into());
            }
            let key = compact(name);
            if key.is_empty() || !quote.contains(&key) {
                v.errors
                    .push("guard: document name does not appear in the quote".into());
            }
        }
        FactValue::ApplicationWindow {
            kind,
            start_date,
            end_date,
        } => {
            let dates: Vec<NaiveDate> = [start_date, end_date]
                .into_iter()
                .flatten()
                .copied()
                .collect();
            if dates.iter().any(|d| !(2000..=2100).contains(&d.year())) {
                v.errors.push("range: date year out of range".into());
            }
            if start_date.zip(*end_date).is_some_and(|(s, e)| s > e) {
                v.errors.push("range: start_date is after end_date".into());
            }
            let quoted = dates_in(&evidence.quote);
            for date in &dates {
                if !quoted.contains(date) {
                    v.errors
                        .push(format!("guard: date {date} does not appear in the quote"));
                }
            }
            match kind {
                WindowKind::Dated if dates.is_empty() => {
                    v.errors.push("range: dated window needs a date".into());
                }
                WindowKind::AlwaysOpen => {
                    if !dates.is_empty() {
                        v.errors
                            .push("range: always_open window carries no dates".into());
                    }
                    if !contains_any(&quote, &["상시", "연중", "수시"]) {
                        v.errors
                            .push("guard: quote does not say 상시/연중/수시".into());
                    }
                }
                WindowKind::UntilBudgetExhausted => {
                    if !(quote.contains("예산") && contains_any(&quote, &["소진", "조기마감"]))
                    {
                        v.errors.push("guard: quote does not say 예산 소진".into());
                    }
                }
                WindowKind::Dated => {}
            }
        }
    }
    v
}

// ── Model output processing ──────────────────────────────────────────────

fn parse_date(value: Option<&str>, errors: &mut Vec<String>) -> Option<NaiveDate> {
    let value = value?;
    match NaiveDate::parse_from_str(value, "%Y-%m-%d") {
        Ok(date) => Some(date),
        Err(_) => {
            errors.push("format: dates must be YYYY-MM-DD".into());
            None
        }
    }
}

fn canonical_codes(
    values: Vec<String>,
    canonical: fn(&str) -> Option<&'static str>,
    allowed: &[&str],
    errors: &mut Vec<String>,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for value in values {
        match canonical(&value).filter(|code| allowed.contains(code)) {
            Some(code) if !out.iter().any(|c| c == code) => out.push(code.to_string()),
            Some(_) => {}
            None => errors.push(format!("code: unknown code {}", value.trim())),
        }
    }
    out
}

fn normalize_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Decode one raw section into a value, confidence and evidence input.
fn decode(
    fact_type: FactType,
    raw: &Value,
) -> Result<(FactValue, Confidence, EvidenceIn, Vec<String>), String> {
    let mut errors = Vec::new();
    let fmt_err = |e: serde_json::Error| format!("format: {e}");
    let decoded = match fact_type {
        FactType::Age => {
            let a: AgeIn = serde_json::from_value(raw.clone()).map_err(fmt_err)?;
            (
                FactValue::Age {
                    min_age: a.min_age,
                    max_age: a.max_age,
                    basis: a.basis,
                },
                a.confidence,
                a.evidence,
            )
        }
        FactType::Region => {
            let r: CodesIn = serde_json::from_value(raw.clone()).map_err(fmt_err)?;
            let codes = canonical_codes(
                r.codes,
                mazimi_core::canonical_region_code,
                REGION_CODES,
                &mut errors,
            );
            (FactValue::Region { codes }, r.confidence, r.evidence)
        }
        FactType::Enrollment | FactType::Employment => {
            let s: StatusesIn = serde_json::from_value(raw.clone()).map_err(fmt_err)?;
            let value = if fact_type == FactType::Enrollment {
                FactValue::Enrollment {
                    statuses: canonical_codes(
                        s.statuses,
                        mazimi_core::canonical_enrollment_status,
                        ENROLLMENT_CODES,
                        &mut errors,
                    ),
                }
            } else {
                FactValue::Employment {
                    statuses: canonical_codes(
                        s.statuses,
                        mazimi_core::canonical_employment_status,
                        EMPLOYMENT_CODES,
                        &mut errors,
                    ),
                }
            };
            (value, s.confidence, s.evidence)
        }
        FactType::Income => {
            let i: IncomeIn = serde_json::from_value(raw.clone()).map_err(fmt_err)?;
            (
                FactValue::Income {
                    criterion: i.criterion,
                    comparator: i.comparator,
                    value: i.value,
                },
                i.confidence,
                i.evidence,
            )
        }
        FactType::Document => {
            let d: DocumentIn = serde_json::from_value(raw.clone()).map_err(fmt_err)?;
            (
                FactValue::Document {
                    name: normalize_text(&d.name),
                    required: d.required,
                    notes: d
                        .notes
                        .map(|n| normalize_text(&n))
                        .filter(|n| !n.is_empty()),
                },
                d.confidence,
                d.evidence,
            )
        }
        FactType::ApplicationWindow => {
            let w: WindowIn = serde_json::from_value(raw.clone()).map_err(fmt_err)?;
            let start_date = parse_date(w.start_date.as_deref(), &mut errors);
            let end_date = parse_date(w.end_date.as_deref(), &mut errors);
            (
                FactValue::ApplicationWindow {
                    kind: w.kind,
                    start_date,
                    end_date,
                },
                w.confidence,
                w.evidence,
            )
        }
    };
    Ok((decoded.0, decoded.1, decoded.2, errors))
}

fn process_section(
    fact_type: FactType,
    raw: &Value,
    index: usize,
    source: &ExtractionSource,
) -> ProcessedFact {
    let fallback_key = if fact_type == FactType::Document {
        format!("document:#{index}")
    } else {
        fact_type.as_str().to_string()
    };
    let invalid = |errors: Vec<String>,
                   confidence: Option<Confidence>,
                   evidence: Option<Evidence>| ProcessedFact {
        key: fallback_key.clone(),
        fact_type,
        value: raw.clone(),
        evidence,
        confidence,
        state: FactState::Invalid,
        validation: Validation {
            errors,
            flags: vec![],
        },
    };
    let (value, confidence, evidence_in, mut errors) = match decode(fact_type, raw) {
        Ok(decoded) => decoded,
        Err(error) => return invalid(vec![error], None, None),
    };
    let evidence = match anchor(&source.text, &evidence_in.quote, evidence_in.start) {
        Ok((start, end, offset_source)) => Evidence {
            quote: evidence_in.quote,
            start,
            end,
            source_hash: source.hash.clone(),
            offset_source,
        },
        Err(error) => {
            errors.push(error);
            let mut fact = invalid(errors, Some(confidence), None);
            fact.key = value.key();
            fact.value = serde_json::to_value(&value).unwrap_or_else(|_| raw.clone());
            return fact;
        }
    };
    let mut validation = validate_fact(&value, &evidence, source);
    errors.append(&mut validation.errors);
    validation.errors = errors;
    let state = if !validation.errors.is_empty() {
        FactState::Invalid
    } else if confidence != Confidence::High || !validation.flags.is_empty() {
        FactState::Flagged
    } else {
        FactState::Pending
    };
    ProcessedFact {
        key: value.key(),
        fact_type,
        value: serde_json::to_value(&value).expect("fact values serialize"),
        evidence: Some(evidence),
        confidence: Some(confidence),
        state,
        validation,
    }
}

/// Turn a raw model response into facts for storage. Rule-covered types are
/// dropped (rules win), duplicates keep the first occurrence, and a
/// response that is not the expected top-level object yields an error.
pub fn process_output(
    output: &Value,
    source: &ExtractionSource,
    coverage: &RuleCoverage,
) -> Result<Processed, String> {
    if !output.is_object() {
        return Err("format: output must be a JSON object".into());
    }
    let raw: RawOutput =
        serde_json::from_value(output.clone()).map_err(|e| format!("format: {e}"))?;
    if raw
        .documents
        .as_ref()
        .is_some_and(|d| d.len() > MAX_DOCUMENTS)
    {
        return Err(format!("format: more than {MAX_DOCUMENTS} documents"));
    }
    let mut counters = ProcessCounters::default();
    let mut facts: Vec<ProcessedFact> = Vec::new();
    for (index, (fact_type, value)) in raw.sections().into_iter().enumerate() {
        if coverage.covers(fact_type) {
            counters.excluded_by_rules += 1;
            continue;
        }
        let fact = process_section(fact_type, &value, index, source);
        // On a key collision keep the better fact (pending > flagged >
        // invalid); the other one is counted as invalid.
        let rank = |state: FactState| match state {
            FactState::Pending => 0,
            FactState::Flagged => 1,
            FactState::Invalid => 2,
        };
        if let Some(existing) = facts.iter().position(|f| f.key == fact.key) {
            counters.invalid += 1;
            if rank(fact.state) >= rank(facts[existing].state) {
                continue;
            }
            let replaced = facts.remove(existing);
            match replaced.state {
                FactState::Pending => counters.pending -= 1,
                FactState::Flagged => counters.flagged -= 1,
                FactState::Invalid => {
                    counters.invalid -= 1;
                    if replaced.validation.guard_rejected() {
                        counters.rejected_by_guard -= 1;
                    }
                    if replaced.validation.evidence_rejected() {
                        counters.invalid_evidence -= 1;
                    }
                }
            }
        }
        match fact.state {
            FactState::Pending => counters.pending += 1,
            FactState::Flagged => counters.flagged += 1,
            FactState::Invalid => {
                counters.invalid += 1;
                if fact.validation.guard_rejected() {
                    counters.rejected_by_guard += 1;
                }
                if fact.validation.evidence_rejected() {
                    counters.invalid_evidence += 1;
                }
            }
        }
        facts.push(fact);
    }
    Ok(Processed { facts, counters })
}

#[cfg(test)]
mod tests {
    use super::super::source::source_hash;
    use super::*;
    use serde_json::json;

    const TEXT: &str = "[제목] 부산 청년 월세 지원\n[요약] 부산광역시 거주 만 19세~34세 무주택 청년\n[sprtTrgtCn] 기준 중위소득 150% 이하, 소득분위 8분위 이하\n[제출서류] 신분증, 주민등록 등본, 임대차계약서(해당자)\n[aplyYmd] 신청기간: 2026.03.02 ~ 2026.04.30\n[기타] 예산 소진 시 조기 마감, 재학생 및 휴학생, 미취업 청년";

    fn source() -> ExtractionSource {
        ExtractionSource {
            text: TEXT.into(),
            hash: source_hash(TEXT),
        }
    }

    fn evidence(quote: &str) -> Value {
        let byte = TEXT.find(quote).expect("quote in fixture");
        json!({"quote": quote, "start": TEXT[..byte].chars().count()})
    }

    fn one(output: Value) -> ProcessedFact {
        let processed = process_output(&output, &source(), &RuleCoverage::default()).unwrap();
        assert_eq!(processed.facts.len(), 1, "{processed:?}");
        processed.facts.into_iter().next().unwrap()
    }

    fn age(min: i32, max: i32, quote: &str, confidence: &str) -> Value {
        json!({"age": {"min_age": min, "max_age": max, "basis": "man_age", "confidence": confidence, "evidence": evidence(quote)}})
    }

    #[test]
    fn exact_high_confidence_fact_is_pending_with_model_offsets() {
        let fact = one(age(19, 34, "만 19세~34세", "HIGH"));
        assert_eq!(fact.state, FactState::Pending, "{:?}", fact.validation);
        let evidence = fact.evidence.unwrap();
        assert_eq!(evidence.offset_source, OffsetSource::Model);
        assert!(span_matches(
            TEXT,
            evidence.start,
            evidence.end,
            "만 19세~34세"
        ));
        assert_eq!(evidence.source_hash, source_hash(TEXT));
        assert_eq!(fact.key, "age");
    }

    #[test]
    fn non_substring_evidence_is_invalid() {
        let output = json!({"age": {"min_age": 19, "max_age": 39, "basis": "man_age", "confidence": "HIGH",
            "evidence": {"quote": "만 19세~39세", "start": 10}}});
        let fact = one(output);
        assert_eq!(fact.state, FactState::Invalid);
        assert!(fact.validation.evidence_rejected());
        assert!(fact.evidence.is_none());
    }

    #[test]
    fn wrong_offset_is_reanchored_only_when_unambiguous() {
        let output = json!({"age": {"min_age": 19, "max_age": 34, "basis": "man_age", "confidence": "HIGH",
            "evidence": {"quote": "만 19세~34세", "start": 0}}});
        let fact = one(output);
        assert_eq!(fact.state, FactState::Pending);
        assert_eq!(
            fact.evidence.unwrap().offset_source,
            OffsetSource::ServerAnchored
        );
        // "청년" occurs several times: a wrong offset cannot be repaired.
        assert!(anchor(TEXT, "청년", Some(0)).is_err());
        let first = TEXT.find("청년").unwrap();
        let start = TEXT[..first].chars().count() as i64;
        assert_eq!(
            anchor(TEXT, "청년", Some(start)).unwrap().2,
            OffsetSource::Model
        );
    }

    #[test]
    fn stored_evidence_must_match_offsets_and_hash() {
        let value = FactValue::Age {
            min_age: Some(19),
            max_age: Some(34),
            basis: AgeBasis::ManAge,
        };
        let byte = TEXT.find("만 19세~34세").unwrap();
        let start = TEXT[..byte].chars().count();
        let good = Evidence {
            quote: "만 19세~34세".into(),
            start,
            end: start + "만 19세~34세".chars().count(),
            source_hash: source_hash(TEXT),
            offset_source: OffsetSource::Model,
        };
        assert!(validate_fact(&value, &good, &source()).errors.is_empty());
        let shifted = Evidence {
            start: start + 1,
            end: good.end + 1,
            ..good.clone()
        };
        assert!(validate_fact(&value, &shifted, &source()).evidence_rejected());
        let stale = Evidence {
            source_hash: source_hash("old text"),
            ..good.clone()
        };
        assert!(validate_fact(&value, &stale, &source()).evidence_rejected());
        let changed = ExtractionSource {
            text: TEXT.replace("19세", "18세"),
            hash: source_hash(&TEXT.replace("19세", "18세")),
        };
        assert!(validate_fact(&value, &good, &changed).evidence_rejected());
    }

    #[test]
    fn age_values_out_of_range_or_absent_from_quote_are_rejected() {
        assert!(one(age(19, 340, "만 19세~34세", "HIGH"))
            .validation
            .errors
            .iter()
            .any(|e| e.starts_with("range:")));
        assert!(one(age(30, 20, "만 19세~34세", "HIGH"))
            .validation
            .errors
            .iter()
            .any(|e| e.starts_with("range:")));
        let invented = one(age(18, 34, "만 19세~34세", "HIGH"));
        assert_eq!(invented.state, FactState::Invalid);
        assert!(invented.validation.guard_rejected());
    }

    #[test]
    fn age_without_age_phrase_is_rejected_by_guard() {
        let fact = one(age(8, 8, "소득분위 8분위 이하", "HIGH"));
        assert!(fact
            .validation
            .errors
            .contains(&"guard: quote has no age phrase".to_string()));
    }

    #[test]
    fn lower_confidence_or_non_man_age_is_flagged_not_pending() {
        assert_eq!(
            one(age(19, 34, "만 19세~34세", "MEDIUM")).state,
            FactState::Flagged
        );
        let year_age = json!({"age": {"min_age": 19, "max_age": 34, "basis": "year_age", "confidence": "HIGH", "evidence": evidence("만 19세~34세")}});
        let fact = one(year_age);
        assert_eq!(fact.state, FactState::Flagged);
        assert!(!fact.validation.flags.is_empty());
    }

    #[test]
    fn income_requires_an_income_phrase_criterion_token_and_quoted_value() {
        let ok = one(
            json!({"income": {"criterion": "median_income_percent", "comparator": "lte", "value": 150,
            "confidence": "HIGH", "evidence": evidence("기준 중위소득 150% 이하")}}),
        );
        assert_eq!(ok.state, FactState::Pending, "{:?}", ok.validation);
        let decile = one(
            json!({"income": {"criterion": "income_decile", "comparator": "lte", "value": 8,
            "confidence": "HIGH", "evidence": evidence("소득분위 8분위 이하")}}),
        );
        assert_eq!(decile.state, FactState::Pending, "{:?}", decile.validation);
        // An amount sentence is not an income requirement.
        let no_phrase = one(
            json!({"income": {"criterion": "median_income_percent", "comparator": "lte", "value": 34,
            "confidence": "HIGH", "evidence": evidence("만 19세~34세")}}),
        );
        assert!(no_phrase.validation.guard_rejected());
        let wrong_criterion = one(
            json!({"income": {"criterion": "kosaf_bracket", "comparator": "lte", "value": 8,
            "confidence": "HIGH", "evidence": evidence("소득분위 8분위 이하")}}),
        );
        assert!(wrong_criterion.validation.guard_rejected());
        let out_of_range = one(
            json!({"income": {"criterion": "income_decile", "comparator": "lte", "value": 150,
            "confidence": "HIGH", "evidence": evidence("기준 중위소득 150% 이하")}}),
        );
        assert!(out_of_range
            .validation
            .errors
            .iter()
            .any(|e| e.starts_with("range:")));
    }

    #[test]
    fn income_irrelevant_phrases_do_not_count_as_income_mentions() {
        assert!(!mentions_income("연말정산 소득공제 가능"));
        assert!(!mentions_income("소득 세 납부 내역"));
        assert!(mentions_income("가구 소득 인정액"));
        assert!(mentions_income("기준 중위 소득"));
    }

    #[test]
    fn unknown_codes_are_rejected_and_aliases_canonicalized() {
        let fact = one(
            json!({"region": {"codes": ["부산광역시", "atlantis"], "confidence": "HIGH",
            "evidence": evidence("부산광역시 거주")}}),
        );
        assert_eq!(fact.state, FactState::Invalid);
        assert!(fact
            .validation
            .errors
            .iter()
            .any(|e| e.starts_with("code:")));
        let ok = one(json!({"region": {"codes": ["busan"], "confidence": "HIGH",
            "evidence": evidence("부산광역시 거주")}}));
        assert_eq!(ok.state, FactState::Pending);
        assert_eq!(ok.value["codes"], json!(["busan"]));
        let unnamed = one(json!({"region": {"codes": ["daegu"], "confidence": "HIGH",
            "evidence": evidence("부산광역시 거주")}}));
        assert!(unnamed.validation.guard_rejected());
        let other = one(
            json!({"employment": {"statuses": ["other"], "confidence": "HIGH",
            "evidence": evidence("미취업 청년")}}),
        );
        assert!(other
            .validation
            .errors
            .iter()
            .any(|e| e.starts_with("code:")));
    }

    #[test]
    fn status_facts_need_keywords_in_the_quote() {
        let ok = one(
            json!({"enrollment": {"statuses": ["enrolled", "on_leave"], "confidence": "HIGH",
            "evidence": evidence("재학생 및 휴학생")}}),
        );
        assert_eq!(ok.state, FactState::Pending, "{:?}", ok.validation);
        let graduated = one(
            json!({"enrollment": {"statuses": ["graduated"], "confidence": "HIGH",
            "evidence": evidence("재학생 및 휴학생")}}),
        );
        assert!(graduated.validation.guard_rejected());
        let unemployed = one(
            json!({"employment": {"statuses": ["unemployed"], "confidence": "HIGH",
            "evidence": evidence("미취업 청년")}}),
        );
        assert_eq!(unemployed.state, FactState::Pending);
    }

    #[test]
    fn documents_need_their_name_quoted_and_are_keyed_uniquely() {
        let output = json!({"documents": [
            {"name": "주민등록등본", "required": true, "notes": null, "confidence": "HIGH", "evidence": evidence("주민등록 등본")},
            {"name": "주민등록 등본", "required": true, "notes": null, "confidence": "HIGH", "evidence": evidence("주민등록 등본")},
            {"name": "가족관계증명서", "required": true, "notes": null, "confidence": "HIGH", "evidence": evidence("신분증")},
            {"name": "<b>x</b>", "required": true, "notes": null, "confidence": "HIGH", "evidence": evidence("신분증")}
        ]});
        let processed = process_output(&output, &source(), &RuleCoverage::default()).unwrap();
        assert_eq!(processed.facts.len(), 3);
        assert_eq!(processed.facts[0].state, FactState::Pending);
        assert_eq!(processed.facts[0].key, "document:주민등록등본");
        assert!(processed.facts[1].validation.guard_rejected());
        assert!(processed.facts[2]
            .validation
            .errors
            .iter()
            .any(|e| e.starts_with("range:")));
        assert_eq!(processed.counters.invalid, 3, "duplicate counted invalid");
    }

    #[test]
    fn windows_need_quoted_dates_or_textual_kind() {
        let dated = one(
            json!({"application_window": {"kind": "dated", "start_date": "2026-03-02", "end_date": "2026-04-30",
            "confidence": "HIGH", "evidence": evidence("신청기간: 2026.03.02 ~ 2026.04.30")}}),
        );
        assert_eq!(dated.state, FactState::Pending, "{:?}", dated.validation);
        let invented = one(
            json!({"application_window": {"kind": "dated", "start_date": "2026-03-01", "end_date": "2026-04-30",
            "confidence": "HIGH", "evidence": evidence("신청기간: 2026.03.02 ~ 2026.04.30")}}),
        );
        assert!(invented.validation.guard_rejected());
        let reversed = one(
            json!({"application_window": {"kind": "dated", "start_date": "2026-04-30", "end_date": "2026-03-02",
            "confidence": "HIGH", "evidence": evidence("신청기간: 2026.03.02 ~ 2026.04.30")}}),
        );
        assert!(reversed
            .validation
            .errors
            .iter()
            .any(|e| e.starts_with("range:")));
        let budget = one(
            json!({"application_window": {"kind": "until_budget_exhausted", "start_date": null, "end_date": null,
            "confidence": "HIGH", "evidence": evidence("예산 소진 시 조기 마감")}}),
        );
        assert_eq!(budget.state, FactState::Pending);
        let bad_format = one(
            json!({"application_window": {"kind": "dated", "start_date": "03/02/2026", "end_date": null,
            "confidence": "HIGH", "evidence": evidence("신청기간: 2026.03.02 ~ 2026.04.30")}}),
        );
        assert!(bad_format
            .validation
            .errors
            .iter()
            .any(|e| e.starts_with("format:")));
    }

    #[test]
    fn date_parser_reads_korean_dotted_and_compact_forms() {
        let dates = dates_in("2026년 3월 2일부터 2026.4.30, 20261231 까지 / 2026-13-01");
        assert_eq!(
            dates,
            vec![
                NaiveDate::from_ymd_opt(2026, 3, 2).unwrap(),
                NaiveDate::from_ymd_opt(2026, 4, 30).unwrap(),
                NaiveDate::from_ymd_opt(2026, 12, 31).unwrap(),
            ]
        );
    }

    #[test]
    fn rule_covered_types_are_dropped_and_counted() {
        let coverage = RuleCoverage {
            age: true,
            ..RuleCoverage::default()
        };
        let processed =
            process_output(&age(19, 34, "만 19세~34세", "HIGH"), &source(), &coverage).unwrap();
        assert!(processed.facts.is_empty());
        assert_eq!(processed.counters.excluded_by_rules, 1);
    }

    #[test]
    fn malformed_sections_fail_alone_and_malformed_outputs_fail_whole() {
        let output = json!({
            "age": {"min_age": "nineteen"},
            "income": {"criterion": "income_decile", "comparator": "lte", "value": 8,
                "confidence": "HIGH", "evidence": evidence("소득분위 8분위 이하")}
        });
        let processed = process_output(&output, &source(), &RuleCoverage::default()).unwrap();
        assert_eq!(processed.facts.len(), 2);
        assert_eq!(processed.facts[0].state, FactState::Invalid);
        assert_eq!(processed.facts[1].state, FactState::Pending);
        assert!(
            process_output(&json!({"answer": 1}), &source(), &RuleCoverage::default()).is_err()
        );
        assert!(process_output(&json!([]), &source(), &RuleCoverage::default()).is_err());
        let too_many = json!({"documents": vec![json!({}); MAX_DOCUMENTS + 1]});
        assert!(process_output(&too_many, &source(), &RuleCoverage::default()).is_err());
    }

    fn source_of(text: &str) -> ExtractionSource {
        ExtractionSource {
            text: text.into(),
            hash: source_hash(text),
        }
    }

    fn single(text: &str, output: Value) -> ProcessedFact {
        let processed =
            process_output(&output, &source_of(text), &RuleCoverage::default()).unwrap();
        processed.facts.into_iter().next().expect("one fact")
    }

    fn quoted(text: &str, quote: &str) -> Value {
        json!({"quote": quote, "start": super::super::provider::char_offset(text, quote).unwrap()})
    }

    #[test]
    fn exclusive_age_bounds_must_be_converted_to_inclusive_columns() {
        let text = "[요약] 만 19세 이상 39세 미만 청년";
        let quote = "만 19세 이상 39세 미만";
        let age = |max: i32| {
            json!({"age": {"min_age": 19, "max_age": max, "basis": "man_age", "confidence": "HIGH",
                "evidence": quoted(text, quote)}})
        };
        let wrong = single(text, age(39));
        assert!(wrong.validation.guard_rejected(), "{:?}", wrong.validation);
        let right = single(text, age(38));
        assert_eq!(right.state, FactState::Pending, "{:?}", right.validation);
        let text = "[요약] 만 18세 초과 청년";
        let above = |min: i32| {
            json!({"age": {"min_age": min, "max_age": null, "basis": "man_age", "confidence": "HIGH",
                "evidence": quoted(text, "만 18세 초과")}})
        };
        assert!(single(text, above(18)).validation.guard_rejected());
        assert_eq!(single(text, above(19)).state, FactState::Pending);
    }

    #[test]
    fn gyeonggi_gwangju_city_is_not_the_gwangju_province() {
        let text = "[요약] 경기도 광주시 거주 청년";
        let fact = single(
            text,
            json!({"region": {"codes": ["gwangju"], "confidence": "HIGH", "evidence": quoted(text, "경기도 광주시 거주")}}),
        );
        assert!(fact.validation.guard_rejected());
    }

    #[test]
    fn region_without_residence_cue_is_flagged() {
        let text = "[제목] 부산 청년 문화 패스";
        let fact = single(
            text,
            json!({"region": {"codes": ["busan"], "confidence": "HIGH", "evidence": quoted(text, "부산 청년")}}),
        );
        assert_eq!(fact.state, FactState::Flagged, "{:?}", fact.validation);
    }

    #[test]
    fn student_status_is_not_satisfied_by_jaehaksaeng() {
        let text = "[요약] 재학생 대상";
        let fact = single(
            text,
            json!({"employment": {"statuses": ["student"], "confidence": "HIGH", "evidence": quoted(text, "재학생 대상")}}),
        );
        assert!(fact.validation.guard_rejected());
    }

    #[test]
    fn income_comparator_must_match_the_quoted_word() {
        let text = "[요약] 기준 중위소득 150% 이하";
        let income = |comparator: &str| {
            json!({"income": {"criterion": "median_income_percent", "comparator": comparator, "value": 150,
                "confidence": "HIGH", "evidence": quoted(text, "기준 중위소득 150% 이하")}})
        };
        assert_eq!(single(text, income("lte")).state, FactState::Pending);
        assert!(single(text, income("lt")).validation.guard_rejected());
        assert!(single(text, income("gte")).validation.guard_rejected());
    }

    #[test]
    fn overlapping_occurrences_are_ambiguous() {
        assert!(anchor("가가가", "가가", Some(5)).is_err());
        assert_eq!(anchor("가가나", "가가", Some(5)).unwrap().0, 0);
    }

    #[test]
    fn a_valid_fact_wins_over_an_earlier_invalid_duplicate() {
        let text = "[제출서류] 신분증, 주민등록 등본";
        let output = json!({"documents": [
            {"name": "주민등록등본", "required": true, "notes": null, "confidence": "HIGH",
             "evidence": {"quote": "없는 문장", "start": 0}},
            {"name": "주민등록 등본", "required": true, "notes": null, "confidence": "HIGH",
             "evidence": quoted(text, "주민등록 등본")}
        ]});
        let processed =
            process_output(&output, &source_of(text), &RuleCoverage::default()).unwrap();
        assert_eq!(processed.facts.len(), 1);
        assert_eq!(processed.facts[0].state, FactState::Pending);
        assert_eq!(processed.counters.invalid, 1);
        assert_eq!(processed.counters.invalid_evidence, 0);
    }

    #[test]
    fn over_long_document_names_keep_a_bounded_key() {
        let value = FactValue::Document {
            name: "가".repeat(500),
            required: true,
            notes: None,
        };
        assert!(value.key().chars().count() <= 120);
    }

    #[test]
    fn quote_length_is_bounded() {
        assert!(anchor(TEXT, "만", None).is_err());
        assert!(anchor(TEXT, &"가".repeat(MAX_QUOTE_CHARS + 1), None).is_err());
        assert!(anchor(TEXT, "  ", None).is_err());
    }
}
