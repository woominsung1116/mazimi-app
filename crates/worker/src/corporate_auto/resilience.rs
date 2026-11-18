//! Redesign resilience for evidence checks: a fact survives a site redesign
//! while the page still states it, and a changed value is never confirmed
//! or updated from here (design §3.6, accuracy first).
//!
//! **Contract (current-fact rule, 2026-10-26; supersedes rounds 7 and 8
//! items 1 and 2).**
//!
//! 1. **A stored fact is current while the page states it as stored**
//!    ([`current_on_evidence`]): every fragment of its quote verbatim on the
//!    page in the one canonical text ([`super::evidence::canonical`]: NFKC,
//!    whitespace, zero-width marks, quote and dash forms, Latin case), held
//!    in the offer's own place ([`in_its_place`]: the stored identity's
//!    headings, labels, sentence, holder count and notes), the offer's own
//!    section ([`offer_section`]) saying what it said at the last
//!    confirmation ([`section_digest`]), and the value window around it
//!    read by the deterministic parser as its stored baseline. A changed
//!    section holds the fact ([`SECTION_CHANGED`]) until fresh extraction
//!    re-derives it ([`super::evidence::rebaseline_from_fresh`]); a fact
//!    captured before sections were read is held to the rest of the rule
//!    once and read in full from its next confirmation. Why: the quote and its values are what the
//!    offer rests on; text elsewhere on the page says nothing about them.
//!    The whole-page digest ([`Fingerprint::page`]) is stored with every
//!    confirmation as telemetry and never hides a fact.
//! 2. **A fact whose page no longer states it hides** (quote gone, window
//!    or field changed) for fresh extraction. No value is proposed or
//!    applied here; [`shadow_update`] is a metric only. Deliberately not
//!    handled: wording or punctuation that changed around the same value
//!    (a dropped period is another quote), which fresh extraction re-reads.
//! 3. **Text is judged once** (2026-10-27; supersedes §16, the whole page
//!    read on every run, and §23 for confirmed facts). A fact read for the
//!    first time (no stored baseline: first adoption, publish, reextract)
//!    gets the whole reading: the offer's own section, then the page's
//!    main content and its banners and notices (not footers, asides,
//!    sidebars, navigation, unselected tabs or terms notices), clause by
//!    clause ([`clause_ending`]), dates, exclusions and structured data. A
//!    fact confirmed before (a stored baseline, its section unchanged:
//!    rule 1) is not re-judged on text already judged: each run reads only
//!    its own dates (an end passed, yearless and relative dates read
//!    against its first sight, the time of day on the end day, every
//!    reading of an order-ambiguous date; a start ahead), two signals of
//!    its own end ([`confirmed_end`]), structured data (JSON-LD, microdata)
//!    saying it ended, sold out, was cancelled or has not started, and, on
//!    a page with several offers, a finished ending elsewhere that names
//!    the offer ([`ended_by_name_elsewhere`]). Why: an unchanged section is
//!    the text that was judged at confirmation, and re-reading it with
//!    every new rule re-judged live offers whose pages had not changed
//!    (the 2026-10-19 false endings). Text that changes holds the fact
//!    ([`SECTION_CHANGED`]). Deliberately not handled here: an unnamed,
//!    undated ending added outside the offer's section of a page with
//!    several offers. Each reason has a key ([`reason_key`]) for the
//!    metrics and the `hide_only` allowlist.
//!
//! The anchors locate the fact and report the tier that holds it: the exact
//! anchor ([`evidence::evaluate_fact`]), the normalized anchor (formatting
//! changed, `confirmed_fuzzy`) or a moved element (`relocated`). The
//! section around the statement is also read for exclusion and negation
//! markers. The idea of element fingerprints follows Scrapling's adaptive
//! matching (github.com/D4Vinci/Scrapling, BSD-3); no code of it is used.
//!
//! Everything here is pure and owns its data (no parsed document is kept),
//! so it can run on a blocking thread.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use chrono::NaiveDate;
use regex::Regex;
use scraper::Html;
use serde::{Deserialize, Serialize};

use super::evidence::{
    element_hidden, evaluate_fact, normalized_compact, split_fragments, Ages, FactOutcome,
    PageEvidence,
};
use super::values::value_signature;
use crate::ai_extraction::crosscheck::parse_age;

/// Structural fingerprint similarity under which a confirmed element is
/// reported as relocated (a redesign moved it).
pub const RELOCATED_BELOW_SCORE: f64 = 0.90;
const MAX_QUOTE_CHARS: usize = 300;
const MAX_ELEMENT_CHARS: usize = 1_000;
const MAX_FINGERPRINT_TEXT: usize = 300;
const MAX_LABEL_CHARS: usize = 60;
const MAX_ROW_LABEL_CHARS: usize = 15;
const MAX_TITLE_LEVELS: usize = 6;
/// The label of a tier head that shows no text (an icon): tiers told apart
/// only this way are indistinguishable, which keeps them from confirming.
const UNREADABLE_LABEL: &str = "\u{2205}";
const WINDOW_RADIUS_CHARS: usize = 150;

// ── normalization ─────────────────────────────────────────────────────────

fn fold_char(c: char) -> char {
    match c {
        '\u{FF01}'..='\u{FF5E}' => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c),
        '\u{3000}' => ' ',
        '\u{2010}'..='\u{2015}' | '\u{2212}' | '\u{FE63}' => '-',
        '\u{301C}' | '\u{223C}' => '~',
        _ => c.to_lowercase().next().unwrap_or(c),
    }
}

/// Folded text and, per folded char, the index of its source char.
#[derive(Debug, Clone, Default)]
pub struct Folded {
    pub chars: Vec<char>,
    pub source: Vec<usize>,
}

/// Lowercase, full-width to half-width, dashes and tildes unified, every
/// whitespace and punctuation mark dropped except `%` and the separators
/// that sit next to a digit (`1.5`, `19~34`, `1/2`), thousands commas
/// dropped. Letters (Hangul included) and digits are kept exactly.
pub fn fold(text: &str) -> Folded {
    let mapped: Vec<char> = text.chars().map(fold_char).collect();
    let mut out = Folded::default();
    for (index, &c) in mapped.iter().enumerate() {
        if c.is_whitespace() || c == super::evidence::DIGIT_GAP {
            // Whitespace between two digits stays a separator, so two
            // numbers never merge into one.
            let next = mapped[index + 1..]
                .iter()
                .find(|n| !n.is_whitespace() && **n != super::evidence::DIGIT_GAP);
            if out.chars.last().is_some_and(|p| p.is_ascii_digit())
                && next.is_some_and(|n| n.is_ascii_digit())
            {
                out.chars.push(super::evidence::DIGIT_GAP);
                out.source.push(index);
            }
            continue;
        }
        let digit_near = |offset: isize| {
            let at = index as isize + offset;
            at >= 0 && (at as usize) < mapped.len() && mapped[at as usize].is_ascii_digit()
        };
        let keep = c.is_alphanumeric()
            || c == '%'
            || (matches!(c, '.' | '~' | '-' | '/' | '+' | ':')
                && (digit_near(-1) || digit_near(1)));
        if keep {
            out.chars.push(c);
            out.source.push(index);
        }
    }
    out
}

pub fn fold_str(text: &str) -> String {
    fold(text).chars.into_iter().collect()
}

/// Text with full-width forms folded but nothing dropped, so the unit
/// parser reads `１０％` as `10%`.
fn width_normalized(text: &str) -> String {
    text.chars().map(fold_char).collect()
}

/// Levenshtein distance over chars, two rows.
fn levenshtein(a: &[char], b: &[char]) -> usize {
    if a.is_empty() {
        return b.len();
    }
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    let mut current = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        current[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            current[j + 1] = (previous[j + 1] + 1)
                .min(current[j] + 1)
                .min(previous[j] + usize::from(ca != cb));
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[b.len()]
}

/// 1 − distance / longer length on folded text (1.0 when both are empty).
/// Used only to rank and to report structure changes, never to accept.
pub fn similarity(a: &str, b: &str) -> f64 {
    let a: Vec<char> = fold(a)
        .chars
        .into_iter()
        .take(MAX_FINGERPRINT_TEXT)
        .collect();
    let b: Vec<char> = fold(b)
        .chars
        .into_iter()
        .take(MAX_FINGERPRINT_TEXT)
        .collect();
    let longest = a.len().max(b.len());
    if longest == 0 {
        return 1.0;
    }
    1.0 - levenshtein(&a, &b) as f64 / longest as f64
}

// ── values ────────────────────────────────────────────────────────────────

/// Number runs of folded text, each with whether it is the number of a
/// unit-bearing value (`3000원`, `10%`, `34세`) or another number (`1인`,
/// `Lv.1`, a code), and the folded text itself.
fn numbers(text: &str) -> (String, Vec<(usize, usize, bool)>) {
    let folded = fold_str(text);
    let unit_starts: HashSet<usize> = super::values::UNIT_VALUE
        .captures_iter(&folded)
        .filter_map(|c| c.get(1).map(|m| m.start()))
        .collect();
    let mut runs = Vec::new();
    let mut start: Option<usize> = None;
    for (at, c) in folded.char_indices() {
        let continues = c.is_ascii_digit() || (c == '.' && start.is_some());
        match (continues, start) {
            (true, None) => start = Some(at),
            (false, Some(from)) => {
                runs.push((from, at, unit_starts.contains(&from)));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(from) = start {
        runs.push((from, folded.len(), unit_starts.contains(&from)));
    }
    (folded, runs)
}

/// The quote folded with only its own value masked: `Some` when the quote
/// states exactly one unit-bearing value (that number becomes `#`, every
/// other digit and word stays), `None` otherwise (no value proposal is
/// possible for it).
pub fn identity_sentence(text: &str) -> Option<String> {
    let (folded, runs) = numbers(text);
    let units: Vec<&(usize, usize, bool)> = runs.iter().filter(|r| r.2).collect();
    let [(from, to, _)] = units.as_slice() else {
        return None;
    };
    Some(format!("{}#{}", &folded[..*from], &folded[*to..]))
}

// ── second extractor ──────────────────────────────────────────────────────

/// Units read by [`values_b`], longest first so a longer unit wins.
const UNITS_B: &[&str] = &[
    "퍼센트",
    "포인트",
    "months",
    "month",
    "years",
    "weeks",
    "point",
    "year",
    "days",
    "week",
    "만 원",
    "천 원",
    "만원",
    "천원",
    "개월",
    "day",
    "gb",
    "tb",
    "%",
    "원",
    "년",
    "일",
    "주",
    "세",
    "살",
    "회",
    "잔",
    "개",
    "p",
];

/// An independent reading of unit-bearing numbers: a hand-written scanner
/// (no regular expression) over the same unit vocabulary as
/// [`value_signature`], returning the same normalized form. Two readings
/// that disagree mean one of them misread the text, so nothing is applied.
pub fn values_b(text: &str) -> String {
    // Full-width digits, signs and letters as ASCII.
    let chars: Vec<char> = text
        .chars()
        .map(|c| {
            if ('\u{FF01}'..='\u{FF5E}').contains(&c) {
                char::from_u32(c as u32 - 0xFEE0).unwrap_or(c)
            } else if c == '\u{3000}' {
                ' '
            } else {
                c
            }
        })
        .collect();
    let lower: Vec<char> = chars.iter().map(|c| c.to_ascii_lowercase()).collect();
    let unit_at = |at: usize| -> Option<&'static str> {
        UNITS_B.iter().copied().find(|unit| {
            let unit_chars: Vec<char> = unit.chars().collect();
            if at + unit_chars.len() > lower.len()
                || lower[at..at + unit_chars.len()] != unit_chars[..]
            {
                return false;
            }
            if matches!(*unit, "p" | "point") {
                // A word must end after the bare `p`/`point` unit.
                let next = at + unit_chars.len();
                return next >= lower.len() || !lower[next].is_alphanumeric();
            }
            true
        })
    };
    let mut values = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        if !chars[index].is_ascii_digit() {
            index += 1;
            continue;
        }
        if let Some((won, end)) = compound_won_b(&chars, index) {
            values.push(format!("{won}원"));
            index = end;
            continue;
        }
        // Integer part: digits and commas; then an optional decimal part.
        let mut integer_end = index + 1;
        while integer_end < chars.len()
            && (chars[integer_end].is_ascii_digit() || chars[integer_end] == ',')
        {
            integer_end += 1;
        }
        let mut found = None;
        let mut end = integer_end;
        while end > index {
            let mut decimal_end = end;
            if end < chars.len()
                && chars[end] == '.'
                && end + 1 < chars.len()
                && chars[end + 1].is_ascii_digit()
            {
                decimal_end = end + 1;
                while decimal_end < chars.len() && chars[decimal_end].is_ascii_digit() {
                    decimal_end += 1;
                }
            }
            if let Some(unit) = unit_at(decimal_end) {
                found = Some((decimal_end, unit));
                break;
            }
            if decimal_end != end {
                if let Some(unit) = unit_at(end) {
                    found = Some((end, unit));
                    break;
                }
            }
            end -= 1;
        }
        match found {
            Some((number_end, unit)) => {
                let number: String = chars[index..number_end]
                    .iter()
                    .filter(|c| **c != ',')
                    .collect();
                let unit_text: String = chars[number_end..number_end + unit.chars().count()]
                    .iter()
                    .collect();
                values.push(super::values::canonical_value(&number, &unit_text));
                index = number_end + unit.chars().count();
            }
            None => index += 1,
        }
    }
    values.sort();
    values.join(",")
}

/// A two-part won amount at `start` (`1만 5천원`, `1만 5,000원`), read by
/// hand: the amount in won and the index after `원`.
fn compound_won_b(chars: &[char], start: usize) -> Option<(u64, usize)> {
    let skip_spaces = |mut at: usize| {
        while at < chars.len() && chars[at] == ' ' {
            at += 1;
        }
        at
    };
    let digits = |mut at: usize, commas: bool| {
        let from = at;
        while at < chars.len() && (chars[at].is_ascii_digit() || (commas && chars[at] == ',')) {
            at += 1;
        }
        let text: String = chars[from..at].iter().filter(|c| **c != ',').collect();
        (text, at)
    };
    let (high, at) = digits(start, true);
    let at = skip_spaces(at);
    if chars.get(at) != Some(&'만') {
        return None;
    }
    let at = skip_spaces(at + 1);
    if !chars.get(at).is_some_and(char::is_ascii_digit) {
        return None;
    }
    let (low, at) = digits(at, true);
    let at = skip_spaces(at);
    let high: u64 = high.parse().ok()?;
    let low: u64 = low.parse().ok()?;
    let (low, at) = if chars.get(at) == Some(&'천') {
        (low * 1_000, skip_spaces(at + 1))
    } else {
        (low, at)
    };
    (chars.get(at) == Some(&'원')).then(|| (high * 10_000 + low, at + 1))
}

/// Whether an age reading agrees with the ages [`values_b`] reads.
fn ages_agree_b(text: &str, min: Option<i32>, max: Option<i32>) -> bool {
    let mut read: HashSet<i32> = values_b(text)
        .split(',')
        .filter_map(|token| {
            token
                .strip_suffix('세')
                .or_else(|| token.strip_suffix('살'))
                .and_then(|n| n.parse().ok())
        })
        .collect();
    read.extend(age_range_starts_b(text));
    let stated: HashSet<i32> = [min, max].into_iter().flatten().collect();
    !stated.is_empty() && read == stated
}

/// A range in words (`10%에서 20%`, `10% 내지 20%`, `1만원부터 2만원까지`).
static VALUE_RANGE_WORDS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\d[\d,.]*\s*(?:%|퍼센트|원|만\s*원|천\s*원|만|천)?\s*(?:에서|내지|부터)\s*\d[\d,.]*\s*(?:%|퍼센트|원|만|천)")
        .expect("valid range-word pattern")
});
/// A stepped list of values (`10%, 15%, 20% 단계별`).
static STEPPED_VALUES: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"\d+\s*(?:%|퍼센트|원)\s*,\s*\d+\s*(?:%|퍼센트|원)\s*,\s*\d+\s*(?:%|퍼센트|원)|단계별",
    )
    .expect("valid stepped pattern")
});

/// A range of a value other than an age (`10~20%`, `1~2만원`, `3-5천원`),
/// read by hand: digits, a range mark, digits and a unit that is not 세/살.
pub(crate) fn value_range_b(text: &str) -> bool {
    let chars: Vec<char> = text.chars().collect();
    let skip = |mut at: usize| {
        while at < chars.len() && chars[at] == ' ' {
            at += 1;
        }
        at
    };
    let digits_end = |mut at: usize| {
        while at < chars.len()
            && (chars[at].is_ascii_digit() || chars[at] == ',' || chars[at] == '.')
        {
            at += 1;
        }
        at
    };
    let mut index = 0;
    while index < chars.len() {
        if !chars[index].is_ascii_digit() || (index > 0 && chars[index - 1].is_ascii_digit()) {
            index += 1;
            continue;
        }
        let first_end = digits_end(index);
        // A unit may come before the mark too (`10%~20%`, `1만~2만원`,
        // `5천원~1만원`).
        let mut unit_end = skip(first_end);
        while matches!(
            chars.get(unit_end),
            Some('%' | '만' | '천' | '원' | '퍼' | '센' | '트')
        ) {
            unit_end += 1;
        }
        let mark = skip(unit_end);
        if matches!(
            chars.get(mark),
            Some('~' | '∼' | '〜' | '～' | '-' | '–' | '—')
        ) {
            let second = skip(mark + 1);
            let second_end = digits_end(second);
            if second_end > second {
                let unit = skip(second_end);
                if matches!(
                    chars.get(unit),
                    Some('%' | '원' | '만' | '천' | 'P' | 'p' | '배' | '개' | '회' | '퍼')
                ) {
                    return true;
                }
            }
        }
        index = first_end.max(index + 1);
    }
    false
}

/// The first bound of an age range (`19~24세`, `만 19세 ~ 24세`), read by
/// hand: digits, an optional 세/살, a range mark, digits and 세/살. The value
/// signature holds only the unit-bearing `24세`.
fn age_range_starts_b(text: &str) -> Vec<i32> {
    let chars: Vec<char> = text.chars().collect();
    let skip = |mut at: usize| {
        while at < chars.len() && chars[at] == ' ' {
            at += 1;
        }
        at
    };
    let mut starts = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        if !chars[index].is_ascii_digit() || (index > 0 && chars[index - 1].is_ascii_digit()) {
            index += 1;
            continue;
        }
        let from = index;
        let mut at = index;
        while at < chars.len() && chars[at].is_ascii_digit() {
            at += 1;
        }
        let first: String = chars[from..at].iter().collect();
        let mut after = skip(at);
        if matches!(chars.get(after), Some('세' | '살')) {
            after = skip(after + 1);
        }
        if matches!(chars.get(after), Some('~' | '∼' | '〜' | '-' | '–')) {
            let second_from = skip(after + 1);
            let mut second_end = second_from;
            while second_end < chars.len() && chars[second_end].is_ascii_digit() {
                second_end += 1;
            }
            let unit_at = skip(second_end);
            if second_end > second_from && matches!(chars.get(unit_at), Some('세' | '살')) {
                if let Ok(n) = first.parse() {
                    starts.push(n);
                }
            }
        }
        index = at;
    }
    starts
}

/// Both extractors read the same values (and, for an age, the same bounds).
pub fn extractors_agree(text: &str) -> bool {
    // A value range (`10~20% 할인`) reads as its upper bound alone in both
    // signatures; it is never proposed as that one value.
    if value_range_b(text) || VALUE_RANGE_WORDS.is_match(text) || STEPPED_VALUES.is_match(text) {
        return false;
    }
    if value_signature(text) != values_b(text) {
        return false;
    }
    match parse_age(text) {
        Ok(reading) => ages_agree_b(text, reading.min, reading.max),
        Err(_) => true,
    }
}

// ── context: ending, exclusion, negation, dates ───────────────────────────

/// When a page is read: the day (KST), the time of day, and the day the
/// fact was first seen. Yearless and relative dates (`3월 31일까지`, `이번
/// 달 말까지`) are read against the first sight, never against today; when
/// that day is unknown they are read as passed.
#[derive(Debug, Clone, Copy)]
pub struct Clock {
    pub today: NaiveDate,
    pub time: chrono::NaiveTime,
    pub seen: Option<NaiveDate>,
    /// The fact is read for the first time (adoption, fresh extraction):
    /// only then are exclusions read. On a page that reads as at its first
    /// sight, an exclusion was already there when it was adopted.
    pub first_sight: bool,
}

impl Clock {
    /// Start of `today`, first seen today.
    pub fn at(today: NaiveDate) -> Self {
        Self {
            today,
            time: chrono::NaiveTime::MIN,
            seen: Some(today),
            first_sight: true,
        }
    }

    /// The moment has passed: an earlier day, or today at a stated time
    /// already reached.
    fn passed(&self, date: NaiveDate, time: Option<chrono::NaiveTime>) -> bool {
        date < self.today || (date == self.today && time.is_some_and(|t| t <= self.time))
    }
}

/// Ending words, folded, read per clause ([`clause_ending`]). An ending
/// governed by a condition right after it (`종료 시`, `종료될 수`, `마감
/// 임박`, `마감까지 3일 남았어요`, `마감 D-5`) or by a trigger right before
/// it (`재고 소진 시 조기 종료`) is not an ended state.
pub const KO_ENDING_MARKERS: &[&str] = &[
    "종료",
    "마감",
    "만료",
    "폐지",
    "끝남",
    "행사끝",
    "이벤트끝",
    "중단",
    "중지",
    "품절",
    "소진",
    "지난이벤트",
    "지난행사",
];
/// The offer or its event said, in a full declarative sentence, to have
/// ended (`이벤트가 종료되었습니다`, `본 혜택은 2026년 10월 15일 마감되었습니다`,
/// `종료된 이벤트입니다`), folded (`section_ended_past`, a shadow metric:
/// one phrase alone is not trusted, see [`confirmed_end`]).
static OFFER_ENDED_PAST: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?:이벤트|프로모션|혜택|행사|모집|판매|할인|쿠폰|캠페인|응모|신청|접수|참여|기획전|특가)[가-힣0-9.~\-]{0,30}?(?:종료|마감|중단|중지|만료|완료|마무리|취소)(?:되었습니다|됐습니다|되었어요|됐어요|하였습니다|했습니다)|(?:종료|마감)된(?:이벤트|행사|프로모션|혜택)입니다",
    )
    .expect("valid offer-ended pattern")
});
/// A word naming the offer or its event.
static OFFER_SUBJECT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"이벤트|프로모션|혜택|행사|모집|판매|할인|쿠폰|캠페인|응모|신청|접수|참여|기획전|특가",
    )
    .expect("valid subject pattern")
});
/// Endings that count only with the offer or its event named in the clause
/// (`수능 끝난 수험생`, `인증을 마쳤다면` are not endings).
const SUBJECT_ENDED: &[&str] = &[
    "끝났",
    "끝난",
    "끝나",
    "마쳤",
    "완료되었",
    "완료됐",
    "마무리되었",
    "마무리됐",
];
/// A condition on the verb (`완료되었다면`, `끝나면`).
static CONDITIONAL_SUFFIX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:끝나|마쳤|완료되었|완료됐|마무리되었|마무리됐)(?:다면|으면|면)")
        .expect("valid conditional suffix")
});
/// An ended state in the past tense: always ended, whatever else the clause
/// says (read as a stated ending; only [`OFFER_ENDED_PAST`] is the trusted
/// past form).
const KO_ENDED_PAST: &[&str] = &[
    "종료되었",
    "종료됐",
    "종료됨",
    "종료하였",
    "마감되었",
    "마감됐",
    "마감됨",
    "만료되었",
    "만료됐",
    "만료됨",
    "폐지되었",
    "폐지됐",
    "중단되었",
    "중단됐",
    "중지되었",
    "중지됐",
    "품절되었",
    "품절됐",
    "소진되었",
    "소진됐",
    "모집완료",
    "접수완료",
    "발급완료",
    "지급완료",
    "제공완료",
    "판매완료",
    "참여완료",
    "모집마감",
    "막을내렸",
    "기간이지났",
    "지났습니다",
];
/// The offer is not on yet (read in the offer's own section only: `다음
/// 이벤트 오픈 예정` elsewhere on the page is about another offer).
const KO_NOT_STARTED: &[&str] = &["준비중", "오픈예정", "출시예정", "시행예정"];
/// An ended state declared in the present (`종료합니다`, `종료된
/// 이벤트입니다`), or participation refused (`신청 기간이 아닙니다`, `응모
/// 불가합니다`).
const KO_ENDED_DECLARED: &[&str] = &[
    "종료합니다",
    "종료입니다",
    "종료된이벤트입니다",
    "종료된행사입니다",
    "마감합니다",
    "마감입니다",
    "중단합니다",
    "품절입니다",
    "기간이아닙니다",
    "기간이아니",
];
/// Participation refused: `신청 불가`, `응모가 불가합니다`, `참여 불가능`; not
/// a list of places or items it does not cover (`이용불가 매장`).
static PARTICIPATION_REFUSED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:신청|참여|응모|접수|가입|발급|이용)(?:이|가|은|는)?불가(?:능)?(?:합니다|입니다|함|해요|하며|하고|하므로|하오니|하여|$)")
        .expect("valid refusal pattern")
});
/// An event called off (`이벤트 취소`, `행사가 취소되었습니다`); a plain
/// `취소` (`주문 취소`, `취소 및 환불`) is not an ending.
static EVENT_CALLED_OFF: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:이벤트|행사|프로모션|캠페인)(?:이|가|은|는|를)?(?:전면)?취소")
        .expect("valid called-off pattern")
});
/// Exclusion and negation, folded, read in the section around a statement
/// (never page-wide: common in terms, `중복 할인 불가`).
pub const KO_EXCLUSION_MARKERS: &[&str] = &[
    "불가",
    "제외",
    "아님",
    "아닙니다",
    "아니에",
    "아니며",
    "아니고",
    "아닌",
    "없습니다",
    "없음",
    "없어요",
    "없으며",
    "없어지",
    "없어졌",
    "않습니다",
    "않음",
    "않아",
    "않는",
    "않으",
    "않고",
    "안됩",
    "안돼",
    "안되",
    "못합니다",
    "못해요",
    "못하며",
    "못받",
    "가입자한정",
    "신규한정",
    "신규고객한정",
    "신규회원한정",
    "첫구매한정",
    "에한함",
    "에한해",
    "에한하여",
];
/// English ending words, whole words on lowercase text, read per clause.
pub const EN_ENDING_MARKERS: &[&str] = &[
    "ended",
    "has ended",
    "is over",
    "expired",
    "has expired",
    "discontinued",
    "unavailable",
    "not available",
    "no longer",
    "sold out",
    "terminated",
    "closed",
    "suspended",
    "cancelled",
    "canceled",
    "withdrawn",
    "finished",
    "not valid",
];
/// English exclusions, whole words, read in the section around a statement.
const EN_EXCLUSION_MARKERS: &[&str] = &[
    "except",
    "excluding",
    "excludes",
    "excluded",
    "not eligible",
    "ineligible",
    "not offered",
];
/// Badges matched as whole words in the original case (`END`), and
/// symbols that mark an audience or option as excluded (`휴학생 X`).
const CASE_MARKERS: &[&str] = &["END", "ENDED", "CLOSED", "SOLD OUT"];
const CROSS_MARKERS: &[char] = &['X', '×', '✕', '✖', '❌'];

