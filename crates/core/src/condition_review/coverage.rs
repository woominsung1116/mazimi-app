//! The completeness check of a condition review (plan "verified conditions"
//! §1.2, audit H1): the one place that decides a review may be `complete`.
//!
//! A review is complete only when every segment of the reviewed text carries
//! a label in the review's coverage map, no segment is `uncheckable`, every
//! `condition` label names at least one rule the review (or its code rules)
//! actually has, and the rule set restricts someone (no rule at all is
//! "verified for everyone", which the plan sends to a human). The importer
//! and the admin route both reach it through [`super::store::validate_with`];
//! a stored row carries the [`VERSION`] it was checked with, and the list
//! serves a `complete` row without one as incomplete ([`served_status`]).
//!
//! Segmentation is deterministic code, a port of the pilot's `segment.py`
//! (`mask`, `split_text`, `seg_id`) over the source's reviewed text fields
//! ([`super::text_fields`]), so the seg ids curators labelled are the ones
//! this module derives. Code fields are not segmented here: a condition they
//! state is the source-code rule the list evaluates beside the review
//! (`eligibility_rules`), and any unknown leaf there already withholds
//! "조건 확인됨".

use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{text_fields, SEPARATOR};
use crate::applicant_review::field_text;

/// The version of this check a stored review was written with. Bump when a
/// change here could turn a stored `complete` into an incomplete set.
pub const VERSION: i16 = 1;

/// Segment labels (plan §1.2).
const LABELS: [&str; 5] = [
    "condition",
    "window",
    "applicant",
    "uncheckable",
    "non_condition",
];

/// One segment of the reviewed text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub seg_id: String,
    pub field: &'static str,
    pub text: String,
}

/// Why a coverage map does not make a set complete. Carries no source text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gap {
    /// A segment of the current text has no label.
    Unlabelled(String),
    /// A label is not one of the plan's labels.
    BadLabel(String),
    /// A segment is a condition no profile field answers.
    Uncheckable(String),
    /// A `condition` label names no rule, or a rule the review lacks.
    UnresolvedRule(String),
    /// The set has no restrictive rule at all.
    NoRestrictiveRule,
    /// The coverage map has no `labels` object.
    NoLabels,
}

/// The reviewed text of `raw_payload` split into labelled units, in field
/// order, each seg id once. `None` for a source with no reviewed field set.
pub fn segments(source_type: &str, raw_payload: &Value) -> Option<Vec<Segment>> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for field in text_fields(source_type)? {
        for text in split_text(&field_text(raw_payload, field)) {
            let seg_id = seg_id(field, &text);
            if seen.insert(seg_id.clone()) {
                out.push(Segment {
                    seg_id,
                    field,
                    text,
                });
            }
        }
    }
    Some(out)
}

/// The gaps of `coverage` against the current text. Empty = the set may be
/// complete. `rule_ids` are the ids of the review's validated rules, its
/// code rules (structural ones included) and its overridden code rules;
/// `restrictive` is how many of those restrict anyone.
pub fn check(
    source_type: &str,
    raw_payload: &Value,
    coverage: &Value,
    rule_ids: &HashSet<String>,
    restrictive: usize,
) -> Vec<Gap> {
    let mut gaps = Vec::new();
    let Some(labels) = coverage.get("labels").and_then(Value::as_object) else {
        return vec![Gap::NoLabels];
    };
    for segment in segments(source_type, raw_payload).unwrap_or_default() {
        let Some(entry) = labels.get(&segment.seg_id) else {
            gaps.push(Gap::Unlabelled(segment.seg_id));
            continue;
        };
        let label = entry.get("label").and_then(Value::as_str).unwrap_or("");
        if !LABELS.contains(&label) {
            gaps.push(Gap::BadLabel(segment.seg_id));
            continue;
        }
        match label {
            "uncheckable" => gaps.push(Gap::Uncheckable(segment.seg_id)),
            "condition" => {
                let refs: Vec<&str> = entry
                    .get("rule_refs")
                    .and_then(Value::as_array)
                    .map(|refs| refs.iter().filter_map(Value::as_str).collect())
                    .unwrap_or_default();
                if refs.is_empty() || !refs.iter().all(|r| rule_ids.contains(*r)) {
                    gaps.push(Gap::UnresolvedRule(segment.seg_id));
                }
            }
            _ => {}
        }
    }
    if restrictive == 0 {
        gaps.push(Gap::NoRestrictiveRule);
    }
    gaps
}

