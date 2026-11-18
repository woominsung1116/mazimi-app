//! Deterministic cross-check of a model fact against its own evidence quote.
//!
//! Each fact type has a rule parser that reads the quote without the model:
//! Korean age ranges, KST application windows, a curated document
//! vocabulary, enrollment/employment cues, region names and income criteria.
//! A fact is *rule-verified* only when the parser reads exactly one value
//! from the quote and that value equals the model's. Parsers are deliberately
//! narrow: anything ambiguous (two ranges, a clock time, a negation, a word
//! outside the vocabulary) is `no_parse`, which keeps the fact with a human.
//!
//! Bump [`PARSER_VERSION`] whenever a parser's accepted language changes; the
//! version is stored with every auto-applied fact so a bad version can be
//! reverted in one action.
use super::facts::{
    document_key, Comparator, FactValue, IncomeCriterion, WindowKind, REGION_CODES,
};
use chrono::{Datelike, NaiveDate};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::LazyLock;

pub const PARSER_VERSION: &str = "crosscheck-v2";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// The rule parser read the same value from the same quote.
    Agree,
    /// The rule parser read a different value.
    Disagree,
    /// The quote is outside what the rule parser accepts.
    NoParse,
}

/// Stored on the candidate (`auto_check`) and shown to the reviewer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CrossCheck {
    pub verdict: Verdict,
    pub parser_version: String,
    /// The parser rule that read the quote (for example `age_range`).
    pub rule: Option<String>,
    /// The parser's own reading, in the fact value's shape.
    pub parsed: Option<Value>,
    /// Why the fact is not rule-verified.
    pub reason: Option<String>,
}

impl CrossCheck {
    fn agree(rule: &str, parsed: Value) -> Self {
        Self {
            verdict: Verdict::Agree,
            parser_version: PARSER_VERSION.into(),
            rule: Some(rule.into()),
            parsed: Some(parsed),
            reason: None,
        }
    }
    fn disagree(rule: &str, parsed: Value, reason: String) -> Self {
        Self {
            verdict: Verdict::Disagree,
            parser_version: PARSER_VERSION.into(),
            rule: Some(rule.into()),
            parsed: Some(parsed),
            reason: Some(reason),
        }
    }
    fn no_parse(reason: impl Into<String>) -> Self {
        Self {
            verdict: Verdict::NoParse,
            parser_version: PARSER_VERSION.into(),
            rule: None,
            parsed: None,
            reason: Some(reason.into()),
        }
    }
    pub fn agrees(&self) -> bool {
        self.verdict == Verdict::Agree
    }
}

/// Re-read `quote` with the rule parser for `value`'s type and compare.
pub fn cross_check(value: &FactValue, quote: &str) -> CrossCheck {
    match value {
        FactValue::Age {
            min_age, max_age, ..
        } => match parse_age(quote) {
            Ok(reading) => {
                let parsed = json!({"min_age": reading.min, "max_age": reading.max});
                if (reading.min, reading.max) == (*min_age, *max_age) {
                    CrossCheck::agree(reading.rule, parsed)
                } else {
                    CrossCheck::disagree(
                        reading.rule,
                        parsed,
                        format!(
                            "rule read min {:?} / max {:?}; model said {min_age:?} / {max_age:?}",
                            reading.min, reading.max
                        ),
                    )
                }
            }
            Err(reason) => CrossCheck::no_parse(reason),
        },
        FactValue::ApplicationWindow {
            kind,
            start_date,
            end_date,
        } => match parse_window(quote) {
            Ok(reading) => {
                let parsed = json!({"kind": reading.kind, "start_date": reading.start, "end_date": reading.end});
                if (reading.kind, reading.start, reading.end) == (*kind, *start_date, *end_date) {
                    CrossCheck::agree(reading.rule, parsed)
                } else {
                    CrossCheck::disagree(
                        reading.rule,
                        parsed.clone(),
                        format!(
                            "rule read {parsed}; model said {kind:?} {start_date:?}~{end_date:?}"
                        ),
                    )
                }
            }
            Err(reason) => CrossCheck::no_parse(reason),
        },
        FactValue::Document {
            name,
            required,
            notes,
        } => check_document(name, *required, notes.as_deref(), quote),
        FactValue::Region { codes } => compare_codes("region_cue", codes, parse_regions(quote)),
        FactValue::Enrollment { statuses } => {
            compare_codes("enrollment_cue", statuses, parse_enrollment(quote))
        }
        FactValue::Employment { statuses } => {
            compare_codes("employment_cue", statuses, parse_employment(quote))
        }
        FactValue::Income {
            criterion,
            comparator,
            value,
        } => {
            match parse_income(quote) {
                Ok(reading) => {
                    let parsed = json!({"criterion": reading.criterion, "comparator": reading.comparator, "value": reading.value});
                    if (reading.criterion, reading.comparator, reading.value)
                        == (*criterion, *comparator, *value)
                    {
                        CrossCheck::agree(reading.rule, parsed)
                    } else {
                        CrossCheck::disagree(
                        reading.rule,
                        parsed.clone(),
                        format!("rule read {parsed}; model said {criterion:?} {comparator:?} {value}"),
                    )
                    }
                }
                Err(reason) => CrossCheck::no_parse(reason),
            }
        }
    }
}

fn compare_codes(
    rule: &str,
    model: &[String],
    parsed: Result<Vec<&'static str>, String>,
) -> CrossCheck {
    match parsed {
        Ok(codes) => {
            let mut model: Vec<&str> = model.iter().map(String::as_str).collect();
            model.sort_unstable();
            model.dedup();
            let mut sorted = codes.clone();
            sorted.sort_unstable();
            if model == sorted {
                CrossCheck::agree(rule, json!(codes))
            } else {
                CrossCheck::disagree(
                    rule,
                    json!(codes),
                    format!("rule read {codes:?}; model said {model:?}"),
                )
            }
        }
        Err(reason) => CrossCheck::no_parse(reason),
    }
}

// ── Text helpers ──────────────────────────────────────────────────────────