/// An ending word with the condition that governs it right after it, an
/// active countdown, or an informational noun (`중단 방법`): taken out of
/// the clause before it is read.
static GOVERNED_ENDING: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(?:종료|마감|만료|폐지|중단|중지|단종|품절|소진)\s*(?:될|되는|되면|된다면|할\s*수|하는\s*경우|하면|시(?:\s|$|에|,)|경우|때|예정|임박|방법|절차|기준|정책|까지\s*\d+\s*(?:일|시간|분)\s*남|(?:까지\s*)?d\s*-\s*(?:\d+|day))|(?:종료|마감|만료|중단|중지|단종|품절)[^.|]{0,8}?(?:될|할)\s*(?:수|경우)|(?:종료|마감|만료|단종|품절|중단)된\s*[가-힣]{1,6}(?:의)?\s*경우|끝\s*(?:나면|나는|날)|(?:유효\s*기간|포인트|쿠폰|적립금|비밀번호|약정|계약|구독)\s*(?:이|가|은|는)?\s*(?:만료|종료|소멸)|\b(?:if|until|when|once|while)\s+(?:\w+\s+){0,3}?(?:sold out|ended|expired|closed|finished|no longer)|\b(?:that|which|who)\s+(?:have|has|had)\s+(?:expired|ended)|\b(?:will|would)\s+no\s+longer|\bnot\s+available\s+(?:to|for|on|in)\b|(?:중단|종료|변경)\s*(?:이|이\s*)?가능|(?:만료|종료|마감)\s*(?:자|전|후)(?:\s|$|에|는|의)|자동\s*(?:으로\s*)?(?:종료|만료|해지|소멸)|소진율|품절률",
    )
    .expect("valid governed-ending pattern")
});
/// The ending is the consequence of a trigger right before it, at the end
/// of the clause: `재고 소진 시 조기 종료`, `품절될 경우 즉시 마감됩니다`.
static TRIGGERED_ENDING: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?:시|경우|때)\s*(?:에는?)?\s*(?:조기\s*|즉시\s*|자동\s*)?(?:종료|마감|중단|중지)\s*(?:될\s*수\s*있\S*|됩니다|합니다|처리됩니다)?\s*$",
    )
    .expect("valid triggered-ending pattern")
});
/// A finished countdown standing alone (`응모 D+3`).
static PASSED_COUNTDOWN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^\s*(?:[가-힣A-Za-z]{1,6}\s*)?d\s*\+\s*[1-9]\d{0,2}\s*$")
        .expect("valid countdown pattern")
});

fn whole_word(text: &str, marker: &str) -> bool {
    text.match_indices(marker).any(|(at, _)| {
        let before = text[..at].chars().next_back();
        let after = text[at + marker.len()..].chars().next();
        !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric)
    })
}

/// How a clause says an offer is not on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ending {
    /// A past-tense ended state (`종료되었습니다`, `모집 완료`, `끝난`).
    Past,
    /// A present declaration, a refusal, a badge, or an ending word that
    /// no condition governs and no date decides.
    Stated,
    /// Not on yet (`오픈 예정`).
    NotStarted,
}

/// One clause read for an ending. An ending word in a clause whose date
/// the reader classifies (an end or a start date) is left to that date
/// (`이벤트 종료: 2099.12.31` is a future end); with a date it cannot
/// classify, the clause is read as ended.
fn clause_ending(clause: &str) -> Option<Ending> {
    let folded = fold_str(clause);
    if folded.is_empty() {
        return None;
    }
    if OFFER_ENDED_PAST.is_match(&folded) {
        return Some(Ending::Past);
    }
    let subject_named = OFFER_SUBJECT.is_match(&folded);
    if KO_ENDED_PAST.iter().any(|m| folded.contains(m))
        || PASSED_COUNTDOWN.is_match(clause)
        || folded.contains("hasfinished")
        || (subject_named
            && (SUBJECT_ENDED.iter().any(|m| folded.contains(m)) || folded.contains("단종"))
            && !CONDITIONAL_SUFFIX.is_match(&folded))
    {
        return Some(Ending::Stated);
    }
    if KO_NOT_STARTED.iter().any(|m| folded.contains(m))
        || whole_word(&clause.to_lowercase(), "coming soon")
        || OPENING_COUNTDOWN.is_match(clause)
    {
        return Some(Ending::NotStarted);
    }
    if CASE_MARKERS.iter().any(|m| whole_word(clause, m))
        || KO_ENDED_DECLARED.iter().any(|m| folded.contains(m))
        || PARTICIPATION_REFUSED.is_match(&folded)
        || EVENT_CALLED_OFF.is_match(&folded)
        || OFFER_WITHDRAWN.is_match(clause)
    {
        return Some(Ending::Stated);
    }
    if TRIGGERED_ENDING.is_match(clause) {
        return None;
    }
    let free = GOVERNED_ENDING.replace_all(clause, " # ");
    let free_folded = fold_str(&free);
    let lower = free.to_lowercase();
    let ending = KO_ENDING_MARKERS.iter().any(|m| free_folded.contains(m))
        || EN_ENDING_MARKERS.iter().any(|m| whole_word(&lower, m));
    if !ending {
        return None;
    }
    // A classified date decides only when it is the ending's own: not
    // after a connective or past form of the ending (`종료되어 … 12월 31일까지
    // 사용 가능`) and not a later use, shipping or service deadline.
    let tokens = date_tokens(clause);
    // An ending from a start date on (`10월 1일부터 학생 할인 이용 종료`)
    // is the offer's ending, whenever it falls.
    let ends_from =
        (0..tokens.len()).any(|i| classify(clause, &tokens, i) == Some(DateRole::Start));
    // The date decides only where the date readers will read it: a date
    // they leave out (`2026년 1월 1일 기준으로 이벤트 종료 안내`) leaves the
    // ending to decide.
    let dates_read = !NOT_A_DEADLINE.is_match(clause) && !NOT_OWN_PERIOD.is_match(clause);
    if !ends_from
        && dates_read
        && !tokens.is_empty()
        && (0..tokens.len()).any(|i| classify(clause, &tokens, i).is_some())
        && !ENDING_CONNECTIVE.is_match(&free)
        && !OTHER_DEADLINE.is_match(clause)
    {
        return None;
    }
    Some(Ending::Stated)
}

/// An ending word in a connective or past form: the clause goes on about
/// something else (`종료되어`, `종료되며`, `마감으로`).
static ENDING_CONNECTIVE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:종료|마감|중단|중지|만료|소진|품절)\s*(?:되어|되며|되었|됐|되고|하여|하며|하고|으로|돼)")
        .expect("valid connective pattern")
});
/// A deadline for something after the offer: shipping, results, use of
/// coupons already issued, service.
static OTHER_DEADLINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)발송|배송|지급|발표|당첨|추첨|사용\s*가능|사용하실|이용\s*가능|a\s*/\s*s|교환|환불|접수만|사용\s*기한")
        .expect("valid other-deadline pattern")
});
/// The offer withdrawn, active voice (`본 혜택은 더 이상 제공하지 않습니다`,
/// `학생 할인은 현재 적용되지 않습니다`).
static OFFER_WITHDRAWN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?:본|이|해당)\s*(?:혜택|이벤트|할인|쿠폰|프로모션|서비스)[^.]{0,30}?(?:제공|진행|운영|지급|적용)(?:하지|되지)\s*않|(?:혜택|이벤트|할인|쿠폰|프로모션|서비스)[^.]{0,20}?(?:더\s*이상|현재)[^.]{0,20}?(?:제공|진행|운영|지급|적용)(?:하지|되지)\s*않",
    )
    .expect("valid withdrawn pattern")
});
/// A countdown to an opening (`오픈 D-3`): not on yet.
static OPENING_COUNTDOWN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:오픈|시작|출시|런칭|개시|사전\s*예약)\s*d\s*-\s*[1-9]")
        .expect("valid opening countdown")
});

/// The clauses of a text: split at sentence ends, `|`, `;`, `,`, brackets,
/// `·`, `—` and a spaced ` - `, and at a `.` or `,` that does not sit
/// between two digits (so `2099.12.31` and `3,000` stay whole).
fn clauses(text: &str) -> Vec<&str> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut out = Vec::new();
    let mut start = 0;
    for (index, &(at, c)) in chars.iter().enumerate() {
        let previous = index.checked_sub(1).map(|i| chars[i].1);
        let next = chars.get(index + 1).map(|&(_, n)| n);
        let between_digits = previous.is_some_and(|p| p.is_ascii_digit())
            && next.is_some_and(|n| n.is_ascii_digit());
        let ends = match c {
            '!' | '?' | '。' | '|' | '\n' | ';' | '(' | ')' | '[' | ']' | '·' | '—' => true,
            '.' | ',' => !between_digits,
            '-' | '–' | '/' => {
                previous.is_some_and(char::is_whitespace) && next.is_some_and(char::is_whitespace)
            }
            _ => false,
        };
        if ends {
            out.push(&text[start..at]);
            start = at + c.len_utf8();
        }
    }
    out.push(&text[start..]);
    out
}

/// The strongest ending any clause of `text` states.
fn text_ending(text: &str) -> Option<Ending> {
    let mut found = None;
    for clause in clauses(text) {
        match clause_ending(clause) {
            Some(Ending::Past) => return Some(Ending::Past),
            Some(other) => {
                if found != Some(Ending::Stated) {
                    found = Some(other);
                }
            }
            None => {}
        }
    }
    found
}

/// `text` (squashed, statement taken out) excludes an audience or option.
fn says_excluded(text: &str) -> bool {
    let folded = fold_str(text);
    let lower = text.to_lowercase();
    KO_EXCLUSION_MARKERS.iter().any(|m| folded.contains(m))
        || EN_EXCLUSION_MARKERS.iter().any(|m| whole_word(&lower, m))
        || text
            .split(|c: char| {
                c.is_whitespace() || matches!(c, '(' | ')' | '[' | ']' | ',' | '.' | '/')
            })
            .any(|token| {
                token.chars().count() == 1 && token.chars().all(|c| CROSS_MARKERS.contains(&c))
            })
}

/// A date in text, the forms in order of precedence:
/// - `03/31/2025`, `12/10/2026` (month or day first: both readings),
///   `26/10/13` (two-digit year first, or last);
/// - `26.10.13` (two-digit year);
/// - `2026.10.13`, `2026. 10. 13`, `2026-10-13`, `2026년 10월 13일`, `10.13`,
///   `10/13`, `10월 13일`;
/// - `2026년 10월`, `9월` (month: its last day);
/// - `2026년` (year: its last day).
static DATE_TOKEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(\d{1,2})/(\d{1,2})/(\d{4}|\d{2})|(\d{2})\.\s*(\d{1,2})\.\s*(\d{1,2})|(?:(\d{4})\s*[.\-/년]\s*)?(\d{1,2})\s*([.\-/]|월)\s*(\d{1,2})\s*일?|(?:(\d{4})\s*년\s*)?(\d{1,2})\s*월|(\d{4})\s*년",
    )
    .expect("valid date pattern")
});
/// `Oct 13, 2026`, `October 13`, `13 Oct 2026` (lowercase text; a gap may
/// be whitespace or the folded digit gap).
const EN_MONTH: &str = r"(jan(?:uary)?|feb(?:ruary)?|mar(?:ch)?|apr(?:il)?|may|june?|july?|aug(?:ust)?|sep(?:t(?:ember)?)?|oct(?:ober)?|nov(?:ember)?|dec(?:ember)?)\.?";
static EN_DATE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"{EN_MONTH}[\s\x1F]*(\d{{1,2}})(?:st|nd|rd|th)?(?:[\s,\x1F]*(\d{{4}}))?|(\d{{1,2}})(?:st|nd|rd|th)?[\s\x1F]*{EN_MONTH}(?:[\s,\x1F]*(\d{{4}}))?"
    ))
    .expect("valid English date pattern")
});
/// What may follow a date before the word that makes it an end or a start:
/// a trailing `.`, a weekday (`(화)`, folded `화`), a time (`23:59`, `오후
/// 6시`, `18시 30분`, `자정`, `정오`).
static DATE_TAIL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^[\s\x1F]*\.?[\s\x1F]*(?:\([^)]{0,6}\)|[월화수목금토일](?:요일)?)?[\s\x1F]*(?:(오전|오후|새벽|아침|낮|저녁|밤|am|pm)?[\s\x1F]*(\d{1,2})[\s\x1F]*(?::[\s\x1F]*(\d{2})|시(?:[\s\x1F]*(\d{1,2})[\s\x1F]*분)?)|(자정|정오))?[\s\x1F]*",
    )
    .expect("valid date tail")
});
/// An end after the date (and its tail): `까지`, `마감`, `종료`, `에 종료`,
/// `부로 종료`, `을 끝으로`, a word or two before 종료 (`2026.10.13 이벤트
/// 종료`), or a period for a month or year (`9월 한정`, `9월 한 달간`).
static END_AFTER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^(?:말[\s\x1F]*)?(?:까지|마감|종료|만료|until|(?:에|부로|로|자로|을[\s\x1F]*끝으로|를[\s\x1F]*끝으로)[\s\x1F]*(?:모두[\s\x1F]*|전면[\s\x1F]*)?(?:종료|마감|만료|중단|끝)|(?:[가-힣A-Za-z]{1,8}[\s\x1F]+){1,2}(?:종료|마감|만료)|한정|한[\s\x1F]*달[\s\x1F]*(?:간|동안)|내내|중[\s\x1F]*(?:진행|한정))",
    )
    .expect("valid end-after pattern")
});
/// An end before the date: a range sign, until/through/valid until/by,
/// or a deadline label (`마감일은`, `종료:`, `기한`).
static END_BEFORE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(?:~|～|∼|〜|-|–|—|until|through|till|ends?(?:\s+on)?|expires?(?:\s+on)?|by|(?:마감|종료|만료)(?:일자|일시|일)?\s*(?:은|는|이|:|：)?|기한\s*(?:은|는|:|：)?)[\s\x1F]*$",
    )
    .expect("valid")
});
/// A start after the date: `부터`, `이후`, read as the offer's start only
/// with an offer verb close after (`부터 적용`, `이후 신청 가능`; `이후 순차
/// 발송` is about something else).
static START_AFTER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:(?:부터|이후)[\s\S]{0,24}?(?:적용|시행|신청|참여|응모|접수|진행|오픈|시작|가입|이용|제공|판매|개시|변경)|(?:[가-힣]{1,4}[\s\x1F]*)?(?:오픈|개시|런칭|출시|시작))")
        .expect("valid")
});
static START_BEFORE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:from|starting|starts?(?:\s+on)?|begins?(?:\s+on)?|effective|(?:시작|오픈|개시)(?:일자|일시|일)?\s*(?:은|는|:|：)?)[\s\x1F]*$")
        .expect("valid")
});
/// A range sign after a date (the date opens a range).
static RANGE_AFTER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:~|～|∼|〜|-|–|—)").expect("valid"));
/// A countdown (`마감 D-3`, `D-12`): its end is that many days after the
/// first sight. An opening countdown (`오픈 D-3`) is captured to be left
/// out.
static DEADLINE_COUNTDOWN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(오픈|시작|출시|런칭|개시|사전[\s\x1F]*예약)?[\s\x1F]*(d)[\s\x1F]*-[\s\x1F]*(\d{1,3})(?:\D|$)")
        .expect("valid countdown")
});
/// A deadline relative to the day it was read: `오늘까지`, `이번 달 말까지`,
/// `금주 한정`, `this month only`.
static RELATIVE_DEADLINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(오늘|금일|today|내일|모레|tomorrow|이번[\s\x1F]*주|금주|주말|this[\s\x1F]+week|이번[\s\x1F]*달|이달|금월|월말|this[\s\x1F]+month|올해|금년|연말|올[\s\x1F]*연말|연내|this[\s\x1F]+year|이번[\s\x1F]*학기|금학기|이번[\s\x1F]*시즌)[\s\x1F]*(?:말|안|중|내)?[\s\x1F]*(?:까지|만|한정|마감|종료|only)",
    )
    .expect("valid relative deadline")
});

/// A weekday right after a date: `(화)`, `화요일`, folded `화`.
static WEEKDAY_AFTER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[\s\x1F]*\.?[\s\x1F]*\(?[\s\x1F]*([월화수목금토일])(?:요일)?[\s\x1F]*\)")
        .expect("valid")
});

#[derive(Debug, Clone, Copy)]
struct DateToken {
    year: Option<i32>,
    month: u32,
    /// `None` for a month-only date (its end is the month's last day).
    day: Option<u32>,
    /// The other reading of an order-ambiguous date (`12/10/2026`).
    alternative: Option<(i32, u32, u32)>,
    /// The weekday written after it (`(화)`), Monday = 0.
    weekday: Option<u32>,
    start: usize,
    end: usize,
}

/// The up to 20 chars before `at`: enough for the words that mark a date
/// as an end or a start, and keeps whole-page scans linear.
fn lead(text: &str, at: usize) -> &str {
    let from = text[..at]
        .char_indices()
        .rev()
        .nth(19)
        .map_or(0, |(i, _)| i);
    &text[from..at]
}

fn en_month(name: &str) -> Option<u32> {
    let months = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    let name = name.trim_end_matches('.');
    months
        .iter()
        .position(|m| name.starts_with(m))
        .map(|i| i as u32 + 1)
}

fn valid(month: u32, day: Option<u32>) -> bool {
    (1..=12).contains(&month) && day.is_none_or(|d| (1..=31).contains(&d))
}

fn date_tokens(text: &str) -> Vec<DateToken> {
    // Not inside a longer number (`1588-1234`, `1.5%`, `12.345`).
    let glued = |start: usize, end: usize| {
        let before = text[..start].chars().next_back();
        let after = text[end..].chars().next();
        before.is_some_and(|b| b.is_ascii_digit() || b == '.' || b == ',' || b == '/')
            || after.is_some_and(|a| {
                a.is_ascii_digit()
                    || a == '%'
                    || a == '/'
                    || a == '.' && text[end + 1..].starts_with(|d: char| d.is_ascii_digit())
            })
    };
    let number = |m: Option<regex::Match<'_>>| m.and_then(|m| m.as_str().parse::<u32>().ok());
    let mut tokens: Vec<DateToken> = Vec::new();
    for c in DATE_TOKEN.captures_iter(text) {
        let Some(whole) = c.get(0) else {
            continue;
        };
        if glued(whole.start(), whole.end()) {
            continue;
        }
        let weekday = WEEKDAY_AFTER.captures(&text[whole.end()..]).and_then(|c| {
            "월화수목금토일"
                .chars()
                .position(|d| c[1].starts_with(d))
                .map(|i| i as u32)
        });
        let token = |year: Option<i32>, month: u32, day: Option<u32>, alternative| DateToken {
            year,
            month,
            day,
            alternative,
            weekday,
            start: whole.start(),
            end: whole.end(),
        };
        let found = if let (Some(a), Some(b), Some(last)) =
            (number(c.get(1)), number(c.get(2)), c.get(3))
        {
            // a/b/yyyy: month first (US) or day first; a/b/yy: also
            // yy/m/d. Every valid reading is kept.
            let mut readings: Vec<(i32, u32, u32)> = Vec::new();
            let tail: i32 = last.as_str().parse().unwrap_or(0);
            if last.as_str().len() == 4 {
                readings.push((tail, a, b));
                readings.push((tail, b, a));
            } else {
                readings.push((2000 + a as i32, b, tail as u32));
                readings.push((2000 + tail, a, b));
                readings.push((2000 + tail, b, a));
            }
            readings.retain(|&(_, m, d)| valid(m, Some(d)));
            readings.dedup();
            match readings.as_slice() {
                [] => None,
                [(y, m, d)] => Some(token(Some(*y), *m, Some(*d), None)),
                [(y, m, d), (y2, m2, d2), ..] => {
                    Some(token(Some(*y), *m, Some(*d), Some((*y2, *m2, *d2))))
                }
            }
        } else if let Some(yy) = number(c.get(4)) {
            Some(token(
                Some(2000 + yy as i32),
                number(c.get(5)).unwrap_or(0),
                number(c.get(6)),
                None,
            ))
        } else if let Some(month) = number(c.get(8)) {
            Some(token(
                number(c.get(7)).map(|y| y as i32),
                month,
                number(c.get(10)),
                None,
            ))
        } else if let Some(month) = number(c.get(12)) {
            Some(token(
                number(c.get(11)).map(|y| y as i32),
                month,
                None,
                None,
            ))
        } else {
            number(c.get(13)).map(|year| token(Some(year as i32), 12, None, None))
        };
        if let Some(found) = found.filter(|t| valid(t.month, t.day)) {
            tokens.push(found);
        }
    }
    // ASCII lowercase keeps byte offsets.
    for c in EN_DATE.captures_iter(&text.to_ascii_lowercase()) {
        let Some(whole) = c.get(0) else {
            continue;
        };
        let (month, day, year) = match c.get(1) {
            Some(name) => (en_month(name.as_str()), c.get(2), c.get(3)),
            None => (
                c.get(5).and_then(|m| en_month(m.as_str())),
                c.get(4),
                c.get(6),
            ),
        };
        let before = text[..whole.start()].chars().next_back();
        let (Some(month), Some(day)) = (month, number(day)) else {
            continue;
        };
        if before.is_some_and(|b| b.is_ascii_alphanumeric()) || !valid(month, Some(day)) {
            continue;
        }
        tokens.push(DateToken {
            year: number(year).map(|y| y as i32),
            month,
            day: Some(day),
            alternative: None,
            weekday: None,
            start: whole.start(),
            end: whole.end(),
        });
    }
    tokens.sort_by_key(|t| t.start);
    tokens.dedup_by_key(|t| t.start);
    tokens
}

/// The time of day stated right after a date, and where the tail ends.
fn date_tail(text: &str, at: usize) -> (Option<chrono::NaiveTime>, usize) {
    let Some(c) = DATE_TAIL.captures(&text[at..]) else {
        return (None, at);
    };
    let end = at + c.get(0).map_or(0, |m| m.end());
    let time = if let Some(word) = c.get(5) {
        // 자정 closes the day; 정오 is noon.
        Some(if word.as_str() == "정오" {
            chrono::NaiveTime::from_hms_opt(12, 0, 0)
        } else {
            chrono::NaiveTime::from_hms_opt(23, 59, 59)
        })
        .flatten()
    } else {
        c.get(2).and_then(|hour| {
            let mut hour: u32 = hour.as_str().parse().ok()?;
            let minute: u32 = c
                .get(3)
                .or(c.get(4))
                .and_then(|m| m.as_str().parse().ok())
                .unwrap_or(0);
            let afternoon = c.get(1).is_some_and(|p| {
                matches!(
                    p.as_str().to_lowercase().as_str(),
                    "오후" | "저녁" | "밤" | "pm"
                )
            });
            if afternoon && hour < 12 {
                hour += 12;
            }
            if hour == 24 {
                return chrono::NaiveTime::from_hms_opt(23, 59, 59);
            }
            chrono::NaiveTime::from_hms_opt(hour, minute, 0)
        })
    };
    (time, end)
}

/// Whether a token is an end or a start date.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DateRole {
    End,
    Start,
}

fn classify(text: &str, tokens: &[DateToken], index: usize) -> Option<DateRole> {
    let token = tokens[index];
    let (_, tail) = date_tail(text, token.end);
    let after = &text[tail..];
    if END_AFTER.is_match(after) || END_BEFORE.is_match(lead(text, token.start)) {
        return Some(DateRole::End);
    }
    let opens_range = RANGE_AFTER.is_match(after)
        && tokens
            .get(index + 1)
            .is_some_and(|next| next.start.saturating_sub(tail) <= 12);
    if START_AFTER.is_match(after) || START_BEFORE.is_match(lead(text, token.start)) || opens_range
    {
        return Some(DateRole::Start);
    }
    None
}

/// Every reading of a token. A year missing from it comes from the token
/// before it when that one is within 12 chars (a range's start; the next
/// year when this month is earlier), otherwise from the first sight
/// ([`infer_year`]); without a first sight there is no reading.
fn token_dates(tokens: &[DateToken], index: usize, clock: &Clock) -> Vec<NaiveDate> {
    let token = tokens[index];
    let mut dates = Vec::new();
    match token.year {
        Some(year) => dates.extend(resolve_date(year, token.month, token.day)),
        None => {
            let start = index
                .checked_sub(1)
                .map(|i| tokens[i])
                .filter(|previous| token.start.saturating_sub(previous.end) <= 12)
                .and_then(|s| s.year.map(|y| (y, s.month)));
            match start {
                Some((year, start_month)) => {
                    let year = if token.month < start_month {
                        year + 1
                    } else {
                        year
                    };
                    dates.extend(resolve_date(year, token.month, token.day));
                }
                None => {
                    if let Some(seen) = clock.seen {
                        dates.extend(infer_year_on(token.month, token.day, token.weekday, seen));
                    }
                }
            }
        }
    }
    if let Some((year, month, day)) = token.alternative {
        dates.extend(NaiveDate::from_ymd_opt(year, month, day));
    }
    dates
}

/// A start date ahead: a date followed by 부터/이후 with an offer verb, or
/// preceded by from/starting/effective, or the first date of a range. Any
/// reading ahead counts.
fn has_future_start_at(text: &str, clock: &Clock) -> bool {
    let tokens = date_tokens(text);
    (0..tokens.len()).any(|index| {
        classify(text, &tokens, index) == Some(DateRole::Start)
            && token_dates(&tokens, index, clock)
                .iter()
                .any(|d| *d > clock.today)
    })
}

fn last_day(year: i32, month: u32) -> Option<NaiveDate> {
    let (next_year, next_month) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    NaiveDate::from_ymd_opt(next_year, next_month, 1)?.pred_opt()
}

fn resolve_date(year: i32, month: u32, day: Option<u32>) -> Option<NaiveDate> {
    match day {
        Some(day) => NaiveDate::from_ymd_opt(year, month, day),
        None => last_day(year, month),
    }
}

/// A date without a year is the year of `anchor` (the first sight), or the
/// next year when that one is at most six months after it (an end date just
/// past New Year).
fn infer_year(month: u32, day: Option<u32>, anchor: NaiveDate) -> Option<NaiveDate> {
    use chrono::Datelike;
    let this_year = resolve_date(anchor.year(), month, day)?;
    if this_year >= anchor {
        return Some(this_year);
    }
    // Only an end just past New Year (at most three months ahead) is read
    // as next year's; a farther one is a stale page from this year.
    let next = resolve_date(anchor.year() + 1, month, day)?;
    Some(if (next - anchor).num_days() <= 92 {
        next
    } else {
        this_year
    })
}

