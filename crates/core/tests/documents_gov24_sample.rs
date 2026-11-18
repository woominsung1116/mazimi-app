//! `documents::split` against 50 hand-labelled 보조금24 구비서류 texts
//! (`tests/fixtures/gov24_documents_sample.json`). A row is right when the
//! checklist equals the label, or when both are "not a list" (the text is
//! then shown as written).

use mazimi_core::documents::{split, states_none};
use serde::Deserialize;

#[derive(Deserialize)]
struct Fixture {
    rows: Vec<Row>,
}

#[derive(Deserialize)]
struct Row {
    id: String,
    text: String,
    expected: Option<Vec<String>>,
}

fn fixture() -> Vec<Row> {
    let text = include_str!("fixtures/gov24_documents_sample.json");
    serde_json::from_str::<Fixture>(text).unwrap().rows
}

fn names(text: &str) -> Option<Vec<String>> {
    split(Some(text)).map(|l| l.items.into_iter().map(|i| i.name).collect())
}

/// Measured 2026-10-29: 41 of 50 (the grouped "○ 신청인 제출서류 / - …"
/// texts, "1.신청서" without a space and "3.(…)" were the fixes over the
/// previous 37). The nine misses are guidance written as lines, a comma
/// list wrapped over two lines and an unmarked heading line.
const MIN_RIGHT: usize = 41;

#[test]
fn gov24_sample_accuracy() {
    let rows = fixture();
    assert_eq!(rows.len(), 50);
    let misses: Vec<&str> = rows
        .iter()
        .filter(|row| names(&row.text) != row.expected)
        .map(|row| row.id.as_str())
        .collect();
    let right = rows.len() - misses.len();
    assert!(right >= MIN_RIGHT, "{right}/50 right, misses: {misses:?}");
}

#[test]
fn grouped_texts_list_the_documents_under_their_titles() {
    let rows = fixture();
    for id in [
        "55e9d5771f71", // o 신청인 제출 서류 / - … / o 공무원 확인 가능 서류 / - …
        "6d9bba2ca368", // ○ 신청인 제출 서류 / - … / <해당 세대 제출> / - …
        "45684e3bf14b", // <공통> / 1. … / ○ … 신청 시 제출 서류 / 1. …
        "7fcb47e99eb9", // ○ 신청인 제출서류 / 1. … / ○ 공무원 확인가능 서류(…) / 1. …
        "c0b38c3eff01", // 3.(본인토지가 아닐경우) …
        "7f75f56c9381", // 1.국가보훈대상자 … (no space after the number)
    ] {
        let row = rows.iter().find(|r| r.id == id).unwrap();
        assert_eq!(names(&row.text), row.expected, "{id}");
    }
}

#[test]
fn no_sample_list_is_read_as_none_required() {
    for row in fixture() {
        assert!(!states_none(Some(&row.text)), "{}", row.id);
    }
}