/// Whitespace, middle dots and tilde/dash variants removed or unified; the
/// form every parser below matches against.
fn squash(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_whitespace() && !matches!(c, '·' | '‧' | '・' | 'ㆍ'))
        .map(|c| match c {
            '∼' | '〜' | '～' | '－' | '–' | '—' => '~',
            other => other,
        })
        .flat_map(char::to_lowercase)
        .collect()
}

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("valid crosscheck regex")
}

/// Words that turn a cue into its opposite or an exception.
const NEGATIONS: &[&str] = &["제외", "불가", "불가능", "아닌", "않은", "미해당", "외의"];

fn has_negation(squashed: &str) -> bool {
    NEGATIONS.iter().any(|n| squashed.contains(n))
}

/// Words that attach an exception or extension to a bound
/// (`만 34세 이하 (군 복무기간만큼 연장)`, `단, 1인 가구는 …`).
const EXCEPTIONS: &[&str] = &["연장", "단,", "단서", "다만", "예외", "최대", "가산"];

fn has_exception(squashed: &str) -> bool {
    EXCEPTIONS.iter().any(|w| squashed.contains(w))
}

// ── Age ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgeReading {
    pub rule: &'static str,
    pub min: Option<i32>,
    pub max: Option<i32>,
}

/// `만 19세 이상 34세 이하`, `19~34세`, `만 19세부터 34세까지`, `만 39세 이하`,
/// `39세 미만` (→ 38), `18세 초과` (→ 19). One age expression per quote; any
/// other `N세`/`N살`, a birth year, or a reversed range is not parsed.
static AGE_RANGE: LazyLock<Regex> =
    LazyLock::new(|| re(r"만?(\d{1,3})세?(?:이상~?|~|부터|에서)만?(\d{1,3})세(이하|미만|까지)?"));
static AGE_UPPER: LazyLock<Regex> = LazyLock::new(|| re(r"만?(\d{1,3})세(이하|미만|까지)"));
static AGE_LOWER: LazyLock<Regex> = LazyLock::new(|| re(r"만?(\d{1,3})세(이상|초과|부터)"));
static AGE_ANY: LazyLock<Regex> = LazyLock::new(|| re(r"\d+(?:세|살)"));
/// Words that make an age describe another person (child, parent, household).
const AGE_OTHER_SUBJECTS: &[&str] = &[
    "자녀",
    "아동",
    "영유아",
    "유아",
    "부모",
    "배우자",
    "가구원",
    "세대원",
    "부양",
];

pub fn parse_age(quote: &str) -> Result<AgeReading, String> {
    let text = squash(quote).replace('-', "~");
    if ["년생", "출생", "생년", "연나이"]
        .iter()
        .any(|w| text.contains(w))
    {
        return Err("age is stated by birth year or 연 나이".into());
    }
    // "만 65세 이상은 제외" is an exclusion, not a bound.
    if has_negation(&text) || has_exception(&text) {
        return Err("the quote has a negation, exception or extension".into());
    }
    // "자녀 만 7세 이하" is someone else's age, not the applicant's.
    if AGE_OTHER_SUBJECTS.iter().any(|w| text.contains(w)) {
        return Err("the age may describe someone other than the applicant".into());
    }
    let parse = |s: &str| s.parse::<i32>().ok();
    let mut rest = text.clone();
    // `2026~34세` is not an age: every match must start at a number boundary.
    let at_boundary = |start: usize| !text[..start].ends_with(|c: char| c.is_ascii_digit());
    let ranges: Vec<_> = AGE_RANGE.captures_iter(&text).collect();
    if ranges
        .iter()
        .any(|c| !at_boundary(c.get(0).expect("match").start()))
    {
        return Err("an age number is part of a longer number".into());
    }
    if ranges.len() > 1 {
        return Err("more than one age range in the quote".into());
    }
    let mut reading = if let Some(range) = ranges.first() {
        let (Some(min), Some(mut max)) = (parse(&range[1]), parse(&range[2])) else {
            return Err("unreadable age".into());
        };
        if range.get(3).map(|m| m.as_str()) == Some("미만") {
            max -= 1;
        }
        rest = rest.replacen(&range[0], " ", 1);
        Some(AgeReading {
            rule: "age_range",
            min: Some(min),
            max: Some(max),
        })
    } else {
        None
    };
    let uppers: Vec<(i32, String)> = AGE_UPPER
        .captures_iter(&rest)
        .filter_map(|c| Some((parse(&c[1])?, c[2].to_string())))
        .collect();
    let lowers: Vec<(i32, String)> = AGE_LOWER
        .captures_iter(&rest)
        .filter_map(|c| Some((parse(&c[1])?, c[2].to_string())))
        .collect();
    let without_upper = AGE_UPPER.replace_all(&rest, " ");
    let leftover = AGE_LOWER.replace_all(&without_upper, " ");
    if AGE_ANY.is_match(&leftover) {
        return Err("the quote names another age".into());
    }
    if reading.is_some() && (!uppers.is_empty() || !lowers.is_empty()) {
        return Err("more than one age expression in the quote".into());
    }
    if uppers.len() > 1 || lowers.len() > 1 {
        return Err("more than one age bound of one kind".into());
    }
    if reading.is_none() {
        let max = uppers
            .first()
            .map(|(age, word)| if word == "미만" { age - 1 } else { *age });
        let min = lowers
            .first()
            .map(|(age, word)| if word == "초과" { age + 1 } else { *age });
        reading = match (min, max) {
            (None, None) => return Err("no age expression".into()),
            (Some(_), Some(_)) => Some(AgeReading {
                rule: "age_bounds",
                min,
                max,
            }),
            (None, Some(_)) => Some(AgeReading {
                rule: "age_upper",
                min,
                max,
            }),
            (Some(_), None) => Some(AgeReading {
                rule: "age_lower",
                min,
                max,
            }),
        };
    }
    let reading = reading.expect("set above");
    let in_range = |v: Option<i32>| v.is_none_or(|v| (0..=100).contains(&v));
    if !in_range(reading.min)
        || !in_range(reading.max)
        || reading.min.zip(reading.max).is_some_and(|(a, b)| a > b)
    {
        return Err("age out of range or reversed".into());
    }
    Ok(reading)
}

// ── Application window (KST calendar dates) ──────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowReading {
    pub rule: &'static str,
    pub kind: WindowKind,
    pub start: Option<NaiveDate>,
    pub end: Option<NaiveDate>,
}