/// [`infer_year`], with a written weekday deciding when exactly one of the
/// nearby years has the date on that weekday (`3월 31일(화)` is 2026).
fn infer_year_on(
    month: u32,
    day: Option<u32>,
    weekday: Option<u32>,
    anchor: NaiveDate,
) -> Option<NaiveDate> {
    use chrono::Datelike;
    if let (Some(day), Some(weekday)) = (day, weekday) {
        let matching: Vec<NaiveDate> = (anchor.year() - 1..=anchor.year() + 1)
            .filter_map(|year| NaiveDate::from_ymd_opt(year, month, day))
            .filter(|date| date.weekday().num_days_from_monday() == weekday)
            .collect();
        if let [only] = matching.as_slice() {
            return Some(*only);
        }
    }
    infer_year(month, day, anchor)
}

/// A passed end: an end date (closing a range, opened by `~`, followed by
/// 까지/마감/종료/…, or labelled 마감일/until/…) with any reading passed,
/// a yearless end date with no first sight to read it against, or a
/// relative deadline (`오늘까지`, `이번 달 말까지`) whose period after the
/// first sight is over (or with no first sight at all).
fn has_past_end_at(text: &str, clock: &Clock) -> bool {
    let tokens = date_tokens(text);
    let dated = (0..tokens.len()).any(|index| {
        if classify(text, &tokens, index) != Some(DateRole::End) {
            return false;
        }
        let dates = token_dates(&tokens, index, clock);
        let (time, _) = date_tail(text, tokens[index].end);
        dates.is_empty() || dates.iter().any(|d| clock.passed(*d, time))
    });
    let countdown_passed = DEADLINE_COUNTDOWN.captures_iter(text).any(|c| {
        if c.get(1).is_some() {
            return false; // an opening countdown is a start, not an end
        }
        // `D` must stand alone (not the end of `AD-3`, `GD-1`).
        let d = c.get(2).expect("d group");
        if text[..d.start()]
            .chars()
            .next_back()
            .is_some_and(|b| b.is_ascii_alphanumeric())
        {
            return false;
        }
        let days: u64 = c[3].parse().unwrap_or(0);
        match clock.seen {
            None => true,
            Some(seen) => seen
                .checked_add_days(chrono::Days::new(days))
                .is_none_or(|end| end < clock.today),
        }
    });
    dated
        || countdown_passed
        || RELATIVE_DEADLINE.captures_iter(text).any(|c| {
            let Some(seen) = clock.seen else {
                return true;
            };
            let word = fold_str(&c[1]);
            let end = match word.as_str() {
                "오늘" | "금일" | "today" => Some(seen),
                "내일" | "tomorrow" => seen.succ_opt(),
                "모레" => seen.succ_opt().and_then(|d| d.succ_opt()),
                "이번학기" | "금학기" => {
                    use chrono::Datelike;
                    // 1학기 to June, summer to August, 2학기 to December,
                    // winter to February.
                    let (year, month) = match seen.month() {
                        1 | 2 => (seen.year(), 2),
                        3..=6 => (seen.year(), 6),
                        7 | 8 => (seen.year(), 8),
                        _ => (seen.year(), 12),
                    };
                    last_day(year, month)
                }
                "이번시즌" => seen.checked_add_days(chrono::Days::new(92)),
                "이번주" | "금주" | "주말" | "thisweek" => {
                    use chrono::Datelike;
                    seen.checked_add_days(chrono::Days::new(u64::from(
                        6 - seen.weekday().num_days_from_monday(),
                    )))
                }
                "올해" | "금년" | "연말" | "올연말" | "연내" | "thisyear" => {
                    use chrono::Datelike;
                    NaiveDate::from_ymd_opt(seen.year(), 12, 31)
                }
                _ => {
                    use chrono::Datelike;
                    last_day(seen.year(), seen.month())
                }
            };
            end.is_none_or(|end| end < clock.today)
        })
}

/// An end date before `today` (start of day, first seen today).
#[cfg(test)]
fn has_past_end_date(text: &str, today: NaiveDate) -> bool {
    has_past_end_at(text, &Clock::at(today))
}

/// A start date after `today` (first seen today).
#[cfg(test)]
fn has_future_start(text: &str, today: NaiveDate) -> bool {
    has_future_start_at(text, &Clock::at(today))
}

/// A label of the offer's own period (`이벤트 기간`, `신청 기간`, `마감일`,
/// `유효기간`).
static PERIOD_LABEL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:이벤트|행사|신청|접수|참여|응모|판매|진행|혜택|적용|프로모션|모집)?\s*(?:기간|마감\s*일?|종료\s*일?|기한)|유효\s*기간")
        .expect("valid period label")
});
/// A sentence about another date than the offer's own period: a phase
/// (`1차`), shipping or results, a cut-off for members (`…까지 가입한`), last
/// year or an earlier event.
static NOT_OWN_PERIOD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\d+\s*차|회차|발송|발표|배송|지급|당첨|추첨|경품|가입한|가입하신|졸업|구매한|구매하신|등록한|이전|전년|작년|지난해|지난\s*(?:이벤트|행사|해)|기존")
        .expect("valid not-own-period pattern")
});

/// A sentence whose date is no deadline of an offer on the page: a
/// condition (`…까지 인증하지 않은 경우`, `if you didn't …`), another offer
/// (`다음 이벤트`), a birth year (`1998~2007년 출생자`), a grade or criteria
/// period, or [`NOT_OWN_PERIOD`].
static NOT_A_DEADLINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\bif\b|\bunless\b|경우|라면|다면|않으면|다음\s*(?:이벤트|행사|시즌|프로모션)|차기|\bnext\s+(?:event|season)|출생|년생|학번|등급|기준")
        .expect("valid not-a-deadline pattern")
});

/// The sentences of `text` that may carry a deadline of `offer`, joined
/// (` | `): not about another subject ([`about`]: `IE는 6월 15일 지원이
/// 종료되어`, `가입자 : … 12월 31일까지 자격 유지`), and not a condition, another
/// offer or another date ([`NOT_A_DEADLINE`], [`NOT_OWN_PERIOD`]).
fn deadline_sentences(text: &str, offer: &OfferIdentity) -> String {
    sentences_of(text)
        .into_iter()
        .filter(|s| {
            !NOT_A_DEADLINE.is_match(s)
                && !NOT_OWN_PERIOD.is_match(s)
                && about(s, offer) != About::Other
        })
        .collect::<Vec<_>>()
        .join(" | ")
}

/// The sentences of a text: split at sentence ends and `|`, at a `.` only
/// before whitespace or the end and not after a digit.
fn sentences_of(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut previous: Option<char> = None;
    let mut chars = text.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        let next = chars.peek().map(|(_, n)| *n);
        // A `.` after a digit is part of a date (`2026. 10. 13.`).
        let sentence_dot = c == '.'
            && next.is_none_or(char::is_whitespace)
            && !previous.is_some_and(|p| p.is_ascii_digit());
        previous = Some(c);
        if matches!(c, '!' | '?' | '。' | '|' | '\n') || sentence_dot {
            out.push(&text[start..at]);
            start = at + c.len_utf8();
        }
    }
    out.push(&text[start..]);
    out
}

/// A passed end of the offer's own period: in a sentence that labels a
/// period (`이벤트 기간: 2026.09.01 ~ 2026.10.13`, `마감일 2026.10.13`) or
/// closes a range, and is not about a phase, shipping, results, a member
/// cut-off or last year (`section_date_past`, a shadow metric: one date
/// alone is not trusted, see [`confirmed_end`]).
fn own_period_passed(text: &str, offer: &OfferIdentity, clock: &Clock) -> bool {
    sentences_of(text).into_iter().any(|sentence| {
        // Another subject's range is no period of the offer (`여신금융협회
        // 심의필 제…호 (2025.10.28~2026.10.27)`), as for deadlines.
        if NOT_OWN_PERIOD.is_match(sentence) || about(sentence, offer) == About::Other {
            return false;
        }
        let tokens = date_tokens(sentence);
        let labelled = PERIOD_LABEL.is_match(sentence);
        (0..tokens.len()).any(|index| {
            if classify(sentence, &tokens, index) != Some(DateRole::End) {
                return false;
            }
            let closes_range = index.checked_sub(1).is_some_and(|previous| {
                classify(sentence, &tokens, previous) == Some(DateRole::Start)
            });
            let (time, _) = date_tail(sentence, tokens[index].end);
            (labelled || closes_range)
                && token_dates(&tokens, index, clock)
                    .iter()
                    .any(|d| clock.passed(*d, time))
        })
    })
}

/// Why the section around a statement keeps it from being shown, the most
/// certain reason first: a past-tense ending, a passed end date, another
/// stated ending, a start ahead, an exclusion. `pieces` are read for dates
/// and exclusions, `readable` (the same text without skipped navigation and
/// unselected tabs) for endings; the statement itself is taken out (it may
/// carry its own condition, 제외; its own end date still counts).
fn section_reason(
    offer: &OfferIdentity,
    own: &[&str],
    pieces: &[&str],
    period: &[&str],
    readable: &[&str],
    statement: &str,
    clock: &Clock,
) -> Option<&'static str> {
    let statement_squash = squash(statement);
    // Dates are read without sentences about terms or privacy (`2026년 11월
    // 1일부터 개정 약관 적용` is no start of this offer), whole sentences so
    // each keeps its subject for [`about`].
    let dated: Vec<String> = pieces
        .iter()
        .map(|piece| {
            if !about_terms(piece) {
                return piece.to_string();
            }
            sentences_of(piece)
                .into_iter()
                .filter(|sentence| !about_terms(sentence) || names_offer_ending(sentence))
                .collect::<Vec<_>>()
                .join(" | ")
        })
        .collect();
    let pieces_for_dates: Vec<&str> = dated.iter().map(String::as_str).collect();
    // Ending words are read only for a fact read the first time; a
    // confirmed fact's section is the text judged then (module doc, 3).
    let endings: Vec<Ending> = readable
        .iter()
        .filter(|_| clock.first_sight)
        .filter_map(|piece| text_ending(&squash(piece).replace(&statement_squash, " | ")))
        .collect();
    if endings.contains(&Ending::Past) {
        return Some(SECTION_ENDED_PAST);
    }
    if period
        .iter()
        .any(|piece| own_period_passed(piece, offer, clock))
    {
        return Some(SECTION_DATE_PAST);
    }
    if pieces_for_dates
        .iter()
        .any(|piece| has_past_end_at(&deadline_sentences(piece, offer), clock))
    {
        return Some(SECTION_DATE_OTHER);
    }
    if endings.contains(&Ending::Stated) {
        return Some(SECTION_ENDED);
    }
    if endings.contains(&Ending::NotStarted)
        || pieces_for_dates
            .iter()
            .any(|piece| has_future_start_at(&deadline_sentences(piece, offer), clock))
    {
        return Some(SECTION_NOT_STARTED);
    }
    if clock.first_sight {
        return exclusion_reason(own, pieces, statement);
    }
    None
}

/// Ordinary commerce wording that excludes something other than the offer
/// (`중복 사용 불가`, `일부 품목 제외`, `타 쿠폰과 중복 불가`, `온라인 주문에
/// 한해`).
static GENERIC_EXCLUSION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?:(?:유효\s*기간|기한|사용\s*기한|제한|수수료|연회비|가입비|위약금)(?:이|은|는)?\s*없|(?:사용|충전|적립|이용|결제)\s*(?:&\s*\S+\s*)?불가\s*(?:매장|지점|점포|:)|(?:금액|비용|수수료|배송비|할인|포인트)[^.]{0,12}?(?:을|를)?\s*제외한|중복\s*(?:사용|적용|할인|참여|발급)?\s*(?:은|이|는)?\s*(?:불가|안\s*됩|되지\s*않|할\s*수\s*없)|(?:타|다른)\s*(?:쿠폰|할인|이벤트|프로모션|혜택)[^.]{0,20}?(?:불가|제외|않|없)|일부\s*(?:품목|상품|매장|메뉴|지점|브랜드|카테고리|제품)[^.]{0,20}?(?:제외|불가|않|없)|(?:온라인|오프라인|매장|주문|상품|품목|배송|결제|앱|모바일)[^.]{0,12}?에\s*한(?:해|하여|함|합니다))",
    )
    .expect("valid generic exclusion")
});
/// A hypothetical exclusion (`… 아닐 경우`, `… 불가할 수 있습니다`): it
/// describes a rule, not a current one. A scope (`온라인 구매 시 제외`) is a
/// current exclusion.
static EXCLUSION_CONDITION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)경우|때|라면|다면|으면|하면|이면|\bif\b|\bunless\b|수\s*있습니다")
        .expect("valid exclusion condition")
});

/// Wording that cancels the offer itself (`본 혜택은 … 제외`, `해당 할인은
/// 적용되지 않습니다`).
static OFFER_CANCELLED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?:본|이|해당)\s*(?:혜택|이벤트|할인|프로모션|쿠폰|서비스)\s*(?:은|는|이|가|에서|의)?[^.]{0,30}?(?:제외|적용\s*불가|참여\s*불가|신청\s*불가|적용되지\s*않|해당되지\s*않|제공되지\s*않|지급되지\s*않|받을\s*수\s*없)",
    )
    .expect("valid offer-cancelled pattern")
});
/// Words of a statement that say nothing about whom or what it is for.
const GENERIC_WORDS: &[&str] = &[
    "포인트",
    "발행",
    "충전",
    "등급",
    "카드",
    "상품",
    "매장",
    "사용",
    "금액",
    "진입",
    "최초",
    "등록",
    "음료",
    "할인",
    "혜택",
    "인증",
    "적립",
    "제공",
    "이용",
    "구매",
    "결제",
    "최대",
    "이상",
    "이하",
    "고객",
    "가능",
    "적용",
    "매월",
    "전체",
    "모든",
    "지급",
    "쿠폰",
    "무료",
    "추가",
    "경우",
    "이벤트",
    "대상",
    "기간",
    "회원",
    "제시",
    "가입",
    "신청",
    "참여",
    "첫",
    "전",
    "후",
];

/// The words naming a statement's audience, product or place (`학생`,
/// `음료`, `숙소`): particles cut off, generic and verb-like words dropped.
fn subject_words(statement: &str) -> Vec<String> {
    statement
        .split(|c: char| c.is_whitespace() || !(c.is_alphanumeric() || c == '%'))
        .filter_map(|token| {
            let mut word = token.to_string();
            for particle in PARTICLES {
                if word.chars().count() > particle.chars().count() + 1 && word.ends_with(particle) {
                    word.truncate(word.len() - particle.len());
                    break;
                }
            }
            let hangul = word.chars().all(|c| ('가'..='힣').contains(&c));
            let verb_like = ["니다", "해", "하는", "하여", "드려", "되는"]
                .iter()
                .any(|e| word.ends_with(e));
            (hangul
                && word.chars().count() >= 2
                && !verb_like
                && !GENERIC_WORDS.contains(&word.as_str()))
            .then_some(word)
        })
        .collect()
}

/// An exclusion that keeps this offer from being shown, read at first sight
/// only. In the statement's own element and its labels any exclusion that
/// is not ordinary commerce wording counts (`학생 10% 할인 (온라인 제외)`);
/// in a separate note of the section only one that names the offer's
/// subject (its audience, product or value) or cancels the offer itself.
fn exclusion_reason(own: &[&str], pieces: &[&str], statement: &str) -> Option<&'static str> {
    let statement_squash = squash(statement);
    let subject = subject_words(statement);
    let values: Vec<String> = value_signature(statement)
        .split(',')
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .collect();
    // A conditional rule (`… 아닐 경우 이용할 수 없습니다`, `등급 변경 시 …
    // 불가할 수 있습니다`) excludes no one today.
    let excluding = |clause: &str| {
        let folded = fold_str(clause);
        (KO_EXCLUSION_MARKERS.iter().any(|m| folded.contains(m)) || says_excluded(clause))
            && !EXCLUSION_CONDITION.is_match(clause)
    };
    let names_subject = |clause: &str| {
        let folded = fold_str(clause);
        subject.iter().any(|word| folded.contains(word.as_str()))
            || value_signature(clause)
                .split(',')
                .any(|v| !v.is_empty() && values.iter().any(|own| own == v))
    };
    for piece in own {
        let rest = squash(piece).replace(&statement_squash, " | ");
        if clauses(&rest).into_iter().any(|clause| {
            let clause = GENERIC_EXCLUSION.replace_all(clause, " # ");
            // A bare marker next to the statement (`(자퇴/휴학/…/졸업생 제외)`
            // with the list quoted) is the statement's own condition.
            let bare = fold_str(&clause).chars().count() <= 3;
            excluding(&clause) && !bare
        }) {
            return Some(SECTION_EXCLUDED);
        }
    }
    for piece in pieces {
        let rest = squash(piece).replace(&statement_squash, " | ");
        if clauses(&rest).into_iter().any(|clause| {
            // Ordinary commerce wording is taken out; what remains is read.
            let clause = GENERIC_EXCLUSION.replace_all(clause, " # ");
            excluding(&clause) && (OFFER_CANCELLED.is_match(&clause) || names_subject(&clause))
        }) {
            return Some(SECTION_EXCLUDED);
        }
    }
    None
}

/// Korean particles cut off a word's end ([`subject_words`],
/// [`content_words`]).
const PARTICLES: &[&str] = &[
    "에서", "으로", "에게", "이며", "이고", "은", "는", "이", "가", "을", "를", "의", "에", "로",
    "와", "과", "도", "만", "시",
];

// ── the offer relation ────────────────────────────────────────────────────

/// Whom a clause is about, relative to one offer ([`about`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum About {
    /// Its subject points at the offer, names an offer noun or a word of
    /// the offer's own name.
    ThisOffer,
    /// It states no subject (`조기 마감되었습니다`, `오늘까지만 진행`, a badge).
    Unstated,
    /// Its subject is something else (`IE는`, `가입자 :`, `Custom domains`).
    Other,
}

/// The words that name one offer: its heading chain (folded, as the reader
/// stores headings) and, where known, its programme title (`갤럭시 캠퍼스 교육
/// 할인`). A quote's words are not names: they are full of shared words
/// (`인증`, `고객`).
#[derive(Debug, Clone, Default)]
pub struct OfferIdentity {
    words: Vec<String>,
}

impl OfferIdentity {
    pub fn new<'t>(names: impl IntoIterator<Item = &'t str>) -> Self {
        let mut words: Vec<String> = names.into_iter().flat_map(content_words).collect();
        words.sort();
        words.dedup();
        Self { words }
    }
}

/// A noun naming an offer or its event, Korean or English (whole English
/// words).
static OFFER_NOUN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)이벤트|프로모션|혜택|행사|모집|판매|할인|쿠폰|캠페인|응모|신청|접수|참여|기획전|특가|\b(?:offers?|promotions?|deals?|discounts?|campaigns?|events?|sales?|programs?|programmes?|plans?|pricing|benefits?|coupons?)\b")
        .expect("valid offer noun")
});
/// Words that point at the offer on its own page (`본 혜택`, `해당 이벤트`,
/// `this offer`).
const POINTER_WORDS: &[&str] = &["본", "해당", "이번", "this"];
/// Last syllables of a verb stem: a `-는`/`-은` after them modifies a noun
/// (`진행되는`, `제공하는`, `있는`), it marks no subject.
const VERB_STEM_ENDS: &[char] = &['되', '하', '있', '없', '않', '받', '주'];
/// The whole subject is a label of a period ([`PERIOD_LABEL`]).
fn period_label_only(subject: &str) -> bool {
    let label = subject.trim_matches(|c: char| !c.is_alphanumeric());
    PERIOD_LABEL
        .find(label)
        .is_some_and(|m| m.start() == 0 && m.end() == label.len())
}

/// English words that close a clause's subject (`Custom domains are`).
const EN_AUXILIARIES: &[&str] = &[
    "is", "are", "was", "were", "has", "have", "had", "will", "can", "may", "must", "does", "do",
    "did",
];

/// Whom `clause` is about, relative to `offer`. Its subject is the label
/// before a colon, the Korean words up to the first topic or subject
/// particle (`은`/`는`/`이`/`가`, after a stem of two or more letters and
/// not a number: `28세가` is no subject), or the English words before the
/// first auxiliary. With none, the subject is `Unstated`; one that points
/// at the offer (`본`, `해당`, `this`), holds an offer noun ([`OFFER_NOUN`])
/// or shares a word with the offer's name is `ThisOffer`; any other is
/// `Other`. Shared by the date reading and by display conditions.
/// Deliberately not handled: a nominal sentence's subject (`라이선스 종료`
/// states none, so it is `Unstated`).
pub fn about(clause: &str, offer: &OfferIdentity) -> About {
    match subject_of(clause) {
        None => About::Unstated,
        Some(subject)
            if names_offer(&subject, offer) || OFFER_NOUN.is_match(&subject.to_lowercase()) =>
        {
            About::ThisOffer
        }
        // A bare label of a period (`기간:`, `마감일`) names no other
        // subject: the period is the offer's.
        Some(subject) if period_label_only(&subject) => About::Unstated,
        Some(_) => About::Other,
    }
}

/// `clause` names `offer` itself, by a pointer word or a word of its name:
/// what text outside the offer's section needs to be about it (an offer
/// noun alone, `서류 인증 신청이`, is any offer's).
pub fn names_this_offer(clause: &str, offer: &OfferIdentity) -> bool {
    subject_of(clause).is_some_and(|subject| names_offer(&subject, offer))
}

fn names_offer(subject: &str, offer: &OfferIdentity) -> bool {
    let lower = subject.to_lowercase();
    lower
        .split_whitespace()
        .next()
        .is_some_and(|word| POINTER_WORDS.contains(&word))
        || content_words(subject).iter().any(|word| {
            // Headings are stored folded (`학생할인`): a subject word inside
            // a word of the name names it.
            offer.words.iter().any(|name| name.contains(word.as_str()))
        })
}

/// The explicit subject of a clause ([`about`]).
fn subject_of(clause: &str) -> Option<String> {
    let clause = clause.trim();
    if let Some((label, _)) = clause.split_once([':', '：']) {
        let label = label.trim();
        if !label.is_empty()
            && label.chars().count() <= 20
            && !label.chars().any(|c| c.is_ascii_digit())
        {
            return Some(label.to_string());
        }
    }
    let tokens: Vec<&str> = clause.split_whitespace().collect();
    for (index, token) in tokens.iter().enumerate() {
        let word = token.trim_end_matches(|c: char| !c.is_alphanumeric());
        if word.starts_with(|c: char| c.is_ascii_digit()) {
            continue;
        }
        let stem = ["은", "는", "이", "가"]
            .iter()
            .find_map(|particle| word.strip_suffix(particle))
            .filter(|stem| stem.chars().count() >= 2)
            // `진행되는 혜택`, `할 수 있는`: a verb modifier, not a subject.
            .filter(|stem| !stem.ends_with(VERB_STEM_ENDS));
        if let Some(stem) = stem {
            let mut subject = tokens[..index].join(" ");
            subject.push(' ');
            subject.push_str(stem);
            return Some(subject.trim().to_string());
        }
        if index > 0 && EN_AUXILIARIES.contains(&word.to_lowercase().as_str()) {
            return Some(tokens[..index].join(" "));
        }
    }
    // A date in parentheses qualifies the words right before them: their
    // subject is the date's (`여신금융협회 심의필 제…호 (2025.10.28~2026.10.27)`).
    let first = date_tokens(clause).first().map(|t| t.start)?;
    let open = clause[..first].rfind('(')?;
    if clause[open..first].contains(')') {
        return None;
    }
    let head = clause[..open].trim();
    (!fold_str(head).is_empty()).then(|| head.to_string())
}

/// The content words of `text`, lowercase, particles cut off, two or more
/// letters.
fn content_words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter_map(|token| {
            let mut word = token.to_lowercase();
            for particle in PARTICLES {
                if word.chars().count() > particle.chars().count() + 1 && word.ends_with(particle) {
                    word.truncate(word.len() - particle.len());
                    break;
                }
            }
            (word.chars().count() >= 2).then_some(word)
        })
        .collect()
}

// ── the whole page ────────────────────────────────────────────────────────

/// Elements whose text is not page text.
fn not_page_text(name: &str) -> bool {
    matches!(name, "script" | "style" | "template" | "meta" | "link")
}

/// Inline elements: their edges are not a break in the text (`<b>1</b>0`
/// reads `10`).
const INLINE_TAGS: &[&str] = &[
    "a", "abbr", "b", "bdi", "bdo", "cite", "code", "data", "del", "dfn", "em", "font", "i", "ins",
    "kbd", "label", "mark", "q", "s", "samp", "small", "span", "strike", "strong", "sub", "sup",
    "time", "u", "var", "tspan",
];

/// Attributes whose value is wording a reader sees (tooltips, image text).
const WORDING_ATTRS: &[&str] = &[
    "title",
    "alt",
    "aria-label",
    "data-tooltip",
    "data-title",
    "data-tip",
];

/// Marks in the digest: hidden text, attribute text, a table cell that
/// holds several blocks or spans rows or columns, and a control's state
/// (disabled, selected, current). Moving text in or out of view, between
/// cells, or a cell's span or a control's state changing, is a change.
const HIDDEN_OPEN: char = '\u{2}';
const HIDDEN_CLOSE: char = '\u{3}';
const ATTRIBUTE: char = '\u{4}';
const CELL_OPEN: char = '\u{5}';
const CELL_CLOSE: char = '\u{6}';
const STATE: char = '\u{7}';

/// A block that is only render-time furniture, folded so it does not
/// change the digest: a board counter standing alone (`조회 123`), an age
/// standing alone (`3시간 전`, `등록 2일 전`), a countdown standing alone
/// (`D-3`, sign kept), a post date standing alone (`등록일 2026.10.01
/// 14:33`). A number in any other sentence is never folded (`댓글 3개 이상
/// 작성 시`, `가입 후 D+30일 이내`, `등록일 2026.01.01 이후 가입`).
static STEADY_BLOCKS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    [
        (
            r"(?i)^(조회수?|views?|hits?|댓글|좋아요|공감|추천수?)\s*[:：]?\s*\d[\d,]*$",
            "$1 <n>",
        ),
        (
            r"(?i)^(?:(등록|작성|게시|수정|업데이트|updated|posted|published)\s*[:：]?\s*)?\d+\s*(초|분|시간)\s*전$",
            "<ago>",
        ),
        (
            r"(?i)^(등록|작성|게시|수정|업데이트|updated|posted|published)\s*[:：]?\s*\d+\s*(일|주|개월|달)\s*전$",
            "$1 <ago>",
        ),
        (r"(?i)^([가-힣A-Za-z]{0,6}\s*)d\s*([-+])\s*\d{1,3}$", "${1}D$2<n>"),
        (
            r"(?i)^(등록일|작성일|게시일|수정일|최종\s*수정일?|업데이트|updated|posted|published)\s*[:：]?\s*\d{4}[.\-/년]\s?\d{1,2}[.\-/월]\s?\d{1,2}일?\.?(\s*\([^)]{1,3}\))?(\s*\d{1,2}:\d{2}(:\d{2})?)?$",
            "$1 <date>",
        ),
    ]
    .into_iter()
    .map(|(pattern, replacement)| (Regex::new(pattern).expect("valid steady block"), replacement))
    .collect()
});
/// Board labels next to which a number standing alone in its own block is
/// a counter (`<dt>조회</dt><dd>123</dd>`).
static BOARD_LABEL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(조회수?|views?|hits?|댓글|좋아요|공감|추천수?)\s*[:：]?$").expect("valid")
});

