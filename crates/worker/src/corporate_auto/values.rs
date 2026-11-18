//! Values as a page states them: the one parser and the one ko/en
//! normaliser behind "does the page state this value".
//!
//! **Invariant.** Two texts state the same value exactly when they share
//! its [`concept`]: a number with a language-neutral unit (`6개월`,
//! `6 months` and `six months` are `6month`; `1년`, `one-year` and `a
//! year` are `1year`; `₩6,600` and `6,600원` are `6600won`; `18세` is
//! `18age`), or a claim word (`무료` and `free` are `claim:free`). The
//! evidence job compares a fact's value window with its baseline through
//! [`same_values`]; the weekly audit checks a stored title or summary
//! through [`concepts`]. Nothing else in corporate_auto decides it.
//!
//! **Why.** Stored titles and summaries are Korean prose about pages that
//! are often English; a value written in the other language, as a word or
//! with a currency sign is still the same value.
//!
//! **Deliberately not handled.** Which audience or condition a value is
//! stated for (a page stating `2년` for teachers supports a stored `2년`),
//! ranges, number words beyond twelve, and units this list does not name.
//! [`value_signature`], the stored baselines' form, is unchanged by the
//! normaliser: it only applies when two signatures or texts are compared.

use regex::Regex;
use std::collections::BTreeSet;
use std::sync::LazyLock;

pub(crate) static UNIT_VALUE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(\d[\d,]*(?:\.\d+)?)(%|퍼센트|만 ?원|천 ?원|원|개월|년|일|주|세|살|회|잔|개|gb|tb|p(?:oint)?\b|포인트|months?|years?|days?|weeks?)",
    )
    .expect("valid unit pattern")
});

/// A won amount written in two parts: `1만 5천원`, `1만 5,000원`.
pub(crate) static COMPOUND_WON: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(\d[\d,]*(?:\.\d+)?)\s*만\s*(?:(\d+)\s*천\s*|(\d[\d,]*)\s*)원")
        .expect("valid compound pattern")
});

/// The normalized token of one value: commas and spaces dropped, unit
/// lowercased, a won amount in 만/천 written out in won (`1만 원`, `1만원`
/// and `10,000원` all read `10000원`). An amount that is not a whole number
/// of won keeps its written unit.
pub fn canonical_value(number: &str, unit: &str) -> String {
    let number = number.replace(',', "");
    let unit = unit.to_lowercase().replace(' ', "");
    let scale = match unit.as_str() {
        "만원" => 10_000.0,
        "천원" => 1_000.0,
        _ => return format!("{number}{unit}"),
    };
    match number.parse::<f64>() {
        Ok(n) if (n * scale).fract() == 0.0 && n * scale < 1e15 => {
            format!("{}원", (n * scale) as u64)
        }
        _ => format!("{number}{unit}"),
    }
}

/// Every value in `text` with its byte span, compound won amounts first
/// (`1만 5천원` is one value, `15000원`, never its `5천원` part).
pub(crate) fn value_spans(text: &str) -> Vec<(usize, usize, String)> {
    let mut spans: Vec<(usize, usize, String)> = COMPOUND_WON
        .captures_iter(text)
        .filter_map(|c| {
            let whole = c.get(0)?;
            let high: f64 = c[1].replace(',', "").parse().ok()?;
            let low: f64 = match (c.get(2), c.get(3)) {
                (Some(thousands), _) => thousands.as_str().parse::<f64>().ok()? * 1_000.0,
                (_, Some(won)) => won.as_str().replace(',', "").parse().ok()?,
                _ => return None,
            };
            let won = high * 10_000.0 + low;
            (won.fract() == 0.0 && won < 1e15)
                .then(|| (whole.start(), whole.end(), format!("{}원", won as u64)))
        })
        .collect();
    let simple: Vec<(usize, usize, String)> = UNIT_VALUE
        .captures_iter(text)
        .filter_map(|c| {
            let whole = c.get(0)?;
            let inside = spans
                .iter()
                .any(|(start, end, _)| whole.start() < *end && *start < whole.end());
            (!inside).then(|| (whole.start(), whole.end(), canonical_value(&c[1], &c[2])))
        })
        .collect();
    spans.extend(simple);
    spans.sort_by_key(|(start, _, _)| *start);
    spans
}