static FULL_DATE: LazyLock<Regex> =
    LazyLock::new(|| re(r"(\d{4})\s*(?:[./\-]|년)\s*(\d{1,2})\s*(?:[./\-]|월)\s*(\d{1,2})\s*일?"));
/// Eight digits; the next character is checked separately so adjacent
/// dates (`20260302~20260430`) are both found.
static COMPACT_DATE: LazyLock<Regex> = LazyLock::new(|| re(r"(?:^|[^0-9])((\d{4})(\d{2})(\d{2}))"));
/// Month and day without a year: only valid after a full date (range end).
static PARTIAL_DATE: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?:^|[^0-9])((\d{1,2})\s*(?:[./]|월)\s*(\d{1,2})\s*일?)"));
/// A clock time narrows a day bound; the columns store whole KST days, so
/// such a quote stays with a human.
static CLOCK_TIME: LazyLock<Regex> = LazyLock::new(|| {
    re(r"\d{1,2}\s*:\s*\d{2}|\d{1,2}\s*시\s*(?:\d{1,2}\s*분|까지|마감|부터|정각)")
});

fn blank(text: &mut String, start: usize, end: usize) {
    let filler: String = text[start..end]
        .chars()
        .map(|c| " ".repeat(c.len_utf8()))
        .collect();
    text.replace_range(start..end, &filler);
}

/// Dates in quote order: `2026.03.02`, `2026-03-02`, `2026년 3월 2일`,
/// `20260302`, and a year-less range end (`~ 04.30`, `~ 4월 30일`) that takes
/// the previous date's year (the next year when it would fall before it).
fn ordered_dates(quote: &str) -> Result<Vec<(usize, usize, NaiveDate)>, String> {
    let mut found: Vec<(usize, usize, NaiveDate)> = Vec::new();
    let mut masked = quote.to_string();
    let date = |y: &str, m: &str, d: &str| -> Result<NaiveDate, String> {
        let (y, m, d) = (
            y.parse::<i32>().map_err(|_| "bad year")?,
            m.parse::<u32>().map_err(|_| "bad month")?,
            d.parse::<u32>().map_err(|_| "bad day")?,
        );
        NaiveDate::from_ymd_opt(y, m, d)
            .filter(|date| (2000..=2100).contains(&date.year()))
            .ok_or_else(|| "invalid calendar date".to_string())
    };
    for c in FULL_DATE.captures_iter(quote) {
        let whole = c.get(0).expect("match");
        found.push((whole.start(), whole.end(), date(&c[1], &c[2], &c[3])?));
    }
    for (start, end, _) in found.clone() {
        blank(&mut masked, start, end);
    }
    let compact: Vec<_> = COMPACT_DATE
        .captures_iter(&masked)
        .filter(|c| {
            let end = c.get(1).expect("group").end();
            !masked[end..].starts_with(|ch: char| ch.is_ascii_digit())
        })
        .map(|c| {
            let whole = c.get(1).expect("group");
            (
                whole.start(),
                whole.end(),
                c[2].to_string(),
                c[3].to_string(),
                c[4].to_string(),
            )
        })
        .collect();
    for (start, end, y, m, d) in compact {
        found.push((start, end, date(&y, &m, &d)?));
        blank(&mut masked, start, end);
    }
    found.sort_by_key(|(start, ..)| *start);
    let partial: Vec<_> = PARTIAL_DATE
        .captures_iter(&masked)
        .map(|c| {
            let whole = c.get(1).expect("group");
            (
                whole.start(),
                whole.end(),
                c[2].to_string(),
                c[3].to_string(),
            )
        })
        .collect();
    for (start, end, m, d) in partial {
        let Some(previous) = found.iter().rev().find(|(s, ..)| *s < start).map(|f| f.2) else {
            return Err("a date without a year has no preceding full date".into());
        };
        let mut resolved = date(&previous.year().to_string(), &m, &d)?;
        if resolved < previous {
            resolved = date(&(previous.year() + 1).to_string(), &m, &d)?;
        }
        found.push((start, end, resolved));
    }
    found.sort_by_key(|(start, ..)| *start);
    Ok(found)
}

/// The squashed text right after a date, without a trailing `.` or a
/// weekday such as `(목)`.
fn after_date(rest: &str) -> String {
    let mut chars: Vec<char> = squash(rest).chars().collect();
    loop {
        if chars.first() == Some(&'.') {
            chars.remove(0);
        } else if chars.len() >= 3 && chars[0] == '(' && chars[2] == ')' {
            chars.drain(..3);
        } else {
            break;
        }
    }
    chars.into_iter().take(6).collect()
}

/// Not bare 상시/연중: `상시근로자`, `연중 2회` are not windows.
const ALWAYS_OPEN_CUES: &[&str] = &[
    "상시모집",
    "상시접수",
    "상시신청",
    "연중상시",
    "연중모집",
    "연중접수",
    "연중신청",
];
/// A dated quote must say it is the application window...
const WINDOW_CUES: &[&str] = &["신청", "접수", "모집", "공모"];
/// ...and not another period of the program (사업기간, 교육기간, 결과 발표).
const OTHER_PERIODS: &[&str] = &[
    "사업기간",
    "활동기간",
    "교육기간",
    "운영기간",
    "계약기간",
    "지원기간",
    "근무기간",
    "발표",
    "선정",
    "지급",
    "결과",
];

fn budget_cue(squashed: &str) -> bool {
    squashed.contains("예산") && (squashed.contains("소진") || squashed.contains("조기마감"))
}