/// `text` with every thousands comma between digits dropped (`3,000` reads
/// `3000`); a comma followed by other than exactly three digits stays
/// (`1,2차`).
fn drop_thousands_commas(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    for (index, &c) in chars.iter().enumerate() {
        if c == ','
            && index > 0
            && chars[index - 1].is_ascii_digit()
            && chars.len() >= index + 4
            && chars[index + 1..index + 4].iter().all(char::is_ascii_digit)
            && chars.get(index + 4).is_none_or(|n| !n.is_ascii_digit())
        {
            continue;
        }
        out.push(c);
    }
    out
}

/// Hashes page text as it is read (no length cap): width, dash and tilde
/// variants and case folded, whitespace dropped (kept as a gap where it
/// separates two digits, or a digit from a `.` or `,`: `2. 5%` is not
/// `2.5%`), thousands commas dropped, render-time furniture blocks folded
/// ([`STEADY_BLOCKS`]), every other character kept in order, values, units,
/// footnote marks (※ * † ‡) and punctuation included.
#[derive(Default)]
struct PageStream {
    hasher: sha2::Sha256,
    last: Option<char>,
    gap: bool,
    /// The current block's text (inline runs joined), not yet hashed.
    run: String,
    /// The previous block's text, squashed (a board label before a count).
    previous: String,
    /// The current block's texts, each with whether it is skipped for
    /// endings (navigation, an unselected tab).
    segments: Vec<(String, bool)>,
    /// The current block's element and whether it is outside the main
    /// content (footer, aside, sidebar).
    block_tag: String,
    aside: bool,
    blocks: Vec<Block>,
    /// Fold per-request tokens ([`REQUEST_TOKEN`]): the section reading.
    fold_tokens: bool,
}

/// A per-request token: 32 or more hexadecimal characters with a letter
/// among them (a trace or session id, a cache key). No offer, price,
/// condition or date is written so; folded to one mark in the section
/// digest so a page served again reads the same.
static REQUEST_TOKEN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b[0-9a-f]{32,}\b").expect("valid token pattern"));

/// One block of page text as the ending and date scans read it.
#[derive(Debug, Clone, Default)]
struct Block {
    segments: Vec<(String, bool)>,
    /// The block's own element (`dt`, `th`, `p`, …).
    tag: String,
    /// In a footer, an aside or a sidebar: not read page-wide.
    aside: bool,
}

impl PageStream {
    fn emit(&mut self, c: char) {
        use sha2::Digest;
        let mut buffer = [0u8; 4];
        self.hasher.update(c.encode_utf8(&mut buffer).as_bytes());
        self.last = Some(c);
    }

    fn hash_char(&mut self, c: char) {
        if c.is_whitespace() || matches!(c, '\u{200b}'..='\u{200d}' | '\u{2060}' | '\u{feff}') {
            self.gap = true;
            return;
        }
        let number_like = |x: char| x.is_ascii_digit() || x == '.' || x == ',';
        if self.gap
            && self.last.is_some_and(number_like)
            && number_like(c)
            && (c.is_ascii_digit() || self.last.is_some_and(|p| p.is_ascii_digit()))
        {
            self.emit(super::evidence::DIGIT_GAP);
        }
        self.gap = false;
        self.emit(c);
    }

    /// Hashes the pending run, folded where it is furniture.
    fn flush_run(&mut self) {
        if self.run.trim().is_empty() {
            self.run.clear();
            return;
        }
        let run = std::mem::take(&mut self.run);
        let squashed = squash(&run);
        let mut steady = None;
        for (pattern, replacement) in STEADY_BLOCKS.iter() {
            if pattern.is_match(&squashed) {
                steady = Some(pattern.replace(&squashed, *replacement).into_owned());
                break;
            }
        }
        if steady.is_none()
            && BOARD_LABEL.is_match(&self.previous)
            && squashed.chars().all(|c| c.is_ascii_digit() || c == ',')
        {
            steady = Some("<n>".to_string());
        }
        self.previous = squashed;
        let run = if self.fold_tokens {
            REQUEST_TOKEN
                .replace_all(&run, |token: &regex::Captures<'_>| {
                    if token[0].chars().any(|c| c.is_ascii_alphabetic()) {
                        "<token>".to_string()
                    } else {
                        token[0].to_string()
                    }
                })
                .into_owned()
        } else {
            run
        };
        let text = steady.unwrap_or_else(|| drop_thousands_commas(&run));
        for c in text.chars().map(fold_char) {
            self.hash_char(c);
        }
    }

    /// A block edge: the run is hashed and the block's texts are kept.
    fn boundary(&mut self) {
        self.flush_run();
        if !self.segments.is_empty() {
            self.blocks.push(Block {
                segments: std::mem::take(&mut self.segments),
                tag: self.block_tag.clone(),
                aside: self.aside,
            });
        }
        self.gap = true;
    }

    /// A structural mark in reading order.
    fn mark(&mut self, c: char) {
        self.flush_run();
        self.emit(c);
    }

    fn feed(&mut self, raw: &str, skipped: bool) {
        self.run.push_str(raw);
        self.segments.push((raw.to_string(), skipped));
    }
}

/// What the page's structured data (JSON-LD, microdata) states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stated {
    /// `validThrough`, `endDate`, … with its time when given.
    End(NaiveDate, Option<chrono::NaiveTime>),
    /// `validFrom`, `startDate`, `availabilityStarts`.
    Start(NaiveDate),
    /// `availability` SoldOut/Discontinued, `eventStatus` EventCancelled.
    Ended,
    /// `availability` OutOfStock and similar, `eventStatus` EventPostponed.
    Unavailable,
}

/// The page read whole: the digest every confirmation compares, its blocks
/// for the ending and date scans, and what its structured data states.
struct PageRead {
    digest: String,
    blocks: Vec<Block>,
    stated: Vec<Stated>,
    /// The JSON-LD scripts, for [`trusted_states`] once the URL is known.
    scripts: Vec<String>,
}

/// A schema.org key in JSON-LD, also as a compact or full IRI
/// (`schema:validThrough`, `https://schema.org/validThrough`).
static LD_VALUE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#""(?:[^"]*[:/#])?(validThrough|endDate|availabilityEnds|expires|validFrom|startDate|availabilityStarts|availability|eventStatus)"\s*:\s*(?:\{\s*"@id"\s*:\s*)?"([^"]*)""#,
    )
    .expect("valid JSON-LD pattern")
});
static ISO_DATE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*(\d{4})-(\d{1,2})-(\d{1,2})(?:[T ](\d{1,2}):(\d{2}))?").expect("valid")
});

/// Types that describe a site, a list or a breadcrumb, never the offer.
const NOT_OFFER_TYPES: &[&str] = &[
    "ItemList",
    "BreadcrumbList",
    "WebSite",
    "WebPage",
    "CollectionPage",
    "SearchResultsPage",
    "Organization",
    "Corporation",
    "LocalBusiness",
    "Brand",
    "Person",
    "SiteNavigationElement",
];

fn last_segment(key: &str) -> &str {
    key.rsplit([':', '/', '#']).next().unwrap_or(key)
}

/// The page's own names (`<title>`, `og:title`, every `h1`), folded.
fn page_names(document: &Html) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let select = |css: &str| scraper::Selector::parse(css).expect("valid selector");
    names.extend(
        document
            .select(&select("title"))
            .map(|e| e.text().collect::<String>()),
    );
    names.extend(
        document
            .select(&select("h1"))
            .map(|e| e.text().collect::<String>()),
    );
    names.extend(
        document
            .select(&select("meta[property=\"og:title\"]"))
            .filter_map(|e| e.value().attr("content").map(str::to_string)),
    );
    names
        .into_iter()
        .map(|n| fold_str(&n))
        .filter(|n| n.chars().count() >= 2)
        .collect()
}

/// An item with this name describes this page: no name, or a name the
/// page's title or headline holds (or holds it).
fn names_page(name: Option<&str>, names: &[String]) -> bool {
    match name.map(fold_str).filter(|n| n.chars().count() >= 2) {
        None => true,
        Some(name) => names
            .iter()
            .any(|page| page.contains(&name) || name.contains(page.as_str())),
    }
}

fn json_get<'v>(object: &'v serde_json::Value, key: &str) -> Option<&'v serde_json::Value> {
    object
        .as_object()?
        .iter()
        .find(|(k, _)| last_segment(k) == key)
        .map(|(_, v)| v)
}

fn json_text(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Object(_) => json_get(value, "@id")
            .or_else(|| json_get(value, "id"))
            .and_then(json_text),
        serde_json::Value::Array(items) => items.first().and_then(json_text),
        _ => None,
    }
}

fn json_types(value: &serde_json::Value) -> Vec<String> {
    match json_get(value, "@type") {
        Some(serde_json::Value::String(s)) => vec![last_segment(s).to_string()],
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .filter_map(|i| i.as_str().map(|s| last_segment(s).to_string()))
            .collect(),
        _ => Vec::new(),
    }
}

/// What one item and its own offers state.
fn json_item_states(item: &serde_json::Value, out: &mut Vec<Stated>) {
    let Some(object) = item.as_object() else {
        return;
    };
    for (key, value) in object {
        let key = last_segment(key);
        if key == "offers" {
            match value {
                serde_json::Value::Array(offers) => {
                    for offer in offers {
                        json_item_states(offer, out);
                    }
                }
                offer => json_item_states(offer, out),
            }
        } else if let Some(text) = json_text(value) {
            out.extend(stated_value(key, &text));
        }
    }
}

/// What the page's own structured data states. Only the item describing
/// this page is read: a `mainEntity`, or the single top-level item (not a
/// list, breadcrumb, site or organisation) whose name is absent or names
/// the page; with several top-level items, those that name the page. List
/// members and related products are never read. Unreadable JSON-LD is read
/// value by value (fail closed).
fn structured_states(document: &Html, scripts: &[String]) -> Vec<Stated> {
    let names = page_names(document);
    let mut out = Vec::new();
    for script in scripts {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(script.trim()) else {
            for c in LD_VALUE.captures_iter(script) {
                out.extend(stated_value(&c[1], &c[2]));
            }
            continue;
        };
        let roots: Vec<&serde_json::Value> = match &value {
            serde_json::Value::Array(items) => items.iter().collect(),
            other => vec![other],
        };
        let roots: Vec<&serde_json::Value> = roots
            .into_iter()
            .flat_map(|root| match json_get(root, "@graph") {
                Some(serde_json::Value::Array(graph)) => graph.iter().collect(),
                _ => vec![root],
            })
            .collect();
        let mut main_entities = Vec::new();
        let mut candidates = Vec::new();
        for root in roots {
            if let Some(main) = json_get(root, "mainEntity") {
                match main {
                    serde_json::Value::Array(items) => main_entities.extend(items.iter()),
                    item => main_entities.push(item),
                }
                continue;
            }
            if json_types(root)
                .iter()
                .any(|t| NOT_OFFER_TYPES.contains(&t.as_str()))
            {
                continue;
            }
            candidates.push(root);
        }
        for item in main_entities {
            json_item_states(item, &mut out);
        }
        let single = candidates.len() == 1;
        for item in candidates {
            let name = json_get(item, "name").and_then(json_text);
            if (single || name.is_some()) && names_page(name.as_deref(), &names) {
                json_item_states(item, &mut out);
            }
        }
    }
    // Microdata: top-level itemscope items, the same rule.
    let scope_selector = scraper::Selector::parse("[itemscope]").expect("valid selector");
    let items: Vec<scraper::ElementRef<'_>> = document
        .select(&scope_selector)
        .filter(|e| {
            !e.ancestors()
                .filter_map(scraper::ElementRef::wrap)
                .any(|a| a.value().attr("itemscope").is_some())
        })
        .filter(|e| {
            let kind = e.value().attr("itemtype").map(last_segment).unwrap_or("");
            !NOT_OFFER_TYPES.contains(&kind)
        })
        .collect();
    let single = items.len() == 1;
    for item in items {
        let props: Vec<(String, String)> = item
            .descendants()
            .filter_map(scraper::ElementRef::wrap)
            .filter_map(|e| {
                let props = e.value().attr("itemprop")?;
                let value = e
                    .value()
                    .attr("content")
                    .or(e.value().attr("datetime"))
                    .or(e.value().attr("href"))
                    .map(str::to_string)
                    .unwrap_or_else(|| e.text().collect::<String>());
                Some(
                    props
                        .split_whitespace()
                        .map(|p| (p.to_string(), value.trim().to_string()))
                        .collect::<Vec<_>>(),
                )
            })
            .flatten()
            .collect();
        let name = props
            .iter()
            .find(|(k, _)| k == "name")
            .map(|(_, v)| v.clone());
        if (single || name.is_some()) && names_page(name.as_deref(), &names) {
            for (key, value) in &props {
                out.extend(stated_value(key, value));
            }
        }
    }
    out
}

/// What one structured-data value states.
fn stated_value(key: &str, value: &str) -> Option<Stated> {
    let key = key.rsplit([':', '/', '#']).next().unwrap_or(key);
    match key {
        "availability" => {
            let value = value
                .rsplit(['/', ':', '#'])
                .next()
                .unwrap_or(value)
                .to_ascii_lowercase();
            match value.as_str() {
                "soldout" | "discontinued" => Some(Stated::Ended),
                "outofstock" | "backorder" | "preorder" | "presale" => Some(Stated::Unavailable),
                _ => None,
            }
        }
        "eventStatus" => {
            let value = value
                .rsplit(['/', ':', '#'])
                .next()
                .unwrap_or(value)
                .to_ascii_lowercase();
            match value.as_str() {
                "eventcancelled" => Some(Stated::Ended),
                "eventpostponed" | "eventrescheduled" => Some(Stated::Unavailable),
                _ => None,
            }
        }
        _ => {
            let c = ISO_DATE.captures(value)?;
            let date = NaiveDate::from_ymd_opt(
                c[1].parse().ok()?,
                c[2].parse().ok()?,
                c[3].parse().ok()?,
            )?;
            let time = match (c.get(4), c.get(5)) {
                (Some(h), Some(m)) => chrono::NaiveTime::from_hms_opt(
                    h.as_str().parse().ok()?,
                    m.as_str().parse().ok()?,
                    0,
                ),
                _ => None,
            };
            // Only the offer's own start and end: a publication, founding,
            // review or price-validity date says nothing about the offer.
            match key {
                "validFrom" | "startDate" | "availabilityStarts" => Some(Stated::Start(date)),
                "validThrough" | "endDate" | "availabilityEnds" | "expires" => {
                    Some(Stated::End(date, time))
                }
                _ => None,
            }
        }
    }
}

/// The keys `structured_ended` trusts: the offer's own end and status.
const TRUSTED_KEYS: &[&str] = &["validThrough", "endDate", "availability", "eventStatus"];

/// An offer or an event (`Offer`, `AggregateOffer`, `Event`, `SaleEvent`, …).
fn offer_or_event(types: &[String]) -> bool {
    types
        .iter()
        .any(|t| t == "Offer" || t == "AggregateOffer" || t.ends_with("Event"))
}

/// The same absolute URL: scheme and host compared without case, the
/// fragment and a trailing slash dropped. A relative URL is never the page.
fn same_page_url(a: &str, b: &str) -> bool {
    let normal = |url: &str| -> Option<String> {
        let url = url.trim();
        let url = url.split('#').next().unwrap_or(url);
        let at = url.find("://")?;
        let path_at = url[at + 3..].find('/').map_or(url.len(), |p| at + 3 + p);
        Some(format!(
            "{}{}",
            url[..path_at].to_ascii_lowercase(),
            url[path_at..].trim_end_matches('/')
        ))
    };
    matches!((normal(a), normal(b)), (Some(x), Some(y)) if x == y)
}

/// What one trusted item states itself ([`TRUSTED_KEYS`], nothing nested):
/// an end, sold out, discontinued, cancelled.
fn trusted_values<'k>(props: impl Iterator<Item = (&'k str, String)>, out: &mut Vec<Stated>) {
    for (key, value) in props {
        if !TRUSTED_KEYS.contains(&last_segment(key)) {
            continue;
        }
        if let Some(stated @ (Stated::Ended | Stated::End(..))) = stated_value(key, &value) {
            out.push(stated);
        }
    }
}

fn json_trusted(item: &serde_json::Value, out: &mut Vec<Stated>) {
    if let Some(object) = item.as_object() {
        trusted_values(
            object
                .iter()
                .filter_map(|(k, v)| json_text(v).map(|text| (k.as_str(), text))),
            out,
        );
    }
}

/// A microdata item's own properties (not those of items nested in it).
fn micro_props(item: scraper::ElementRef<'_>) -> Vec<(String, String)> {
    item.descendants()
        .filter_map(scraper::ElementRef::wrap)
        .filter(|e| e.id() != item.id())
        .filter(|e| {
            // The nearest item above the property is this one.
            e.ancestors()
                .filter_map(scraper::ElementRef::wrap)
                .find(|a| a.value().attr("itemscope").is_some())
                .is_some_and(|a| a.id() == item.id())
        })
        .filter_map(|e| {
            let props = e.value().attr("itemprop")?;
            let value = e
                .value()
                .attr("content")
                .or(e.value().attr("datetime"))
                .or(e.value().attr("href"))
                .map(str::to_string)
                .unwrap_or_else(|| e.text().collect::<String>());
            Some(
                props
                    .split_whitespace()
                    .map(|p| (p.to_string(), value.trim().to_string()))
                    .collect::<Vec<_>>(),
            )
        })
        .flatten()
        .collect()
}

/// What `structured_ended` trusts: only the page's `mainEntity`, or the one
/// offer or event whose `url` is this page's URL, and only its own end
/// (`validThrough`, `endDate`) and status (`availability`, `eventStatus`).
/// Publication, founding, review and price-validity dates, other items,
/// and JSON-LD that does not parse are never read.
fn trusted_states(document: &Html, scripts: &[String], page_url: Option<&str>) -> Vec<Stated> {
    let mut out = Vec::new();
    let mut url_items = 0usize;
    let mut url_states = Vec::new();
    let for_page = |types: Vec<String>, url: Option<String>| {
        offer_or_event(&types)
            && page_url
                .zip(url)
                .is_some_and(|(page, url)| same_page_url(page, &url))
    };
    for script in scripts {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(script.trim()) else {
            continue;
        };
        let roots: Vec<&serde_json::Value> = match &value {
            serde_json::Value::Array(items) => items.iter().collect(),
            other => vec![other],
        };
        let roots: Vec<&serde_json::Value> = roots
            .into_iter()
            .flat_map(|root| match json_get(root, "@graph") {
                Some(serde_json::Value::Array(graph)) => graph.iter().collect(),
                _ => vec![root],
            })
            .collect();
        let by_id: HashMap<&str, &serde_json::Value> = roots
            .iter()
            .filter_map(|root| Some((json_get(root, "@id")?.as_str()?, *root)))
            .collect();
        for root in &roots {
            let Some(main) = json_get(root, "mainEntity") else {
                continue;
            };
            let items: Vec<&serde_json::Value> = match main {
                serde_json::Value::Array(items) => items.iter().collect(),
                item => vec![item],
            };
            for item in items {
                // A reference (`{"@id": …}`) is read where it is defined.
                let referenced = item
                    .as_object()
                    .filter(|o| o.len() == 1)
                    .and_then(|_| json_get(item, "@id")?.as_str())
                    .and_then(|id| by_id.get(id).copied());
                json_trusted(referenced.unwrap_or(item), &mut out);
            }
        }
        // Top-level items and graph nodes only: an offer nested in a
        // product (one size sold out) is not the page's offer.
        for object in roots.iter().copied() {
            let url = json_get(object, "url").and_then(json_text);
            if for_page(json_types(object), url) {
                url_items += 1;
                json_trusted(object, &mut url_states);
            }
        }
    }
    let scope_selector = scraper::Selector::parse("[itemscope]").expect("valid selector");
    for item in document.select(&scope_selector) {
        let props = micro_props(item);
        let main = item.value().attr("itemprop").is_some_and(|p| {
            p.split_whitespace()
                .any(|p| last_segment(p) == "mainEntity")
        });
        if main {
            trusted_values(props.iter().map(|(k, v)| (k.as_str(), v.clone())), &mut out);
            continue;
        }
        let nested = item
            .ancestors()
            .filter_map(scraper::ElementRef::wrap)
            .any(|a| a.value().attr("itemscope").is_some());
        if nested {
            continue;
        }
        let kind = item
            .value()
            .attr("itemtype")
            .map(|t| last_segment(t).to_string());
        let url = props
            .iter()
            .find(|(k, _)| last_segment(k) == "url")
            .map(|(_, v)| v.clone());
        if for_page(kind.into_iter().collect(), url) {
            url_items += 1;
            trusted_values(
                props.iter().map(|(k, v)| (k.as_str(), v.clone())),
                &mut url_states,
            );
        }
    }
    if url_items == 1 {
        out.extend(url_states);
    }
    out
}

/// Navigation (`nav`, `menu`, `select`, a navigation role, or a list whose
/// every item is one short link) and an unselected tab: their labels name
/// other pages or views, so they are not read for endings. Everything else
/// is read, controls included (a disabled `신청 마감` button, a selected tab,
/// a card made clickable).
fn skipped_label(element: scraper::ElementRef<'_>) -> bool {
    let value = element.value();
    let role = value.attr("role").map(str::trim);
    if matches!(value.name(), "nav" | "menu" | "select")
        || matches!(role, Some("navigation" | "menu" | "menubar"))
    {
        return true;
    }
    if role == Some("tab") {
        let selected = value.attr("aria-selected") == Some("true")
            || value.attr("aria-current").is_some_and(|c| c != "false")
            || value.classes().any(|c| {
                matches!(
                    c.to_ascii_lowercase().as_str(),
                    "on" | "active" | "selected" | "current" | "is-active"
                )
            });
        return !selected;
    }
    if matches!(value.name(), "ul" | "ol") {
        let items: Vec<scraper::ElementRef<'_>> = element
            .children()
            .filter_map(scraper::ElementRef::wrap)
            .filter(|e| e.value().name() == "li")
            .collect();
        // A list of short links that says something is sold out is a
        // banner or tile list, not navigation: it is read.
        let stock = items
            .iter()
            .any(|item| STOCK_WORDS.is_match(&item.text().collect::<String>()));
        return !items.is_empty()
            && !stock
            && items.iter().all(|item| {
                let links: Vec<scraper::ElementRef<'_>> = item
                    .descendants()
                    .filter_map(scraper::ElementRef::wrap)
                    .filter(|e| matches!(e.value().name(), "a" | "button"))
                    .collect();
                let text: String = item.text().collect::<String>().split_whitespace().collect();
                // A menu item is its link: text beside the link (`… 종료
                // 안내 <a>자세히</a>`) is read.
                let link_text: String = links
                    .first()
                    .map(|l| l.text().collect::<String>().split_whitespace().collect())
                    .unwrap_or_default();
                links.len() == 1
                    && link_text == text
                    && text.chars().count() <= MAX_LINK_LABEL_CHARS
                    && !item
                        .descendants()
                        .filter_map(scraper::ElementRef::wrap)
                        .any(|e| {
                            matches!(
                                e.value().name(),
                                "p" | "div"
                                    | "h1"
                                    | "h2"
                                    | "h3"
                                    | "h4"
                                    | "h5"
                                    | "h6"
                                    | "img"
                                    | "table"
                            )
                        })
            });
    }
    false
}

/// Stock words (`품절`, `재고 소진`, `Sold out`).
static STOCK_WORDS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)품절|재고\s*소진|\bsold\s*out\b|\bout\s+of\s+stock\b")
        .expect("valid stock pattern")
});

/// Longest text a menu item may have (`지난 이벤트 보기`).
const MAX_LINK_LABEL_CHARS: usize = 20;

/// Outside the main content: a footer, an aside, a sidebar.
fn aside_region(element: scraper::ElementRef<'_>) -> bool {
    let value = element.value();
    // A footer or aside inside an article or section belongs to it.
    let sectioned = || {
        element
            .ancestors()
            .filter_map(scraper::ElementRef::wrap)
            .any(|a| matches!(a.value().name(), "article" | "section"))
    };
    (matches!(value.name(), "footer" | "aside") && !sectioned())
        || matches!(
            value.attr("role").map(str::trim),
            Some("contentinfo" | "complementary")
        )
        || value.classes().chain(value.id()).any(|token| {
            matches!(
                token.to_ascii_lowercase().as_str(),
                "footer" | "sidebar" | "side-bar" | "aside" | "site-footer" | "global-footer"
            )
        })
}

/// Visible text of a section element for the reading around a statement
/// held by `keep`: footers, asides and sidebars inside it are left out
/// (unless they hold the statement), and with `labels` navigation and
/// unselected tabs are taken out (` | `) too.
fn section_text(element: scraper::ElementRef<'_>, keep: ego_tree::NodeId, labels: bool) -> String {
    section_text_as(element, keep, labels, false)
}

/// The offer's own period text: [`section_text`] without the items of a
/// list of other offers (a list item or link that holds a link and not the
/// statement, `<li><a>여름방학 특가 2026.07.01 ~ 08.31</a></li>`).
fn period_text(element: scraper::ElementRef<'_>, keep: ego_tree::NodeId) -> String {
    section_text_as(element, keep, true, true)
}

fn section_text_as(
    element: scraper::ElementRef<'_>,
    keep: ego_tree::NodeId,
    labels: bool,
    period: bool,
) -> String {
    let mut text = String::new();
    // Blocks are kept apart (` | `), so a clause never runs from one block
    // into the next; a label cell runs into its value cell.
    let mut previous_block: Option<(ego_tree::NodeId, String)> = None;
    'nodes: for node in element.descendants() {
        let Some(fragment) = node.value().as_text() else {
            continue;
        };
        let block = node.ancestors().find_map(|a| {
            let value = a.value().as_element()?;
            (!INLINE_TAGS.contains(&value.name())).then(|| (a.id(), value.name().to_string()))
        });
        let mut skipped = false;
        for ancestor in node.ancestors() {
            if let Some(value) = ancestor.value().as_element() {
                if element_hidden(value) {
                    continue 'nodes;
                }
                if ancestor.id() != element.id()
                    && scraper::ElementRef::wrap(ancestor).is_some_and(aside_region)
                {
                    let holds_statement = ancestor.descendants().any(|d| d.id() == keep);
                    if !holds_statement {
                        continue 'nodes;
                    }
                }
                if period
                    && ancestor.id() != element.id()
                    && (value.name() == "a"
                        || (value.name() == "li"
                            && ancestor
                                .descendants()
                                .any(|d| d.value().as_element().is_some_and(|e| e.name() == "a"))))
                    && !ancestor.descendants().any(|d| d.id() == keep)
                {
                    continue 'nodes;
                }
            }
            if labels && scraper::ElementRef::wrap(ancestor).is_some_and(skipped_label) {
                skipped = true;
            }
            if ancestor.id() == element.id() {
                break;
            }
        }
        if !fragment.trim().is_empty() {
            if let (Some((last_id, last_tag)), Some((id, tag))) = (&previous_block, &block) {
                let label_pair =
                    matches!(last_tag.as_str(), "dt" | "th") && matches!(tag.as_str(), "dd" | "td");
                if last_id != id && !label_pair {
                    text.push_str(" | ");
                }
            }
            previous_block = block;
        }
        text.push_str(if skipped { " | " } else { fragment });
        text.push(' ');
    }
    squash(&text)
}

