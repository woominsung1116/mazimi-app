//! What a notice states about the day it counts age on, read from the
//! source's own condition text at normalisation (review R-M1). Two readings,
//! each only when the text states it unambiguously of the applicant
//! (review B3):
//! - [`stated_age_basis`]: a reference day for 만 나이 ("2026.1.1. 기준",
//!   "12월 31일 현재", "연 나이", "신청일 기준"), stored as
//!   `programs.age_basis` and read by `rule_engine::AgeBasis::for_program`.
//!   A day counts only inside the applicant's age expression: the clause of
//!   "만 N세…" and a parenthetical right after it. A residence or income
//!   reference date elsewhere in the text states nothing, and a negated
//!   "연 나이" ("연 나이가 아닌", "연 나이 아님") states nothing;
//! - [`birth_range`]: eligibility stated as birth dates ("1991.1.1.~
//!   2007.12.31. 출생"), which is a rule on birth year, not on age. A range
//!   counts only when it agrees with the row's own age band in the notice
//!   year (first year = year − max − 1, last = year − min, ±1), so a child's
//!   birth range ("2025년 출생아 부모") is never an applicant rule.
//!
//! Two different conventions in one record state nothing.
use chrono::NaiveDate;
use mazimi_core::models::NoticeAgeBasis;
use regex::Regex;
use std::sync::OnceLock;

struct Patterns {
    full_day: Regex,
    month_day: Regex,
    year_age: Regex,
    negation: Regex,
    other_fact: Regex,
    application_day: Regex,
    age_expression: Regex,
    birth_range: Regex,
    birth_years: Regex,
}

fn patterns() -> &'static Patterns {
    static PATTERNS: OnceLock<Patterns> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        let compile = |p: &str| Regex::new(p).expect("static pattern compiles");
        const DAY: &str = r"(?P<y>\d{4})\s*[.\-년]\s*(?P<m>\d{1,2})\s*[.\-월]\s*(?P<d>\d{1,2})\s*[.일]?";
        Patterns {
            full_day: compile(&format!(r"{DAY}\s*(?:일\s*)?(?:현재|기준)")),
            month_day: compile(
                r"(?:^|[^0-9.])(?P<m>1|12)\s*[.월]\s*(?P<d>1|31)\s*[.일]?\s*(?:일\s*)?(?:현재|기준)",
            ),
            year_age: compile(r"연\s?나이"),
            negation: compile(r"않|아닌|아니|아님|안\s*함|제외"),
            other_fact: compile(r"거주|주민등록|주소|소득|재직"),
            application_day: compile(r"(?:신청|접수)\s*일\s*(?:현재|기준)"),
            age_expression: compile(
                r"만\s*나이|만\s*\d{1,2}\s*세(?:\s*[~∼\-]\s*(?:만\s*)?\d{1,2}\s*세)?|만\s*\d{1,2}\s*[~∼\-]\s*(?:만\s*)?\d{1,2}\s*세|연\s?나이",
            ),
            birth_range: compile(&format!(
                r"{}\s*~\s*{}\s*(?:사이\s*)?(?:에\s*)?출생",
                DAY.replace("?P<y>", "?P<y1>")
                    .replace("?P<m>", "?P<m1>")
                    .replace("?P<d>", "?P<d1>"),
                DAY.replace("?P<y>", "?P<y2>")
                    .replace("?P<m>", "?P<m2>")
                    .replace("?P<d>", "?P<d2>")
            )),
            birth_years: compile(
                r"(?P<y1>\d{4})\s*년?\s*~\s*(?P<y2>\d{4})\s*년\s*(?:생|출생)|(?P<a>\d{4})\s*년생\s*~\s*(?P<b>\d{4})\s*년생",
            ),
        }
    })
}

/// Where a clause ends: the reading never crosses one.
fn is_boundary(c: char) -> bool {
    matches!(c, ',' | '(' | ')' | '○' | '※' | '·' | ';' | '\n' | '/')
}