pub fn parse_window(quote: &str) -> Result<WindowReading, String> {
    if CLOCK_TIME.is_match(quote) {
        return Err("the quote has a clock time; day bounds would change it".into());
    }
    let squashed = squash(quote);
    if has_negation(&squashed) {
        return Err("the quote has a negation or exception".into());
    }
    let always = ALWAYS_OPEN_CUES.iter().any(|c| squashed.contains(c));
    let budget = budget_cue(&squashed);
    let dates = ordered_dates(quote)?;
    if !dates.is_empty() {
        if !WINDOW_CUES.iter().any(|c| squashed.contains(c)) {
            return Err("dates without 신청/접수/모집: may not be the application window".into());
        }
        if OTHER_PERIODS.iter().any(|c| squashed.contains(c)) {
            return Err("the quote names another period (사업·교육기간, 발표 …)".into());
        }
    }
    let reading = |rule, kind, start, end| WindowReading {
        rule,
        kind,
        start,
        end,
    };
    match dates.as_slice() {
        [] => match (always, budget) {
            (true, false) => Ok(reading(
                "window_always_open",
                WindowKind::AlwaysOpen,
                None,
                None,
            )),
            (false, true) => Ok(reading(
                "window_budget",
                WindowKind::UntilBudgetExhausted,
                None,
                None,
            )),
            (true, true) => Err("상시 and 예산 소진 together".into()),
            (false, false) => Err("no date or window cue".into()),
        },
        _ if always => Err("상시/연중 next to a date".into()),
        [(_, end, date)] => {
            let after = after_date(&quote[*end..]);
            if after.starts_with("까지") || after.starts_with("마감") {
                Ok(reading("window_end", WindowKind::Dated, None, Some(*date)))
            } else if after.starts_with("부터") || after == "~" {
                Ok(reading(
                    "window_start",
                    WindowKind::Dated,
                    Some(*date),
                    None,
                ))
            } else {
                Err("one date without 부터/까지".into())
            }
        }
        [(_, first_end, start), (second_start, _, end)] => {
            let between = squash(&quote[*first_end..*second_start]);
            let separated = between.contains('~')
                || between.contains('-')
                || between.contains("부터")
                || between.contains("에서");
            if !separated {
                return Err("two dates without a range separator".into());
            }
            if start > end {
                return Err("start date is after end date".into());
            }
            Ok(reading(
                "window_range",
                WindowKind::Dated,
                Some(*start),
                Some(*end),
            ))
        }
        _ => Err("more than two dates (rounds or several windows)".into()),
    }
}

// ── Documents ─────────────────────────────────────────────────────────────

/// Curated Korean document vocabulary: `(canonical, aliases)`. Aliases are
/// matched on the squashed text with longest-match tokenization, so
/// `사업자등록증명원` never reads as `사업자등록증`. A generic word (등본,
/// 증명서, 서류) is never an alias.
pub const DOCUMENT_VOCABULARY: &[(&str, &[&str])] = &[
    ("신분증", &["신분증"]),
    ("주민등록증", &["주민등록증"]),
    ("주민등록등본", &["주민등록등본", "주민등록표등본"]),
    ("주민등록초본", &["주민등록초본", "주민등록표초본"]),
    ("가족관계증명서", &["가족관계증명서", "가족관계증명원"]),
    ("기본증명서", &["기본증명서"]),
    ("혼인관계증명서", &["혼인관계증명서"]),
    ("재학증명서", &["재학증명서"]),
    ("휴학증명서", &["휴학증명서"]),
    ("졸업증명서", &["졸업증명서"]),
    ("졸업예정증명서", &["졸업예정증명서"]),
    ("성적증명서", &["성적증명서"]),
    ("재직증명서", &["재직증명서"]),
    ("소득금액증명원", &["소득금액증명원", "소득금액증명"]),
    (
        "건강보험 자격득실확인서",
        &[
            "건강보험자격득실확인서",
            "건강보험자격득실증명서",
            "자격득실확인서",
        ],
    ),
    (
        "건강보험료 납부확인서",
        &["건강보험료납부확인서", "건강보험료납부증명서"],
    ),
    (
        "고용보험 피보험자격 이력내역서",
        &["고용보험피보험자격이력내역서", "피보험자격이력내역서"],
    ),
    ("사업자등록증", &["사업자등록증"]),
    ("사업자등록증명원", &["사업자등록증명원", "사업자등록증명"]),
    ("지방세 납세증명서", &["지방세납세증명서"]),
    ("국세 납세증명서", &["국세납세증명서"]),
    ("임대차계약서", &["임대차계약서", "주택임대차계약서"]),
    ("통장 사본", &["통장사본"]),
    ("장애인증명서", &["장애인증명서"]),
    (
        "기초생활수급자 증명서",
        &["기초생활수급자증명서", "수급자증명서"],
    ),
    ("차상위계층 확인서", &["차상위계층확인서", "차상위확인서"]),
    ("한부모가족 증명서", &["한부모가족증명서"]),
    ("병적증명서", &["병적증명서"]),
    ("자기소개서", &["자기소개서"]),
    ("사업계획서", &["사업계획서"]),
    ("신청서", &["신청서", "지원신청서", "참가신청서"]),
    (
        "개인정보 수집·이용 동의서",
        &[
            "개인정보수집이용동의서",
            "개인정보제공동의서",
            "개인정보활용동의서",
        ],
    ),
];

/// The vocabulary entry for a document name, if the name is exactly one of
/// its aliases (after squashing).
pub fn vocabulary_entry(name: &str) -> Option<&'static str> {
    let key = squash(name);
    DOCUMENT_VOCABULARY
        .iter()
        .find(|(_, aliases)| aliases.contains(&key.as_str()))
        .map(|(canonical, _)| *canonical)
}