/// A table cell that holds two or more blocks of text (`<td><p>대학생</p>
/// <p>대학원생</p></td>`): which cell a block sits in is meaning.
fn multi_block_cell(element: scraper::ElementRef<'_>) -> bool {
    element
        .children()
        .filter_map(scraper::ElementRef::wrap)
        .filter(|child| {
            !INLINE_TAGS.contains(&child.value().name())
                && !child.text().collect::<String>().trim().is_empty()
        })
        .count()
        >= 2
}

fn read_page(document: &Html) -> PageRead {
    let mut page = read_tree(document.tree.root());
    page.stated = structured_states(document, &page.scripts);
    page
}

/// [`read_page`] of one subtree (an offer's section): the same text, marks
/// and folding as the page digest, from `root` down.
fn read_tree(root: ego_tree::NodeRef<'_, scraper::Node>) -> PageRead {
    read_tree_without_chrome(root, None)
}

/// Page chrome: a footer, an aside or a sidebar ([`aside_region`]), or
/// navigation (`nav`, a menu). It frames every page of a site and says
/// nothing about the offer.
fn chrome(element: scraper::ElementRef<'_>) -> bool {
    let value = element.value();
    aside_region(element)
        || matches!(value.name(), "nav" | "menu")
        || matches!(
            value.attr("role").map(str::trim),
            Some("navigation" | "menu" | "menubar")
        )
}

/// A link's or a control's label inside page chrome (a link, a form
/// control or an ARIA widget role): it names another page or a setting,
/// not the offer.
fn chrome_label(element: scraper::ElementRef<'_>) -> bool {
    let value = element.value();
    matches!(value.name(), "a" | "button" | "select" | "option" | "label")
        || matches!(
            value.attr("role").map(str::trim),
            Some(
                "button"
                    | "link"
                    | "tab"
                    | "menuitem"
                    | "option"
                    | "combobox"
                    | "listbox"
                    | "checkbox"
                    | "radio"
                    | "switch"
            )
        )
}

/// [`read_tree`] of an offer's section when `keep` (the statement's own
/// element) is given: a chrome subtree ([`chrome`]) that does not hold it is
/// read as a visitor sees it and nothing else. Its visible sentences count
/// (an ending or a condition in a footer, an aside or the navigation is the
/// offer's text, review A-H1); what only markup carries does not: hidden
/// nodes, control state, wording attributes, cell marks, and link or
/// control labels ([`chrome_label`]), which name other pages (a menu item,
/// a language selector).
fn read_tree_without_chrome(
    root: ego_tree::NodeRef<'_, scraper::Node>,
    keep: Option<ego_tree::NodeId>,
) -> PageRead {
    use ego_tree::iter::Edge;
    let mut stream = PageStream {
        fold_tokens: keep.is_some(),
        ..PageStream::default()
    };
    let mut ld_scripts: Vec<String> = Vec::new();
    // Per open element: skipped, hidden, skipped for endings, aside, block
    // element name, and whether a cell mark was written.
    struct Open {
        skipped: bool,
        hidden: bool,
        label: bool,
        aside: bool,
        cell: bool,
        chrome: bool,
    }
    let mut stack: Vec<Open> = Vec::new();
    let mut block_tags: Vec<String> = Vec::new();
    let mut marked = false;
    for edge in root.traverse() {
        match edge {
            Edge::Open(node) => match node.value() {
                scraper::Node::Element(element) => {
                    let parent = stack.last();
                    let (parent_skipped, parent_hidden, parent_label, parent_aside, parent_chrome) =
                        parent.map_or((false, false, false, false, false), |p| {
                            (p.skipped, p.hidden, p.label, p.aside, p.chrome)
                        });
                    let name = element.name();
                    let script_type = element.attr("type").map(|t| {
                        t.split(';')
                            .next()
                            .unwrap_or("")
                            .trim()
                            .to_ascii_lowercase()
                    });
                    if name == "script" && script_type.as_deref() == Some("application/ld+json") {
                        ld_scripts.push(
                            node.children()
                                .filter_map(|c| c.value().as_text().map(|t| t.to_string()))
                                .collect(),
                        );
                    }
                    let in_chrome = parent_chrome
                        || keep.is_some_and(|keep| {
                            scraper::ElementRef::wrap(node).is_some_and(chrome)
                                && !node.descendants().any(|d| d.id() == keep)
                        });
                    let skipped = parent_skipped
                        || not_page_text(name)
                        || (in_chrome
                            && (element_hidden(element)
                                || scraper::ElementRef::wrap(node).is_some_and(chrome_label)));
                    let hidden = parent_hidden || element_hidden(element);
                    let label =
                        parent_label || scraper::ElementRef::wrap(node).is_some_and(skipped_label);
                    let aside =
                        parent_aside || scraper::ElementRef::wrap(node).is_some_and(aside_region);
                    let mut cell = false;
                    if !skipped {
                        if !INLINE_TAGS.contains(&name) {
                            stream.boundary();
                            block_tags.push(name.to_string());
                            stream.block_tag = name.to_string();
                            stream.aside = aside;
                        }
                        if matches!(name, "td" | "th") && !in_chrome {
                            let spans: Vec<String> = ["rowspan", "colspan"]
                                .iter()
                                .filter_map(|a| {
                                    let n: u32 = element.attr(a)?.trim().parse().ok()?;
                                    (n > 1).then(|| format!("{}{n}", &a[..1]))
                                })
                                .collect();
                            let multi =
                                scraper::ElementRef::wrap(node).is_some_and(multi_block_cell);
                            if multi || !spans.is_empty() {
                                stream.mark(CELL_OPEN);
                                for span in spans {
                                    for c in span.chars() {
                                        stream.emit(c);
                                    }
                                }
                                cell = true;
                            }
                        }
                        let disabled = element.attr("disabled").is_some()
                            || element.attr("aria-disabled") == Some("true");
                        let selected = element.attr("aria-selected") == Some("true");
                        let current = element.attr("aria-current").is_some_and(|c| c != "false");
                        for (on, code) in [(disabled, 'd'), (selected, 's'), (current, 'c')] {
                            if on && !in_chrome {
                                stream.mark(STATE);
                                stream.emit(code);
                            }
                        }
                        let input_value = (name == "input"
                            && element.attr("type").is_some_and(|t| {
                                matches!(
                                    t.trim().to_ascii_lowercase().as_str(),
                                    "submit" | "button" | "reset"
                                )
                            }))
                        .then(|| element.attr("value"))
                        .flatten();
                        for value in WORDING_ATTRS
                            .iter()
                            .filter_map(|attr| element.attr(attr))
                            .chain(input_value)
                            .filter(|v| !in_chrome && !v.trim().is_empty())
                        {
                            if hidden && !marked {
                                stream.mark(HIDDEN_OPEN);
                                marked = true;
                            }
                            stream.mark(ATTRIBUTE);
                            stream.gap = false;
                            stream.feed(value, label);
                            stream.boundary();
                        }
                    }
                    stack.push(Open {
                        skipped,
                        hidden,
                        label,
                        aside,
                        cell,
                        chrome: in_chrome,
                    });
                }
                scraper::Node::Text(text) => {
                    let Some(open) = stack.last() else {
                        continue;
                    };
                    if open.skipped {
                        continue;
                    }
                    if text.trim().is_empty() {
                        stream.run.push(' ');
                        continue;
                    }
                    let (hidden, label) = (open.hidden, open.label);
                    if hidden && !marked {
                        stream.mark(HIDDEN_OPEN);
                        marked = true;
                    }
                    stream.feed(text, label);
                }
                _ => {}
            },
            Edge::Close(node) => {
                let Some(element) = node.value().as_element() else {
                    continue;
                };
                let Some(open) = stack.pop() else {
                    continue;
                };
                if open.skipped {
                    continue;
                }
                if !INLINE_TAGS.contains(&element.name()) {
                    stream.boundary();
                    block_tags.pop();
                    stream.block_tag = block_tags.last().cloned().unwrap_or_default();
                    stream.aside = stack.last().is_some_and(|p| p.aside);
                }
                if open.cell {
                    stream.mark(CELL_CLOSE);
                }
                let parent_hidden = stack.last().is_some_and(|p| p.hidden);
                if open.hidden && !parent_hidden && marked {
                    stream.mark(HIDDEN_CLOSE);
                    marked = false;
                }
            }
        }
    }
    stream.boundary();
    let digest = {
        use sha2::Digest;
        format!(
            "{DIGEST_VERSION}{:x}",
            std::mem::take(&mut stream.hasher).finalize()
        )
    };
    PageRead {
        digest,
        blocks: stream.blocks,
        stated: Vec::new(),
        scripts: ld_scripts,
    }
}

/// The digest's version prefix. A digest of another version was made with
/// other folds: it is no baseline (the fact is adopted again as on first
/// sight while its adoption window is open, otherwise hidden).
const DIGEST_VERSION: &str = "v2:";

/// Blocks read page-wide: not in a footer, an aside or a sidebar, and not a
/// notice about terms or privacy (`2026년 11월 1일부터 개정 약관 적용`).
fn page_wide(block: &Block) -> bool {
    let text: String = block.segments.iter().map(|(t, _)| t.as_str()).collect();
    !block.aside && (!about_terms(&text) || names_offer_ending(&text))
}

/// Text that names an offer or its event with an ending or a date: never
/// skipped as a terms notice (`약관 개정에 따라 학생 할인 혜택이
/// 종료되었습니다`).
fn names_offer_ending(text: &str) -> bool {
    let folded = fold_str(text);
    OFFER_SUBJECT.is_match(&folded)
        && (KO_ENDING_MARKERS.iter().any(|m| folded.contains(m)) || !date_tokens(text).is_empty())
}

/// The identity of the offer `quote` states: the headings above its
/// first holder.
fn quote_offer(view: &PageView, quote: &str) -> Option<OfferIdentity> {
    let fragment = split_fragments(quote).into_iter().next()?;
    let i = holders(view, &fold_str(&fragment)).into_iter().next()?;
    Some(OfferIdentity::new(
        view.elements[i].headings.iter().map(String::as_str),
    ))
}

/// An aside block with a sentence whose subject names `offer` and that
/// states an ending ([`names_offer_ending`]).
fn aside_names_offer_ending(block: &Block, offer: Option<&OfferIdentity>) -> bool {
    let Some(offer) = offer else {
        return false;
    };
    let text: String = block.segments.iter().map(|(t, _)| t.as_str()).collect();
    sentences_of(&squash(&text))
        .iter()
        .any(|sentence| names_this_offer(sentence, offer) && names_offer_ending(sentence))
}

/// Text about terms or privacy.
fn about_terms(text: &str) -> bool {
    let folded = fold_str(text);
    ["약관", "개인정보", "처리방침", "이용정책"]
        .iter()
        .any(|m| folded.contains(m))
        || ["terms", "privacy"]
            .iter()
            .any(|m| whole_word(&text.to_lowercase(), m))
}

/// Why the page as a whole says the offer is not on, read on every run
/// (and by fresh extraction before it proposes anything), first seen today.
pub fn page_blocked(view: &PageView, quote: &str) -> Option<&'static str> {
    let clock = view.clock(Some(view.today), true);
    quote_section_reason(view, quote, &clock).or_else(|| page_blocked_at(view, quote, &clock))
}

/// The page shows what its structured data says: an ending clause in the
/// main content, or the structured end day written on the page.
fn visibly_ended(view: &PageView, end: Option<NaiveDate>, clock: &Clock) -> bool {
    use chrono::Datelike;
    let blocks: Vec<String> = view
        .page
        .blocks
        .iter()
        .filter(|b| page_wide(b))
        .map(|b| {
            squash(
                &b.segments
                    .iter()
                    .map(|(t, _)| t.as_str())
                    .collect::<String>(),
            )
        })
        .collect();
    if blocks
        .iter()
        .any(|text| matches!(text_ending(text), Some(Ending::Past | Ending::Stated)))
    {
        return true;
    }
    let Some(day) = end else {
        return false;
    };
    let _ = clock;
    let forms = [
        format!("{}.{:02}.{:02}", day.year(), day.month(), day.day()),
        format!("{}-{:02}-{:02}", day.year(), day.month(), day.day()),
        format!("{}.{}.{}", day.year(), day.month(), day.day()),
        format!("{}년 {}월 {}일", day.year(), day.month(), day.day()),
    ];
    blocks
        .iter()
        .any(|text| forms.iter().any(|form| text.contains(form.as_str())))
}

/// [`page_blocked`] at a given clock (the fact's first sight).
fn page_blocked_at(view: &PageView, quote: &str, clock: &Clock) -> Option<&'static str> {
    // Trusted structured data counts only with a visible signal on the
    // page: a stale SEO `validThrough` or a past launch Event on an
    // evergreen page says nothing alone.
    if let Some(end) = view.structured_ended(clock) {
        if visibly_ended(view, end, clock) {
            return Some(PAGE_STRUCTURED_ENDED);
        }
        return Some(PAGE_STRUCTURED_OTHER);
    }
    // Any other item named as the page, or another key: not trusted.
    for stated in &view.page.stated {
        match *stated {
            Stated::Ended => return Some(PAGE_STRUCTURED_OTHER),
            Stated::End(date, time) if clock.passed(date, time) => {
                return Some(PAGE_STRUCTURED_OTHER)
            }
            Stated::Unavailable => return Some(PAGE_STRUCTURED_OTHER),
            Stated::Start(date) if date > clock.today => return Some(PAGE_STRUCTURED_OTHER),
            _ => {}
        }
    }
    // A confirmed fact: only a finished ending elsewhere that names it.
    if !clock.first_sight {
        return ended_by_name_elsewhere(view, quote).then_some(PAGE_SAYS_ENDED);
    }
    // Endings: every block of the main content and its banners and
    // notices, outside navigation and unselected tabs, clause by clause,
    // with the statement itself taken out. A label cell (`dt`, `th`) is
    // read with its value cell (`마감일 2026.12.31`).
    let statements: Vec<String> = split_fragments(quote).iter().map(|f| squash(f)).collect();
    // A footer, an aside or a sidebar block is read too when it names this
    // offer with an ending (review A-H1); another event's ending there says
    // nothing about it.
    let offer = quote_offer(view, quote);
    let blocks: Vec<&Block> = view
        .page
        .blocks
        .iter()
        .filter(|b| page_wide(b) || (b.aside && aside_names_offer_ending(b, offer.as_ref())))
        .collect();
    let mut index = 0;
    while index < blocks.len() {
        let take = |block: &Block, text: &mut String| {
            for (piece, skipped) in &block.segments {
                text.push_str(if *skipped { " | " } else { piece });
            }
        };
        let mut text = String::new();
        take(blocks[index], &mut text);
        if matches!(blocks[index].tag.as_str(), "dt" | "th")
            && blocks
                .get(index + 1)
                .is_some_and(|next| matches!(next.tag.as_str(), "dd" | "td"))
        {
            text.push(' ');
            index += 1;
            take(blocks[index], &mut text);
        }
        index += 1;
        let mut text = squash(&text);
        for statement in statements.iter().filter(|s| !s.is_empty()) {
            text = text.replace(statement.as_str(), " | ");
        }
        if matches!(text_ending(&text), Some(Ending::Past | Ending::Stated)) {
            return Some(PAGE_SAYS_ENDED);
        }
    }
    // Dates: the same blocks, labels included, raw and folded, sentence by
    // sentence without the ones that carry no deadline of an offer.
    let mut joined = String::new();
    for (index, block) in blocks.iter().enumerate() {
        if index > 0 {
            let pair = matches!(blocks[index - 1].tag.as_str(), "dt" | "th")
                && matches!(block.tag.as_str(), "dd" | "td");
            joined.push_str(if pair { " " } else { " | " });
        }
        for (piece, _) in &block.segments {
            joined.push_str(piece);
        }
    }
    let offer = split_fragments(quote)
        .into_iter()
        .next()
        .and_then(|fragment| holders(view, &fold_str(&fragment)).into_iter().next())
        .map(|i| OfferIdentity::new(view.elements[i].headings.iter().map(String::as_str)))
        .unwrap_or_default();
    let text = deadline_sentences(&joined, &offer);
    let folded = fold_str(&text);
    if has_past_end_at(&text, clock) || has_past_end_at(&folded, clock) {
        return Some(PAGE_DATE_PAST);
    }
    if has_future_start_at(&text, clock) || has_future_start_at(&folded, clock) {
        return Some(PAGE_NOT_STARTED);
    }
    None
}

/// On a page with several offers, an element outside the offer's own
/// section (and outside footers, asides, navigation and unselected tabs)
/// says, in a finished sentence whose subject names this offer
/// ([`names_this_offer`]), that it ended ([`OFFER_ENDED_PAST`],
/// [`OFFER_ENDED_STATEMENT`]: `학생 할인 이벤트가 종료되었습니다`, `This offer
/// has ended`). On a page with one offer the section is the page, judged
/// at confirmation.
fn ended_by_name_elsewhere(view: &PageView, quote: &str) -> bool {
    let Some(fragment) = split_fragments(quote).into_iter().next() else {
        return false;
    };
    let Some(i) = holders(view, &fold_str(&fragment)).into_iter().next() else {
        return false;
    };
    let section = offer_section(view, i);
    if section.value().name() == "body" {
        return false;
    }
    let offer = OfferIdentity::new(view.elements[i].headings.iter().map(String::as_str));
    let outside = |k: usize| {
        let e = view.element_ref(k);
        e.id() != section.id()
            && !e.ancestors().any(|a| a.id() == section.id())
            && !section.ancestors().any(|a| a.id() == e.id())
            && !std::iter::once(e)
                .chain(e.ancestors().filter_map(scraper::ElementRef::wrap))
                .any(|a| aside_region(a) || skipped_label(a) || view.texts.hidden(a))
    };
    (0..view.elements.len()).filter(|&k| outside(k)).any(|k| {
        sentences_of(&squash(&view.elements[k].text))
            .iter()
            .any(|sentence| {
                !NOT_A_STATEMENT.is_match(sentence)
                    && names_this_offer(sentence, &offer)
                    && (OFFER_ENDED_PAST.is_match(&fold_str(sentence))
                        || OFFER_ENDED_STATEMENT.is_match(sentence))
            })
    })
}

// Why a fact is hidden, as recorded in the check details. Each has a key
// ([`reason_key`]) used by the health metrics and by the `hide_only`
// allowlist.
/// Two independent signals in the offer's own section: its labelled own
/// period has passed, and the offer itself is said to have ended (or its
/// trusted structured data ended on that day).
pub const SECTION_ENDED_CONFIRMED: &str =
    "the offer's own period has passed and the offer is said to have ended";
/// The offer's own section says, in the past tense, that it ended.
pub const SECTION_ENDED_PAST: &str = "the offer's section says it ended";
/// An end date in the offer's own section has passed.
pub const SECTION_DATE_PAST: &str = "an end date in the offer's section has passed";
/// A passed date in the offer's own section that is not plainly its own
/// period (a phase, a shipping date, a cut-off, an unlabelled end).
pub const SECTION_DATE_OTHER: &str = "a date in the offer's section has passed";
/// The offer's own section states an ending (not in the past tense).
pub const SECTION_ENDED: &str = "the offer's section states an ending";
/// The offer's own section says it is not on yet.
pub const SECTION_NOT_STARTED: &str = "the offer's section says it has not started";
/// The offer's own section excludes or negates it.
pub const SECTION_EXCLUDED: &str = "the text around the quote excludes or negates it";
/// A clause of the main content says an offer ended.
pub const PAGE_SAYS_ENDED: &str = "the page says an offer ended or is not on yet";
/// An end date in the main content has passed.
pub const PAGE_DATE_PAST: &str = "the page states an end date that has passed";
/// A start date in the main content is still ahead.
pub const PAGE_NOT_STARTED: &str = "the page states a start date still ahead";
/// Trusted structured data ([`trusted_states`]): an end passed, sold out,
/// discontinued, cancelled.
pub const PAGE_STRUCTURED_ENDED: &str = "the page's structured data says the offer ended";
/// Structured data: a start ahead, out of stock, postponed, or an end
/// read from an item or key `structured_ended` does not trust.
pub const PAGE_STRUCTURED_OTHER: &str =
    "the page's structured data says the offer is not available now";
/// The offer's own section reads otherwise than at the last confirmation
/// (a sentence added, removed or reworded): held for fresh extraction,
/// which re-reads it and re-baselines the fact
/// ([`super::evidence::rebaseline_from_fresh`]).
pub const SECTION_CHANGED: &str = "the offer's section changed since the last confirmation";
/// A stored baseline (a section digest, a value window) was written by a
/// reader this build does not have: it can neither confirm nor refute the
/// page. Re-baselined on the same page bytes, otherwise held for fresh
/// extraction ([`super::evidence`]).
pub const BASELINE_UNREADABLE: &str = "the stored baseline was written by another reader version";

/// The key of a hide reason: `section_date_past`, `section_changed`, …
pub fn reason_key(reason: &str) -> Option<&'static str> {
    Some(match reason {
        SECTION_ENDED_CONFIRMED => "section_ended_confirmed",
        SECTION_ENDED_PAST => "section_ended_past",
        SECTION_DATE_PAST => "section_date_past",
        SECTION_DATE_OTHER => "section_date_other",
        SECTION_ENDED => "section_ended",
        SECTION_NOT_STARTED => "section_not_started",
        SECTION_EXCLUDED => "section_excluded",
        PAGE_SAYS_ENDED => "ending_scan",
        PAGE_DATE_PAST => "date_past",
        PAGE_NOT_STARTED => "not_started",
        PAGE_STRUCTURED_ENDED => "structured_ended",
        PAGE_STRUCTURED_OTHER => "structured_other",
        SECTION_CHANGED => "section_changed",
        BASELINE_UNREADABLE => "baseline_unreadable",
        _ => return None,
    })
}

/// Every reason with its key, for metrics.
pub const HIDE_REASONS: &[&str] = &[
    SECTION_ENDED_CONFIRMED,
    SECTION_ENDED_PAST,
    SECTION_DATE_PAST,
    SECTION_DATE_OTHER,
    SECTION_ENDED,
    SECTION_NOT_STARTED,
    SECTION_EXCLUDED,
    PAGE_SAYS_ENDED,
    PAGE_DATE_PAST,
    PAGE_NOT_STARTED,
    PAGE_STRUCTURED_ENDED,
    PAGE_STRUCTURED_OTHER,
    SECTION_CHANGED,
    BASELINE_UNREADABLE,
];

// ── elements, fingerprints, identities ────────────────────────────────────

/// What a fact's element looked like when it was last confirmed. Only
/// reports structure changes; [`Identity`] decides.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Fingerprint {
    pub tag: String,
    pub text: String,
    #[serde(default)]
    pub classes: Vec<String>,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub attr_keys: Vec<String>,
    #[serde(default)]
    pub path: Vec<String>,
    #[serde(default)]
    pub parent_tag: Option<String>,
    #[serde(default)]
    pub parent_text: String,
    #[serde(default)]
    pub siblings: Vec<String>,
    #[serde(default)]
    pub heading: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub position: usize,
    /// Identity of every element that held the quote when captured.
    #[serde(default)]
    pub identities: Vec<Identity>,
    /// Digest of the whole page's text when captured (see `read_page`).
    /// Telemetry, and the first sight's lifetime: never a reason to hide.
    #[serde(default)]
    pub page: String,
    /// The day the fact was first seen on this page: yearless and relative
    /// dates are read against it.
    #[serde(default)]
    pub seen: Option<NaiveDate>,
    /// SHA-256 of the page's bytes as read at this confirmation
    /// ([`PageView::bytes`]): the one reader-independent witness that the
    /// page is the page confirmed, so a reader of another version may
    /// re-baseline the fact on it ([`super::evidence`]). Absent on facts
    /// confirmed before it was stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<String>,
}

/// Which offer a statement belongs to, folded (digits kept).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Identity {
    /// The whole heading chain, nearest first.
    pub headings: Vec<String>,
    /// Structural labels: row and column headers (th, or the first td of a
    /// row), `dt`, block title, list-item lead, parent list item, image
    /// `alt`, `aria-label`.
    pub labels: Vec<String>,
    /// [`identity_sentence`] of the sentence holding the quote, when that
    /// sentence states exactly one value.
    #[serde(default)]
    pub sentence: Option<String>,
    /// The whole sentence holding the quote, folded.
    #[serde(default)]
    pub context: String,
    /// That sentence as written (whitespace squashed) with only its one
    /// value masked: a proposal needs it unchanged, so a value change that
    /// comes with a formatting change (`5% 적립` → `15 % 적립!`) is not
    /// attributed and goes to fresh extraction.
    #[serde(default)]
    pub raw_sentence: Option<String>,
    /// The holding element's whole text with its attribute texts (titles,
    /// alts), folded: text added next to the statement in the same element
    /// (a note, a badge, a tooltip condition) is another page.
    #[serde(default)]
    pub element: String,
    /// Digest of the text around the statement up to its section and the
    /// footnotes below it, order kept within each text (see
    /// `surroundings_of`). Unlisted wording still changes it.
    #[serde(default)]
    pub notes: String,
    /// Every unit-bearing value in that scope, sorted: a value changed far
    /// from the quote (a cap, another condition) changes it.
    #[serde(default)]
    pub values: Vec<String>,
    /// The same digest without the values: equal when only values moved
    /// (used to recognise a neighbour fact's own proven change).
    #[serde(default)]
    pub wording: String,
    /// How many elements in this place held the quote at capture.
    #[serde(default)]
    pub holders: usize,
    /// This place and masked sentence named exactly one statement on the
    /// page, so a changed value can be attributed to it.
    #[serde(default)]
    pub distinct: bool,
    /// [`section_digest`] of the offer's own section at capture. Empty for
    /// a fact captured before it existed (that fact's section is read from
    /// its next confirmation on).
    #[serde(default)]
    pub section: String,
    /// The sentences the offer's section said at this confirmation, as
    /// short hashes ([`section_said`]): the text judged then, which
    /// re-baselining does not read again ([`new_text_reason`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub said: Vec<String>,
}

/// Locators stored with a fact besides its quote. Never shown.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Anchors {
    #[serde(default)]
    pub fingerprint: Option<Fingerprint>,
    /// Quotes replaced by a live relocation or update, newest first (≤ 3).
    #[serde(default)]
    pub previous_quotes: Vec<String>,
    /// The first quote this fact was seeded with, kept once replaced so the
    /// audit seed never re-inserts it.
    #[serde(default)]
    pub origin_quote: Option<String>,
    /// The section that holds the fact ([`SECTION_CHANGED`]) since a pass
    /// first read it: once it has stood a day, the pass re-baselines the
    /// fact on it ([`super::evidence::fresh_anchors`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_section: Option<PendingSection>,
}

/// A changed section of a held fact, and when a pass first read it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingSection {
    pub digest: String,
    pub since: chrono::DateTime<chrono::Utc>,
}