/// Sorted multiset of unit-bearing values in `text`, normalized
/// ([`canonical_value`], full-width forms as ASCII): "10%", "6600원",
/// "1만 원" and "1만원" read "10000원", "1만 5천원" reads "15000원".
pub fn value_signature(text: &str) -> String {
    // Full-width forms read as their ASCII ones (`１０％` is `10%`).
    let text: String = text
        .chars()
        .map(|c| match c {
            '\u{FF01}'..='\u{FF5E}' => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c),
            '\u{3000}' => ' ',
            _ => c,
        })
        .collect();
    let mut values: Vec<String> = value_spans(&text).into_iter().map(|(_, _, v)| v).collect();
    values.sort();
    values.join(",")
}

// ── the ko/en normaliser ──────────────────────────────────────────────────

/// A value token's unit, language-neutral.
fn unit_concept(unit: &str) -> &str {
    match unit {
        "%" | "퍼센트" => "%",
        "원" | "won" => "won",
        "개월" | "month" | "months" => "month",
        "년" | "year" | "years" => "year",
        "일" | "day" | "days" => "day",
        "주" | "week" | "weeks" => "week",
        "세" | "살" => "age",
        "회" => "times",
        "p" | "point" | "포인트" => "point",
        other => other,
    }
}

/// One value token (`6months`, `6개월`, `6600원`) as its concept
/// (`6month`, `6600won`).
pub fn concept(token: &str) -> String {
    let split = token
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(token.len());
    let (number, unit) = token.split_at(split);
    format!("{number}{}", unit_concept(unit))
}

/// Number words before a unit (`six months`, `a full year`, `one-year`,
/// `한 달`) and currency signs (`₩6,600`, `$100`, `100달러`), written as the
/// digits and units [`UNIT_VALUE`] reads.
static NUMBER_WORD_EN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(an?|one|two|three|four|five|six|seven|eight|nine|ten|eleven|twelve)(?:\s+(?:full|entire|whole))?[\s-]+(year|month|week|day)s?\b")
        .expect("valid number-word pattern")
});
/// A hyphenated period (`6-month`, `1-year`, `14-day`, `14-days`).
static HYPHEN_PERIOD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(\d{1,3})\s*-\s*(year|month|week|day)s?\b")
        .expect("valid hyphen-period pattern")
});
static NUMBER_WORD_KO: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(한|두|세|네|다섯|여섯|\d+)\s*(달|해)").expect("valid Korean number-word pattern")
});
static FIRST_MONTH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)첫\s*달|\bfirst\s+month\b").expect("valid first-month pattern")
});
static WON_SIGN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[₩￦]\s*(\d[\d,]*)").expect("valid won-sign pattern"));
static DOLLAR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(?:US\s*)?\$\s*(\d[\d,.]*)|미화\s*(\d[\d,.]*)\s*달러|(\d[\d,.]*)\s*(?:달러|usd)\b",
    )
    .expect("valid dollar pattern")
});
/// Claim words, by concept: a claim without a number.
const CLAIMS: &[(&str, &str)] = &[
    (
        r"(?i)무료|공짜|\bfree\b|\bno\s+cost\b|\bat\s+no\s+charge\b",
        "claim:free",
    ),
    (r"(?i)평생|\blifetime\b", "claim:lifetime"),
    // A free period that lasts while the reader studies (`for as long as
    // the use is part of your studies`, `for the duration of your study`,
    // `재학 기간 동안`): a term of its own, so a page that also states a
    // fixed one states two. `while you're a student` is not one: it says who
    // may renew (`renew yearly while you're a student`), not for how long.
    (
        r"(?i)\bfor\s+(?:as\s+long\s+as|the\s+duration\s+of)\b[^.]*?\b(?:stud(?:y|ies|ying|ent)|enrol\w*|course|school)|재학\s*기간\s*동안",
        "term:enrolled",
    ),
    (r"(?i)무제한|\bunlimited\b", "claim:unlimited"),
];
/// How a page claims a percent is a benefit: a discount, a sale, a saving
/// or points back (`10% 할인`, `save 50%`, `20% off`, `5% 적립`).
static DISCOUNT_CLAIM: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)할인|세일|저렴|적립|\boff\b|\bdiscount(?:s|ed)?\b|\bsav(?:e|es|ing|ings)\b")
        .expect("valid discount claim pattern")
});