/// Cues right after a document name that make it conditional.
const OPTIONAL_CUES: &[&str] = &[
    "해당자",
    "해당시",
    "해당하는경우",
    "해당경우",
    "선택",
    "필요시",
    "필요한경우",
    "있는경우",
    "소지자",
];
const DOCUMENT_NEGATIONS: &[&str] = &["제외", "불필요", "생략", "제출하지"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentReading {
    pub canonical: &'static str,
    pub required: bool,
}

/// Every vocabulary document named in the quote, longest match first, with
/// whether the text right after it makes it conditional.
pub fn parse_documents(quote: &str) -> Result<Vec<DocumentReading>, String> {
    let text = squash(quote);
    // `등본 또는 초본`, `둘 중 택1`: a choice, not a list of requirements.
    if DOCUMENT_CHOICES.iter().any(|w| text.contains(w)) {
        return Err("the quote offers a choice between documents".into());
    }
    let chars: Vec<char> = text.chars().collect();
    // Characters that belong to a name or its own tail; a cue anywhere else
    // (`해당자에 한해 제출: …`, `제출 불필요: …`) applies to the whole list.
    let mut covered = vec![false; chars.len()];
    let mut aliases: Vec<(Vec<char>, &'static str)> = DOCUMENT_VOCABULARY
        .iter()
        .flat_map(|(canonical, aliases)| aliases.iter().map(|a| (a.chars().collect(), *canonical)))
        .collect();
    aliases.sort_by_key(|(alias, _)| std::cmp::Reverse(alias.len()));
    let mut out: Vec<DocumentReading> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let hit = aliases
            .iter()
            .find(|(alias, _)| chars[i..].starts_with(alias));
        let Some((alias, canonical)) = hit else {
            i += 1;
            continue;
        };
        let end = i + alias.len();
        // The clause after the name, up to the next list separator.
        let tail: String = chars[end..]
            .iter()
            .take_while(|c| !matches!(c, ',' | '/' | ';' | '\n' | '、'))
            .take(14)
            .collect();
        for slot in covered
            .iter_mut()
            .skip(i)
            .take(alias.len() + tail.chars().count())
        {
            *slot = true;
        }
        if DOCUMENT_NEGATIONS.iter().any(|n| tail.contains(n)) {
            return Err(format!("'{canonical}' is negated in the quote"));
        }
        let required = !OPTIONAL_CUES.iter().any(|cue| tail.contains(cue));
        match out.iter_mut().find(|d| d.canonical == *canonical) {
            Some(existing) if existing.required != required => {
                return Err(format!("'{canonical}' is both required and conditional"));
            }
            Some(_) => {}
            None => out.push(DocumentReading {
                canonical,
                required,
            }),
        }
        i = end;
    }
    let outside: String = chars
        .iter()
        .zip(&covered)
        .filter(|(_, covered)| !**covered)
        .map(|(c, _)| *c)
        .collect();
    if OPTIONAL_CUES
        .iter()
        .chain(DOCUMENT_NEGATIONS)
        .any(|cue| outside.contains(cue))
    {
        return Err("a condition or negation outside the names applies to the list".into());
    }
    Ok(out)
}

const DOCUMENT_CHOICES: &[&str] = &["또는", "택1", "택일", "중1", "중하나"];

fn check_document(name: &str, required: bool, notes: Option<&str>, quote: &str) -> CrossCheck {
    let Some(canonical) = vocabulary_entry(name) else {
        return CrossCheck::no_parse(format!(
            "'{name}' is not in the curated document vocabulary"
        ));
    };
    let documents = match parse_documents(quote) {
        Ok(documents) => documents,
        Err(reason) => return CrossCheck::no_parse(reason),
    };
    let Some(found) = documents.iter().find(|d| d.canonical == canonical) else {
        return CrossCheck::no_parse(format!("the quote does not name '{canonical}'"));
    };
    let parsed = json!({"name": canonical, "required": found.required});
    if found.required != required {
        return CrossCheck::disagree(
            "document_vocabulary",
            parsed,
            format!(
                "rule read required={}; model said required={required}",
                found.required
            ),
        );
    }
    // Free-text notes reach program_documents.description: only verbatim.
    if let Some(notes) = notes.filter(|n| !squash(n).is_empty()) {
        if !squash(quote).contains(&squash(notes)) {
            return CrossCheck::disagree(
                "document_vocabulary",
                parsed,
                "notes are not a verbatim part of the quote".into(),
            );
        }
    }
    // Same identity as the stored fact key, so the applied row is the one
    // the reviewer would see.
    debug_assert!(!document_key(name).is_empty());
    CrossCheck::agree("document_vocabulary", parsed)
}

// ── Region ────────────────────────────────────────────────────────────────

const RESIDENCE_CUES: &[&str] = &["거주", "주소", "주민등록", "소재", "전입"];

fn region_aliases() -> Vec<(Vec<char>, &'static str)> {
    let mut aliases: Vec<(Vec<char>, &'static str)> = Vec::new();
    for code in REGION_CODES {
        let names: &[&str] = match *code {
            "seoul" => &["서울특별시", "서울시", "서울"],
            "busan" => &["부산광역시", "부산시", "부산"],
            "daegu" => &["대구광역시", "대구시", "대구"],
            "incheon" => &["인천광역시", "인천시", "인천"],
            "gwangju" => &["광주광역시", "광주"],
            "daejeon" => &["대전광역시", "대전시", "대전"],
            "ulsan" => &["울산광역시", "울산시", "울산"],
            "sejong" => &["세종특별자치시", "세종시", "세종"],
            "gyeonggi" => &["경기도", "경기"],
            "gangwon" => &["강원특별자치도", "강원도", "강원"],
            "chungbuk" => &["충청북도", "충북"],
            "chungnam" => &["충청남도", "충남"],
            "jeonbuk" => &["전북특별자치도", "전라북도", "전북"],
            "jeonnam" => &["전라남도", "전남"],
            "gyeongbuk" => &["경상북도", "경북"],
            "gyeongnam" => &["경상남도", "경남"],
            "jeju" => &["제주특별자치도", "제주도", "제주"],
            _ => &[],
        };
        for name in names {
            aliases.push((name.chars().collect(), code));
        }
    }
    aliases.sort_by_key(|(alias, _)| std::cmp::Reverse(alias.len()));
    aliases
}

/// Province-level region codes named in a residence requirement. `광주시`
/// (a Gyeonggi city) and university names (`부산대`) are not provinces.
pub fn parse_regions(quote: &str) -> Result<Vec<&'static str>, String> {
    let text = squash(quote);
    if !RESIDENCE_CUES.iter().any(|c| text.contains(c)) {
        return Err("no residence cue (거주/주소/주민등록/소재)".into());
    }
    if has_negation(&text) || text.contains("타지역") || text.contains("외지역") {
        return Err("the quote excludes a region".into());
    }
    let chars: Vec<char> = text.chars().collect();
    let aliases = region_aliases();
    let mut codes: Vec<&'static str> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let Some((alias, code)) = aliases.iter().find(|(a, _)| chars[i..].starts_with(a)) else {
            i += 1;
            continue;
        };
        let end = i + alias.len();
        let next = chars.get(end).copied();
        if *code == "gwangju" && alias.len() == 2 && next == Some('시') {
            return Err("광주시 is a Gyeonggi city, not 광주광역시".into());
        }
        // 부산대(학교), 충남대: a university, not a residence.
        let ends_in_unit = matches!(alias.last(), Some('시' | '도'));
        if !ends_in_unit && next == Some('대') {
            i = end;
            continue;
        }
        if !codes.contains(code) {
            codes.push(code);
        }
        i = end;
    }
    if codes.is_empty() {
        return Err("no province or metropolitan city named".into());
    }
    Ok(codes)
}