/// One sentence of an element, with what identity checks need.
#[derive(Debug, Clone)]
struct Sentence {
    raw: String,
    folded: String,
    key: Option<String>,
    raw_key: Option<String>,
    signature: String,
}

impl Sentence {
    fn new(raw: String) -> Self {
        Self {
            folded: fold_str(&raw),
            key: identity_sentence(&raw),
            raw_key: raw_identity_sentence(&raw),
            signature: value_signature(&width_normalized(&raw)),
            raw,
        }
    }
}

/// The sentence as written (whitespace squashed) with its one unit-bearing
/// value's number masked, or `None` when it states zero or several values.
fn raw_identity_sentence(text: &str) -> Option<String> {
    let text = squash(text);
    let values: Vec<regex::Captures<'_>> = super::values::UNIT_VALUE.captures_iter(&text).collect();
    let [value] = values.as_slice() else {
        return None;
    };
    let number = value.get(1)?;
    Some(format!(
        "{}#{}",
        &text[..number.start()],
        &text[number.end()..]
    ))
}

impl Element {
    fn sentences(&self) -> &[Sentence] {
        self.sentences.get_or_init(|| {
            sentences(&self.text)
                .into_iter()
                .map(Sentence::new)
                .collect()
        })
    }
}

/// A visible element of a page. Labels, notes, scope and the fingerprint
/// need the document and are built on first use: most elements are never
/// a holder or a candidate.
#[derive(Debug)]
struct Element {
    node: ego_tree::NodeId,
    text: String,
    folded: String,
    sentences: std::cell::OnceCell<Vec<Sentence>>,
    ancestors: Vec<usize>,
    headings: Vec<String>,
    /// The outermost heading of the chain: the section root is the ancestor
    /// that holds it.
    outer_heading: Option<ego_tree::NodeId>,
    /// The element's text with its attribute texts, folded (identity).
    whole: std::cell::OnceCell<String>,
    labels: std::cell::OnceCell<Vec<String>>,
    surroundings: std::cell::OnceCell<(String, String, Vec<String>)>,
    /// Texts read for ending markers: the whole section and the footnotes
    /// below it.
    scope: std::cell::OnceCell<Vec<String>>,
    fingerprint: std::cell::OnceCell<Fingerprint>,
}

const CANDIDATE_TAGS: &[&str] = &[
    "p",
    "li",
    "td",
    "th",
    "dd",
    "dt",
    "span",
    "div",
    "section",
    "article",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "strong",
    "em",
    "b",
    "label",
    "a",
    "small",
    "figcaption",
    "blockquote",
    "caption",
    "summary",
    "main",
    "aside",
];

fn truncate(text: &str, chars: usize) -> String {
    text.chars().take(chars).collect()
}

fn squash(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Visible element texts (raw, folded, whether they carry a value),
/// computed once per element.
#[derive(Default)]
struct Texts {
    raw: std::cell::RefCell<HashMap<ego_tree::NodeId, String>>,
    folded: std::cell::RefCell<HashMap<ego_tree::NodeId, String>>,
    valued: std::cell::RefCell<HashMap<ego_tree::NodeId, bool>>,
    hidden: std::cell::RefCell<HashMap<ego_tree::NodeId, bool>>,
    attr_text: std::cell::RefCell<HashMap<ego_tree::NodeId, String>>,
}

impl Texts {
    fn of(&self, element: scraper::ElementRef<'_>) -> String {
        if let Some(text) = self.raw.borrow().get(&element.id()) {
            return text.clone();
        }
        let text = element_text(element);
        self.raw.borrow_mut().insert(element.id(), text.clone());
        text
    }

    fn folded(&self, element: scraper::ElementRef<'_>) -> String {
        if let Some(text) = self.folded.borrow().get(&element.id()) {
            return text.clone();
        }
        let text = fold_str(&self.of(element));
        self.folded.borrow_mut().insert(element.id(), text.clone());
        text
    }

    /// The element or one of its ancestors is hidden (cached top-down).
    fn hidden(&self, element: scraper::ElementRef<'_>) -> bool {
        if let Some(hidden) = self.hidden.borrow().get(&element.id()) {
            return *hidden;
        }
        let hidden = element_hidden(element.value())
            || element
                .parent()
                .and_then(scraper::ElementRef::wrap)
                .is_some_and(|parent| self.hidden(parent));
        self.hidden.borrow_mut().insert(element.id(), hidden);
        hidden
    }

    /// Visible attribute texts of the element and its descendants (`title`,
    /// `alt`, `aria-label`, tooltips): a condition or a value moved into a
    /// tooltip is a change to the statement.
    fn attrs(&self, element: scraper::ElementRef<'_>) -> String {
        if let Some(text) = self.attr_text.borrow().get(&element.id()) {
            return text.clone();
        }
        let text = std::iter::once(element)
            .chain(element.descendants().filter_map(scraper::ElementRef::wrap))
            .filter(|e| !self.hidden(*e))
            .flat_map(|e| {
                [
                    "title",
                    "alt",
                    "aria-label",
                    "data-tooltip",
                    "data-title",
                    "data-tip",
                ]
                .into_iter()
                .filter_map(move |a| e.value().attr(a).map(str::to_string))
            })
            .collect::<Vec<_>>()
            .join(" ");
        self.attr_text
            .borrow_mut()
            .insert(element.id(), text.clone());
        text
    }

    fn valued(&self, element: scraper::ElementRef<'_>) -> bool {
        if let Some(valued) = self.valued.borrow().get(&element.id()) {
            return *valued;
        }
        let valued = carries_value(&self.of(element));
        self.valued.borrow_mut().insert(element.id(), valued);
        valued
    }
}

/// Visible text of one element, skipping hidden descendants.
fn element_text(element: scraper::ElementRef<'_>) -> String {
    let mut text = String::new();
    'nodes: for node in element.descendants() {
        let Some(fragment) = node.value().as_text() else {
            continue;
        };
        for ancestor in node.ancestors() {
            if ancestor.id() == element.id() {
                break;
            }
            if ancestor.value().as_element().is_some_and(element_hidden) {
                continue 'nodes;
            }
        }
        text.push_str(fragment);
        text.push(' ');
        if text.len() > MAX_ELEMENT_CHARS * 8 {
            break;
        }
    }
    squash(&text)
}

fn heading_level(element: scraper::ElementRef<'_>) -> Option<usize> {
    let name = element.value().name();
    if let Some(level) = name.strip_prefix('h').and_then(|l| l.parse::<usize>().ok()) {
        return (1..=6).contains(&level).then_some(level);
    }
    (element.value().attr("role") == Some("heading")).then(|| {
        element
            .value()
            .attr("aria-level")
            .and_then(|l| l.parse().ok())
            .unwrap_or(2)
    })
}

fn visible(texts: &Texts, element: scraper::ElementRef<'_>) -> bool {
    !texts.hidden(element)
}

fn short(text: &str) -> Option<String> {
    let length = text.chars().count();
    (1..=MAX_LABEL_CHARS)
        .contains(&length)
        .then(|| fold_str(text))
}

fn short_label(texts: &Texts, element: scraper::ElementRef<'_>) -> Option<String> {
    short(&texts.of(element))
}

fn carries_value(text: &str) -> bool {
    !value_signature(&width_normalized(text)).is_empty()
}

fn classes(element: scraper::ElementRef<'_>) -> Vec<&str> {
    let mut classes: Vec<&str> = element.value().classes().collect();
    classes.sort_unstable();
    classes
}

/// `other` is another row of the same list as `element` (another tier or
/// another statement): the same tag and a value of its own, with the same
/// classes or more than a short label's text. A short value-less sibling
/// (`<span>1인</span>`) or a short title (`1개월 이용권`) is a label.
fn row_like(
    texts: &Texts,
    element: scraper::ElementRef<'_>,
    other: scraper::ElementRef<'_>,
) -> bool {
    element.value().name() == other.value().name()
        && texts.valued(other)
        && (classes(element) == classes(other)
            || texts.of(other).chars().count() > MAX_ROW_LABEL_CHARS)
}

fn cells(row: scraper::ElementRef<'_>) -> Vec<scraper::ElementRef<'_>> {
    row.children()
        .filter_map(scraper::ElementRef::wrap)
        .filter(|c| matches!(c.value().name(), "td" | "th"))
        .collect()
}

/// The header grid of a table: row and column headers of `cell`, with
/// `colspan`/`rowspan` and several header rows honoured. Column headers are
/// the th cells of header rows (a `thead` row, or a row made only of th)
/// above the cell's columns, outermost first; row headers are the th cells
/// and short value-less td descriptors to its left in its grid row,
/// including cells spanning down from rows above.
fn table_labels(texts: &Texts, cell: scraper::ElementRef<'_>) -> Vec<String> {
    let Some(table) = cell
        .ancestors()
        .filter_map(scraper::ElementRef::wrap)
        .find(|a| a.value().name() == "table")
    else {
        return Vec::new();
    };
    let rows: Vec<scraper::ElementRef<'_>> = table
        .descendants()
        .filter_map(scraper::ElementRef::wrap)
        .filter(|e| {
            e.value().name() == "tr"
                && e.ancestors()
                    .filter_map(scraper::ElementRef::wrap)
                    .find(|a| a.value().name() == "table")
                    .is_some_and(|t| t.id() == table.id())
        })
        .collect();
    let span = |c: scraper::ElementRef<'_>, attr: &str| {
        c.value()
            .attr(attr)
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(1)
            .clamp(1, 50)
    };
    // grid[row][column] = cell occupying it.
    let mut grid: Vec<Vec<Option<scraper::ElementRef<'_>>>> = vec![Vec::new(); rows.len()];
    let mut header_row = vec![false; rows.len()];
    for (r, row) in rows.iter().enumerate() {
        let row_cells = cells(*row);
        header_row[r] = row
            .ancestors()
            .filter_map(scraper::ElementRef::wrap)
            .any(|a| a.value().name() == "thead")
            || (!row_cells.is_empty() && row_cells.iter().all(|c| c.value().name() == "th"));
        let mut column = 0;
        for c in row_cells {
            while grid[r].get(column).is_some_and(Option::is_some) {
                column += 1;
            }
            for dr in 0..span(c, "rowspan") {
                if r + dr >= rows.len() {
                    break;
                }
                for dc in 0..span(c, "colspan") {
                    let line = &mut grid[r + dr];
                    if line.len() <= column + dc {
                        line.resize(column + dc + 1, None);
                    }
                    line[column + dc] = Some(c);
                }
            }
            column += span(c, "colspan");
        }
    }
    let mut position: Option<(usize, Vec<usize>)> = None;
    for (r, line) in grid.iter().enumerate() {
        let columns: Vec<usize> = (0..line.len())
            .filter(|&k| line[k].is_some_and(|c| c.id() == cell.id()))
            .collect();
        if !columns.is_empty() {
            position = Some((r, columns));
            break;
        }
    }
    let Some((row, columns)) = position else {
        return Vec::new();
    };
    let mut labels: Vec<String> = Vec::new();
    let push = |c: scraper::ElementRef<'_>, labels: &mut Vec<String>| {
        if let Some(label) = short_label(texts, c) {
            if !labels.contains(&label) {
                labels.push(label);
            }
        }
    };
    let first = columns[0];
    for k in 0..first {
        if let Some(c) = grid[row].get(k).copied().flatten() {
            let descriptor = c.value().name() == "th"
                || (texts.of(c).chars().count() <= MAX_ROW_LABEL_CHARS && !texts.valued(c));
            if descriptor {
                push(c, &mut labels);
            }
        }
    }
    for (r, line) in grid.iter().enumerate() {
        if !header_row[r] || r == row {
            continue;
        }
        for &k in &columns {
            if let Some(c) = line.get(k).copied().flatten() {
                if c.value().name() == "th" {
                    push(c, &mut labels);
                }
            }
        }
    }
    labels
}

/// Tier or block titles of `element`: climbing from the element towards the
/// section, the immediately preceding visible sibling at each level when it
/// is a short label (not a heading, not another row of the same list). An
/// image says it with its `alt`. A head that shows no text at all (an icon
/// badge) cannot be attributed: it yields the [`UNREADABLE_LABEL`] marker,
/// so tiers told apart only by icons collapse into one place and never
/// confirm or propose. Climbing stops at the first level whose previous
/// sibling is another row (the list of tiers) or a heading.
fn block_titles(texts: &Texts, element: scraper::ElementRef<'_>) -> Vec<String> {
    let mut labels = Vec::new();
    let mut level = element;
    for _ in 0..MAX_TITLE_LEVELS {
        let previous = level
            .prev_siblings()
            .filter_map(scraper::ElementRef::wrap)
            .find(|s| visible(texts, *s));
        match previous {
            Some(sibling) if heading_level(sibling).is_some() => break,
            Some(sibling) if row_like(texts, level, sibling) => break,
            Some(sibling) => {
                let alt = std::iter::once(sibling)
                    .chain(sibling.descendants().filter_map(scraper::ElementRef::wrap))
                    .find(|d| d.value().name() == "img")
                    .and_then(|img| img.value().attr("alt").and_then(short));
                let text = texts.of(sibling);
                if text.is_empty() && alt.is_none() {
                    labels.push(UNREADABLE_LABEL.to_string());
                } else if let Some(label) = short(&text).or(alt) {
                    labels.push(label);
                }
                break;
            }
            None => {}
        }
        let Some(parent) = level.parent().and_then(scraper::ElementRef::wrap) else {
            break;
        };
        if parent
            .children()
            .filter_map(scraper::ElementRef::wrap)
            .any(|c| heading_level(c).is_some())
        {
            break;
        }
        level = parent;
    }
    labels
}

/// Structural labels of an element (see [`Identity::labels`]).
fn labels_of(texts: &Texts, element: scraper::ElementRef<'_>) -> Vec<String> {
    let mut labels = Vec::new();
    let up_to = |name: &str, depth: usize| {
        std::iter::once(element)
            .chain(element.ancestors().filter_map(scraper::ElementRef::wrap))
            .take(depth)
            .find(|e| e.value().name() == name)
    };
    if let Some(cell) = up_to("td", 4) {
        labels.extend(table_labels(texts, cell));
    }
    if let Some(definition) = up_to("dd", 4) {
        let term = definition
            .prev_siblings()
            .filter_map(scraper::ElementRef::wrap)
            .find(|s| s.value().name() == "dt");
        labels.extend(term.and_then(|t| short_label(texts, t)));
    }
    labels.extend(block_titles(texts, element));
    if let Some(item) = up_to("li", 4) {
        let lead = item
            .children()
            .filter_map(scraper::ElementRef::wrap)
            // Not the element itself nor an ancestor of it; a child of the
            // element is its own lead (`<li><strong>1인</strong> …</li>`).
            .find(|c| {
                matches!(c.value().name(), "strong" | "b" | "em" | "span" | "label")
                    && c.id() != element.id()
                    && !element.ancestors().any(|a| a.id() == c.id())
                    && !c.descendants().any(|d| d.id() == element.id())
            });
        labels.extend(lead.and_then(|l| short_label(texts, l)));
        // A nested list: the parent item's own text is the tier name.
        let parent_item = item
            .ancestors()
            .filter_map(scraper::ElementRef::wrap)
            .skip_while(|a| !matches!(a.value().name(), "ul" | "ol"))
            .nth(1)
            .filter(|a| a.value().name() == "li");
        if let Some(parent_item) = parent_item {
            let own: String = parent_item
                .children()
                .filter_map(|c| c.value().as_text().map(|t| t.to_string()))
                .collect();
            labels.extend(short(&squash(&own)));
        }
    }
    // An accessible group name.
    let named = std::iter::once(element)
        .chain(element.ancestors().filter_map(scraper::ElementRef::wrap))
        .take(4)
        .find_map(|e| e.value().attr("aria-label"));
    labels.extend(named.and_then(short));
    // Each label once, first source first (a row header can also be the
    // cell's previous sibling).
    let mut seen = HashSet::new();
    labels.retain(|label| seen.insert(label.clone()));
    labels
}

/// The section's footnotes: footnote-like blocks after it (`※ …`, `* …`,
/// a notes or terms block) up to the next block that holds a heading, and
/// any footnote its text points to by a numbered marker (`할인※1` and a
/// `※1 …` list after the next section), wherever it sits.
fn footnotes<'a>(texts: &Texts, root: scraper::ElementRef<'a>) -> Vec<scraper::ElementRef<'a>> {
    let mut found = adjacent_footnotes(texts, root);
    // The whole page holds every footnote already; a section without a
    // numbered marker points to none.
    if matches!(root.value().name(), "body" | "html") {
        return found;
    }
    let markers = numbered_markers(&texts.of(root));
    if markers.is_empty() {
        return found;
    }
    let page = root
        .ancestors()
        .filter_map(scraper::ElementRef::wrap)
        .last()
        .unwrap_or(root);
    // One pass over the text nodes: a footnote is the block whose text
    // starts with one of the markers.
    for node in page.descendants() {
        let Some(text) = node.value().as_text() else {
            continue;
        };
        let lead = text.trim_start();
        let marked = markers.iter().any(|marker| {
            lead.strip_prefix(marker.as_str())
                .is_some_and(|rest| !rest.starts_with(|c: char| c.is_ascii_digit()))
        });
        if !marked {
            continue;
        }
        let Some(block) = node
            .ancestors()
            .filter_map(scraper::ElementRef::wrap)
            .find(|a| !INLINE_TAGS.contains(&a.value().name()))
        else {
            continue;
        };
        let inside_root = block.id() == root.id() || block.ancestors().any(|a| a.id() == root.id());
        let outside_content = std::iter::once(block)
            .chain(block.ancestors().filter_map(scraper::ElementRef::wrap))
            .any(aside_region);
        if inside_root
            || outside_content
            || !visible(texts, block)
            || found.iter().any(|f| f.id() == block.id())
        {
            continue;
        }
        found.push(block);
    }
    found
}

/// The numbered footnote markers a text uses (`할인※1` gives `※1`).
fn numbered_markers(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut markers: Vec<String> = Vec::new();
    for (at, &c) in chars.iter().enumerate() {
        if !matches!(c, '※' | '*' | '＊') {
            continue;
        }
        let digits: String = chars[at + 1..]
            .iter()
            .take_while(|d| d.is_ascii_digit())
            .collect();
        if !digits.is_empty() {
            let marker = format!("{c}{digits}");
            if !markers.contains(&marker) {
                markers.push(marker);
            }
        }
    }
    markers
}

fn adjacent_footnotes<'a>(
    texts: &Texts,
    root: scraper::ElementRef<'a>,
) -> Vec<scraper::ElementRef<'a>> {
    let mut found = Vec::new();
    let mut node = root;
    loop {
        for sibling in node.next_siblings().filter_map(scraper::ElementRef::wrap) {
            if !visible(texts, sibling) {
                continue;
            }
            let has_heading = std::iter::once(sibling)
                .chain(sibling.descendants().filter_map(scraper::ElementRef::wrap))
                .any(|d| heading_level(d).is_some());
            if has_heading {
                return found;
            }
            // A footer, an aside or a sidebar is the site's, never a
            // footnote of the section.
            if sibling.value().name() == "footer" || aside_region(sibling) {
                continue;
            }
            let text = texts.of(sibling);
            let lead = text.trim_start();
            let marked = lead.starts_with(['※', '*', '＊', '†', '‡', '¹', '²', '³'])
                || lead.starts_with("주)")
                || lead.starts_with("[주]")
                || lead.starts_with("유의사항");
            let classed = sibling.value().classes().any(|class| {
                let class = class.to_ascii_lowercase();
                ["note", "caution", "notice", "terms", "guide", "disclaimer"]
                    .iter()
                    .any(|h| class.contains(h))
            });
            if marked || classed {
                found.push(sibling);
            }
        }
        match node.parent().and_then(scraper::ElementRef::wrap) {
            Some(parent) if !matches!(parent.value().name(), "body" | "html") => node = parent,
            _ => return found,
        }
    }
}

/// One text as the surroundings read it: folded, its visible attribute
/// texts appended, every unit-bearing value masked as `#` in place (order
/// kept) and collected into `values`.
fn surrounding_entry(
    texts: &Texts,
    node: scraper::ElementRef<'_>,
    values: &mut Vec<String>,
) -> String {
    let text = width_normalized(&format!("{} {}", texts.of(node), texts.attrs(node)));
    for found in super::values::UNIT_VALUE.captures_iter(&text) {
        values.push(format!(
            "{}{}",
            found[1].replace(',', ""),
            found[2].to_lowercase().replace(' ', "")
        ));
    }
    let masked = super::values::UNIT_VALUE.replace_all(&text, "#");
    fold_str(&masked).replace(super::evidence::DIGIT_GAP, " ")
}

/// What surrounds the statement, for "is this still the same page around
/// it": per level from the element's parent up to its section (`chain`),
/// the full texts of the other children in document order (headings and
/// hidden parts left out, values masked in place), then the footnotes
/// below the section. Each level's entries are sorted, so a reorder of
/// whole sibling blocks (tiers) keeps it; within a text the order of words
/// and digits is kept, so swapping 대학생/대학원생, online/store or `1인당
/// 3매`/`3인당 1매` changes it. Levels with no other child (pure wrappers)
/// are skipped, so adding wrappers keeps it. Returns the digest and the
/// sorted multiset of every value in scope (a cap changed far from the
/// quote changes it).
fn surroundings_of(
    texts: &Texts,
    element: scraper::ElementRef<'_>,
    chain: &[scraper::ElementRef<'_>],
    notes_below: &[scraper::ElementRef<'_>],
) -> (String, String, Vec<String>) {
    let mut values = Vec::new();
    let mut levels: Vec<String> = Vec::new();
    let mut level_of =
        |nodes: Vec<scraper::ElementRef<'_>>, loose: String, values: &mut Vec<String>| {
            let mut entries: Vec<String> = nodes
                .into_iter()
                .filter(|n| visible(texts, *n) && heading_level(*n).is_none())
                .map(|n| surrounding_entry(texts, n, values))
                .filter(|e| !e.trim().is_empty())
                .collect();
            if !loose.trim().is_empty() {
                entries.push(loose);
            }
            if !entries.is_empty() {
                entries.sort();
                levels.push(entries.join("\u{1}"));
            }
        };
    if chain.is_empty() {
        let siblings: Vec<scraper::ElementRef<'_>> = element
            .prev_siblings()
            .chain(element.next_siblings())
            .filter_map(scraper::ElementRef::wrap)
            .collect();
        level_of(siblings, String::new(), &mut values);
    }
    let mut inner = element;
    for &ancestor in chain {
        let others: Vec<scraper::ElementRef<'_>> = ancestor
            .children()
            .filter_map(scraper::ElementRef::wrap)
            .filter(|c| c.id() != inner.id())
            .collect();
        let loose: String = ancestor
            .children()
            .filter_map(|c| c.value().as_text().map(|t| t.to_string()))
            .collect();
        let loose = fold_str(&loose);
        level_of(others, loose, &mut values);
        inner = ancestor;
    }
    level_of(notes_below.to_vec(), String::new(), &mut values);
    values.sort();
    use sha2::{Digest, Sha256};
    let hash = |text: String| -> String {
        Sha256::digest(text.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    };
    let wording = hash(levels.join("\u{2}"));
    // The digest covers the values too, so it alone tells any change; the
    // wording digest and the multiset are kept beside it to recognise a
    // neighbour fact's own proven change.
    levels.push(values.join(","));
    (hash(levels.join("\u{2}")), wording, values)
}

/// A parsed page in the form resolution needs. Keeps the document for the
/// per-element parts built on first use, so it stays on the thread that
/// built it (the evidence job builds and drops it on a blocking thread).
pub struct PageView {
    pub evidence: PageEvidence,
    page: PageRead,
    document: Html,
    texts: Texts,
    elements: Vec<Element>,
    /// Today in KST, for end dates.
    today: NaiveDate,
    /// The moment the page is read (KST): the time of day on an end day.
    now: chrono::NaiveDateTime,
    /// What `structured_ended` trusts ([`trusted_states`]); set again by
    /// [`Self::with_url`] once the page's URL is known.
    trusted: Vec<Stated>,
    /// SHA-256 (hex) of the page as given, before any reading.
    bytes: String,
}

impl PageView {
    /// `visible` is `evidence::visible_text(html)`.
    /// Read today at the end of the day: any time stated for today has
    /// passed (no wall clock is read). The evidence job uses [`Self::at`].
    pub fn new(html: &str, visible: &str) -> Self {
        let today = super::health::kst_today();
        Self::at(
            html,
            visible,
            today.and_hms_opt(23, 59, 59).expect("valid time"),
        )
    }

    /// Read at `now` (KST).
    pub fn at(html: &str, visible: &str, now: chrono::NaiveDateTime) -> Self {
        let document = Html::parse_document(&super::evidence::strip_comments(html));
        let texts = Texts::default();
        let elements = collect_elements(&document, &texts);
        let page = read_page(&document);
        let trusted = trusted_states(&document, &page.scripts, None);
        let bytes = {
            use sha2::Digest;
            sha2::Sha256::digest(html.as_bytes())
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect()
        };
        Self {
            bytes,
            evidence: PageEvidence::from_visible_text(visible),
            page,
            document,
            texts,
            elements,
            today: now.date(),
            now,
            trusted,
        }
    }

    /// The page as read at `url`: an offer or event whose `url` is this
    /// page is trusted structured data too.
    /// SHA-256 (hex) of the page's bytes: identical only for the same page.
    pub fn bytes(&self) -> &str {
        &self.bytes
    }

    pub fn with_url(mut self, url: &str) -> Self {
        self.trusted = trusted_states(&self.document, &self.page.scripts, Some(url));
        self
    }

    /// The trusted structured end ([`trusted_states`]) that has passed:
    /// `Some(None)` for a status (sold out, discontinued, cancelled),
    /// `Some(Some(day))` for an end day.
    fn structured_ended(&self, clock: &Clock) -> Option<Option<NaiveDate>> {
        self.trusted.iter().find_map(|stated| match *stated {
            Stated::Ended => Some(None),
            Stated::End(date, time) if clock.passed(date, time) => Some(Some(date)),
            _ => None,
        })
    }

    /// Digest of the whole page's text.
    pub fn page_digest(&self) -> &str {
        &self.page.digest
    }

    /// The clock this page is read at: its day, the time of day (now on
    /// the current KST day, the end of an earlier day, the start of a later
    /// one), and the fact's first sight.
    pub fn clock(&self, seen: Option<NaiveDate>, first_sight: bool) -> Clock {
        let now = self.now;
        let time = match self.today.cmp(&now.date()) {
            std::cmp::Ordering::Less => {
                chrono::NaiveTime::from_hms_opt(23, 59, 59).expect("valid time")
            }
            std::cmp::Ordering::Equal => now.time(),
            std::cmp::Ordering::Greater => chrono::NaiveTime::MIN,
        };
        Clock {
            today: self.today,
            time,
            seen,
            first_sight,
        }
    }

    fn element_ref(&self, i: usize) -> scraper::ElementRef<'_> {
        self.document
            .tree
            .get(self.elements[i].node)
            .and_then(scraper::ElementRef::wrap)
            .expect("collected element is in the document")
    }

    fn labels(&self, i: usize) -> &[String] {
        self.elements[i]
            .labels
            .get_or_init(|| labels_of(&self.texts, self.element_ref(i)))
    }

    /// The ancestors from the parent up to the section root: the ancestor
    /// that holds the outermost heading of the chain (body when there is
    /// none). No depth cap: nine wrapper levels still reach the section.
    fn chain(&self, i: usize) -> Vec<scraper::ElementRef<'_>> {
        let heading = self.elements[i]
            .outer_heading
            .and_then(|node| self.document.tree.get(node))
            .and_then(scraper::ElementRef::wrap);
        let mut chain = Vec::new();
        for ancestor in self
            .element_ref(i)
            .ancestors()
            .filter_map(scraper::ElementRef::wrap)
        {
            if matches!(ancestor.value().name(), "body" | "html") {
                break;
            }
            chain.push(ancestor);
            if heading.is_some_and(|h| h.ancestors().any(|a| a.id() == ancestor.id())) {
                break;
            }
        }
        chain
    }

    /// The section read around the statement: the ancestor that holds its
    /// heading. With no heading it is the nearest block container, never
    /// the whole page.
    fn root(&self, i: usize) -> scraper::ElementRef<'_> {
        let chain = self.chain(i);
        let element = self.element_ref(i);
        if self.elements[i].outer_heading.is_none() {
            return chain
                .iter()
                .copied()
                .find(|a| !INLINE_TAGS.contains(&a.value().name()))
                .unwrap_or(element);
        }
        chain.last().copied().unwrap_or(element)
    }

    fn surroundings(&self, i: usize) -> &(String, String, Vec<String>) {
        self.elements[i].surroundings.get_or_init(|| {
            let chain = self.chain(i);
            let top = chain.last().copied().unwrap_or_else(|| self.element_ref(i));
            let below = footnotes(&self.texts, top);
            surroundings_of(&self.texts, self.element_ref(i), &chain, &below)
        })
    }

    fn notes(&self, i: usize) -> &str {
        &self.surroundings(i).0
    }

    fn wording(&self, i: usize) -> &str {
        &self.surroundings(i).1
    }

    fn scope_values(&self, i: usize) -> &[String] {
        &self.surroundings(i).2
    }

    /// The element's folded text with its attribute texts.
    fn whole(&self, i: usize) -> &str {
        self.elements[i].whole.get_or_init(|| {
            let attrs = self.texts.attrs(self.element_ref(i));
            if attrs.trim().is_empty() {
                self.elements[i].folded.clone()
            } else {
                format!("{}|{}", self.elements[i].folded, fold_str(&attrs))
            }
        })
    }

    /// Read for ending markers: the whole section (never cut) and the
    /// footnotes below it.
    fn scope(&self, i: usize) -> &[String] {
        self.elements[i].scope.get_or_init(|| {
            let root = self.root(i);
            let keep = self.elements[i].node;
            std::iter::once(section_text(root, keep, false))
                .chain(
                    footnotes(&self.texts, root)
                        .into_iter()
                        .map(|f| section_text(f, keep, false)),
                )
                .collect()
        })
    }

    fn fingerprint(&self, i: usize) -> &Fingerprint {
        self.elements[i].fingerprint.get_or_init(|| {
            let element = self.element_ref(i);
            let parent = element.parent().and_then(scraper::ElementRef::wrap);
            let siblings: Vec<scraper::ElementRef<'_>> = parent
                .map(|p| p.children().filter_map(scraper::ElementRef::wrap).collect())
                .unwrap_or_default();
            let mut path: Vec<String> = element
                .ancestors()
                .filter_map(scraper::ElementRef::wrap)
                .take(8)
                .map(|a| a.value().name().to_string())
                .collect();
            path.reverse();
            let mut attr_keys: Vec<String> = element
                .value()
                .attrs()
                .map(|(k, _)| k.to_string())
                .filter(|k| !matches!(k.as_str(), "class" | "id" | "style"))
                .collect();
            attr_keys.sort();
            Fingerprint {
                tag: element.value().name().to_string(),
                text: truncate(&self.elements[i].text, MAX_FINGERPRINT_TEXT),
                classes: element.value().classes().map(str::to_string).collect(),
                id: element.value().id().map(str::to_string),
                attr_keys,
                path,
                parent_tag: parent.map(|p| p.value().name().to_string()),
                parent_text: parent
                    .map(|p| truncate(&self.texts.of(p), 200))
                    .unwrap_or_default(),
                siblings: siblings
                    .iter()
                    .take(12)
                    .map(|s| s.value().name().to_string())
                    .collect(),
                heading: self.elements[i].headings.first().cloned(),
                label: self.labels(i).first().cloned(),
                position: siblings
                    .iter()
                    .position(|s| s.id() == element.id())
                    .unwrap_or(0),
                identities: Vec::new(),
                page: String::new(),
                seen: None,
                bytes: None,
            }
        })
    }

    fn same_place(&self, i: usize, headings: &[String], labels: &[String]) -> bool {
        self.elements[i].headings.as_slice() == headings && self.labels(i) == labels
    }
}