/// The clauses of `text` with their byte offsets: a value and the claim that
/// makes it a benefit sit in one clause (`대학생 10% 할인, 출석률 80% 이상`
/// claims 10%, never 80%; `To stay on the free plan, verify your status once
/// a year` states no free year). A `.` ends a clause only where it ends a
/// sentence and a `,` only outside a number, so `1.5` and `12,5` stay whole.
pub(crate) fn clauses(text: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    let mut previous: Option<char> = None;
    while let Some((at, c)) = chars.next() {
        let next = chars.peek().map(|(_, n)| *n);
        let between_digits = previous.is_some_and(|p| p.is_ascii_digit())
            && next.is_some_and(|n| n.is_ascii_digit());
        previous = Some(c);
        let ends = match c {
            '.' => next.is_none_or(char::is_whitespace),
            ',' => !between_digits,
            ';' | ':' | '!' | '?' | '|' | '(' | ')' | '[' | ']' | '·' | '\n' => true,
            _ => false,
        };
        if ends {
            out.push((start, &text[start..at]));
            start = at + c.len_utf8();
        }
    }
    out.push((start, &text[start..]));
    out
}

/// Every whole percent from 1 to 90 in `text` ([`value_spans`]) whose clause
/// claims it as a benefit ([`DISCOUNT_CLAIM`], before or after the number),
/// with its byte span. A decimal or a part of a longer number is no percent
/// here (`1.5%`, `100%`).
pub fn discount_percents(text: &str) -> Vec<(usize, usize, u32)> {
    clauses(text)
        .into_iter()
        .filter(|(_, clause)| DISCOUNT_CLAIM.is_match(clause))
        .flat_map(|(offset, clause)| {
            value_spans(clause)
                .into_iter()
                .filter_map(move |(start, end, token)| {
                    let n: u32 = token.strip_suffix('%')?.parse().ok()?;
                    (1..=90)
                        .contains(&n)
                        .then_some((offset + start, offset + end, n))
                })
        })
        .collect()
}

/// The free periods `text` states, in months: a period ([`concepts`]:
/// `1년`, `six months`, `a year`) in a clause that claims it free
/// (`claim:free`). Weeks and days are [`free_terms`].
pub fn free_periods(text: &str) -> BTreeSet<u32> {
    free_terms(text)
        .into_iter()
        .filter_map(|(_, term, _)| match term {
            Term::Months(n) => Some(n),
            Term::Days(_) | Term::Enrolled => None,
        })
        .collect()
}

/// A free period as a page states it: whole months (a year is 12, `30
/// days` is 1), or days (`1 week` is 7, `14 days` is 14).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Term {
    Months(u32),
    Days(u32),
    /// As long as the reader studies (`term:enrolled`).
    Enrolled,
}

impl Term {
    /// Shorter than a month: a trial, never an offer.
    pub fn under_a_month(self) -> bool {
        matches!(self, Term::Days(days) if days < 30)
    }

    /// The period in Korean (`1년`, `6개월`, `7일`).
    pub fn korean(self) -> String {
        match self {
            Term::Months(n) if n.is_multiple_of(12) => format!("{}년", n / 12),
            Term::Months(n) => format!("{n}개월"),
            Term::Days(n) => format!("{n}일"),
            Term::Enrolled => "재학 기간".to_string(),
        }
    }

