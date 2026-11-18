//! Dates as Korean notices write them: `2026. 3. 3.(화)`, `2026-03-03`,
//! `2026년 3월 3일`, and the yearless `11. 27.(금)` / `11월 27일` that
//! follow a full date in a period (`2026. 3. 3. ~ 11. 27.`). Dependency
//! free; no regex. Amounts (`1.5배`), phone numbers and times are not dates.
use chrono::{Datelike, NaiveDate};

/// One date found in a text, with its char span (`start..end`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FoundDate {
    pub date: NaiveDate,
    pub start: usize,
    pub end: usize,
    /// Written without a year (`11. 27.`); its year was inferred.
    pub yearless: bool,
}

fn read_number(chars: &[char], at: usize) -> Option<(u32, usize, usize)> {
    let mut end = at;
    while end < chars.len() && chars[end].is_ascii_digit() && end - at < 5 {
        end += 1;
    }
    if end == at || (end < chars.len() && chars[end].is_ascii_digit()) {
        return None;
    }
    let text: String = chars[at..end].iter().collect();
    text.parse().ok().map(|n| (n, end - at, end))
}

fn skip_spaces(chars: &[char], mut at: usize) -> usize {
    while at < chars.len() && chars[at] == ' ' {
        at += 1;
    }
    at
}

/// Every date in `text`, in order. A yearless date takes the year of the
/// date before it (the next year when it would fall before that date), or
/// `default_year` when it is the first.
pub fn find_dates(text: &str, default_year: i32) -> Vec<FoundDate> {
    let chars: Vec<char> = text.chars().collect();
    let mut found: Vec<FoundDate> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let starts_number = chars[i].is_ascii_digit() && (i == 0 || !chars[i - 1].is_ascii_digit());
        if !starts_number {
            i += 1;
            continue;
        }
        // Right after a range sign (`2026.11.20~12.5`): a bare `M.D` is the end.
        let after_range = found
            .last()
            .is_some_and(|prev| range_joint(&chars[prev.end.min(i)..i].iter().collect::<String>()));
        match date_at(
            &chars,
            i,
            found.last().map(|f| f.date),
            default_year,
            after_range,
        ) {
            Some(date) => {
                i = date.end;
                found.push(date);
            }
            None => {
                while i < chars.len() && chars[i].is_ascii_digit() {
                    i += 1;
                }
            }
        }
    }
    found
}

/// Whether the text between two dates joins them as a range: a range sign,
/// with spaces, a weekday "(수)" and a time "10:00" allowed around it
/// ("2026. 10. 1.(수) 10:00 ~ 11. 30.").
pub fn range_joint(between: &str) -> bool {
    let mut text: String = between.chars().filter(|c| !c.is_whitespace()).collect();
    // Weekday groups.
    while let (Some(open), Some(close)) = (text.find('('), text.find(')')) {
        if open >= close || close - open > 12 {
            break;
        }
        text.replace_range(open..=close, "");
    }
    // Times: digits ':' digits (':' digits).
    let chars: Vec<char> = text.chars().collect();
    let mut kept = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_ascii_digit() {
            let mut j = i;
            while j < chars.len() && (chars[j].is_ascii_digit() || chars[j] == ':') {
                j += 1;
            }
            let token: String = chars[i..j].iter().collect();
            if token.contains(':') && token.split(':').all(|p| (1..=2).contains(&p.len())) {
                i = j;
                continue;
            }
            kept.push_str(&token);
            i = j;
            continue;
        }
        kept.push(chars[i]);
        i += 1;
    }
    matches!(
        kept.as_str(),
        "~" | "∼" | "〜" | "-" | "–" | ".~" | ".-" | "부터~" | "~까지"
    )
}

/// [`find_dates`] read on `today`: a notice that starts with a yearless
/// date more than about six months ahead was written last year
/// ("접수기간: 12. 1. ~ 1. 31." read in January).
pub fn find_dates_near(text: &str, today: NaiveDate) -> Vec<FoundDate> {
    let found = find_dates(text, today.year());
    match found.first() {
        Some(first) if first.yearless && (first.date - today).num_days() > 183 => {
            find_dates(text, today.year() - 1)
        }
        _ => found,
    }
}

