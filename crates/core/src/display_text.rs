//! Display normalizer for collected text (v1 feedback F4.5, F5.6).
//!
//! Sources format summaries and conditions in many ways (`○`, `ㅇ`, `-`,
//! `①`, `1.`, wrapped lines, inline bullets). [`normalize`] turns one text
//! into clean items for display. It **adds no words and drops no content**:
//! it only removes leading list symbols and collapses whitespace. The stored
//! text is never changed.
//!
//! Rules, applied in order:
//! 1. Line breaks (`\r\n`, `\r`, `\n`) split the text into lines, and an
//!    inline bullet (`○` and the like, `※`, `①`…`⑳`) in the middle of a line
//!    starts a new piece there.
//! 2. When any piece starts with a list symbol or an enumerator, pieces
//!    without one continue the item before them (a wrapped line). Without
//!    any symbol, every line is its own item.
//! 3. Each item loses its leading symbols and enumerators (`1.`, `1)`,
//!    `(1)`, `가.`, `가)`, when a space follows), and its whitespace
//!    collapses to single spaces. Empty items are dropped.
//! 4. A single item without symbols is split into sentences after `.`
//!    following a Hangul syllable ("…다.", "…함."), or after `!` / `?`,
//!    when whitespace follows. Dates and decimals ("26. 1. 1.", "1.5")
//!    never split.
use serde::{Deserialize, Serialize};

/// The cleaned form of one text.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayText {
    /// The items joined with `\n`; `None` when the text is absent or blank.
    pub display: Option<String>,
    /// The items; render them as bullets when there are several.
    pub lines: Vec<String>,
}

/// Symbols removed at the start of an item.
const LEADING: &[char] = &[
    '○', '●', '◦', '•', '❍', '◯', '◎', '▪', '■', '□', '◆', '◇', '◈', '▣', '▶', '►', '▷', '▸', '▹',
    '‣', '⁃', '❖', '-', '–', '—', '·', '∙', '※', '*', '★', '☆', '✓', '✔', '➢', '→', '⇒', 'ㅇ',
];

/// Symbols that start a new item even in the middle of a line. Word-joining
/// marks (`·`, `-`, `‧`) are not among them.
const INLINE: &[char] = &[
    '○', '●', '◦', '❍', '◯', '▪', '■', '□', '◆', '◇', '◈', '▣', '▶', '►', '▷', '▸', '▹', '❖', '※',
];

/// Hangul syllables used as enumerators (`가.`, `나)`).
const HANGUL_ENUM: &[char] = &[
    '가', '나', '다', '라', '마', '바', '사', '아', '자', '차', '카', '타', '파', '하',
];

pub(crate) fn is_circled_number(c: char) -> bool {
    ('\u{2460}'..='\u{2473}').contains(&c)
}

fn is_hangul_syllable(c: char) -> bool {
    ('\u{AC00}'..='\u{D7A3}').contains(&c)
}

/// Length in bytes of a leading enumerator (`1. `, `1) `, `(1) `, `가. `,
/// `가) `), including the space after it. A date ("26. 1. 1.") is not one.
pub(crate) fn enumerator_len(s: &str) -> Option<usize> {
    let chars: Vec<(usize, char)> = s.char_indices().collect();
    let space_after = |i: usize| chars.get(i).is_some_and(|(_, c)| c.is_whitespace());
    let end = |i: usize| chars.get(i).map_or(s.len(), |(b, _)| *b);
    let digits = chars.iter().take_while(|(_, c)| c.is_ascii_digit()).count();
    if (1..=2).contains(&digits)
        && matches!(chars.get(digits), Some((_, '.' | ')')))
        && space_after(digits + 1)
    {
        // "26. 1. 1." is a date: the next token is another number + '.'.
        let rest = s[end(digits + 1)..].trim_start();
        let next_digits = rest.chars().take_while(char::is_ascii_digit).count();
        let date = next_digits > 0 && rest[next_digits..].starts_with('.');
        return (!date).then(|| end(digits + 2));
    }
    // "-1. " after a circled number ("⑥-1. 혼인관계증명서").
    if chars.first().is_some_and(|(_, c)| *c == '-') {
        let inner = chars[1..]
            .iter()
            .take_while(|(_, c)| c.is_ascii_digit())
            .count();
        if (1..=2).contains(&inner)
            && matches!(chars.get(1 + inner), Some((_, '.' | ')')))
            && space_after(2 + inner)
        {
            return Some(end(3 + inner));
        }
    }
    if chars.first().is_some_and(|(_, c)| *c == '(') {
        let inner = chars[1..]
            .iter()
            .take_while(|(_, c)| c.is_ascii_digit())
            .count();
        if (1..=2).contains(&inner)
            && matches!(chars.get(1 + inner), Some((_, ')')))
            && space_after(2 + inner)
        {
            return Some(end(3 + inner));
        }
    }
    if chars.first().is_some_and(|(_, c)| HANGUL_ENUM.contains(c))
        && matches!(chars.get(1), Some((_, '.' | ')')))
        && space_after(2)
    {
        return Some(end(3));
    }
    None
}