/// The status the list serves for a stored review: a `complete` row that
/// was not written through [`check`] (no `coverage_version`, or an older
/// one) is incomplete.
pub fn served_status(stored: &str, coverage_version: Option<i16>) -> &str {
    if stored == "complete" && coverage_version.is_none_or(|v| v < VERSION) {
        "incomplete"
    } else {
        stored
    }
}

/// `segment.py` `seg_id`: the first 12 hex digits of SHA-256 over the field
/// name, a unit separator and the segment text.
fn seg_id(field: &str, text: &str) -> String {
    let digest = Sha256::digest(format!("{field}{SEPARATOR}{text}").as_bytes());
    format!("{digest:x}")[..12].to_owned()
}

// ── segment.py `mask` ──────────────────────────────────────────────────────

/// Invisible and formatting characters `mask` removes.
fn invisible(c: char) -> bool {
    matches!(c,
        '\u{200b}'..='\u{200f}'
        | '\u{2028}'..='\u{202e}'
        | '\u{2060}'..='\u{2064}'
        | '\u{2066}'..='\u{2069}'
        | '\u{180e}'
        | '\u{034f}'
        | '\u{fe00}'..='\u{fe0f}'
        | '\u{feff}'
        | '\u{ad}')
}

/// Contact patterns and their replacement, in `segment.py` order (a mirror
/// of the worker's `redact_bounded` contact patterns).
static CONTACT: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    [
        (
            r"[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}",
            "[이메일 생략]",
        ),
        (
            r"\b\d{6}\s*[\-\x{2010}-\x{2015}\x{2212}]?\s*[1-8]\d{6}\b",
            "[주민번호 생략]",
        ),
        (
            r"(^|\D)((?:\+82[\s\-]?0?|0)1[016789][\s\-.]?\d{3,4}[\s\-.]?\d{4})\b",
            "${1}[전화번호 생략]",
        ),
        (
            r"(^|\D)(\(?0\d{1,2}\)?[\s\-.]?\d{3,4}[\s\-.]?\d{4}|1[5-9]\d{2}[\s\-.]?\d{4})\b",
            "${1}[전화번호 생략]",
        ),
        (
            r"(?i)(전화|연락처|문의|\b(?:tel|fax|phone|mobile)\b)\s*[.:：]?\s*(?:\(?\d{2,4}\)?[\s\-.]*\d{3,4}[\s\-.]*\d{4}|\d{3,4}[\s\-.]*\d{4})\b",
            "${1} [전화번호 생략]",
        ),
        (r"담당자\s*[:：]?\s*[가-힣]{2,4}", "담당자 [이름 생략]"),
        (
            r"담당\s*[:：]?\s*[가-힣]{2,4}\s*(주임|대리|과장|팀장|매니저|차장|부장|사원|선생님)",
            "담당 [이름 생략] ${1}",
        ),
        (
            r"(?i)<?\s*/?\s*untrusted[\W_]*source[\W_]*text\s*>?",
            "[구분자 생략]",
        ),
    ]
    .into_iter()
    .map(|(pattern, replacement)| (Regex::new(pattern).expect("mask pattern"), replacement))
    .collect()
});

fn mask(text: &str) -> String {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut text: String = text
        .chars()
        .filter(|&c| c == '\n' || (c as u32 >= 32 && c as u32 != 127))
        .filter(|&c| !invisible(c))
        .collect();
    for (pattern, replacement) in CONTACT.iter() {
        text = pattern.replace_all(&text, *replacement).into_owned();
    }
    text
}

// ── segment.py `split_text` ────────────────────────────────────────────────

const BULLETS: &str = "○●◦‣•▶►□■※ㅇ-*·";