fn date_at(
    chars: &[char],
    start: usize,
    previous: Option<NaiveDate>,
    default_year: i32,
    after_range: bool,
) -> Option<FoundDate> {
    // A decimal or a version (`3.5`, `v1.2.3`) preceded by a letter is not a date.
    if start > 0 && (chars[start - 1].is_ascii_alphabetic() || chars[start - 1] == '.') {
        return None;
    }
    let (first, first_len, mut at) = read_number(chars, start)?;
    at = skip_spaces(chars, at);
    let sep1 = *chars.get(at)?;
    if !matches!(sep1, '.' | '-' | '/' | '년' | '월') {
        return None;
    }
    at = skip_spaces(chars, at + 1);
    let (second, second_len, after_second) = read_number(chars, at)?;
    if second_len > 2 {
        return None;
    }
    // Month-day forms: `11. 27.` / `11. 27(금)` / `11월 27일`.
    let month_day = |end: usize| -> Option<FoundDate> {
        if first_len > 2 || !(1..=12).contains(&first) || !(1..=31).contains(&second) {
            return None;
        }
        let year = match previous {
            Some(p) if (first, second) < (p.month(), p.day()) => p.year() + 1,
            Some(p) => p.year(),
            None => default_year,
        };
        NaiveDate::from_ymd_opt(year, first, second).map(|date| FoundDate {
            date,
            start,
            end,
            yearless: true,
        })
    };
    if sep1 == '월' {
        let at = skip_spaces(chars, after_second);
        return (chars.get(at) == Some(&'일'))
            .then(|| month_day(at + 1))
            .flatten();
    }
    let mut at2 = skip_spaces(chars, after_second);
    let sep2 = chars.get(at2).copied();
    let full_year = first_len == 4 && (1990..=2100).contains(&first);
    if full_year
        && matches!(sep2, Some('.' | '-' | '/' | '월'))
        && (sep1 != '년' || sep2 == Some('월'))
    {
        at2 = skip_spaces(chars, at2 + 1);
        let (third, third_len, mut end) = read_number(chars, at2)?;
        if third_len > 2 {
            return None;
        }
        if sep2 == Some('월') {
            let after = skip_spaces(chars, end);
            if chars.get(after) != Some(&'일') {
                return None;
            }
            end = after + 1;
        } else if chars.get(end) == Some(&'.') {
            end += 1;
        }
        let date = NaiveDate::from_ymd_opt(first as i32, second, third)?;
        return Some(FoundDate {
            date,
            start,
            end,
            yearless: false,
        });
    }
    if full_year {
        return None;
    }
    // The end of a range may be bare: `~12.5`.
    if sep1 == '.' && after_range && !matches!(sep2, Some('.') | Some('(')) {
        return month_day(after_second);
    }
    // Yearless dotted form must close with `.` or `(` so `1.5배` never reads.
    if sep1 == '.' && matches!(sep2, Some('.') | Some('(')) {
        let end = if sep2 == Some('.') { at2 + 1 } else { at2 };
        // Not a three-part number without a year (`1.2.3`); a time may follow.
        let next = skip_spaces(chars, end);
        if sep2 == Some('.') && chars.get(next).is_some_and(|c| c.is_ascii_digit()) {
            let time = read_number(chars, next).is_some_and(|(_, _, e)| chars.get(e) == Some(&':'));
            if !time {
                return None;
            }
        }
        return month_day(end);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dates(text: &str) -> Vec<String> {
        find_dates(text, 2026)
            .iter()
            .map(|d| d.date.format("%Y-%m-%d").to_string())
            .collect()
    }

    #[test]
    fn notice_forms_and_ranges() {
        assert_eq!(
            dates("접수기간: 2026. 3. 3.(화) ~ 11. 27.(금) * 예산소진시 마감"),
            ["2026-03-03", "2026-11-27"]
        );
        assert_eq!(
            dates("신청 기간 2026.02.02 ~ 2026.11.30"),
            ["2026-02-02", "2026-11-30"]
        );
        assert_eq!(
            dates("2026년 10월 1일부터 10월 31일까지"),
            ["2026-10-01", "2026-10-31"]
        );
        assert_eq!(dates("2026-12-14 18:00 마감"), ["2026-12-14"]);
        assert_eq!(
            dates("모집 2026. 11. 20. ~ 1. 15."),
            ["2026-11-20", "2027-01-15"]
        );
        assert_eq!(dates("11월 27일 발표"), ["2026-11-27"]);
        assert_eq!(dates("면접 11. 3.(월) 예정"), ["2026-11-03"]);
        assert_eq!(dates("마감 11. 27. 18:00까지"), ["2026-11-27"]);
        // A bare end after a range sign (code review H3).
        assert_eq!(dates("2026.11.20~12.5"), ["2026-11-20", "2026-12-05"]);
        assert_eq!(
            dates("2026.10.1 ~ 11.30 접수"),
            ["2026-10-01", "2026-11-30"]
        );
        assert_eq!(
            dates("2026. 3. 3.(화) ~ 11. 27"),
            ["2026-03-03", "2026-11-27"]
        );
        // Read in January: the yearless start belongs to last year (M3).
        let january = NaiveDate::from_ymd_opt(2027, 1, 10).unwrap();
        let near: Vec<String> = find_dates_near("접수기간: 12. 1. ~ 1. 31.", january)
            .iter()
            .map(|d| d.date.to_string())
            .collect();
        assert_eq!(near, ["2026-12-01", "2027-01-31"]);
    }

    #[test]
    fn numbers_that_are_not_dates() {
        for text in [
            "지원금 1.5배",
            "문의 010-1234-5678",
            "버전 v1.2.3",
            "09:00~18:00",
            "월 30만원",
            "2026년 상반기",
            "1.2.3 항목",
            "2026. 13. 40.",
        ] {
            assert!(dates(text).is_empty(), "{text}: {:?}", dates(text));
        }
    }
}