    /// A concept (`6month`, `1year`, `14day`) as a period.
    pub fn parse(concept: &str) -> Option<Term> {
        if concept == "term:enrolled" {
            return Some(Term::Enrolled);
        }
        let unit_at = concept.find(|c: char| !c.is_ascii_digit())?;
        let n: u32 = concept[..unit_at].parse().ok()?;
        let term = match &concept[unit_at..] {
            "year" => Term::Months(n.checked_mul(12)?),
            "month" => Term::Months(n),
            "week" => Term::Days(n.checked_mul(7)?),
            "day" if n > 0 && n.is_multiple_of(30) => Term::Months(n / 30),
            "day" => Term::Days(n),
            _ => return None,
        };
        (n > 0).then_some(term)
    }
}

/// A cadence, not a period (`verify your status once a year`, `each
/// year`, `per month`, `매년`, `연 2회`).
static FREQUENCY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:once|twice|every|each|per)\s+(?:a\s+|an\s+)?(?:\d+\s*)?(?:years?|months?|weeks?|days?)\b|매\s*(?:년|월|주|일)|(?:연|월|주)\s*\d+\s*회")
        .expect("valid frequency pattern")
});

/// `text` with its cadences ([`FREQUENCY`]) taken out.
pub fn without_cadence(text: &str) -> String {
    FREQUENCY.replace_all(text, " ").into_owned()
}

/// The one word that labels a free period a trial (`free trial`, `try it
/// free for 14 days`, `무료 체험`): a trial is an offer only when the same
/// sentence binds it to students, and then only with its term shown.
static TRIAL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\btrials?\b|\btry\b|체험").expect("valid trial pattern"));

/// Whether `text` labels a free period a trial ([`TRIAL`]).
pub fn is_trial(text: &str) -> bool {
    TRIAL.is_match(text)
}

/// Every free period `text` states, with the byte offset of the clause
/// that claims it free ([`clauses`], `claim:free`) and whether that clause
/// labels it a trial ([`is_trial`]).
pub fn free_terms(text: &str) -> Vec<(usize, Term, bool)> {
    clauses(text)
        .into_iter()
        .flat_map(|(offset, clause)| {
            let found = concepts(clause);
            if !found.contains("claim:free") {
                return Vec::new();
            }
            let trial = is_trial(clause);
            found
                .iter()
                .filter_map(|concept| Term::parse(concept))
                .map(|term| (offset, term, trial))
                .collect()
        })
        .collect()
}

static CLAIM_PATTERNS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    CLAIMS
        .iter()
        .map(|(pattern, concept)| (Regex::new(pattern).expect("valid claim pattern"), *concept))
        .collect()
});

fn number_of(word: &str) -> Option<u32> {
    let n = match word.to_lowercase().as_str() {
        "a" | "an" | "one" | "한" => 1,
        "two" | "두" => 2,
        "three" | "세" => 3,
        "four" | "네" => 4,
        "five" | "다섯" => 5,
        "six" | "여섯" => 6,
        "seven" => 7,
        "eight" => 8,
        "nine" => 9,
        "ten" => 10,
        "eleven" => 11,
        "twelve" => 12,
        digits => return digits.parse().ok(),
    };
    Some(n)
}

/// `text` with number words, `첫 달` and currency signs written as digits
/// and units.
fn spelled_out(text: &str) -> String {
    let text = HYPHEN_PERIOD.replace_all(text, "$1 ${2}s");
    let text = NUMBER_WORD_EN.replace_all(&text, |c: &regex::Captures| match number_of(&c[1]) {
        Some(n) => format!("{n} {}s", &c[2]),
        None => c[0].to_string(),
    });
    let text = NUMBER_WORD_KO.replace_all(&text, |c: &regex::Captures| match number_of(&c[1]) {
        Some(n) => format!("{n}{}", if &c[2] == "달" { "개월" } else { "년" }),
        None => c[0].to_string(),
    });
    let text = FIRST_MONTH.replace_all(&text, "1개월");
    let text = WON_SIGN.replace_all(&text, "${1}원");
    DOLLAR
        .replace_all(&text, |c: &regex::Captures| {
            let n = (1..=3).find_map(|i| c.get(i)).map_or("", |m| m.as_str());
            format!(" {}usd ", n.replace(',', ""))
        })
        .into_owned()
}