/// Python's `\d` on str: any Unicode decimal digit (Nd), as the regex
/// crate's Unicode `\d` reads it.
fn is_digit(c: char) -> bool {
    static DIGIT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d$").expect("digit pattern"));
    c.is_ascii_digit() || (!c.is_ascii() && DIGIT.is_match(c.encode_utf8(&mut [0; 4])))
}

/// Leading decimal digits of `chars`.
fn digit_run(chars: &[char]) -> usize {
    chars.iter().take_while(|c| is_digit(**c)).count()
}

/// `\d{1,2}\.` at the start of `chars`: the index after the dot.
fn numbered(chars: &[char]) -> Option<usize> {
    let run = digit_run(chars);
    ((1..=2).contains(&run) && chars.get(run) == Some(&'.')).then_some(run + 1)
}

/// Whether a bullet (`_BULLET`'s lookahead) starts at `chars[0]`.
fn bullet_at(chars: &[char]) -> bool {
    let space_at = |i: usize| chars.get(i).is_some_and(|c| c.is_whitespace());
    let Some(&first) = chars.first() else {
        return false;
    };
    if (BULLETS.contains(first) || ('\u{2460}'..='\u{2473}').contains(&first)) && space_at(1) {
        return true;
    }
    if ('가'..='하').contains(&first) && chars.get(1) == Some(&'.') && space_at(2) {
        return true;
    }
    if let Some(after) = numbered(chars) {
        // (?!\d)(?!\s\d{1,2}\.) then \s.
        let next_numbered = chars.get(after + 1..).and_then(numbered).is_some();
        if space_at(after) && !next_numbered {
            return true;
        }
    }
    if first == '(' {
        let run = digit_run(&chars[1..]);
        if (1..=2).contains(&run) && chars.get(1 + run) == Some(&')') && space_at(2 + run) {
            return true;
        }
    }
    let heading: Vec<char> = "중복혜택".chars().collect();
    chars.starts_with(&heading) && space_at(heading.len())
}

/// `_split_before_bullets`: cut before every bullet that follows whitespace,
/// except a number that continues a date ("2026. 5. 11.").
fn split_before_bullets(line: &[char]) -> Vec<&[char]> {
    let mut cuts = Vec::new();
    for pos in 1..line.len() {
        if !line[pos - 1].is_whitespace() || !bullet_at(&line[pos..]) {
            continue;
        }
        let date_part = is_digit(line[pos])
            && pos >= 3
            && is_digit(line[pos - 3])
            && line[pos - 2] == '.'
            && line[pos - 1].is_whitespace();
        if !date_part {
            cuts.push(pos);
        }
    }
    let mut out = Vec::new();
    let mut last = 0;
    for pos in cuts {
        out.push(&line[last..pos]);
        last = pos;
    }
    out.push(&line[last..]);
    out
}

/// `_AFTER.split`: after "…다." followed by whitespace, and after ';'.
fn split_after_sentences(chunk: &[char]) -> Vec<&[char]> {
    let mut out = Vec::new();
    let (mut last, mut i) = (0, 1);
    while i <= chunk.len() {
        let spaces = |from: usize| {
            chunk[from..]
                .iter()
                .take_while(|c| c.is_whitespace())
                .count()
        };
        let sentence_end = i >= 3
            && chunk[i - 1] == '.'
            && chunk[i - 2] == '다'
            && ('가'..='힣').contains(&chunk[i - 3]);
        if sentence_end && spaces(i) > 0 {
            out.push(&chunk[last..i]);
            last = i + spaces(i);
            i = last.max(i + 1);
            continue;
        }
        if chunk[i - 1] == ';' {
            out.push(&chunk[last..i]);
            last = i + spaces(i);
            i = last.max(i + 1);
            continue;
        }
        i += 1;
    }
    out.push(&chunk[last.min(chunk.len())..]);
    out
}

