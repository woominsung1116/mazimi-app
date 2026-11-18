//! Display form of a programme title (`title_display`, served next to the
//! stored `title`, which never changes).
//!
//! Sources prepend notice-status markers to titles ("(변경·중지·연장 등 )부산광역시
//! …", "[변경] …", "(재공고) …", "※ …") and leave stray spaces inside
//! brackets ("지원(초음파 검사비 )"). [`title_display`] removes only those:
//!
//! 1. Whitespace runs collapse to one space; the ends are trimmed.
//! 2. At the start, repeatedly: a `※` mark, and a bracket group (`()`, `[]`,
//!    `【】`, `〈〉`, `<>`, full-width) whose every word is an administrative
//!    status word ([`ADMIN_WORDS`], joined by spaces, `·`, `,`, `/` or `등`).
//!    A group with any other word ("(광양시)", "(재)", "[2026년 마감]",
//!    "(청년 금융교육 지원)") is content and stays.
//! 3. Spaces right after an opening bracket and right before a closing one.
//!
//! It never removes a word that is not on the list, and a title it would
//! empty (or cut below two characters) is returned with only rules 1 and 3
//! applied.

/// Notice-status words a source puts in a leading bracket. Ordered longest
/// first so a compound ("변경공고") is consumed whole.
const ADMIN_WORDS: &[&str] = &[
    "기간연장",
    "마감연장",
    "접수연장",
    "일부변경",
    "변경공고",
    "정정공고",
    "연장공고",
    "재공고",
    "재게시",
    "재모집",
    "최초공개",
    "변경",
    "중지",
    "연장",
    "정정",
    "수정",
    "재개",
    "등",
];

const OPEN: &[char] = &['(', '[', '【', '〈', '<', '（', '［', '「'];
const CLOSE: &[char] = &[')', ']', '】', '〉', '>', '）', '］', '」'];

fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Brackets whose inner spaces are tightened: not `<` / `>`, which titles
/// also use as comparison signs ("소득 < 100%", code review L3).
const TIGHT_OPEN: &[char] = &['(', '[', '【', '〈', '（', '［', '「'];
const TIGHT_CLOSE: &[char] = &[')', ']', '】', '〉', '）', '］', '」'];

/// Spaces after an opening bracket or before a closing one.
fn tighten_brackets(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    for (i, c) in chars.iter().enumerate() {
        if c.is_whitespace() {
            let after_open = out.chars().last().is_some_and(|p| TIGHT_OPEN.contains(&p));
            let before_close = chars.get(i + 1).is_some_and(|n| TIGHT_CLOSE.contains(n));
            if after_open || before_close {
                continue;
            }
        }
        out.push(*c);
    }
    out
}

/// Whether `inner` (a bracket's content) is the sources' compound
/// boilerplate: administrative words only, and several of them or a list
/// closed by `등` ("변경·중지·연장 등"). A single status ("(중지)", "[변경]",
/// "(재공고)", "(마감연장)") tells the user something about the programme and
/// stays (code review M5).
fn administrative(inner: &str) -> bool {
    let mut rest: String = inner
        .chars()
        .filter(|c| !c.is_whitespace() && !matches!(c, '·' | ',' | '/' | '‧' | '・'))
        .collect();
    let (mut words, mut listed) = (0, false);
    while !rest.is_empty() {
        match ADMIN_WORDS.iter().find(|w| rest.starts_with(**w)) {
            Some(word) => {
                if *word == "등" {
                    listed = true;
                } else {
                    words += 1;
                }
                rest.drain(..word.len());
            }
            None => return false,
        };
    }
    words >= 2 || (words >= 1 && listed)
}

/// The leading administrative group of `text`, as a byte length to cut.
fn leading_admin(text: &str) -> Option<usize> {
    let first = text.chars().next()?;
    if first == '※' {
        return Some(first.len_utf8());
    }
    let open = OPEN.iter().position(|o| *o == first)?;
    let close = CLOSE[open];
    let body = &text[first.len_utf8()..];
    let end = body.find(close)?;
    administrative(&body[..end]).then(|| first.len_utf8() + end + close.len_utf8())
}

