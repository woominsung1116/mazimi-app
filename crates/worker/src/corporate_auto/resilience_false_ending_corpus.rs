//! Negative corpus for the ending rule (2026-10-26): every fact production
//! hid as ended although its offer runs, replayed with its stored quote,
//! value baseline and anchors on the page as fetched that day
//! (`tests/fixtures/false_ending_20261026`). The clauses that hid them are
//! about something else: an eligibility rule (`no longer eligible`,
//! `졸업…사용 불가`), a stacking rule (`중복 가입 불가`), an FAQ question, a
//! product badge (`일시품절`), board metadata (`게시종료일`), a membership
//! term (`자격 유지 후 만료`).
//!
//! - No confirmed fact (a stored baseline) is hidden by any of them: each
//!   confirms, except the ones [`STAYS_HIDDEN`] lists, for other reasons.
//! - Once confirmed and its section captured, each fact is held when its
//!   own text then says the offer ended.

use super::page_changed_corpus::{entries_in, resolve_entry, resolve_with, view_of, Entry};
use super::*;
use crate::corporate_auto::evidence::visible_text;

const FIXTURES: &str = "false_ending_20261026";

/// Facts that stay hidden, and why (none by an ending word):
/// - 할리스: the FAQ's value window changed (`window_changed`).
/// - 부산대 Google's exclusion fact: never confirmed (no baseline), so it
///   is read as on first sight, its adoption window closed.
const STAYS_HIDDEN: &[(&str, &str)] = &[
    (
        "1만원 이상부터 5천원 단위로 크라운 1개씩 추가 적립되고 있습니다.",
        "window_changed",
    ),
    (
        "휴학생 및 졸업생은 제공 대상에서 제외됩니다.",
        "first_sight",
    ),
];

fn baseline(entry: &Entry) -> bool {
    entry.window_signature.is_some() || entry.shadow_window_signature.is_some()
}

#[test]
fn confirmed_offers_are_not_hidden_by_clauses_about_something_else() {
    let mut wrong = Vec::new();
    for (entry, html) in entries_in(FIXTURES) {
        let resolution = resolve_entry(&entry, &html);
        let expected = STAYS_HIDDEN
            .iter()
            .find(|(quote, _)| *quote == entry.quote)
            .map(|(_, label)| *label);
        let ok = match expected {
            None => resolution.confirms(),
            Some("first_sight") => !baseline(&entry),
            Some(label) => match &resolution {
                Resolution::Ambiguous(reason) => reason_key(reason) == Some(label),
                other => other.label() == label,
            },
        };
        if !ok {
            wrong.push(format!(
                "{} | {:?}: {resolution:?}",
                entry.title, entry.quote
            ));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// The page with `ending` written as a paragraph in front of the element
/// that writes the quote: the last visible place the longest head of the
/// quote occurs (not in a script, a style or a tag; markup inside the quote
/// ends the head).
fn page_with_ending_before_quote(entry: &Entry, html: &str, ending: &str) -> String {
    let in_text = |at: usize| {
        let before = &html[..at];
        let open_tag = before
            .rfind('<')
            .is_some_and(|lt| !before[lt..].contains('>'));
        let in_block = |open: &str, close: &str| {
            before
                .rfind(open)
                .is_some_and(|o| before.rfind(close).is_none_or(|c| c < o))
        };
        !open_tag && !in_block("<script", "</script>") && !in_block("<style", "</style>")
    };
    let chars: Vec<char> = entry.quote.chars().collect();
    let at = (4..=chars.len().min(24))
        .rev()
        .find_map(|len| {
            let head: String = chars[..len].iter().collect();
            html.rmatch_indices(head.as_str())
                .map(|(at, _)| at)
                .find(|&at| in_text(at))
        })
        .unwrap_or_else(|| panic!("{}: the quote is written on the page", entry.title));
    // In front of the element holding the quote: its opening tag.
    let element = html[..at].rfind('<').unwrap_or(at);
    format!("{}<p>{ending}</p>{}", &html[..element], &html[element..])
}

#[test]
fn each_confirmed_offer_is_held_once_its_own_text_says_it_ended() {
    let shown = |e: &Entry| baseline(e) && !STAYS_HIDDEN.iter().any(|(quote, _)| *quote == e.quote);
    for (entry, html) in entries_in(FIXTURES).into_iter().filter(|(e, _)| shown(e)) {
        // Its next confirmation captures its section (2a, legacy facts).
        let visible = visible_text(&html);
        let anchors = Anchors {
            fingerprint: capture(&view_of(&entry, &html, &visible), &entry.quote),
            ..Anchors::default()
        };
        let ending = if entry.quote.is_ascii() {
            "This offer has ended."
        } else {
            "본 혜택은 종료되었습니다."
        };
        let ended = page_with_ending_before_quote(&entry, &html, ending);
        let resolution = resolve_with(&entry, &ended, &anchors);
        assert!(!resolution.confirms(), "{}: {resolution:?}", entry.title);
    }
}

/// The reader's work stays bounded on these large real pages (up to 600 KB):
/// a first-sight read and a confirmed read of each fact take well under a
/// second each in a release build and a few in a debug build. A quadratic
/// footnote search once made the differential gate spin for hours.
#[test]
fn reading_each_real_page_stays_bounded() {
    for (entry, html) in entries_in(FIXTURES) {
        let started = std::time::Instant::now();
        let _ = resolve_entry(&entry, &html);
        let visible = visible_text(&html);
        let _ = page_blocked(&view_of(&entry, &html, &visible), &entry.quote);
        let elapsed = started.elapsed();
        assert!(
            elapsed < std::time::Duration::from_secs(20),
            "{}: {elapsed:?}",
            entry.title
        );
    }
}

/// A date range on the page that is no period of the offer never ends it
/// once it passes: LG's card-ad compliance stamps (`여신금융협회 심의필 제…호
/// (2025.10.28~2026.10.27)`) at +30 and +90 days (release review
/// 2026-10-28). The offer's own period written on the same page still ends
/// it.
#[test]
fn a_compliance_stamps_range_is_no_period_of_the_offer() {
    let (entry, html) = entries_in(FIXTURES)
        .into_iter()
        .find(|(e, _)| e.title.starts_with("LG전자"))
        .expect("the LG page");
    let at = |html: &str, days: i64| {
        let kst = chrono::FixedOffset::east_opt(9 * 3_600).unwrap();
        let now = (entry.fetched_at + chrono::Duration::days(days))
            .with_timezone(&kst)
            .naive_local();
        let view = PageView::at(html, &visible_text(html), now).with_url(&entry.url);
        resolve(
            &view,
            &StoredFact {
                quote: &entry.quote,
                baseline: entry
                    .window_signature
                    .as_deref()
                    .or(entry.shadow_window_signature.as_deref()),
                ages: crate::corporate_auto::evidence::Ages {
                    min: entry.min_age,
                    max: entry.max_age,
                },
                adoption_open: false,
                anchors: &entry.anchors,
            },
        )
    };
    for days in [30, 90] {
        let resolution = at(&html, days);
        assert!(resolution.confirms(), "+{days} days: {resolution:?}");
    }
    let own = page_with_ending_before_quote(&entry, &html, "이벤트 기간: 2026.09.01 ~ 2026.10.13");
    let resolution = at(&own, 30);
    assert_eq!(
        resolution,
        Resolution::Ambiguous(SECTION_DATE_PAST.into()),
        "its own period still ends it"
    );
}
