//! 주민등록번호- and account-shaped digit runs, by structure: one function,
//! ported line for line to `apps/mobile/lib/formFill/digits.ts`. Both read
//! `docs/validation/sensitive-digit-cases.json`; change the two together.
//!
//! The digit skeleton of a text:
//! - a digit is any decimal digit (any script), or a Hangul numeral syllable
//!   (영 공 일 이 삼 사 오 육 륙 칠 팔 구) inside a run of six or more of them;
//! - digit groups join across a gap of one to three characters that holds
//!   no Hangul syllable and no Latin word of two letters or more: so `-`,
//!   ` `, `·`, `ㅡ` (a jamo), `一`, a lone `o` all join, while a word ends
//!   the run.
//!
//! A run is a 주민등록번호 when it has thirteen digits with a 1–8 seventh
//! digit (six then seven), or six digits, a mark, and a 1–8 digit (the rest
//! often starred: "900101-1******"). Otherwise ten or more digits that are
//! not a phone number are an account number.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    Rrn,
    Account,
}

fn hangul_numeral(c: char) -> Option<char> {
    Some(match c {
        '영' | '공' => '0',
        '일' => '1',
        '이' => '2',
        '삼' => '3',
        '사' => '4',
        '오' => '5',
        '육' | '륙' => '6',
        '칠' => '7',
        '팔' => '8',
        '구' => '9',
        _ => return None,
    })
}

/// A decimal digit of any script, as ASCII. A digit whose value is not
/// known here reads as 5: inside the 1–8 range, so the rrn rule errs toward
/// refusing.
fn decimal(c: char) -> Option<char> {
    if c.is_ascii_digit() {
        return Some(c);
    }
    let code = c as u32;
    for zero in [0xFF10, 0x0660, 0x06F0, 0x0966, 0x09E6, 0x0E50] {
        if (zero..zero + 10).contains(&code) {
            return char::from_digit(code - zero, 10);
        }
    }
    c.is_numeric().then_some('5')
}

fn syllable(c: char) -> bool {
    ('\u{AC00}'..='\u{D7A3}').contains(&c)
}

fn phone(digits: &str) -> bool {
    // +82 10-…: the national number with its 0.
    let national = match digits.strip_prefix("82") {
        Some(rest) if (10..=11).contains(&rest.len()) && !rest.starts_with('0') => {
            format!("0{rest}")
        }
        _ => digits.to_owned(),
    };
    let d = national.as_bytes();
    let len = d.len();
    let at = |i: usize| d.get(i).copied().unwrap_or(b'x');
    (at(0) == b'0' && at(1) == b'1' && b"016789".contains(&at(2)) && (10..=11).contains(&len))
        || (at(0) == b'0' && at(1) == b'2' && (9..=10).contains(&len))
        || (at(0) == b'0' && (b'3'..=b'6').contains(&at(1)) && (10..=11).contains(&len))
        || (national.starts_with("070") && len == 11)
        || (national.starts_with("050") && (11..=12).contains(&len))
}

/// The sensitive runs of `chars`: `(start, end, shape)` as char indices.
pub fn sensitive_runs(chars: &[char]) -> Vec<(usize, usize, Shape)> {
    let n = chars.len();
    let mut digit: Vec<Option<char>> = chars.iter().map(|c| decimal(*c)).collect();
    // Hangul numerals count only in a run of six or more.
    let mut i = 0;
    while i < n {
        if hangul_numeral(chars[i]).is_none() {
            i += 1;
            continue;
        }
        let start = i;
        while i < n && hangul_numeral(chars[i]).is_some() {
            i += 1;
        }
        if i - start >= 6 {
            for k in start..i {
                digit[k] = hangul_numeral(chars[k]);
            }
        }
    }
    let latin_word = |k: usize| {
        chars[k].is_ascii_alphabetic()
            && ((k > 0 && chars[k - 1].is_ascii_alphabetic())
                || chars.get(k + 1).is_some_and(|c| c.is_ascii_alphabetic()))
    };
    let joiner = |k: usize| digit[k].is_none() && !syllable(chars[k]) && !latin_word(k);
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        if digit[i].is_none() {
            i += 1;
            continue;
        }
        let start = i;
        let mut end = i;
        let mut digits = String::new();
        let mut first_group: Option<(usize, bool)> = None;
        let mut j = i;
        while j < n {
            if let Some(d) = digit[j] {
                digits.push(d);
                end = j + 1;
                j += 1;
                continue;
            }
            let gap = (j..n).take_while(|k| joiner(*k)).count();
            if (1..=3).contains(&gap) && digit.get(j + gap).is_some_and(Option::is_some) {
                if first_group.is_none() {
                    let marked = (j..j + gap).any(|k| !matches!(chars[k], ' ' | '\t' | '\n'));
                    first_group = Some((digits.len(), marked));
                }
                j += gap;
            } else {
                break;
            }
        }
        let seventh = digits
            .as_bytes()
            .get(6)
            .is_some_and(|d| (b'1'..=b'8').contains(d));
        let rrn = seventh
            && ((digits.len() == 13 && matches!(first_group, None | Some((6, _))))
                || matches!(first_group, Some((6, true)) if digits.len() <= 13));
        if rrn {
            out.push((start, end, Shape::Rrn));
        } else if digits.len() >= 10 && !phone(&digits) {
            out.push((start, end, Shape::Account));
        }
        i = end.max(i + 1);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The client's port reads the same fixture
    /// (apps/mobile/scripts/check-chat-form-fill.cjs).
    #[test]
    fn digit_runs_classify_as_the_client_does() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/validation/sensitive-digit-cases.json");
        let cases: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        for case in cases["cases"].as_array().unwrap() {
            let text = case["text"].as_str().unwrap();
            let want: Vec<&str> = case["shapes"]
                .as_array()
                .unwrap()
                .iter()
                .map(|s| s.as_str().unwrap())
                .collect();
            let got: Vec<&str> = sensitive_runs(&text.chars().collect::<Vec<_>>())
                .iter()
                .map(|r| match r.2 {
                    Shape::Rrn => "rrn",
                    Shape::Account => "account",
                })
                .collect();
            assert_eq!(got, want, "{text:?}");
        }
    }
}
