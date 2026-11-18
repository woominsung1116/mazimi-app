//! Must-confirm corpus for the current-fact rule (2026-10-26): every fact
//! production hid only because its page changed somewhere else, replayed
//! with its stored quote, value baseline and anchors on the page as fetched
//! that day (`tests/fixtures/page_changed_20261026`).
//!
//! - Each fact listed in [`CONFIRMS`] confirms: its quote is on the page and
//!   its value window reads as the stored baseline.
//! - Every fact hides after a one-value mutation of its quote on the page,
//!   and, where its window holds values, after a mutation of a value next
//!   to the quote.

use super::*;
use crate::corporate_auto::evidence::visible_text;

/// One production fact and the page it was replayed on (the fixture
/// `index.json` row; shared with the false-ending corpus).
#[derive(serde::Deserialize)]
pub(super) struct Entry {
    pub title: String,
    pub url: String,
    pub file: String,
    pub sha256: String,
    pub fetched_at: chrono::DateTime<chrono::Utc>,
    pub quote: String,
    pub window_signature: Option<String>,
    pub shadow_window_signature: Option<String>,
    pub min_age: Option<i32>,
    pub max_age: Option<i32>,
    pub anchors: Anchors,
}

/// Facts whose page still states them as stored. The other three changed
/// punctuation on the page (1Password and Notion dropped the closing
/// period, Azure's became a footnote `*`): canonical text keeps
/// punctuation, so they stay hidden for fresh extraction.
const CONFIRMS: &[&str] = &[
    "If you are a student we have a special non-commercial license for you, which is free for the duration of your study.",
    "Verified students on GitHub Education get free access to Copilot's features.",
    "Free GitHub Pro while you are a student.",
    "You will receive 12 months of Tower for free.",
    "학생 인증 후 10% 할인 혜택을 제공해 드립니다.",
    "30일마다 인증 갱신 시, 할인 쿠폰이 재발급됩니다.",
    "적격한 교육용 전자 메일 주소가 있는 교육자와 학생은 Office 365 A1을 무료로 이용할 수 있습니다.",
];

const FIXTURES: &str = "page_changed_20261026";

/// The entries of a fixture directory under `tests/fixtures`, each with
/// its page, checked against the digest it was saved with.
pub(super) fn entries_in(dir: &str) -> Vec<(Entry, String)> {
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(dir);
    let index = std::fs::read_to_string(fixtures.join("index.json")).expect("index");
    let entries: Vec<Entry> = serde_json::from_str(&index).expect("index parses");
    entries
        .into_iter()
        .map(|entry| {
            let html = std::fs::read_to_string(fixtures.join(&entry.file)).expect("page");
            use sha2::Digest;
            let digest: String = sha2::Sha256::digest(html.as_bytes())
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            assert_eq!(digest, entry.sha256, "{} unchanged", entry.file);
            (entry, html)
        })
        .collect()
}

/// The page as the evidence job reads it at the entry's fetch time.
pub(super) fn view_of(entry: &Entry, html: &str, visible: &str) -> PageView {
    let kst = chrono::FixedOffset::east_opt(9 * 3_600).unwrap();
    let now = entry.fetched_at.with_timezone(&kst).naive_local();
    PageView::at(html, visible, now).with_url(&entry.url)
}

pub(super) fn resolve_entry(entry: &Entry, html: &str) -> Resolution {
    resolve_with(entry, html, &entry.anchors)
}

/// [`resolve_entry`] with other stored anchors (as captured at a later
/// confirmation).
pub(super) fn resolve_with(entry: &Entry, html: &str, anchors: &Anchors) -> Resolution {
    let visible = visible_text(html);
    let view = view_of(entry, html, &visible);
    // Live reads the stored baseline, else the shadow one (evidence::assess).
    let baseline = entry
        .window_signature
        .as_deref()
        .or(entry.shadow_window_signature.as_deref());
    resolve(
        &view,
        &StoredFact {
            quote: &entry.quote,
            baseline,
            ages: Ages {
                min: entry.min_age,
                max: entry.max_age,
            },
            adoption_open: false,
            anchors,
        },
    )
}

/// `text` with its first value changed: a digit raised by one, or the
/// first number word swapped; a quote without either loses its last word.
fn one_value_changed(text: &str) -> String {
    if let Some(at) = text.find(|c: char| c.is_ascii_digit()) {
        let digit = text[at..].chars().next().unwrap().to_digit(10).unwrap();
        let mut out = text.to_string();
        out.replace_range(at..at + 1, &((digit + 1) % 10).to_string());
        return out;
    }
    for (word, other) in [("two", "three"), ("one", "two"), ("free", "paid")] {
        if let Some(at) = text.to_lowercase().find(word) {
            let mut out = text.to_string();
            out.replace_range(at..at + word.len(), other);
            return out;
        }
    }
    match text.trim_end().rsplit_once(' ') {
        Some((head, _)) => format!("{head} 변경"),
        None => format!("{text} 변경"),
    }
}

/// The page with the quote's text mutated where the page writes it. Tries
/// the whole quote, then ever shorter prefixes holding its first value, so
/// markup inside the quote does not stop the mutation.
fn page_with_quote_mutated(html: &str, quote: &str) -> Option<String> {
    let chars: Vec<char> = quote.chars().collect();
    for len in (8..=chars.len()).rev() {
        let part: String = chars[..len].iter().collect();
        let changed = one_value_changed(&part);
        if changed != part && html.contains(&part) {
            return Some(html.replace(&part, &changed));
        }
    }
    None
}

#[test]
fn facts_whose_page_changed_elsewhere_confirm_on_their_own_evidence() {
    let mut wrong = Vec::new();
    for (entry, html) in entries_in(FIXTURES) {
        let resolution = resolve_entry(&entry, &html);
        let expected = CONFIRMS.contains(&entry.quote.as_str());
        if resolution.confirms() != expected {
            wrong.push(format!(
                "{} | {:?}: {resolution:?}",
                entry.title, entry.quote
            ));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

#[test]
fn a_one_value_mutation_of_each_fact_hides_it() {
    for (entry, html) in entries_in(FIXTURES) {
        let mutated = page_with_quote_mutated(&html, &entry.quote)
            .unwrap_or_else(|| panic!("{}: the quote is written on the page", entry.title));
        let resolution = resolve_entry(&entry, &mutated);
        assert!(!resolution.confirms(), "{}: {resolution:?}", entry.title);
        // A value next to the quote: every one the window holds is changed.
        let baseline = entry.window_signature.as_deref().unwrap_or("");
        if let Some(token) = baseline.split([',', '|']).find(|t| !t.is_empty()) {
            let number: String = token.chars().take_while(char::is_ascii_digit).collect();
            let unit = &token[number.len()..];
            let raised = (number.parse::<u64>().unwrap() + 1).to_string();
            let mut window_mutated = html.clone();
            for written in [format!("{number} {unit}"), format!("{number}{unit}")] {
                window_mutated = window_mutated.replace(&written, &format!("{raised} {unit}"));
            }
            assert_ne!(
                window_mutated, html,
                "{}: {token} is written on the page",
                entry.title
            );
            let resolution = resolve_entry(&entry, &window_mutated);
            assert!(
                !resolution.confirms(),
                "{} window: {resolution:?}",
                entry.title
            );
        }
    }
}