/// The display title of `title` (module rules).
pub fn title_display(title: &str) -> String {
    let base = tighten_brackets(&collapse(title));
    let mut text = base.as_str();
    while let Some(cut) = leading_admin(text) {
        text = text[cut..].trim_start();
    }
    if text.chars().count() < 2 {
        return base;
    }
    text.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn administrative_prefixes_are_removed() {
        for (raw, shown) in [
            (
                "(변경·중지·연장 등 )부산광역시 평생함께 청년모두가 주거비 지원",
                "부산광역시 평생함께 청년모두가 주거비 지원",
            ),
            ("[변경·정정] 청년 월세 지원", "청년 월세 지원"),
            ("(변경 등) 청년 면접수당", "청년 면접수당"),
            ("※ 청년 교통비 지원", "청년 교통비 지원"),
            ("※(변경·연장 등) 청년 교통비 지원", "청년 교통비 지원"),
            ("（정정·수정）청년 창업 지원", "청년 창업 지원"),
        ] {
            assert_eq!(title_display(raw), shown, "{raw}");
        }
    }

    #[test]
    fn stray_spaces_and_whitespace_are_cleaned() {
        for (raw, shown) in [
            (
                "임신부 산전검사비 지원(초음파 기형아 풍진검사비 )",
                "임신부 산전검사비 지원(초음파 기형아 풍진검사비)",
            ),
            (
                "안양 청년 전월세보증금 대출이자 지원(청년 인터레스트(人.터.Rest)사업 )",
                "안양 청년 전월세보증금 대출이자 지원(청년 인터레스트(人.터.Rest)사업)",
            ),
            ("  청년   월세  지원 ", "청년 월세 지원"),
            ("청년 지원( 부산 )", "청년 지원(부산)"),
            // `<` and `>` are left alone: they are comparison signs too.
            ("소득 < 100% 청년 지원", "소득 < 100% 청년 지원"),
            ("청년 지원 > 주거", "청년 지원 > 주거"),
        ] {
            assert_eq!(title_display(raw), shown, "{raw}");
        }
    }

    /// Content in a leading bracket is never dropped.
    #[test]
    fn meaningful_brackets_stay() {
        for raw in [
            "(광양시) 청년 월세 지원",
            "(재)부산테크노파크 청년 인턴",
            "[남구] 청년 정책",
            "[2026년 마감] 청년 저축",
            "[7월 마감] 청년 저축",
            "(청년 금융교육 지원) 강의",
            "(추가) 청년 장학금",
            "(2차) 청년 장학금",
            "(등록금) 지원",
            "(등) 지원",
            "(수정구) 청년 지원",
            // A single status is information, not boilerplate (code review M5).
            "(중지) 청년 월세 지원",
            "[중지] 청년 면접수당",
            "(마감연장) 청년 장학금",
            "(재공고) 청년 면접수당",
            "[변경] 청년 월세 지원",
            "(최초공개) 청년 문화패스",
            "(변경가능) 청년 지원",
            "<청년> 지원",
            "청년 월세 지원",
        ] {
            assert_eq!(title_display(raw), raw, "{raw}");
        }
    }

    #[test]
    fn a_title_of_markers_only_is_kept() {
        assert_eq!(title_display("(변경)"), "(변경)");
        assert_eq!(title_display("※"), "※");
        assert_eq!(title_display(""), "");
        assert_eq!(title_display("(변경) 가"), "(변경) 가");
    }

    /// Measurement over a title export (one JSON object with `"t"` per
    /// line): `TITLE_EXPORT=titles.jsonl cargo test -p mazimi-core --lib
    /// title_display::tests::measure -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn measure() {
        let path = std::env::var("TITLE_EXPORT").expect("TITLE_EXPORT");
        let text = std::fs::read_to_string(path).unwrap();
        let (mut total, mut changed, mut prefix) = (0, 0, 0);
        for line in text.lines() {
            let row: serde_json::Value = serde_json::from_str(line).unwrap();
            let raw = row["t"].as_str().unwrap_or("");
            total += 1;
            let shown = title_display(raw);
            if shown != raw {
                changed += 1;
                let tidy = tighten_brackets(&collapse(raw));
                if shown != tidy {
                    prefix += 1;
                }
                println!("{raw}\t=>\t{shown}");
            }
        }
        println!("total={total} changed={changed} admin_prefix={prefix}");
    }

    /// Every word the display keeps is in the raw title, in order.
    #[test]
    fn the_display_adds_nothing() {
        for raw in [
            "(변경·중지·연장 등 )부산광역시 평생함께 청년모두가 주거비 지원",
            "[변경·정정] 청년 월세 지원",
            "임신부 산전검사비 지원(초음파 기형아 풍진검사비 )",
        ] {
            let shown = title_display(raw);
            let compact = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
            assert!(compact(raw).ends_with(&compact(&shown)), "{raw} -> {shown}");
        }
    }
}