/// `segment.py` `split_text`: masked text to ordered segment texts.
fn split_text(text: &str) -> Vec<String> {
    let masked = mask(text);
    let mut out = Vec::new();
    for line in masked.split('\n') {
        let chars: Vec<char> = line.chars().collect();
        for chunk in split_before_bullets(&chars) {
            for piece in split_after_sentences(chunk) {
                let piece: String = piece.iter().collect();
                let piece = piece.trim();
                if piece
                    .chars()
                    .any(|c| c.is_ascii_alphanumeric() || ('가'..='힣').contains(&c))
                {
                    out.push(piece.to_owned());
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Expected values are `segment.py`'s own output on the same input.
    #[test]
    fn text_splits_at_bullets_sentences_and_semicolons_but_not_dates() {
        assert_eq!(
            split_text("○ 만 19~34세 청년 ○ 부산 거주자\n- 소득 무관"),
            ["○ 만 19~34세 청년", "○ 부산 거주자", "- 소득 무관"]
        );
        assert_eq!(
            split_text("지원합니다. 신청은 온라인; 방문 가능"),
            ["지원합니다.", "신청은 온라인;", "방문 가능"]
        );
        assert_eq!(
            split_text("기간: 2026. 5. 11. ~ 2026. 6. 30. 1. 대상 2. 서류"),
            ["기간: 2026. 5. 11. ~ 2026. 6. 30. 1. 대상", "2. 서류"]
        );
        assert_eq!(
            split_text("가. 신청자격 (1) 재학생"),
            ["가. 신청자격", "(1) 재학생"]
        );
        // Contact data is masked before the ids are taken.
        assert_eq!(
            split_text("문의 051-123-4567 담당자 홍길동"),
            ["문의 [전화번호 생략] 담당자 [이름 생략]"]
        );
        assert!(split_text("  ---  \n\n").is_empty());
        // Full-width digits number a list item as `\d` does.
        assert_eq!(
            split_text("대상 １. 청년 ２. 학생"),
            ["대상", "１. 청년", "２. 학생"]
        );
    }

    #[test]
    fn an_unlabelled_uncheckable_or_unbacked_segment_is_a_gap() {
        let raw = json!({"addAplyQlfcCndCn": "○ 무주택 세대주 ○ 중위소득 150% 이하"});
        let segs = segments("youth_center", &raw).unwrap();
        assert_eq!(segs.len(), 2);
        let ids: HashSet<String> = ["R1".to_owned()].into_iter().collect();
        let labels = |a: Value, b: Value| json!({"labels": {segs[0].seg_id.clone(): a, segs[1].seg_id.clone(): b}});
        let condition = json!({"label": "condition", "rule_refs": ["R1"]});
        assert!(check(
            "youth_center",
            &raw,
            &labels(condition.clone(), condition.clone()),
            &ids,
            1
        )
        .is_empty());
        let gaps = check("youth_center", &raw, &json!({}), &ids, 1);
        assert_eq!(gaps, [Gap::NoLabels]);
        let only_one = json!({"labels": {segs[0].seg_id.clone(): condition.clone()}});
        assert_eq!(
            check("youth_center", &raw, &only_one, &ids, 1),
            [Gap::Unlabelled(segs[1].seg_id.clone())]
        );
        let uncheckable = json!({"label": "uncheckable", "check_kind": "housing"});
        assert_eq!(
            check(
                "youth_center",
                &raw,
                &labels(condition.clone(), uncheckable),
                &ids,
                1
            ),
            [Gap::Uncheckable(segs[1].seg_id.clone())]
        );
        let missing = json!({"label": "condition", "rule_refs": ["R9"]});
        assert_eq!(
            check(
                "youth_center",
                &raw,
                &labels(condition.clone(), missing),
                &ids,
                1
            ),
            [Gap::UnresolvedRule(segs[1].seg_id.clone())]
        );
        let none = json!({"label": "non_condition"});
        assert_eq!(
            check("youth_center", &raw, &labels(none.clone(), none), &ids, 0),
            [Gap::NoRestrictiveRule]
        );
    }

    #[test]
    fn a_complete_row_written_before_the_check_is_served_incomplete() {
        assert_eq!(served_status("complete", None), "incomplete");
        assert_eq!(served_status("complete", Some(VERSION)), "complete");
        assert_eq!(served_status("conflict", None), "conflict");
        assert_eq!(served_status("incomplete", Some(VERSION)), "incomplete");
    }
}