/// Whether the text starts with a list symbol or an enumerator.
pub(crate) fn starts_with_marker(s: &str) -> bool {
    let s = s.trim_start();
    let Some(first) = s.chars().next() else {
        return false;
    };
    leading_symbol(s).is_some()
        || is_circled_number(first)
        || enumerator_len(s).is_some()
        || type_label(s)
}

/// "1유형:", "2유형)": a numbered type that starts an item. It is content,
/// so it starts a new item but is never removed.
fn type_label(s: &str) -> bool {
    let digits = s.chars().take_while(char::is_ascii_digit).count();
    digits > 0 && {
        let rest = &s[digits..];
        rest.starts_with("유형") && rest["유형".len()..].trim_start().starts_with([':', ')'])
    }
}

/// Byte length of one leading list symbol, if any. `-` and `*` count only
/// when no digit follows ("-10%" is a number).
pub(crate) fn leading_symbol(s: &str) -> Option<usize> {
    let mut chars = s.chars();
    let first = chars.next()?;
    let next = chars.next();
    // Letters used as bullets ("o 신청자격", "ㅁ 프로그램명") count only with
    // a space after them; private-use glyphs are bullet fonts ("").
    let letter_bullet = matches!(first, 'o' | 'ㅁ' | 'º') && next.is_some_and(char::is_whitespace);
    let private_use = ('\u{E000}'..='\u{F8FF}').contains(&first);
    if !(LEADING.contains(&first) || letter_bullet || private_use) {
        return None;
    }
    if matches!(first, '-' | '*' | '–') && next.is_some_and(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(first.len_utf8())
}

/// Remove every leading symbol and enumerator, then collapse whitespace.
pub(crate) fn clean(item: &str) -> String {
    clean_traced(item).0
}

/// [`clean`] and the leading text it removed.
fn clean_traced(item: &str) -> (String, &str) {
    let mut s = item.trim_start();
    loop {
        if let Some(len) = leading_symbol(s) {
            s = s[len..].trim_start();
        } else if s.chars().next().is_some_and(is_circled_number) {
            let len = s.chars().next().map_or(0, char::len_utf8);
            s = s[len..].trim_start();
        } else if let Some(len) = enumerator_len(s) {
            s = s[len..].trim_start();
        } else {
            break;
        }
    }
    let removed = &item[..item.len() - s.len()];
    (s.split_whitespace().collect::<Vec<_>>().join(" "), removed)
}

/// A line split at inline bullets: each piece keeps its bullet.
pub(crate) fn pieces(line: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, c) in line.char_indices() {
        let before = line[start..i].trim_end();
        // "(※ 참고)": a symbol right after an opening bracket is quoted.
        if i > start
            && (INLINE.contains(&c) || is_circled_number(c))
            && !before.trim().is_empty()
            && !before.ends_with(['(', '[', '<'])
        {
            out.push(&line[start..i]);
            start = i;
        }
    }
    out.push(&line[start..]);
    out
}