fn collect_elements(document: &Html, texts: &Texts) -> Vec<Element> {
    let mut index_of = HashMap::new();
    let mut elements = Vec::new();
    let mut stack: Vec<(usize, String, ego_tree::NodeId)> = Vec::new();
    for node in document.root_element().descendants() {
        let Some(element) = scraper::ElementRef::wrap(node) else {
            continue;
        };
        if !visible(texts, element) {
            continue;
        }
        let level = heading_level(element);
        let name = element.value().name();
        if !CANDIDATE_TAGS.contains(&name) && level.is_none() {
            continue;
        }
        let text = texts.of(element);
        let chars = text.chars().count();
        let headings: Vec<String> = stack.iter().rev().map(|(_, h, _)| h.clone()).collect();
        // The section is the one of the nearest heading that holds or
        // precedes the element (a logo `h1` in the header is no section).
        let outer_heading = stack.last().map(|(_, _, node)| *node);
        // The header of an article or section is its own (`<article>
        // <header><h2>`), not the site's.
        let site_chrome = element
            .ancestors()
            .filter_map(scraper::ElementRef::wrap)
            .any(|a| {
                let sectioned = || {
                    a.ancestors()
                        .filter_map(scraper::ElementRef::wrap)
                        .any(|b| matches!(b.value().name(), "article" | "section"))
                };
                (a.value().name() == "header" && !sectioned())
                    || a.value().name() == "nav"
                    || matches!(
                        a.value().attr("role").map(str::trim),
                        Some("banner" | "navigation")
                    )
            });
        if let Some(level) = level.filter(|_| !site_chrome) {
            while stack.last().is_some_and(|(l, _, _)| *l >= level) {
                stack.pop();
            }
            if chars > 0 {
                stack.push((level, fold_str(&truncate(&text, 200)), element.id()));
            }
        }
        if !(2..=MAX_ELEMENT_CHARS).contains(&chars) {
            continue;
        }
        let ancestors = element
            .ancestors()
            .filter_map(|a| index_of.get(&a.id()).copied())
            .collect();
        index_of.insert(element.id(), elements.len());
        elements.push(Element {
            node: element.id(),
            folded: texts.folded(element),
            sentences: std::cell::OnceCell::new(),
            text,
            ancestors,
            headings,
            outer_heading,
            whole: std::cell::OnceCell::new(),
            labels: std::cell::OnceCell::new(),
            surroundings: std::cell::OnceCell::new(),
            scope: std::cell::OnceCell::new(),
            fingerprint: std::cell::OnceCell::new(),
        });
    }
    elements
}

/// `needle` occurs in `hay` (both folded) not glued to a longer number:
/// `5off` is not in `15off`, `3000원` is not in `13000원`.
fn find_bounded(hay: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    let starts_digit = needle.starts_with(|c: char| c.is_ascii_digit());
    let ends_digit = needle.ends_with(|c: char| c.is_ascii_digit());
    hay.match_indices(needle).map(|(at, _)| at).find(|&at| {
        // Glued: a digit, or a `.`/`,` with a digit on its other side (a
        // decimal or thousands separator, not a sentence end).
        let mut before = hay[..at].chars().rev();
        let glued_before = match before.next() {
            Some(c) if c.is_ascii_digit() => true,
            Some('.' | ',') => before.next().is_some_and(|c| c.is_ascii_digit()),
            _ => false,
        };
        let mut after = hay[at + needle.len()..].chars();
        let glued_after = match after.next() {
            Some(c) if c.is_ascii_digit() => true,
            Some('.' | ',') => after.next().is_some_and(|c| c.is_ascii_digit()),
            _ => false,
        };
        !((starts_digit && glued_before) || (ends_digit && glued_after))
    })
}

fn contains_bounded(hay: &str, needle: &str) -> bool {
    find_bounded(hay, needle).is_some()
}

/// The smallest visible elements whose folded text holds `needle`.
fn holders(view: &PageView, needle: &str) -> Vec<usize> {
    if needle.is_empty() {
        return Vec::new();
    }
    let matching: Vec<usize> = (0..view.elements.len())
        .filter(|&i| contains_bounded(&view.elements[i].folded, needle))
        .collect();
    matching
        .iter()
        .copied()
        .filter(|&i| {
            !matching
                .iter()
                .any(|&j| j != i && view.elements[j].ancestors.contains(&i))
        })
        .collect()
}

/// The sentence of `element` that holds `needle` (folded), or none.
fn holding_sentence<'e>(element: &'e Element, needle: &str) -> Option<&'e Sentence> {
    element
        .sentences()
        .iter()
        .find(|s| contains_bounded(&s.folded, needle))
}

/// How many smallest elements in this place hold a sentence with this key.
fn statements_with_key(
    view: &PageView,
    headings: &[String],
    labels: &[String],
    key: &str,
) -> usize {
    let matching: Vec<usize> = (0..view.elements.len())
        .filter(|&i| {
            view.same_place(i, headings, labels)
                && view.elements[i]
                    .sentences()
                    .iter()
                    .any(|s| s.key.as_deref() == Some(key))
        })
        .collect();
    matching
        .iter()
        .filter(|&&i| {
            !matching
                .iter()
                .any(|&j| j != i && view.elements[j].ancestors.contains(&i))
        })
        .count()
}

/// Fingerprint of the first element holding the quote's first fragment,
/// with the identity of every element that holds it, for storing after a
/// confirmation.
pub fn capture(view: &PageView, quote: &str) -> Option<Fingerprint> {
    let fragment = split_fragments(quote).into_iter().next()?;
    let needle = fold_str(&fragment);
    let found = holders(view, &needle);
    let Some(&first) = found.first() else {
        // Text that spans elements: no identity, but the page still binds
        // the next confirmation.
        return view.evidence.contains(&fragment).then(|| Fingerprint {
            page: view.page.digest.clone(),
            seen: Some(view.today),
            bytes: Some(view.bytes.clone()),
            ..Fingerprint::default()
        });
    };
    let mut identities: Vec<Identity> = Vec::new();
    for &i in &found {
        let element = &view.elements[i];
        let holding = holding_sentence(element, &needle);
        let context = holding.map_or_else(|| needle.clone(), |s| s.folded.clone());
        // Counted the way confirmation counts: same place, same sentence.
        let labels = view.labels(i);
        let holders_here = found
            .iter()
            .filter(|&&j| {
                view.same_place(j, &element.headings, labels)
                    && view.elements[j]
                        .sentences()
                        .iter()
                        .any(|s| s.folded == context)
            })
            .count();
        let key = holding.and_then(|s| s.key.clone());
        let place_named = !element.headings.is_empty() || !labels.is_empty();
        let distinct = place_named
            && holders_here == 1
            && key
                .as_deref()
                .is_some_and(|k| statements_with_key(view, &element.headings, labels, k) == 1);
        let identity = Identity {
            headings: element.headings.clone(),
            labels: labels.to_vec(),
            sentence: key,
            context,
            raw_sentence: holding.and_then(|s| s.raw_key.clone()),
            element: view.whole(i).to_string(),
            notes: view.notes(i).to_string(),
            values: view.scope_values(i).to_vec(),
            wording: view.wording(i).to_string(),
            holders: holders_here,
            distinct,
            section: section_digest(view, i),
            said: section_said(view, i),
        };
        if !identities.contains(&identity) {
            identities.push(identity);
        }
    }
    let mut fingerprint = view.fingerprint(first).clone();
    fingerprint.identities = identities;
    fingerprint.page = view.page.digest.clone();
    fingerprint.seen = Some(view.today);
    fingerprint.bytes = Some(view.bytes.clone());
    Some(fingerprint)
}

fn jaccard(a: &[String], b: &[String]) -> f64 {
    let a: HashSet<&String> = a.iter().collect();
    let b: HashSet<&String> = b.iter().collect();
    let union = a.union(&b).count();
    if union == 0 {
        1.0
    } else {
        a.intersection(&b).count() as f64 / union as f64
    }
}

/// Structural similarity of two fingerprints in [0, 1] (reporting only).
pub fn score(stored: &Fingerprint, candidate: &Fingerprint) -> f64 {
    let flag = |same: bool| f64::from(u8::from(same));
    let parts = [
        flag(stored.tag == candidate.tag),
        similarity(&stored.path.join("/"), &candidate.path.join("/")),
        flag(stored.parent_tag == candidate.parent_tag),
        similarity(&stored.siblings.join(","), &candidate.siblings.join(",")),
        jaccard(&stored.classes, &candidate.classes),
        flag(stored.id == candidate.id),
    ];
    parts.iter().sum::<f64>() / parts.len() as f64
}

/// Sentences of an element's text, each at most [`MAX_QUOTE_CHARS`].
fn sentences(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let chars: Vec<char> = text.chars().collect();
    for (index, &c) in chars.iter().enumerate() {
        current.push(c);
        let boundary = matches!(c, '!' | '?' | '。' | '•' | '·' | '|')
            || (c == '.' && chars.get(index + 1).is_none_or(|n| n.is_whitespace()));
        if boundary {
            out.push(
                current
                    .trim()
                    .trim_matches(['•', '·', '|'])
                    .trim()
                    .to_string(),
            );
            current.clear();
        }
    }
    out.push(current.trim().to_string());
    if !out.iter().any(|s| s == text) {
        out.push(text.to_string());
    }
    out.into_iter()
        .filter(|s| (2..=MAX_QUOTE_CHARS).contains(&s.chars().count()))
        .collect()
}

/// The verbatim visible text in `element` that folds to `needle`.
fn verbatim(element: &Element, needle: &str) -> Option<String> {
    let folded = fold(&element.text);
    let chars: String = folded.chars.iter().collect();
    let at = find_bounded(&chars, needle)?;
    let start = chars[..at].chars().count();
    let end = start + needle.chars().count();
    let source: Vec<char> = element.text.chars().collect();
    let from = *folded.source.get(start)?;
    let to = *folded.source.get(end - 1)? + 1;
    Some(source[from..to].iter().collect())
}

fn element_reason(
    view: &PageView,
    i: usize,
    statement: &str,
    clock: &Clock,
) -> Option<&'static str> {
    let mut pieces: Vec<&str> = vec![view.elements[i].text.as_str()];
    pieces.extend(view.scope(i).iter().map(String::as_str));
    let labels = view.labels(i).join(" | ");
    let own = [view.elements[i].text.as_str(), labels.as_str()];
    let root = view.root(i);
    let keep = view.elements[i].node;
    let readable: Vec<String> = std::iter::once(section_text(view.element_ref(i), keep, true))
        .chain(std::iter::once(section_text(root, keep, true)))
        .chain(
            footnotes(&view.texts, root)
                .into_iter()
                .map(|f| section_text(f, keep, true)),
        )
        .collect();
    let readable: Vec<&str> = readable.iter().map(String::as_str).collect();
    let period: Vec<String> = std::iter::once(period_text(root, keep))
        .chain(
            footnotes(&view.texts, root)
                .into_iter()
                .map(|f| period_text(f, keep)),
        )
        .collect();
    let period: Vec<&str> = period.iter().map(String::as_str).collect();
    if confirmed_end(view, i, clock) {
        return Some(SECTION_ENDED_CONFIRMED);
    }
    let offer = OfferIdentity::new(view.elements[i].headings.iter().map(String::as_str));
    section_reason(&offer, &own, &pieces, &period, &readable, statement, clock)
}

/// A whole label of the offer's own period (`이벤트 기간`, `기간`, `행사
/// 기간`, `신청 마감일`, `Offer period`): a phase, an early-bird window, a
/// maintenance window, a validity or a birth year is no such label.
static OWN_PERIOD_LABEL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:(?:이벤트|행사|프로모션|캠페인|혜택|할인|판매|신청|접수|참여|응모|모집|진행)\s*)?(?:기간|일정|마감\s*일?|종료\s*일)$|^(?:(?:offer|event|promotion|campaign|sale)\s+)?(?:period|dates?|end\s+date)$")
        .expect("valid own-period label")
});
/// A date with its year (`2026.10.13`, `2026-10-13`, `2026년 10월 13일`).
static FULL_DATE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(\d{4})\s*(?:[./-]|년)\s*(\d{1,2})\s*(?:[./-]|월)\s*(\d{1,2})\s*일?")
        .expect("valid full date")
});
/// The end of a range after its start (`~ 10.13`, `~ 2026.10.13`), with an
/// optional weekday and time between.
static RANGE_END: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*(?:\(\s*[^)]{1,3}\s*\))?\s*(?:\d{1,2}:\d{2})?\s*[~∼〜–—-]\s*(?:(\d{4})\s*(?:[./-]|년)\s*)?(\d{1,2})\s*(?:[./-]|월)\s*(\d{1,2})\s*일?")
        .expect("valid range end")
});
static TIME_AFTER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*(?:\(\s*[^)]{1,3}\s*\))?\s*(\d{1,2}):(\d{2})").expect("valid time")
});
/// A statement that the offer itself ended: its subject points at this
/// offer (`본 이벤트는 종료되었습니다`, `종료된 이벤트입니다`, `This offer
/// has ended`).
static OFFER_ENDED_STATEMENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:^|[\s\[(])(?:본|이번|해당|이)\s*(?:이벤트|행사|프로모션|캠페인|혜택|프로그램|기획전|모집|판매)\s*(?:은|는|이|가)?\s*[^.?!|]{0,30}?(?:종료|마감)\s*(?:되었습니다|되었어요|됐습니다|됐어요|하였습니다|했습니다|되었음|됨)|(?:종료|마감)된\s*(?:이벤트|행사|프로모션|캠페인|기획전)입니다|\bthis\s+(?:offer|promotion|event|campaign|deal|sale)\s+(?:has\s+)?(?:now\s+)?(?:ended|expired|closed)\b")
        .expect("valid offer-ended statement")
});
/// A question, a condition or a worked example: no current statement.
static NOT_A_STATEMENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\?|^\s*q\s*[.:)]|경우|라면|다면|\bif\b|\bwhen\b|예\s*\)|예시|예를\s*들")
        .expect("valid not-a-statement pattern")
});

/// The passed-or-not end of a labelled own period: the label is a whole
/// [`OWN_PERIOD_LABEL`]; the value is one range with explicit years
/// (`2026.09.01 ~ 2026.10.13`, the end's year may be left out) or, after a
/// deadline label or with `까지`, one date. Anything else (two ranges, a
/// date without a year, a condition) is no own period.
fn own_period_end(label: &str, value: &str) -> Option<(NaiveDate, Option<chrono::NaiveTime>)> {
    let label = squash(label);
    let label = label.trim_matches(|c: char| !c.is_alphanumeric());
    if !OWN_PERIOD_LABEL.is_match(label) {
        return None;
    }
    let value = squash(value);
    if NOT_A_DEADLINE.is_match(&value) || NOT_OWN_PERIOD.is_match(&value) {
        return None;
    }
    let dates: Vec<regex::Captures<'_>> = FULL_DATE.captures_iter(&value).collect();
    let first = dates.first()?;
    let year: i32 = first[1].parse().ok()?;
    let start = NaiveDate::from_ymd_opt(year, first[2].parse().ok()?, first[3].parse().ok()?)?;
    let after = &value[first.get(0)?.end()..];
    let time_of = |rest: &str| {
        TIME_AFTER.captures(rest).and_then(|t| {
            chrono::NaiveTime::from_hms_opt(t[1].parse().ok()?, t[2].parse().ok()?, 0)
        })
    };
    if let Some(range) = RANGE_END.captures(after) {
        let end_year = match range.get(1) {
            Some(y) => y.as_str().parse().ok()?,
            None => year,
        };
        let end =
            NaiveDate::from_ymd_opt(end_year, range[2].parse().ok()?, range[3].parse().ok()?)?;
        let rest = &after[range.get(0)?.end()..];
        // One range only, ending after it starts.
        if end < start || FULL_DATE.is_match(rest) {
            return None;
        }
        return Some((end, time_of(rest)));
    }
    let deadline = label.contains("마감") || label.contains("종료") || after.contains("까지");
    (deadline && dates.len() == 1).then(|| (start, time_of(after)))
}

/// The offer goes on after a part of it ended.
static CONTINUING: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)상시|연중|계속\s*(?:제공|진행|적용|이용|운영)|지속|연장|\d+\s*차|진행(?:됩니다|합니다|\s*중)|\bcontinues?\b|year-round|ongoing|\bextended\b")
        .expect("valid continuing pattern")
});

/// Whether `element` may carry a signal for the statement held by `keep`
/// in `root`: visible, not in a footer, aside, navigation or unselected
/// tab, not a link or linked list item to another offer, and not inside a
/// sibling card (a list item, article, section or card-classed block whose
/// sibling of the same kind holds the statement).
fn signal_place(
    texts: &Texts,
    root: scraper::ElementRef<'_>,
    element: scraper::ElementRef<'_>,
    keep: ego_tree::NodeId,
) -> bool {
    if texts.hidden(element) {
        return false;
    }
    let holds = |e: scraper::ElementRef<'_>| e.descendants().any(|d| d.id() == keep);
    for a in
        std::iter::once(element).chain(element.ancestors().filter_map(scraper::ElementRef::wrap))
    {
        if a.id() == root.id() {
            return true;
        }
        if holds(a) {
            continue;
        }
        let name = a.value().name();
        let linked = name == "a"
            || (name == "li"
                && a.descendants()
                    .any(|d| d.value().as_element().is_some_and(|e| e.name() == "a")));
        if aside_region(a) || skipped_label(a) || linked {
            return false;
        }
        // A card: a list item, an article or section, or a block classed
        // as a card or item.
        let card = matches!(name, "li" | "article" | "section")
            || a.value().classes().any(|c| {
                let c = c.to_ascii_lowercase();
                c.contains("card") || c.contains("item")
            });
        // A box of its own (a sub-event, a banner): a container that does
        // not hold the statement and has two or more blocks of text ends
        // something else.
        let boxed = a.id() != element.id()
            && matches!(
                name,
                "div" | "section" | "article" | "aside" | "li" | "figure"
            )
            && a.children()
                .filter_map(scraper::ElementRef::wrap)
                .filter(|c| !c.text().collect::<String>().trim().is_empty())
                .count()
                >= 2;
        if boxed {
            return false;
        }
        let sibling_card = card
            && a.prev_siblings()
                .chain(a.next_siblings())
                .filter_map(scraper::ElementRef::wrap)
                .any(|s| s.value().name() == name && holds(s));
        if sibling_card {
            return false;
        }
    }
    true
}

/// Two independent signals that the offer ended, both in its own section:
/// exactly one labelled own period whose end has passed (a `dt`/`dd` or
/// `th`/`td` pair, or `기간: …`), and either a statement that the offer
/// itself ended or the trusted structured end ([`trusted_states`]) on that
/// day (or a sold-out, discontinued or cancelled status). The one section
/// reason `hide_only` trusts by default (`section_ended_confirmed`).
fn confirmed_end(view: &PageView, i: usize, clock: &Clock) -> bool {
    let root = view.root(i);
    let keep = view.elements[i].node;
    let mut periods: Vec<(NaiveDate, Option<chrono::NaiveTime>)> = Vec::new();
    let mut ended = false;
    for element in root.descendants().filter_map(scraper::ElementRef::wrap) {
        if !signal_place(&view.texts, root, element, keep) {
            continue;
        }
        let name = element.value().name();
        let value_cell = match name {
            "dt" | "th" => element
                .next_siblings()
                .filter_map(scraper::ElementRef::wrap)
                .next()
                .filter(|n| matches!((name, n.value().name()), ("dt", "dd") | ("th", "td"))),
            _ => None,
        };
        if let Some(cell) = value_cell {
            if signal_place(&view.texts, root, cell, keep) {
                periods.extend(own_period_end(
                    &view.texts.of(element),
                    &view.texts.of(cell),
                ));
            }
            continue;
        }
        let leaf = !INLINE_TAGS.contains(&name)
            && !element
                .children()
                .filter_map(scraper::ElementRef::wrap)
                .any(|c| {
                    !INLINE_TAGS.contains(&c.value().name())
                        && !c.text().collect::<String>().trim().is_empty()
                });
        if !leaf {
            continue;
        }
        let text = squash(&view.texts.of(element));
        for sentence in sentences_of(&text) {
            if let Some((label, value)) = sentence.split_once([':', '：']) {
                periods.extend(own_period_end(label, value));
            }
            if OFFER_ENDED_STATEMENT.is_match(sentence) && !NOT_A_STATEMENT.is_match(sentence) {
                ended = true;
            }
        }
    }
    periods.dedup();
    let [(end, time)] = periods.as_slice() else {
        return false;
    };
    if !clock.passed(*end, *time) {
        return false;
    }
    // A continuing offer wins: an extension, a next phase, an always-on
    // discount (`성원에 힘입어 연장`, `학생 할인은 상시 적용됩니다`, `Student
    // pricing continues`).
    if sentences_of(&section_text(root, keep, true))
        .iter()
        .any(|sentence| CONTINUING.is_match(sentence))
    {
        return false;
    }
    ended
        || match view.structured_ended(clock) {
            Some(None) => true,
            Some(Some(day)) => day == *end,
            None => false,
        }
}

/// The section reason of the first element holding each fragment.
fn quote_section_reason(view: &PageView, quote: &str, clock: &Clock) -> Option<&'static str> {
    split_fragments(quote).iter().find_map(|fragment| {
        holders(view, &fold_str(fragment))
            .into_iter()
            .find_map(|i| element_reason(view, i, fragment, clock))
    })
}

// ── resolution ────────────────────────────────────────────────────────────

/// One stored fact as the resolver needs it.
#[derive(Debug, Clone)]
pub struct StoredFact<'a> {
    pub quote: &'a str,
    pub baseline: Option<&'a str>,
    pub ages: Ages,
    pub adoption_open: bool,
    pub anchors: &'a Anchors,
}