/// Every value and claim `text` states, as concepts. Claims are read on
/// the spaced text (whole words); values on the compacted text, as the
/// evidence job reads its windows (`6 months` is `6months`).
pub fn concepts(text: &str) -> BTreeSet<String> {
    let text = spelled_out(&super::evidence::canonical(text));
    let compacted = super::evidence::compact(&text);
    let mut found: BTreeSet<String> = value_signature(&compacted)
        .split(',')
        .filter(|t| !t.is_empty())
        .map(concept)
        .collect();
    found.extend(
        USD.captures_iter(&compacted)
            .map(|c| format!("{}usd", &c[1])),
    );
    for (pattern, claim) in CLAIM_PATTERNS.iter() {
        if pattern.is_match(&text) {
            found.insert(claim.to_string());
        }
    }
    found
}

/// A dollar amount as [`spelled_out`] writes it.
static USD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(\d[\d.]*)usd").expect("valid usd pattern"));

/// A value signature (`10%,30일|10%,30일`) as concepts, each part's tokens
/// and the parts sorted.
fn signature_concepts(signature: &str) -> Vec<Vec<String>> {
    let mut parts: Vec<Vec<String>> = signature
        .split('|')
        .map(|part| {
            let mut tokens: Vec<String> = part
                .split(',')
                .filter(|t| !t.is_empty())
                .map(concept)
                .collect();
            tokens.sort();
            tokens
        })
        .collect();
    parts.sort();
    parts
}

/// Two value signatures state the same values: the same concepts, as
/// often, around the same occurrences.
pub fn same_values(a: &str, b: &str) -> bool {
    a == b || signature_concepts(a) == signature_concepts(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Review L2: a free period that lasts while the reader studies is a
    /// term of its own; `while you're a student` says who renews, not how
    /// long.
    #[test]
    fn a_free_period_while_studying_is_a_term() {
        let enrolled = |text: &str| {
            free_terms(text)
                .iter()
                .any(|(_, t, _)| *t == Term::Enrolled)
        };
        assert!(enrolled(
            "you can use Sketch for free for as long as the use is part of your studies"
        ));
        assert!(enrolled(
            "a special license which is free for the duration of your study"
        ));
        assert!(enrolled("재학 기간 동안 무료로 이용할 수 있습니다"));
        assert!(!enrolled(
            "Use your school email to sign up for free and renew yearly while you're a student"
        ));
        assert!(!enrolled("free for one year"));
        assert_eq!(Term::Enrolled.korean(), "재학 기간");
        assert!(!Term::Enrolled.under_a_month());
    }

    #[test]
    fn the_same_value_in_either_language_is_one_concept() {
        for (korean, english) in [
            ("6개월 무료", "a 6 month free trial"),
            ("1년 무료", "a full year of Polypane, free"),
            ("1년", "a one-year subscription"),
            ("2년 40% 할인", "a 40% discount valid for 2 years"),
            ("월 6,600원", "매월 ₩6,600이 청구됩니다"),
            ("첫 달 무료", "1개월 동안 무료로"),
            ("1개월", "한 달"),
        ] {
            assert!(
                concepts(korean).is_subset(&concepts(english)),
                "{korean} / {english}: {:?} {:?}",
                concepts(korean),
                concepts(english)
            );
        }
        assert_eq!(
            concepts("만 18세 이상"),
            BTreeSet::from(["18age".to_string()])
        );
        assert_eq!(
            concepts("$100 credit"),
            BTreeSet::from(["100usd".to_string()])
        );
        assert!(!concepts("6개월").is_subset(&concepts("12 months")));
        assert!(!concepts("무료").is_subset(&concepts("40% off")));
    }

    #[test]
    fn signatures_compare_by_concept_and_count() {
        assert!(same_values("6months", "6개월"));
        assert!(same_values("10%,30일|10%,30일", "30days,10%|10%,30일"));
        assert!(!same_values("10%", "15%"));
        assert!(!same_values("3000원,3000원", "3000원"));
        assert_eq!(
            value_signature("６개월"),
            "6개월",
            "the stored form is unchanged"
        );
    }
}