/// The clauses that state the applicant's age: each age expression with the
/// text around it up to the nearest boundary, plus a parenthetical that
/// opens right after it ("만 19세~34세 (2026. 1. 1. 기준)").
fn age_clauses(text: &str) -> Vec<&str> {
    let p = patterns();
    p.age_expression
        .find_iter(text)
        .map(|m| {
            let start = text[..m.start()]
                .char_indices()
                .rev()
                .find(|(_, c)| is_boundary(*c))
                .map_or(0, |(i, c)| i + c.len_utf8());
            let rest = &text[m.end()..];
            let lead = rest.len()
                - rest
                    .trim_start_matches(|c: char| c.is_whitespace())
                    .trim_start_matches(['이', '상', '하', '미', '만', '초', '과'])
                    .trim_start_matches(|c: char| c.is_whitespace())
                    .len();
            let end = if rest[lead..].starts_with('(') {
                let close = rest[lead..]
                    .find(')')
                    .map_or(text.len(), |i| m.end() + lead + i + 1);
                // A parenthetical about another fact ("2026.1.1. 기준 부산
                // 거주자") is not the age's (verify V-M2).
                if p.other_fact.is_match(&text[m.end() + lead..close]) {
                    m.end() + lead
                } else {
                    close
                }
            } else {
                rest.char_indices()
                    .find(|(_, c)| is_boundary(*c))
                    .map_or(text.len(), |(i, _)| m.end() + i)
            };
            &text[start..end]
        })
        .collect()
}

/// The reference day the texts state for counting age, if exactly one.
pub(crate) fn stated_age_basis(texts: &[&str]) -> Option<NoticeAgeBasis> {
    let p = patterns();
    let mut found = Vec::new();
    for text in texts {
        for clause in age_clauses(text) {
            for c in p.full_day.captures_iter(clause) {
                let num = |n: &str| c.name(n).and_then(|m| m.as_str().parse::<u32>().ok());
                let year = c.name("y").and_then(|m| m.as_str().parse::<i32>().ok());
                if let Some(day) =
                    year.and_then(|y| NaiveDate::from_ymd_opt(y, num("m")?, num("d")?))
                {
                    found.push(NoticeAgeBasis::On(day));
                }
            }
            for c in p.month_day.captures_iter(clause) {
                let Some(whole) = c.get(0) else { continue };
                // A full date's month-day part was read above.
                let preceded_by_year = clause[..whole.start()]
                    .trim_end_matches(|ch: char| ch.is_whitespace() || ch == '.' || ch == '년')
                    .ends_with(|ch: char| ch.is_ascii_digit());
                if preceded_by_year {
                    continue;
                }
                match (&c["m"], &c["d"]) {
                    ("1", "1") => found.push(NoticeAgeBasis::YearStart),
                    ("12", "31") => found.push(NoticeAgeBasis::YearEnd),
                    _ => {}
                }
            }
            if p.application_day.is_match(clause) {
                found.push(NoticeAgeBasis::Window);
            }
        }
        // A 연 나이 its sentence negates states nothing ("연 나이가 아닌",
        // "연 나이로 계산하지 않음", "연 나이 적용 안 함").
        let sentence = |at: usize, end: usize| {
            let start = text[..at].rfind(['.', '。', '\n']).map_or(0, |i| i + 1);
            let stop = text[end..]
                .find(['.', '。', '\n'])
                .map_or(text.len(), |i| end + i);
            &text[start..stop]
        };
        let year_age_stated = p
            .year_age
            .find_iter(text)
            .any(|m| !p.negation.is_match(sentence(m.start(), m.end())));
        if year_age_stated {
            found.push(NoticeAgeBasis::YearEnd);
        }
    }
    let mut distinct: Vec<NoticeAgeBasis> = Vec::new();
    for basis in found {
        if !distinct.contains(&basis) {
            distinct.push(basis);
        }
    }
    match distinct.as_slice() {
        [one] => Some(*one),
        _ => None,
    }
}

/// Eligibility stated as a range of birth days, first and last inclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BirthRange {
    pub first: NaiveDate,
    pub last: NaiveDate,
}

impl BirthRange {
    /// Whole calendar years: a birth year alone decides it.
    pub fn whole_years(&self) -> bool {
        use chrono::Datelike;
        (self.first.month(), self.first.day()) == (1, 1)
            && (self.last.month(), self.last.day()) == (12, 31)
    }
}