// ── Enrollment and employment ────────────────────────────────────────────

/// Document names that contain a status word but are not a status.
const STATUS_DOCUMENTS: &[&str] = &[
    "재학증명서",
    "휴학증명서",
    "졸업증명서",
    "졸업예정증명서",
    "재직증명서",
];

fn status_text(quote: &str) -> Result<String, String> {
    let mut text = squash(quote);
    if has_negation(&text) {
        return Err("the quote has a negation or exception".into());
    }
    for document in STATUS_DOCUMENTS {
        text = text.replace(document, " ");
    }
    Ok(text)
}

pub fn parse_enrollment(quote: &str) -> Result<Vec<&'static str>, String> {
    let mut text = status_text(quote)?;
    let mut codes = Vec::new();
    // 졸업예정자 is enrolled (core `canonical_enrollment_status`).
    if text.contains("졸업예정") {
        codes.push("enrolled");
        text = text.replace("졸업예정", " ");
    }
    for (cue, code) in [
        ("재학", "enrolled"),
        ("휴학", "on_leave"),
        ("졸업", "graduated"),
        ("연구생", "research_student"),
        ("수료생", "research_student"),
    ] {
        if text.contains(cue) && !codes.contains(&code) {
            codes.push(code);
        }
    }
    if codes.is_empty() {
        return Err("no enrollment cue".into());
    }
    Ok(codes)
}

pub fn parse_employment(quote: &str) -> Result<Vec<&'static str>, String> {
    let text = status_text(quote)?;
    let mut codes = Vec::new();
    let student_text = ["재학생", "휴학생", "졸업생", "연구생", "대학생", "수료생"]
        .iter()
        .fold(text.clone(), |t, w| t.replace(w, " "));
    for (cues, code) in [
        (&["미취업", "무직", "실업자"][..], "unemployed"),
        (&["취업준비", "취준", "구직"][..], "preparing"),
        (&["재직", "근로자", "직장인"][..], "employed"),
        (&["신규입사", "신규채용", "신입사원"][..], "new_hire"),
        (&["자영업", "프리랜서", "개인사업자"][..], "self_employed"),
    ] {
        if cues.iter().any(|c| text.contains(c)) {
            codes.push(code);
        }
    }
    if student_text.contains("학생") {
        codes.push("student");
    }
    if codes.is_empty() {
        return Err("no employment cue".into());
    }
    Ok(codes)
}

// ── Income ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IncomeReading {
    pub rule: &'static str,
    pub criterion: IncomeCriterion,
    pub comparator: Comparator,
    pub value: i32,
}

static INCOME_MEDIAN: LazyLock<Regex> =
    LazyLock::new(|| re(r"중위소득(?:의)?(\d{1,3})(?:%|퍼센트)(이하|미만|이상|초과|이내)"));
static INCOME_DECILE: LazyLock<Regex> =
    LazyLock::new(|| re(r"소득(?:분위)?(\d{1,2})분위(이하|미만|이상|초과|이내)"));
static INCOME_KOSAF: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?:학자금)?지원(?:구간)?(\d{1,2})구간(이하|미만|이상|초과|이내)"));

static INCOME_LEFTOVER: LazyLock<Regex> =
    LazyLock::new(|| re(r"\d+(?:%|퍼센트|분위|구간)|(?:분위|구간)\d"));