/// A value change seen on the page; never applied from one sighting.
#[derive(Debug, Clone, PartialEq)]
pub struct Proposal {
    /// Verbatim visible sentence carrying the new value.
    pub new_text: String,
    pub old_signature: String,
    pub new_signature: String,
    pub new_ages: Option<(Option<i32>, Option<i32>)>,
    pub extractors_agree: bool,
    pub score: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Resolution {
    /// The exact-anchor decision (confirmed, or a failure other than a
    /// missing quote).
    Exact(FactOutcome),
    /// Confirmed by the normalized anchor (formatting changed).
    Fuzzy {
        window_signature: String,
    },
    /// Confirmed, and the element's structure changed (a redesign moved it);
    /// `quote` is the verbatim text found.
    Relocated {
        quote: String,
        window_signature: String,
        score: f64,
    },
    Proposed(Proposal),
    Ambiguous(String),
}

impl Resolution {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Exact(outcome) => outcome.label(),
            Self::Fuzzy { .. } => "confirmed_fuzzy",
            Self::Relocated { .. } => "relocated",
            Self::Proposed(_) => "update_proposed",
            Self::Ambiguous(_) => "ambiguous",
        }
    }

    pub fn confirms(&self) -> bool {
        matches!(
            self,
            Self::Exact(FactOutcome::Confirmed { .. })
                | Self::Fuzzy { .. }
                | Self::Relocated { .. }
        )
    }
}

/// Every value of the old neighbourhood is still in the new one, counted
/// (three `3000원` before and two after is a dropped value). The new one
/// may hold more.
pub fn window_covers(baseline: &str, current: &str) -> bool {
    let count = |signature: &str| {
        let mut counts: HashMap<String, usize> = HashMap::new();
        for token in signature.split(['|', ',']).filter(|t| !t.is_empty()) {
            *counts.entry(token.to_string()).or_default() += 1;
        }
        counts
    };
    let (old, new) = (count(baseline), count(current));
    old.iter()
        .all(|(token, n)| new.get(token).copied().unwrap_or(0) >= *n)
}

/// Value signature of the page neighbourhood around every bounded
/// occurrence of `text` (width-normalized), built like
/// `PageEvidence::window_signature` (one part per occurrence), so it
/// compares with an exact-path baseline.
fn window_of(view: &PageView, text: &str) -> Option<String> {
    let page = width_normalized(view.evidence.text());
    let needle = width_normalized(&normalized_compact(text));
    let chars: Vec<char> = page.chars().collect();
    let mut parts = Vec::new();
    let mut from = 0;
    while let Some(offset) = find_bounded(&page[from..], &needle) {
        let at = from + offset;
        let start = page[..at].chars().count();
        let end = start + needle.chars().count();
        let low = start.saturating_sub(WINDOW_RADIUS_CHARS);
        let high = (end + WINDOW_RADIUS_CHARS).min(chars.len());
        parts.push(value_signature(
            &chars[low..high].iter().collect::<String>(),
        ));
        from = at + needle.len().max(1);
        if parts.len() >= 20 {
            break;
        }
    }
    if parts.is_empty() {
        return None;
    }
    parts.sort();
    Some(parts.join("|"))
}

fn age_reading(text: &str) -> Option<(Option<i32>, Option<i32>)> {
    parse_age(text).ok().map(|r| (r.min, r.max))
}

/// Where each fragment of the quote is held. `Err` hides the fact with
/// the reason.
fn located(
    view: &PageView,
    fact: &StoredFact<'_>,
    clock: &Clock,
) -> Result<Vec<(String, usize)>, Resolution> {
    let missing = || Resolution::Exact(FactOutcome::QuoteMissing);
    let mut found = Vec::new();
    for fragment in split_fragments(fact.quote) {
        let needle = fold_str(&fragment);
        let held = holders(view, &needle);
        let Some(&first) = held.first() else {
            return Err(missing());
        };
        if contradicted(view, &held, &needle) {
            return Err(Resolution::Ambiguous(
                "the same offer also states another value on the page".into(),
            ));
        }
        for &i in &held {
            if let Some(reason) = element_reason(view, i, &fragment, clock) {
                return Err(Resolution::Ambiguous(reason.into()));
            }
        }
        let text = verbatim(&view.elements[first], &needle).unwrap_or_else(|| fragment.clone());
        if value_signature(&width_normalized(&text)) != value_signature(&fragment) {
            return Err(missing());
        }
        if let Some(stated) = age_reading(&fragment) {
            if age_reading(&width_normalized(&text)) != Some(stated)
                || stated != (fact.ages.min, fact.ages.max)
            {
                return Err(Resolution::Ambiguous("the page reads another age".into()));
            }
        }
        found.push((text, first));
    }
    Ok(found)
}

/// Another visible sentence in the same place (same headings and labels)
/// reads exactly like the sentence holding the quote except for its value:
/// the page contradicts itself about this offer.
fn contradicted(view: &PageView, held: &[usize], needle: &str) -> bool {
    held.iter().any(|&h| {
        let holder = &view.elements[h];
        let Some(own) = holding_sentence(holder, needle) else {
            return false;
        };
        let Some(key) = own.key.as_deref() else {
            return false;
        };
        let labels = view.labels(h);
        (0..view.elements.len()).any(|i| {
            view.same_place(i, &holder.headings, labels)
                && view.elements[i]
                    .sentences()
                    .iter()
                    .any(|s| s.key.as_deref() == Some(key) && s.signature != own.signature)
        })
    })
}

fn structure_score(view: &PageView, fact: &StoredFact<'_>, element: usize) -> f64 {
    fact.anchors
        .fingerprint
        .as_ref()
        .map_or(1.0, |stored| score(stored, view.fingerprint(element)))
}

/// A fact's place on the current page when everything but its surroundings
/// matches (place, sentence, holder count, element text, clean context, no
/// contradiction), with what those surroundings now are. Used to tell a
/// fact whose page changed only by another fact's value from one whose page
/// changed in any other way.
#[derive(Debug, Clone, PartialEq)]
pub struct Neighbour {
    pub wording: String,
    pub values: Vec<String>,
    pub window: Option<String>,
    pub fingerprint: Option<Fingerprint>,
}

pub fn neighbour(view: &PageView, fact: &StoredFact<'_>) -> Option<Neighbour> {
    let fingerprint = fact.anchors.fingerprint.as_ref()?;
    // Only on an unchanged page: a page changed in any way is re-derived by
    // fresh extraction, never followed from another fact's change.
    if fingerprint.page != view.page.digest {
        return None;
    }
    let [identity] = fingerprint.identities.as_slice() else {
        return None;
    };
    let fragments = split_fragments(fact.quote);
    let [fragment] = fragments.as_slice() else {
        return None;
    };
    let needle = fold_str(fragment);
    let held = holders(view, &needle);
    let here: Vec<usize> = held
        .iter()
        .copied()
        .filter(|&i| {
            view.same_place(i, &identity.headings, &identity.labels)
                && view.elements[i]
                    .sentences()
                    .iter()
                    .any(|s| s.folded == identity.context)
        })
        .collect();
    if here.is_empty() || (identity.holders > 0 && here.len() != identity.holders) {
        return None;
    }
    let &found = here
        .iter()
        .find(|&&i| identity.element.is_empty() || view.whole(i) == identity.element)?;
    if contradicted(view, &held, &needle)
        || !held.iter().all(|&i| {
            element_reason(view, i, fragment, &view.clock(Some(view.today), true)).is_none()
        })
    {
        return None;
    }
    let text = verbatim(&view.elements[found], &needle).unwrap_or_else(|| fragment.clone());
    Some(Neighbour {
        wording: view.wording(found).to_string(),
        values: view.scope_values(found).to_vec(),
        window: window_of(view, &text),
        fingerprint: capture(view, fact.quote),
    })
}

/// The stored surroundings and the current ones differ exactly by one
/// value `old` becoming `new` (another fact's change), with the same
/// wording.
pub fn changed_only_by(stored: &Identity, current: &Neighbour, old: &str, new: &str) -> bool {
    if stored.wording.is_empty() || stored.wording != current.wording {
        return false;
    }
    let mut expected = stored.values.clone();
    let Some(at) = expected.iter().position(|v| v == old) else {
        return false;
    };
    expected[at] = new.to_string();
    expected.sort();
    expected == current.values
}

/// Resolves one fact against a page. Pure.
///
/// The offer's section and the page are read for an ending first. Then one
/// rule decides ([`current_on_evidence`]): a fact with a stored value
/// baseline is current while its quote is on the page and its value window
/// reads as that baseline, however the rest of the page changed; a fact
/// without one is adopted under its adoption window. The page digest is
/// telemetry ([`Fingerprint::page`]), never a reason to hide.
pub fn resolve(view: &PageView, fact: &StoredFact<'_>) -> Resolution {
    resolve_inner(view, fact)
}

fn resolve_inner(view: &PageView, fact: &StoredFact<'_>) -> Resolution {
    let stored = fact
        .anchors
        .fingerprint
        .as_ref()
        .filter(|f| f.page.starts_with(DIGEST_VERSION));
    // The first sight: stored with the fact, or today when this is it (a
    // new fact, or one adopted again).
    // The first sight: stored with the fact (also under an older digest
    // version), or today when this is it. A fact adopted before pages were
    // digested with no stored first sight has none: its yearless and
    // relative dates read as passed.
    let any_seen = fact.anchors.fingerprint.as_ref().and_then(|f| f.seen);
    let seen = match (stored, fact.baseline) {
        (Some(fingerprint), _) => fingerprint.seen,
        (None, None) => Some(view.today),
        (None, Some(_)) => any_seen,
    };
    // First sight is a fact never confirmed (no stored baseline), not one
    // whose anchors predate the page digest: a legacy fact is adopted once
    // on its own evidence and its section captured at that confirmation.
    let clock = view.clock(seen, fact.baseline.is_none());
    // The offer's own section first (its reasons are the most certain),
    // then the page as a whole.
    if let Some(reason) = quote_section_reason(view, fact.quote, &clock)
        .or_else(|| page_blocked_at(view, fact.quote, &clock))
    {
        return Resolution::Ambiguous(reason.into());
    }
    match fact.baseline {
        Some(_) => current_on_evidence(view, fact, &clock),
        None => locate(view, fact, &clock),
    }
}

/// A stored fact is current while the deterministic parser reads it on the
/// page as stored: every fragment of its quote verbatim under
/// [`super::evidence::canonical`], in the offer's own place
/// ([`in_its_place`]), and the value window around it equal to the stored
/// baseline. Anything else hides it with that reason (the quote
/// gone, the window or a field changed); a fact whose wording really
/// changed is re-derived by fresh extraction, never followed.
fn current_on_evidence(view: &PageView, fact: &StoredFact<'_>, clock: &Clock) -> Resolution {
    match evaluate_fact(&view.evidence, fact.quote, fact.baseline, fact.ages, false) {
        FactOutcome::Confirmed { .. } => match in_its_place(view, fact) {
            Ok(()) => locate(view, fact, clock),
            Err(hidden) => hidden,
        },
        outcome => Resolution::Exact(outcome),
    }
}

/// The quote still belongs to the same offer, in its own section. A fact
/// captured with its section ([`Identity::section`]) needs that section
/// to read as then ([`section_digest`]): layout folds away, while a
/// banner, a condition, a tooltip, a tab label, a tier gone, added or
/// swapped, or a rewording anywhere in it holds the fact for fresh
/// extraction ([`SECTION_CHANGED`]). A fact captured before sections were
/// read needs every place that held its quote (headings, labels, sentence)
/// to hold it still, as often (`QuoteMissing` otherwise); its section is
/// captured at its next confirmation.
fn in_its_place(view: &PageView, fact: &StoredFact<'_>) -> Result<(), Resolution> {
    let Some(fingerprint) = fact.anchors.fingerprint.as_ref() else {
        return Ok(());
    };
    let fragment = split_fragments(fact.quote)
        .into_iter()
        .next()
        .ok_or(Resolution::Exact(FactOutcome::QuoteMissing))?;
    let held = holders(view, &fold_str(&fragment));
    for identity in &fingerprint.identities {
        if !identity.section.is_empty() {
            match section_reads_as_stored(view, &held, &identity.section) {
                Ok(true) => {}
                Ok(false) => return Err(Resolution::Ambiguous(SECTION_CHANGED.into())),
                Err(()) => return Err(Resolution::Ambiguous(BASELINE_UNREADABLE.into())),
            }
            continue;
        }
        let here = held
            .iter()
            .filter(|&&i| {
                view.same_place(i, &identity.headings, &identity.labels)
                    && (identity.context.is_empty()
                        || view.elements[i]
                            .sentences()
                            .iter()
                            .any(|s| s.folded == identity.context))
            })
            .count();
        if here == 0 || (identity.holders > 0 && here != identity.holders) {
            return Err(Resolution::Exact(FactOutcome::QuoteMissing));
        }
    }
    Ok(())
}

/// The offer's own section, the one scope of "text bound to the offer"
/// for the current-fact rule and the ending scan. On a page that carries
/// one offer ([`single_offer`]) it is the whole page (`<body>`); on a page
/// with several, the nearest ancestor of the
/// element that holds the outermost heading of its heading chain, matched
/// by its text (`대학생 멤버십` over `혜택`, so its `지원 대상` subsection is in
/// it; an ARIA or a `<header>` heading the same), or the element's own
/// section ([`PageView::root`]) with no heading.
fn offer_section(view: &PageView, i: usize) -> scraper::ElementRef<'_> {
    let element = view.element_ref(i);
    let page = || {
        element
            .ancestors()
            .filter_map(scraper::ElementRef::wrap)
            .find(|a| a.value().name() == "body")
            .unwrap_or_else(|| view.root(i))
    };
    let Some(outer) = view.elements[i].headings.last() else {
        return if single_offer(view, i, None) {
            page()
        } else {
            view.root(i)
        };
    };
    let outer = fold_str(outer);
    let section = element
        .ancestors()
        .filter_map(scraper::ElementRef::wrap)
        .take_while(|a| !matches!(a.value().name(), "body" | "html"))
        .find(|a| {
            a.descendants()
                .filter_map(scraper::ElementRef::wrap)
                .any(|h| heading_level(h).is_some() && fold_str(&view.texts.of(h)) == outer)
        })
        .unwrap_or_else(|| view.root(i));
    if single_offer(view, i, Some(section)) {
        page()
    } else {
        section
    }
}

/// The page carries one offer: no element stating a value sits under a
/// heading outside the offer's `section` (with no section, under any
/// heading). An unheaded banner or hero line is no other offer.
fn single_offer(view: &PageView, i: usize, section: Option<scraper::ElementRef<'_>>) -> bool {
    let inside = |k: usize| {
        section.is_some_and(|s| {
            let e = view.element_ref(k);
            e.id() == s.id() || e.ancestors().any(|a| a.id() == s.id())
        })
    };
    (0..view.elements.len()).all(|k| {
        k == i
            || view.elements[k].headings.is_empty()
            || inside(k)
            || value_signature(&view.elements[k].text).is_empty()
    })
}

/// The version prefix of a section digest ([`section_digest`]). A stored
/// section of another version was read by another reader: it is compared
/// by that reader while it is kept ([`LEGACY_SECTION_VERSION`]), and is
/// otherwise unreadable to this one ([`BASELINE_UNREADABLE`]). `s4:` reads
/// page chrome as visible text (review A-H1); `s3:`, which skipped it, was
/// never released, so no stored section carries it.
pub const SECTION_VERSION: &str = "s4:";
/// The section digest before [`SECTION_VERSION`]: the page digest's prefix,
/// the section read with its chrome and its per-request tokens.
pub const LEGACY_SECTION_VERSION: &str = "v2:";

/// What the offer's section says, as a digest: the page digest's own
/// reading ([`read_tree`]) of the section and of the footnotes below it,
/// so markup, wrappers and layout fold away exactly as there, and a table
/// cell's span, a control's state, digit separators, a word or a mark
/// added, removed or changed (a banner, a condition, a tooltip, a tier
/// swapped) change it, hidden text included. Two things that frame every
/// page and state no offer are left out (production 2026-10-29 to 11-10,
/// offers held daily with an unchanged visible text): page chrome
/// ([`chrome`]: navigation, footers, asides) that does not hold the
/// statement, and per-request tokens ([`REQUEST_TOKEN`], Microsoft's hidden
/// trace id). On a page with one offer the section stays `<body>`: a
/// banner, a popup or a hero line outside the main content is the offer's
/// text too. Deliberately not handled: text elsewhere on a page with
/// several offers, which says nothing about this one.
fn section_digest(view: &PageView, i: usize) -> String {
    let keep = view.elements[i].node;
    let section = offer_section(view, i);
    std::iter::once(section)
        .chain(footnotes(&view.texts, section))
        .map(|part| {
            let digest = read_tree_without_chrome(*part, Some(keep)).digest;
            format!(
                "{SECTION_VERSION}{}",
                digest.strip_prefix(DIGEST_VERSION).unwrap_or(&digest)
            )
        })
        .collect::<Vec<_>>()
        .join("+")
}

/// [`section_digest`] as [`LEGACY_SECTION_VERSION`] read it, for a section
/// stored before [`SECTION_VERSION`]: a fact whose section still reads the
/// same to the reader that stored it is current, and its next confirmation
/// stores the current digest.
fn legacy_section_digest(view: &PageView, i: usize) -> String {
    let section = offer_section(view, i);
    std::iter::once(section)
        .chain(footnotes(&view.texts, section))
        .map(|root| read_tree(*root).digest)
        .collect::<Vec<_>>()
        .join("+")
}

/// Whether a stored section reads the same now, by the reader that stored
/// it. `Err` when no reader of this build reads its version.
fn section_reads_as_stored(view: &PageView, held: &[usize], stored: &str) -> Result<bool, ()> {
    let digest: fn(&PageView, usize) -> String = if stored.starts_with(SECTION_VERSION) {
        section_digest
    } else if stored.starts_with(LEGACY_SECTION_VERSION) {
        legacy_section_digest
    } else {
        return Err(());
    };
    Ok(held.iter().any(|&i| digest(view, i) == stored))
}

/// The sentences of the offer's section held by element `i` and of its
/// footnotes, as read for endings.
fn section_sentences(view: &PageView, i: usize) -> Vec<String> {
    let keep = view.elements[i].node;
    let section = offer_section(view, i);
    std::iter::once(section)
        .chain(footnotes(&view.texts, section))
        .flat_map(|root| {
            sentences_of(&section_text(root, keep, true))
                .into_iter()
                .map(str::trim)
                .filter(|s| !fold_str(s).is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .collect()
}

/// A sentence as [`Identity::said`] keeps it: twelve hex digits of the
/// hash of its folded text.
fn said_hash(sentence: &str) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(fold_str(sentence).as_bytes())[..6]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// What the offer's section says, sentence by sentence, at a confirmation.
fn section_said(view: &PageView, i: usize) -> Vec<String> {
    let mut said: Vec<String> = section_sentences(view, i)
        .iter()
        .map(|s| said_hash(s))
        .collect();
    said.sort();
    said.dedup();
    said
}

/// The current digest of the section that holds `fact`'s quote, for the
/// way back from a hold ([`Anchors::pending_section`]).
pub fn held_section(view: &PageView, fact: &StoredFact<'_>) -> Option<String> {
    let fragment = split_fragments(fact.quote).into_iter().next()?;
    let i = holders(view, &fold_str(&fragment)).into_iter().next()?;
    Some(section_digest(view, i))
}

/// Why the text added to a held fact's section since its last confirmation
/// keeps it from being re-baselined: an ending read in the new sentences
/// only ([`text_ending`]; the text judged then, [`Identity::said`], is not
/// read again). A fact confirmed before sentences were kept has no record
/// of what was judged: its page is read in full ([`page_blocked`]).
pub fn new_text_reason(view: &PageView, fact: &StoredFact<'_>) -> Option<&'static str> {
    let said: HashSet<&str> = fact
        .anchors
        .fingerprint
        .iter()
        .flat_map(|f| f.identities.iter())
        .flat_map(|identity| identity.said.iter().map(String::as_str))
        .collect();
    if said.is_empty() {
        return page_blocked(view, fact.quote);
    }
    let fragment = split_fragments(fact.quote).into_iter().next()?;
    let needle = fold_str(&fragment);
    let i = holders(view, &needle).into_iter().next()?;
    let new: Vec<String> = section_sentences(view, i)
        .into_iter()
        .filter(|s| !fold_str(s).contains(&needle) && !said.contains(said_hash(s).as_str()))
        .collect();
    match text_ending(&new.join(" | "))? {
        Ending::Past => Some(SECTION_ENDED_PAST),
        Ending::Stated => Some(SECTION_ENDED),
        Ending::NotStarted => Some(SECTION_NOT_STARTED),
    }
}

/// What the value-change matcher reads on a changed page. A shadow metric
/// only: nothing is proposed or applied from it (fresh extraction owns value
/// changes).
pub fn shadow_update(view: &PageView, fact: &StoredFact<'_>) -> Option<Proposal> {
    match propose(view, fact) {
        Resolution::Proposed(proposal) => Some(proposal),
        _ => None,
    }
}

/// Finds the fact on a page that reads as before (or on first sight) and
/// reports which anchor held it: exact, normalized (formatting) or moved.
fn locate(view: &PageView, fact: &StoredFact<'_>, clock: &Clock) -> Resolution {
    let exact = evaluate_fact(
        &view.evidence,
        fact.quote,
        fact.baseline,
        fact.ages,
        fact.adoption_open,
    );
    match &exact {
        FactOutcome::Confirmed {
            window_signature, ..
        } => {
            return match located(view, fact, clock) {
                Ok(found) => {
                    let moved = found
                        .iter()
                        .map(|(_, e)| structure_score(view, fact, *e))
                        .fold(1.0, f64::min);
                    if moved < RELOCATED_BELOW_SCORE && found.len() == 1 {
                        Resolution::Relocated {
                            quote: fact.quote.to_string(),
                            window_signature: window_signature.clone(),
                            score: moved,
                        }
                    } else {
                        Resolution::Exact(exact)
                    }
                }
                // Text present verbatim but no visible element holds it
                // (it spans blocks): the plain exact decision.
                Err(Resolution::Exact(FactOutcome::QuoteMissing))
                    if split_fragments(fact.quote)
                        .iter()
                        .all(|f| holders(view, &fold_str(f)).is_empty()) =>
                {
                    Resolution::Exact(exact)
                }
                Err(hidden) => hidden,
            };
        }
        FactOutcome::QuoteMissing => {}
        _ => return Resolution::Exact(exact),
    }
    let adopt_or_expire = |window: String, confirmed: Resolution| match fact.baseline {
        Some(baseline) if window_covers(baseline, &window) => confirmed,
        Some(_) => Resolution::Exact(FactOutcome::WindowChanged),
        None if fact.adoption_open => confirmed,
        None => Resolution::Exact(FactOutcome::AdoptionExpired),
    };
    // Normalized anchor: formatting changed, wording and values did not.
    match located(view, fact, clock) {
        Ok(found) => {
            let mut windows = Vec::new();
            for (text, _) in &found {
                let Some(window) = window_of(view, text) else {
                    return Resolution::Exact(FactOutcome::QuoteMissing);
                };
                windows.push(window);
            }
            windows.sort();
            let window = windows.join("|");
            let moved = found
                .iter()
                .map(|(_, e)| structure_score(view, fact, *e))
                .fold(1.0, f64::min);
            let confirmed = if moved < RELOCATED_BELOW_SCORE && found.len() == 1 {
                Resolution::Relocated {
                    quote: found[0].0.clone(),
                    window_signature: window.clone(),
                    score: moved,
                }
            } else {
                Resolution::Fuzzy {
                    window_signature: window.clone(),
                }
            };
            adopt_or_expire(window, confirmed)
        }
        Err(hidden) => hidden,
    }
}

/// A value change: the one place the fact was found, which named exactly
/// one statement, now holds the same sentence with only the fact's own
/// value different, in the same element text otherwise and with the same
/// notes around it.
fn propose(view: &PageView, fact: &StoredFact<'_>) -> Resolution {
    let missing = Resolution::Exact(FactOutcome::QuoteMissing);
    let Some(fingerprint) = fact.anchors.fingerprint.as_ref() else {
        return missing;
    };
    let [identity] = fingerprint.identities.as_slice() else {
        return missing;
    };
    let Some(wanted) = identity.sentence.as_deref() else {
        return missing;
    };
    if identity.holders != 1 || !identity.distinct || split_fragments(fact.quote).len() != 1 {
        return missing;
    }
    let old_signature = value_signature(fact.quote);
    let candidates: Vec<(usize, &Sentence)> = view
        .elements
        .iter()
        .enumerate()
        .filter(|(i, _)| view.same_place(*i, &identity.headings, &identity.labels))
        .flat_map(|(i, element)| {
            element
                .sentences()
                .iter()
                .filter(|s| {
                    s.key.as_deref() == Some(wanted)
                        && s.signature != old_signature
                        && s.raw_key.is_some()
                        && s.raw_key == identity.raw_sentence
                })
                .map(move |s| (i, s))
        })
        .collect();
    // The smallest element of each nested chain.
    let smallest: Vec<(usize, &Sentence)> = candidates
        .iter()
        .copied()
        .filter(|(i, _)| {
            !candidates
                .iter()
                .any(|(j, _)| j != i && view.elements[*j].ancestors.contains(i))
        })
        .collect();
    let signatures: HashSet<&str> = smallest.iter().map(|(_, s)| s.signature.as_str()).collect();
    if signatures.len() > 1 {
        return Resolution::Ambiguous("the same offer shows several new values".into());
    }
    let [(element, sentence)] = smallest.as_slice() else {
        return missing;
    };
    let index = *element;
    // Everything but the value is as it was: the element's other text and
    // the notes around it.
    // The new value must sit in the same kind of element (tag and classes)
    // that held the old one: a new price in another element beside a
    // struck old one is a different statement, re-extracted from scratch.
    let candidate = view.fingerprint(index);
    if candidate.tag != fingerprint.tag || candidate.classes != fingerprint.classes {
        return missing;
    }
    let rebuilt = view
        .whole(index)
        .replacen(&sentence.folded, &identity.context, 1);
    if (!identity.element.is_empty() && rebuilt != identity.element)
        || view.notes(index) != identity.notes
        || view.scope_values(index) != identity.values.as_slice()
    {
        return missing;
    }
    if !view.evidence.contains(&sentence.raw)
        || element_reason(
            view,
            index,
            &sentence.raw,
            &view.clock(Some(view.today), true),
        )
        .is_some()
    {
        return Resolution::Ambiguous(
            "the changed value is not clean verbatim text of this offer".into(),
        );
    }
    Resolution::Proposed(Proposal {
        extractors_agree: extractors_agree(&sentence.raw),
        new_signature: sentence.signature.clone(),
        new_ages: age_reading(&sentence.raw),
        old_signature,
        score: fact
            .anchors
            .fingerprint
            .as_ref()
            .map_or(1.0, |stored| score(stored, view.fingerprint(index))),
        new_text: sentence.raw.clone(),
    })
}

#[cfg(test)]
#[path = "resilience_tests.rs"]
mod tests;

// The reviewer's Part D probes (r1 of 0b087449, r2 of a4e076ee), kept
// verbatim as the must-hide corpus; `tests::partd_gate` runs and asserts.
#[cfg(test)]
#[rustfmt::skip]
#[allow(unused_imports, dead_code, clippy::all)]
#[path = "resilience_partd_probes.rs"]
mod probe_partd;

#[cfg(test)]
#[rustfmt::skip]
#[allow(unused_imports, dead_code, clippy::all)]
#[path = "resilience_r2_probes.rs"]
mod probe_r2;