/// The birth range the texts state, if exactly one and it agrees with the
/// row's age band `[min, max]` in one of the notice's `years`.
pub(crate) fn birth_range(
    texts: &[&str],
    band: (Option<i32>, Option<i32>),
    years: &[i32],
) -> Option<BirthRange> {
    let (Some(min), Some(max)) = band else {
        return None;
    };
    let agrees = |range: &BirthRange| {
        use chrono::Datelike;
        years.iter().any(|year| {
            (range.first.year() - (year - max - 1)).abs() <= 1
                && (range.last.year() - (year - min)).abs() <= 1
        })
    };
    let p = patterns();
    let mut found = Vec::new();
    for text in texts {
        for c in p.birth_range.captures_iter(text) {
            let num = |n: &str| c.name(n).and_then(|m| m.as_str().parse::<u32>().ok());
            let year = |n: &str| c.name(n).and_then(|m| m.as_str().parse::<i32>().ok());
            let first = NaiveDate::from_ymd_opt(year("y1")?, num("m1")?, num("d1")?);
            let last = NaiveDate::from_ymd_opt(year("y2")?, num("m2")?, num("d2")?);
            if let (Some(first), Some(last)) = (first, last) {
                found.push(BirthRange { first, last });
            }
        }
        for c in p.birth_years.captures_iter(text) {
            let year = |a: &str, b: &str| {
                c.name(a)
                    .or_else(|| c.name(b))
                    .and_then(|m| m.as_str().parse::<i32>().ok())
            };
            let first = year("y1", "a").and_then(|y| NaiveDate::from_ymd_opt(y, 1, 1));
            let last = year("y2", "b").and_then(|y| NaiveDate::from_ymd_opt(y, 12, 31));
            if let (Some(first), Some(last)) = (first, last) {
                found.push(BirthRange { first, last });
            }
        }
    }
    found.dedup();
    match found.as_slice() {
        [one]
            if one.first <= one.last
                && (1900..=2030).contains(&chrono::Datelike::year(&one.first))
                && agrees(one) =>
        {
            Some(*one)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn a_stated_reference_day_is_read_only_inside_the_age_expression() {
        for (text, expected) in [
            (
                "만 19세~34세 (2026. 1. 1. 기준)",
                Some(NoticeAgeBasis::On(d(2026, 1, 1))),
            ),
            ("연령 기준: 공고일(2026.3.2.) 현재", None),
            (
                "나이는 2026.3.2. 기준 만 나이로 산정",
                Some(NoticeAgeBasis::On(d(2026, 3, 2))),
            ),
            (
                "만 39세 이하(1월 1일 기준)",
                Some(NoticeAgeBasis::YearStart),
            ),
            ("12월 31일 기준 만 34세 이하", Some(NoticeAgeBasis::YearEnd)),
            ("연 나이 기준 19세~34세", Some(NoticeAgeBasis::YearEnd)),
            ("신청일 기준 만 19세 이상", Some(NoticeAgeBasis::Window)),
            // A residence or income reference date is not an age basis.
            ("2026. 1. 1. 기준 부산시 거주", None),
            ("소득은 2025.12.31. 기준으로 산정합니다", None),
            // Two conventions state nothing.
            ("1월 1일 기준 만 19세, 신청일 기준 만 34세 이하", None),
            ("", None),
        ] {
            assert_eq!(stated_age_basis(&[text]), expected, "{text}");
        }
    }

    /// The review's notice: eligibility by birth date is a birth-year rule.
    #[test]
    fn a_birth_date_range_is_read_as_birth_years() {
        let band = (Some(19), Some(34));
        let years = [2026];
        let range = birth_range(
            &["만 19세~34세(1991.1.1.~2007.12.31. 출생자)"],
            band,
            &years,
        )
        .unwrap();
        assert_eq!(
            range,
            BirthRange {
                first: d(1991, 1, 1),
                last: d(2007, 12, 31)
            }
        );
        assert!(range.whole_years());
        let stated = birth_range(&["1992년생 ~ 2007년생 청년"], band, &years).unwrap();
        assert_eq!(
            (stated.first, stated.last),
            (d(1992, 1, 1), d(2007, 12, 31))
        );
        // No band, or a band the range does not agree with: nothing.
        assert_eq!(
            birth_range(&["1992년생 ~ 2007년생 청년"], (None, None), &years),
            None
        );
        assert_eq!(
            birth_range(&["2019년생 ~ 2022년생 자녀"], band, &years),
            None
        );
        let partial = birth_range(&["1991. 3. 2. ~ 2007. 3. 1. 사이 출생"], band, &years).unwrap();
        assert!(!partial.whole_years());
        assert_eq!(
            birth_range(&["2026.1.1.~2026.12.31. 신청"], band, &years),
            None
        );
        assert_eq!(
            birth_range(
                &["1991.1.1.~2007.12.31. 출생", "1990.1.1.~2006.12.31. 출생"],
                band,
                &years
            ),
            None
        );
    }
}