/// `기준 중위소득 150% 이하`, `소득분위 8분위 이하`, `학자금 지원구간 8구간 이하`.
/// Exactly one income criterion per quote.
pub fn parse_income(quote: &str) -> Result<IncomeReading, String> {
    let text = squash(quote);
    if has_exception(&text) {
        return Err("the quote has an exception or extension".into());
    }
    let mut readings = Vec::new();
    let mut rest = text.clone();
    for (regex, criterion, rule) in [
        (
            &*INCOME_MEDIAN,
            IncomeCriterion::MedianIncomePercent,
            "income_median",
        ),
        (
            &*INCOME_DECILE,
            IncomeCriterion::IncomeDecile,
            "income_decile",
        ),
        (
            &*INCOME_KOSAF,
            IncomeCriterion::KosafBracket,
            "income_kosaf",
        ),
    ] {
        for c in regex.captures_iter(&text) {
            rest = rest.replacen(&c[0], " ", 1);
            let value = c[1].parse::<i32>().map_err(|_| "unreadable income value")?;
            let comparator = match &c[2] {
                "이하" | "이내" => Comparator::Lte,
                "미만" => Comparator::Lt,
                "이상" => Comparator::Gte,
                _ => Comparator::Gt,
            };
            readings.push(IncomeReading {
                rule,
                criterion,
                comparator,
                value,
            });
        }
    }
    // `50% 초과 100% 이하`, `(1인 가구 180%)`: a second bound or level.
    if INCOME_LEFTOVER.is_match(&rest) {
        return Err("the quote has another income level or bound".into());
    }
    match readings.as_slice() {
        [one] => Ok(*one),
        [] => Err("no income criterion".into()),
        _ => Err("more than one income criterion".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai_extraction::facts::AgeBasis;

    fn age(quote: &str) -> Option<(Option<i32>, Option<i32>)> {
        parse_age(quote).ok().map(|r| (r.min, r.max))
    }

    #[test]
    fn age_ranges_in_common_korean_forms() {
        for (quote, expected) in [
            ("만 19세 이상 34세 이하", (Some(19), Some(34))),
            ("19~34세", (Some(19), Some(34))),
            ("만 19세~34세", (Some(19), Some(34))),
            ("만19세 ~ 만34세", (Some(19), Some(34))),
            ("만 19세부터 34세까지", (Some(19), Some(34))),
            ("만 19세 이상 ~ 39세 이하 청년", (Some(19), Some(39))),
            ("만 19 - 34세", (Some(19), Some(34))),
            ("만 19세 이상 39세 미만", (Some(19), Some(38))),
            ("만 39세 이하", (None, Some(39))),
            ("만 39세까지", (None, Some(39))),
            ("39세 미만", (None, Some(38))),
            ("만 18세 이상", (Some(18), None)),
            ("18세 초과", (Some(19), None)),
            ("만 19세 이상, 34세 이하", (Some(19), Some(34))),
        ] {
            assert_eq!(age(quote), Some(expected), "{quote}");
        }
    }

    #[test]
    fn ambiguous_or_birth_year_ages_are_not_parsed() {
        for quote in [
            "만 19세~34세 (군필자는 최대 39세)",
            "1990년생 ~ 2005년생",
            "만 19~34세, 만 19~39세",
            "34세 이하, 39세 이하",
            "청년",
            "만 40세~34세",
            "만 19세~134세",
            "연 나이 19세 이상",
            "20살 이상",
            "만 65세 이상은 제외",
            "자녀 만 7세 이하",
            "부모 만 60세 이상",
            "2026~34세",
            "만 34세 이하 (군 복무기간만큼 연장)",
            "최대 39세 이하",
        ] {
            assert!(parse_age(quote).is_err(), "{quote}: {:?}", parse_age(quote));
        }
    }

    fn window(quote: &str) -> Option<(WindowKind, Option<NaiveDate>, Option<NaiveDate>)> {
        parse_window(quote).ok().map(|r| (r.kind, r.start, r.end))
    }

    fn d(y: i32, m: u32, day: u32) -> Option<NaiveDate> {
        NaiveDate::from_ymd_opt(y, m, day)
    }

    #[test]
    fn windows_parse_korean_dates_as_kst_calendar_days() {
        use WindowKind::*;
        for (quote, expected) in [
            (
                "신청기간: 2026.03.02 ~ 2026.04.30",
                (Dated, d(2026, 3, 2), d(2026, 4, 30)),
            ),
            (
                "접수 2026년 3월 2일 ~ 2026년 4월 30일",
                (Dated, d(2026, 3, 2), d(2026, 4, 30)),
            ),
            (
                "접수 2026.03.02(월) ~ 04.30(목)",
                (Dated, d(2026, 3, 2), d(2026, 4, 30)),
            ),
            (
                "접수 2026년 3월 2일부터 4월 30일까지",
                (Dated, d(2026, 3, 2), d(2026, 4, 30)),
            ),
            (
                "접수 20260302 ~ 20260430",
                (Dated, d(2026, 3, 2), d(2026, 4, 30)),
            ),
            (
                "접수 20260302~20260430",
                (Dated, d(2026, 3, 2), d(2026, 4, 30)),
            ),
            (
                "접수 2026-12-15 ~ 01.15",
                (Dated, d(2026, 12, 15), d(2027, 1, 15)),
            ),
            ("접수 2026. 4. 30.까지", (Dated, None, d(2026, 4, 30))),
            ("접수 2026.03.02부터", (Dated, d(2026, 3, 2), None)),
            ("상시 모집", (AlwaysOpen, None, None)),
            ("연중 상시 접수", (AlwaysOpen, None, None)),
            ("예산 소진 시 조기 마감", (UntilBudgetExhausted, None, None)),
        ] {
            assert_eq!(window(quote), Some(expected), "{quote}");
        }
    }

    #[test]
    fn ambiguous_windows_are_not_parsed() {
        for quote in [
            "2026.04.30 18:00까지",
            "2026.04.30 18시까지",
            "1차 2026.03.02~03.15, 2차 2026.04.01~04.15",
            "상시 접수 (2026.12.31까지)",
            "연중 상시, 예산 소진 시 마감",
            "2026.03.02 2026.04.30",
            "2026.04.30 ~ 2026.03.02",
            "수시 모집",
            "접수 2026.03.02 ~ 2026.04.30 (공휴일 제외)",
            "사업기간 2026.03.01 ~ 2026.12.31 신청 후 선정",
            "2026.03.02 ~ 2026.04.30",
            "상시근로자 5인 이상",
            "2026.02.30 ~ 2026.03.02",
            "04.30까지",
        ] {
            assert!(
                parse_window(quote).is_err(),
                "{quote}: {:?}",
                parse_window(quote)
            );
        }
    }

    #[test]
    fn documents_use_the_curated_vocabulary_with_longest_match() {
        let docs = parse_documents("신분증, 주민등록 등본, 임대차계약서(해당자)").unwrap();
        assert_eq!(
            docs,
            vec![
                DocumentReading {
                    canonical: "신분증",
                    required: true
                },
                DocumentReading {
                    canonical: "주민등록등본",
                    required: true
                },
                DocumentReading {
                    canonical: "임대차계약서",
                    required: false
                },
            ]
        );
        let docs = parse_documents("사업자등록증명원 1부").unwrap();
        assert_eq!(docs[0].canonical, "사업자등록증명원", "not 사업자등록증");
        let docs = parse_documents("건강보험 자격득실 확인서, 소득금액증명원").unwrap();
        assert_eq!(docs.len(), 2);
        assert_eq!(
            vocabulary_entry("건강보험 자격득실확인서"),
            Some("건강보험 자격득실확인서")
        );
        assert_eq!(vocabulary_entry("가족관계증명원"), Some("가족관계증명서"));
        assert_eq!(
            vocabulary_entry("등본"),
            None,
            "generic words are not aliases"
        );
        assert!(parse_documents("주민등록등본 제출 생략").is_err());
        assert!(parse_documents("해당자에 한해 제출: 장애인증명서").is_err());
        assert!(parse_documents("제출 불필요: 주민등록등본").is_err());
        assert!(parse_documents("주민등록등본 또는 초본").is_err());
    }

    fn doc(name: &str, required: bool, notes: Option<&str>) -> FactValue {
        FactValue::Document {
            name: name.into(),
            required,
            notes: notes.map(str::to_string),
        }
    }

    #[test]
    fn document_cross_check_needs_vocabulary_presence_and_matching_required_flag() {
        let quote = "신분증, 주민등록 등본, 임대차계약서(해당자)";
        assert!(cross_check(&doc("주민등록등본", true, None), quote).agrees());
        assert!(cross_check(&doc("임대차계약서", false, Some("해당자")), quote).agrees());
        let required_mismatch = cross_check(&doc("임대차계약서", true, None), quote);
        assert_eq!(required_mismatch.verdict, Verdict::Disagree);
        let invented_notes = cross_check(&doc("신분증", true, Some("사진 부착")), quote);
        assert_eq!(invented_notes.verdict, Verdict::Disagree);
        assert_eq!(
            cross_check(&doc("주민등록초본", true, None), quote).verdict,
            Verdict::NoParse
        );
        assert_eq!(
            cross_check(&doc("청년 확인 서류", true, None), quote).verdict,
            Verdict::NoParse,
            "outside the vocabulary"
        );
    }

    #[test]
    fn age_and_window_cross_checks_compare_values() {
        let value = FactValue::Age {
            min_age: Some(19),
            max_age: Some(34),
            basis: AgeBasis::ManAge,
        };
        let agree = cross_check(&value, "만 19세 이상 34세 이하");
        assert!(agree.agrees());
        assert_eq!(agree.rule.as_deref(), Some("age_range"));
        assert_eq!(agree.parser_version, PARSER_VERSION);
        let disagree = cross_check(&value, "만 19세 이상 39세 이하");
        assert_eq!(disagree.verdict, Verdict::Disagree);
        assert!(disagree.reason.unwrap().contains("39"));
        // The inclusive conversion is the parser's too: 39세 미만 is 38.
        let exclusive = FactValue::Age {
            min_age: Some(19),
            max_age: Some(39),
            basis: AgeBasis::ManAge,
        };
        assert_eq!(
            cross_check(&exclusive, "만 19세 이상 39세 미만").verdict,
            Verdict::Disagree
        );
        let window = FactValue::ApplicationWindow {
            kind: WindowKind::Dated,
            start_date: d(2026, 3, 2),
            end_date: d(2026, 4, 30),
        };
        assert!(cross_check(&window, "신청기간: 2026.03.02 ~ 2026.04.30").agrees());
        assert_eq!(
            cross_check(&window, "신청기간: 2026.03.02 ~ 2026.05.30").verdict,
            Verdict::Disagree
        );
        let budget = FactValue::ApplicationWindow {
            kind: WindowKind::UntilBudgetExhausted,
            start_date: None,
            end_date: None,
        };
        assert!(cross_check(&budget, "예산 소진 시 조기 마감").agrees());
    }

    #[test]
    fn regions_need_a_residence_cue_and_reject_gyeonggi_gwangju_and_universities() {
        assert_eq!(parse_regions("부산광역시 거주"), Ok(vec!["busan"]));
        assert_eq!(
            parse_regions("서울 또는 경기도에 주민등록을 둔 청년"),
            Ok(vec!["seoul", "gyeonggi"])
        );
        assert_eq!(parse_regions("광주광역시 소재"), Ok(vec!["gwangju"]));
        assert!(parse_regions("경기도 광주시 거주").is_err());
        assert!(
            parse_regions("부산 청년 문화 패스").is_err(),
            "no residence cue"
        );
        assert!(parse_regions("서울 거주자 제외").is_err());
        assert_eq!(
            parse_regions("부산대학교 재학생 중 울산 거주자"),
            Ok(vec!["ulsan"])
        );
        let value = FactValue::Region {
            codes: vec!["gyeonggi".into(), "seoul".into()],
        };
        assert!(cross_check(&value, "서울 또는 경기도에 주민등록을 둔 청년").agrees());
    }

    #[test]
    fn status_cues_map_to_core_codes_and_ignore_document_names() {
        assert_eq!(
            parse_enrollment("재학생 및 휴학생"),
            Ok(vec!["enrolled", "on_leave"])
        );
        assert_eq!(parse_enrollment("졸업예정자"), Ok(vec!["enrolled"]));
        assert_eq!(parse_enrollment("졸업 후 2년 이내"), Ok(vec!["graduated"]));
        assert!(parse_enrollment("재학증명서 1부").is_err());
        assert!(parse_enrollment("휴학생 제외").is_err());
        assert_eq!(parse_employment("미취업 청년"), Ok(vec!["unemployed"]));
        assert_eq!(
            parse_employment("재학생 대상").ok(),
            None,
            "학생 inside 재학생"
        );
        assert_eq!(parse_employment("고등학생"), Ok(vec!["student"]));
        assert_eq!(
            parse_employment("중소기업 재직 청년 및 프리랜서"),
            Ok(vec!["employed", "self_employed"])
        );
        let value = FactValue::Employment {
            statuses: vec!["unemployed".into(), "preparing".into()],
        };
        assert_eq!(
            cross_check(&value, "미취업 청년").verdict,
            Verdict::Disagree,
            "the rule reads only 미취업"
        );
        for code in parse_enrollment("재학생 휴학생 졸업생 연구생").unwrap() {
            assert_eq!(mazimi_core::canonical_enrollment_status(code), Some(code));
        }
    }

    #[test]
    fn income_criteria_parse_one_expression() {
        let income = |q| {
            parse_income(q)
                .ok()
                .map(|r| (r.criterion, r.comparator, r.value))
        };
        use Comparator::*;
        use IncomeCriterion::*;
        assert_eq!(
            income("기준 중위소득 150% 이하"),
            Some((MedianIncomePercent, Lte, 150))
        );
        assert_eq!(
            income("기준중위소득의 60% 이내"),
            Some((MedianIncomePercent, Lte, 60))
        );
        assert_eq!(income("소득분위 8분위 이하"), Some((IncomeDecile, Lte, 8)));
        assert_eq!(income("소득 8분위 미만"), Some((IncomeDecile, Lt, 8)));
        assert_eq!(
            income("학자금 지원구간 8구간 이하"),
            Some((KosafBracket, Lte, 8))
        );
        assert_eq!(income("기준 중위소득 150% 이하, 소득분위 8분위 이하"), None);
        assert_eq!(income("연말정산 소득공제"), None);
        assert_eq!(income("중위소득 수준"), None);
        assert_eq!(income("기준 중위소득 150% 이하 (1인 가구 180%)"), None);
        assert_eq!(income("기준 중위소득 50% 초과 100% 이하"), None);
    }
}