/// Sentences of a single paragraph (rule 4).
fn sentences(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut current = String::new();
    for (i, c) in chars.iter().enumerate() {
        current.push(*c);
        let next_space = chars.get(i + 1).is_some_and(|n| n.is_whitespace());
        let ends = match c {
            '.' => i > 0 && is_hangul_syllable(chars[i - 1]),
            '!' | '?' => true,
            _ => false,
        };
        if ends && next_space {
            out.push(std::mem::take(&mut current));
        }
    }
    out.push(current);
    out.into_iter()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Clean `text` for display (see the module rules).
pub fn normalize(text: Option<&str>) -> DisplayText {
    normalize_traced(text).0
}

/// [`normalize`] and every piece of text it removed (for the lossless
/// test).
fn normalize_traced(text: Option<&str>) -> (DisplayText, Vec<String>) {
    let Some(text) = text else {
        return (DisplayText::default(), Vec::new());
    };
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    // (piece, continues a line: indented or after an inline split)
    let raw: Vec<(&str, bool)> = text
        .lines()
        .flat_map(|line| {
            let indented = line.starts_with(char::is_whitespace);
            pieces(line)
                .into_iter()
                .enumerate()
                .map(move |(i, piece)| (piece, i > 0 || indented))
        })
        .collect();
    let marked = raw.iter().any(|(p, _)| starts_with_marker(p));
    let mut items: Vec<String> = Vec::new();
    for (piece, indented) in raw {
        if piece.trim().is_empty() {
            continue;
        }
        match items.last_mut() {
            // A wrapped line (indented, unmarked) continues its item; a
            // line at column 0 is the source's own line break.
            Some(last) if marked && indented && !starts_with_marker(piece) => {
                last.push(' ');
                last.push_str(piece);
            }
            _ => items.push(piece.to_owned()),
        }
    }
    let mut removed = Vec::new();
    let mut lines: Vec<String> = items
        .iter()
        .map(|item| {
            let (line, gone) = clean_traced(item);
            removed.push(gone.to_owned());
            line
        })
        .filter(|s| !s.is_empty())
        .collect();
    if !marked && lines.len() == 1 {
        lines = sentences(&lines[0]);
    }
    (
        DisplayText {
            display: (!lines.is_empty()).then(|| lines.join("\n")),
            lines,
        },
        removed,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn lines(text: &str) -> Vec<String> {
        normalize(Some(text)).lines
    }

    /// Every non-space character of the input is in the output or in the
    /// leading text the rules removed: counted per character, so a dropped
    /// digit or letter fails.
    fn assert_lossless(input: &str) -> DisplayText {
        use std::collections::BTreeMap;
        let (out, removed) = normalize_traced(Some(input));
        let count = |texts: &[&str]| {
            let mut m: BTreeMap<char, usize> = BTreeMap::new();
            for t in texts {
                for c in t.chars().filter(|c| !c.is_whitespace()) {
                    *m.entry(c).or_default() += 1;
                }
            }
            m
        };
        let mut kept: Vec<&str> = out.lines.iter().map(String::as_str).collect();
        kept.extend(removed.iter().map(String::as_str));
        assert_eq!(count(&[input]), count(&kept), "{input:?} → {:?}", out.lines);
        // What was removed is only list symbols and enumerators.
        for gone in &removed {
            for c in gone.chars().filter(|c| !c.is_whitespace()) {
                assert!(
                    LEADING.contains(&c)
                        || INLINE.contains(&c)
                        || is_circled_number(c)
                        || c.is_ascii_digit()
                        || HANGUL_ENUM.contains(&c)
                        || matches!(c, '.' | ')' | '(' | 'o' | 'ㅁ' | 'º')
                        || ('\u{E000}'..='\u{F8FF}').contains(&c),
                    "removed {c:?} from {input:?}"
                );
            }
        }
        out
    }

    #[test]
    fn the_lossless_check_catches_a_dropped_digit() {
        let (out, removed) = normalize_traced(Some("○ 월 20만원"));
        assert_eq!(out.lines, ["월 20만원"]);
        assert_eq!(removed, ["○ "]);
        // A broken output (one digit short) does not balance.
        let text = "○ 월 20만원";
        let count = |s: &str| s.chars().filter(|c| c.is_ascii_digit()).count();
        assert_ne!(count(text), count("월 2만원") + count(&removed.concat()));
    }

    #[test]
    fn source_lines_at_column_zero_stay_apart() {
        // An unmarked line after a bulleted one is its own line unless
        // indented (a wrap).
        assert_eq!(
            lines("○ 지원대상: 청년\n지원내용: 월 20만원\n  (최대 12개월)"),
            ["지원대상: 청년", "지원내용: 월 20만원 (최대 12개월)"]
        );
        // Latin o, ㅁ, º and bullet-font glyphs are bullets.
        assert_eq!(
            lines("o 신청자격\nㅁ 프로그램명: 청년와락\nº 기간 3개월\n\u{F0A7} 문의처"),
            ["신청자격", "프로그램명: 청년와락", "기간 3개월", "문의처"]
        );
        // "ok" and "ㅁㅁ" are not bullets.
        assert_eq!(lines("ok 사인"), ["ok 사인"]);
        // "1유형:" starts an item and stays.
        assert_eq!(
            lines("1유형: 저소득 청년\n2유형: 일반 청년"),
            ["1유형: 저소득 청년", "2유형: 일반 청년"]
        );
    }

    #[test]
    fn thirty_prod_texts_lose_only_symbols_and_whitespace() {
        let fixture: Value =
            serde_json::from_str(include_str!("../tests/fixtures/display_text_prod.json")).unwrap();
        let cases = fixture["cases"].as_array().unwrap();
        assert_eq!(cases.len(), 30);
        for case in cases {
            let text = case["text"].as_str().unwrap();
            let out = normalize(Some(text));
            assert!(!out.lines.is_empty(), "{text:?}");
            for line in &out.lines {
                assert!(!starts_with_marker(line), "{line:?} from {text:?}");
                assert!(!line.contains("  ") && line.trim() == line, "{line:?}");
                assert!(!line.contains('\n'));
            }
            assert_eq!(out.display.as_deref(), Some(out.lines.join("\n").as_str()));
            assert_lossless(text);
        }
    }

    #[test]
    fn prod_examples_read_as_expected() {
        // gov_benefits: one bullet.
        assert_eq!(
            lines("○ 산모건강관리사 이용 시, 비용의 본인부담금 90% 지원(50만원 한도내)"),
            ["산모건강관리사 이용 시, 비용의 본인부담금 90% 지원(50만원 한도내)"]
        );
        // Word-joining marks stay ("구축‧운영", "초·중·고").
        assert_eq!(
            lines("고립·은둔형 청소년 원스톱 지원체계 구축‧운영"),
            ["고립·은둔형 청소년 원스톱 지원체계 구축‧운영"]
        );
        // A trailing \r\n and a "-" in the middle of a sentence.
        assert_eq!(
            lines("고등학생, 대학생 - 성적우수, 저소득, 특정분야별 장학금 지원 \r\n"),
            ["고등학생, 대학생 - 성적우수, 저소득, 특정분야별 장학금 지원"]
        );
        // ㅇ bullets with nested - lines.
        assert_eq!(
            lines(" ㅇ (사업 기간) 2004년 ~ 계속사업 \n\n ㅇ (사업 대상) 저소득층, 청년\n   - (시행 방법) 국비지원"),
            [
                "(사업 기간) 2004년 ~ 계속사업",
                "(사업 대상) 저소득층, 청년",
                "(시행 방법) 국비지원"
            ]
        );
        // A plain first line, dated sub-bullets (never split), ▹ and an
        // inline ※ note.
        assert_eq!(
            lines("전세사기 피해 예방\r\n- (사업기간) 26. 1. 1. ~ 예산 소진 시까지\r\n ▹무주택  ※ 본인, 배우자 모두 무주택"),
            [
                "전세사기 피해 예방",
                "(사업기간) 26. 1. 1. ~ 예산 소진 시까지",
                "무주택",
                "본인, 배우자 모두 무주택"
            ]
        );
        // A wrapped line continues its bullet.
        assert_eq!(
            lines("○ 중증장애인들이 일상생활을 영위하는데 필요한 서비스를 적절하게 제공\n    함으로써 기본적인 생활보장\n"),
            ["중증장애인들이 일상생활을 영위하는데 필요한 서비스를 적절하게 제공 함으로써 기본적인 생활보장"]
        );
        // ❍ bullets.
        assert_eq!(
            lines("❍ 취업 시 인센티브를 지원\n❍ 지원금을 홍천사랑카드로 지급"),
            ["취업 시 인센티브를 지원", "지원금을 홍천사랑카드로 지급"]
        );
        // Inline ○ list (한국장학재단 제출서류).
        assert_eq!(
            lines(
                "○ 장학생 신청서○ 재학증명서○ 2026년 1학기 성적증명서※ 자세한 사항은 홈페이지 참고"
            ),
            [
                "장학생 신청서",
                "재학증명서",
                "2026년 1학기 성적증명서",
                "자세한 사항은 홈페이지 참고"
            ]
        );
        // A paragraph splits into sentences; decimals and dates do not.
        assert_eq!(
            lines("지원하기 위함. 금리는 1.5%입니다. 기간은 26. 1. 1.부터예요!"),
            [
                "지원하기 위함.",
                "금리는 1.5%입니다.",
                "기간은 26. 1. 1.부터예요!"
            ]
        );
        // Enumerators.
        assert_eq!(
            lines("1. 주민등록초본\n - 2026년 발급분\n2. 소득금액증명원"),
            ["주민등록초본", "2026년 발급분", "소득금액증명원"]
        );
        assert_eq!(
            lines("가. 신청서 1부\n나) 서약서\n(3) 동의서\n① 통장 ② 신분증"),
            ["신청서 1부", "서약서", "동의서", "통장", "신분증"]
        );
        // A negative number is not a bullet; a quoted note stays inline.
        assert_eq!(lines("-10% 할인"), ["-10% 할인"]);
        assert_eq!(
            lines("○ 월 20만원 지원(※ 예산 소진 시 종료)"),
            ["월 20만원 지원(※ 예산 소진 시 종료)"]
        );
    }

    #[test]
    fn absent_or_blank_text_has_nothing_to_show() {
        assert_eq!(normalize(None), DisplayText::default());
        assert_eq!(normalize(Some("  \n ○ \n")), DisplayText::default());
    }
}
