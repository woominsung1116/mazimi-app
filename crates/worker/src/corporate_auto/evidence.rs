//! P1 evidence re-verification (design §3.5–3.6, §6 P1).
//!
//! A published fact stays published while its verbatim evidence quote is
//! still on the live first-party page and the deterministic parser reads
//! the same value. Otherwise the offer is hidden. P1 never changes a fact.
//!
//! Per fact, on every fetch:
//! 1. **Presence.** Every fragment of the quote occurs in the page's visible
//!    text (whitespace-insensitive, exact otherwise). Hidden, `aria-hidden`,
//!    `display:none` and script text never counts.
//! 2. **Parser agreement.** When the quote states an age, `parse_age` must
//!    read exactly the programme's stored `min_age`/`max_age`.
//! 3. **Neighbourhood.** The unit-bearing numbers (%, 원, 개월, 세 …) within
//!    150 compact characters around the quote must equal the signature
//!    recorded at the first live confirmation. A changed value next to the
//!    quote hides the offer even though the quote itself is still there.
//!
//! Seeded evidence comes from the independent offline audit of 2026-10-16
//! (`audit_20261016.json`). It may adopt a baseline only before
//! `adopt_before` (audit date + 60 days); after that, an item that was never
//! confirmed stays as it is and needs fresh extraction (P2).

use super::values::{value_signature, value_spans, COMPOUND_WON, UNIT_VALUE};
use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use regex::Regex;
use reqwest::Url;
use scraper::Html;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::{FromRow, PgPool, Postgres, Transaction};
use std::{
    collections::{BTreeMap, HashSet},
    sync::LazyLock,
};
use tracing::{info, warn};
use uuid::Uuid;

use super::http::{PageHttp, MAX_PAGE_BYTES};
use super::policy::{self, DomainSession, Fetched, PageState};
use super::resilience::{self, Anchors, PageView, Proposal, Resolution, StoredFact};
use super::reverify::{self, Class, RunHealth};
use super::{Flags, VerifyMode};
use crate::ai_extraction::crosscheck::parse_age;
use crate::operations;
use crate::sources::{benefit_pages::normalize_volatile_tokens, fnv_hash_hex};

pub const AUDIT_SOURCE: &str = "audit_20261016";
/// Seeded audit evidence may adopt a baseline only this long after the audit.
pub const AUDIT_ADOPTION_DAYS: i64 = 60;
/// A confirmed offer stays public this long without a fresh confirmation
/// (design §3.6: always-open evidence no older than 14 days).
pub const EVIDENCE_MAX_AGE_DAYS: i64 = 14;
const WINDOW_RADIUS_CHARS: usize = 150;
const MIN_FRAGMENT_CHARS: usize = 6;
const MAX_VISIBLE_CHARS: usize = 300_000;
const MAX_TARGETS_PER_RUN: i64 = 40;
/// Census results older than this do not authorise a fetch.
pub const CENSUS_MAX_AGE_DAYS: i64 = 30;

// ── text ─────────────────────────────────────────────────────────────────

/// Visible text of an HTML page. Script, style and template text, hidden
/// elements, `aria-hidden` and inline `display:none`/`visibility:hidden`
/// subtrees are dropped, so hidden text can never stand in for evidence.
pub fn visible_text(html: &str) -> String {
    let document = Html::parse_document(&strip_comments(html));
    let mut output = String::new();
    'nodes: for node in document.root_element().descendants() {
        let Some(text) = node.value().as_text() else {
            continue;
        };
        for ancestor in node.ancestors() {
            if ancestor.value().as_element().is_some_and(element_hidden) {
                continue 'nodes;
            }
        }
        output.push_str(text);
        output.push(' ');
        if output.len() > MAX_VISIBLE_CHARS * 4 {
            break;
        }
    }
    let cleaned = output.split_whitespace().collect::<Vec<_>>().join(" ");
    cleaned.chars().take(MAX_VISIBLE_CHARS).collect()
}

/// True when this element (and so its subtree) is not shown to a sighted
/// reader without interaction: non-content elements, hidden attributes,
/// inline hiding styles, hiding utility classes and inactive tab panels.
pub(crate) fn element_hidden(element: &scraper::node::Element) -> bool {
    let style = element
        .attr("style")
        .unwrap_or("")
        .to_ascii_lowercase()
        .replace(' ', "");
    matches!(
        element.name(),
        "script" | "style" | "noscript" | "template" | "svg" | "head" | "title"
    ) || element.attr("hidden").is_some()
        || element.attr("inert").is_some()
        || element.attr("aria-hidden") == Some("true")
        || (element.name() == "input" && element.attr("type") == Some("hidden"))
        || style.contains("display:none")
        || style.contains("visibility:hidden")
        || style.contains("opacity:0;")
        || style.ends_with("opacity:0")
        || style.contains("font-size:0")
        || element.attr("class").is_some_and(has_hidden_class)
        || element.attr("class").is_some_and(inactive_tab_panel)
        || struck(element)
}

static HTML_COMMENT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)<!--.*?-->").expect("valid comment pattern"));

/// Removes HTML comments before parsing. React and other renderers split
/// text with `<!-- -->` markers (`10<!-- -->%`), which a parser reads as two
/// text nodes; without the comment the page reads `10%` as shown.
pub fn strip_comments(html: &str) -> std::borrow::Cow<'_, str> {
    HTML_COMMENT.replace_all(html, "")
}

/// Struck-through text (`<del>`, `<s>`, `<strike>`, a line-through class)
/// is a price no longer offered: never evidence.
fn struck(element: &scraper::node::Element) -> bool {
    matches!(element.name(), "del" | "s" | "strike")
        || element.attr("class").is_some_and(|class| {
            class.split_ascii_whitespace().any(|token| {
                let token = token.to_ascii_lowercase();
                token.contains("line-through") || token.contains("strike") || token == "del"
            })
        })
        || element.attr("style").is_some_and(|style| {
            style
                .to_ascii_lowercase()
                .replace(' ', "")
                .contains("text-decoration:line-through")
        })
}

/// A tab panel that is not the selected one is hidden by the site's CSS. An
/// offer only shown there (often an older or other-product duplicate) is
/// not what the page shows by default.
fn inactive_tab_panel(class: &str) -> bool {
    let tokens: Vec<String> = class
        .split_ascii_whitespace()
        .map(str::to_ascii_lowercase)
        .collect();
    tokens
        .iter()
        .any(|t| t == "tab-pane" || t == "tab-panel" || t == "tabpanel")
        && !tokens.iter().any(|t| {
            matches!(
                t.as_str(),
                "active" | "show" | "is-active" | "on" | "current" | "selected"
            )
        })
}

/// Utility classes that hide text from sighted readers (Bootstrap,
/// Tailwind, WordPress, Naver `blind`, Kakao `screen_out`, …). Text under
/// them is not what the page shows, so it can never serve as evidence.
const HIDDEN_CLASSES: &[&str] = &[
    "hidden",
    "hide",
    "d-none",
    "is-hidden",
    "invisible",
    "sr-only",
    "visually-hidden",
    "visuallyhidden",
    "screen-reader-text",
    "screenreader",
    "a11y-hidden",
    "ir",
    "ir_pm",
    "blind",
    "screen_out",
    "screen-out",
    "offscreen",
    "off-screen",
    "hidden-xs-up",
    "u-hidden",
    "is-invisible",
    "skip-link",
];

fn has_hidden_class(class: &str) -> bool {
    let tokens: Vec<String> = class
        .split_ascii_whitespace()
        .map(str::to_ascii_lowercase)
        .collect();
    // Tailwind `hidden md:block`: hidden on phones, shown from a breakpoint
    // up, so the text is on the page a desktop reader sees.
    let shown_from_breakpoint = tokens.iter().any(|token| {
        token.split_once(':').is_some_and(|(prefix, display)| {
            matches!(prefix, "sm" | "md" | "lg" | "xl" | "2xl")
                && matches!(
                    display,
                    "block"
                        | "inline"
                        | "inline-block"
                        | "flex"
                        | "inline-flex"
                        | "grid"
                        | "table"
                        | "contents"
                )
        })
    });
    tokens.iter().any(|token| {
        HIDDEN_CLASSES.contains(&token.as_str()) && !(token == "hidden" && shown_from_breakpoint)
    })
}

/// Whitespace and zero-width characters removed; everything else exact.
pub fn compact(value: &str) -> String {
    let kept: Vec<char> = value
        .chars()
        .filter(|c| {
            !matches!(
                c,
                '\u{200b}' | '\u{200c}' | '\u{200d}' | '\u{2060}' | '\u{feff}'
            )
        })
        .collect();
    let mut out = String::with_capacity(value.len());
    for (index, &c) in kept.iter().enumerate() {
        if !c.is_whitespace() {
            out.push(c);
            continue;
        }
        // Whitespace between two digits keeps a separator: "Lv.1 10%" must
        // not become "Lv.110%" (a different number).
        let next = kept[index + 1..].iter().find(|n| !n.is_whitespace());
        if out.chars().next_back().is_some_and(|p| p.is_ascii_digit())
            && next.is_some_and(|n| n.is_ascii_digit())
        {
            out.push(DIGIT_GAP);
        }
    }
    out
}

/// Stands for whitespace between two digits in compact and folded text.
pub(crate) const DIGIT_GAP: char = '\u{1F}';

pub(crate) fn normalized_compact(value: &str) -> String {
    compact(&normalize_volatile_tokens(&canonical(value)))
}

/// The one canonical text quotes and pages are compared in: NFKC,
/// whitespace runs (NBSP included) as one space, zero-width marks dropped,
/// curly quotes and primes as `'` and `"`, every dash as `-`, Latin letters
/// lowercased. Deliberately kept as written: punctuation (a dropped period
/// is another sentence) and compatibility digits such as `①` or `²` (a
/// value is read only from digits written as digits; full-width digits are
/// digits and fold).
pub fn canonical(text: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    let mut out = String::with_capacity(text.len());
    let mut space = false;
    for c in text.chars() {
        let keeps_form = !c.is_ascii_digit()
            && !('\u{FF10}'..='\u{FF19}').contains(&c)
            && std::iter::once(c).nfkc().any(|d| d.is_ascii_digit());
        let mut push = |d: char| {
            let d = match d {
                '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' | '\u{2032}' => '\'',
                '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' | '\u{2033}' => '"',
                '\u{2010}'..='\u{2015}' | '\u{2212}' | '\u{FE63}' => '-',
                d if d.is_ascii_uppercase() || ('\u{00C0}'..='\u{024F}').contains(&d) => {
                    d.to_lowercase().next().unwrap_or(d)
                }
                d => d,
            };
            if d.is_whitespace() {
                if !space {
                    out.push(' ');
                }
                space = true;
            } else if !matches!(
                d,
                '\u{200b}' | '\u{200c}' | '\u{200d}' | '\u{2060}' | '\u{feff}'
            ) {
                out.push(d);
                space = false;
            }
        };
        if keeps_form {
            push(c);
        } else {
            std::iter::once(c).nfkc().for_each(&mut push);
        }
    }
    out
}

/// An audit quote may join several page fragments with " / " or "…". Each
/// fragment must be verbatim page text on its own.
pub fn split_fragments(quote: &str) -> Vec<String> {
    let mut parts = vec![quote.to_string()];
    for separator in [" / ", "...", "…"] {
        parts = parts
            .iter()
            .flat_map(|part| {
                part.split(separator)
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .collect();
    }
    parts
        .into_iter()
        .map(|part| part.trim().to_string())
        .filter(|part| !part.is_empty())
        .collect()
}

/// Rewrites the one value of a public text (a title, a summary) that reads
/// `old` (a normalized token) to `new`, in the text's own unit form (`1만
/// 원` → `2만 원`, `10,000원` → `20,000원`). `Ok(None)`: the value is not
/// shown there. `Err`: it cannot be rewritten safely and the change must not
/// be applied: the value shows more than once, the new value cannot be
/// written in the shown unit, or the old number also shows in a form the
/// parser cannot read as that value (`1만 할인`, a bare `10000`).
pub fn rewrite_value(
    text: &str,
    old: &str,
    new: &str,
) -> std::result::Result<Option<String>, String> {
    let spans = value_spans(text);
    let hits: Vec<&(usize, usize, String)> = spans.iter().filter(|(_, _, v)| v == old).collect();
    // The old number outside every read value.
    let mut outside = text.to_string();
    for (start, end, _) in spans.iter().rev() {
        outside.replace_range(*start..*end, " ");
    }
    let number: String = old
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let unit = &old[number.len()..];
    let mut forms = vec![number.clone()];
    if unit == "원" {
        if let Ok(won) = number.parse::<u64>() {
            if won % 10_000 == 0 {
                forms.push(format!("{}만", won / 10_000));
            }
            if won % 1_000 == 0 {
                forms.push(format!("{}천", won / 1_000));
            }
        }
    }
    let squeezed = outside.replace(',', "");
    let bare = forms.iter().any(|form| {
        squeezed.match_indices(form.as_str()).any(|(at, _)| {
            let before = squeezed[..at].chars().next_back();
            let after = squeezed[at + form.len()..].chars().next();
            !before.is_some_and(|b| b.is_ascii_digit() || b == '.')
                && !after.is_some_and(|a| a.is_ascii_digit() || a == '.')
        })
    });
    if bare {
        return Err(format!(
            "{old} also shows in a form that cannot be read as the value"
        ));
    }
    let [(start, end, _)] = hits.as_slice() else {
        return if hits.is_empty() {
            Ok(None)
        } else {
            Err(format!("{old} appears more than once"))
        };
    };
    let shown = &text[*start..*end];
    let new_number: String = new
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let new_unit = &new[new_number.len()..];
    if new_unit != unit {
        return Err(format!("{old} and {new} have different units"));
    }
    let rendered = if COMPOUND_WON.is_match(shown) {
        let won: u64 = new_number
            .parse()
            .map_err(|_| "not a whole number of won")?;
        if !won.is_multiple_of(1_000) || won < 10_000 || won.is_multiple_of(10_000) {
            return Err(format!("{new} cannot be written as 만 and 천"));
        }
        format!("{}만 {}천원", won / 10_000, won % 10_000 / 1_000)
    } else {
        let caps = UNIT_VALUE.captures(shown).ok_or("unreadable value")?;
        let shown_unit = caps[2].to_lowercase().replace(' ', "");
        let number = match shown_unit.as_str() {
            "만원" | "천원" => {
                let scale = if shown_unit == "만원" {
                    10_000
                } else {
                    1_000
                };
                let won: u64 = new_number
                    .parse()
                    .map_err(|_| "not a whole number of won")?;
                if !won.is_multiple_of(scale) {
                    return Err(format!("{new} cannot be written in {shown_unit}"));
                }
                (won / scale).to_string()
            }
            _ if caps[1].contains(',') => {
                let digits: Vec<char> = new_number.chars().collect();
                if new_number.contains('.') {
                    new_number.clone()
                } else {
                    let mut grouped = String::new();
                    for (index, digit) in digits.iter().enumerate() {
                        if index > 0 && (digits.len() - index).is_multiple_of(3) {
                            grouped.push(',');
                        }
                        grouped.push(*digit);
                    }
                    grouped
                }
            }
            _ => new_number.clone(),
        };
        let at = caps.get(1).expect("number group");
        format!("{}{}{}", &shown[..at.start()], number, &shown[at.end()..])
    };
    let rewritten = format!("{}{}{}", &text[..*start], rendered, &text[*end..]);
    // Read back: exactly the new value where the old one was.
    let check = value_spans(&rewritten);
    if check.iter().any(|(_, _, v)| v == old) || !check.iter().any(|(_, _, v)| v == new) {
        return Err("the rewritten text does not read the new value".into());
    }
    Ok(Some(rewritten))
}

fn char_floor(text: &str, byte: usize, back_chars: usize) -> usize {
    let mut index = byte;
    for _ in 0..back_chars {
        match text[..index].char_indices().next_back() {
            Some((i, _)) => index = i,
            None => return 0,
        }
    }
    index
}

fn char_ceil(text: &str, byte: usize, forward_chars: usize) -> usize {
    text[byte..]
        .char_indices()
        .nth(forward_chars)
        .map_or(text.len(), |(i, _)| byte + i)
}

/// False when a hit of `needle` at `[start, end)` sits inside a longer
/// number: a quote starting with a digit preceded by a digit (or by a `.`/`,`
/// that follows a digit), or ending with a digit followed by one. So
/// `5,000원` never matches inside `15,000원`, nor `5%` inside `1.5%`.
pub(crate) fn number_bounded(before: &[char], after: Option<char>, needle: &str) -> bool {
    let starts_digit = needle.chars().next().is_some_and(|c| c.is_ascii_digit());
    let ends_digit = needle
        .chars()
        .next_back()
        .is_some_and(|c| c.is_ascii_digit());
    let previous = before.last().copied();
    let glued_before = match previous {
        Some(c) if c.is_ascii_digit() => true,
        Some('.' | ',') => before
            .get(before.len().wrapping_sub(2))
            .is_some_and(|c| c.is_ascii_digit()),
        _ => false,
    };
    let glued_after = after.is_some_and(|c| c.is_ascii_digit() || c == ',' || c == '.');
    !((starts_digit && glued_before) || (ends_digit && glued_after))
}

/// All byte offsets of `needle` in `haystack` that are not inside a longer
/// number, overlapping hits included.
fn occurrences(haystack: &str, needle: &str) -> Vec<usize> {
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(offset) = haystack[from..].find(needle) {
        let at = from + offset;
        let before: Vec<char> = haystack[..at]
            .chars()
            .rev()
            .take(2)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let after = haystack[at + needle.len()..].chars().next();
        if !number_bounded(&before, after, needle) {
            from = at + haystack[at..].chars().next().map_or(1, char::len_utf8);
            continue;
        }
        found.push(at);
        from = at + haystack[at..].chars().next().map_or(1, char::len_utf8);
        if found.len() >= 20 {
            break;
        }
    }
    found
}

/// Prepared page text for evidence checks.
pub struct PageEvidence {
    text: String,
}

impl PageEvidence {
    pub fn from_html(html: &str) -> Self {
        Self::from_visible_text(&visible_text(html))
    }

    pub fn from_visible_text(text: &str) -> Self {
        Self {
            text: normalized_compact(text),
        }
    }

    pub fn contains(&self, fragment: &str) -> bool {
        let needle = normalized_compact(fragment);
        !needle.is_empty() && !occurrences(&self.text, &needle).is_empty()
    }

    /// Signature of the neighbourhood of every occurrence of every fragment.
    pub fn window_signature(&self, quote: &str) -> Option<String> {
        let mut parts = Vec::new();
        for fragment in split_fragments(quote) {
            let needle = normalized_compact(&fragment);
            let hits = occurrences(&self.text, &needle);
            if hits.is_empty() {
                return None;
            }
            for hit in hits {
                let start = char_floor(&self.text, hit, WINDOW_RADIUS_CHARS);
                let end = char_ceil(&self.text, hit + needle.len(), WINDOW_RADIUS_CHARS);
                parts.push(value_signature(&self.text[start..end]));
            }
        }
        parts.sort();
        Some(parts.join("|"))
    }

    pub fn hash(&self) -> String {
        fnv_hash_hex(self.text.as_bytes())
    }

    /// The normalized compact page text every check runs on.
    pub(crate) fn text(&self) -> &str {
        &self.text
    }
}

// ── facts ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ages {
    pub min: Option<i32>,
    pub max: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FactOutcome {
    Confirmed {
        window_signature: String,
        adopted: bool,
    },
    QuoteMissing,
    FieldMismatch(String),
    WindowChanged,
    AdoptionExpired,
}

impl FactOutcome {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Confirmed { .. } => "confirmed",
            Self::QuoteMissing => "quote_missing",
            Self::FieldMismatch(_) => "field_mismatch",
            Self::WindowChanged => "window_changed",
            Self::AdoptionExpired => "adoption_expired",
        }
    }
}

/// Reading of a quote by the deterministic parser: an age range when the
/// quote states one that `parse_age` can read, otherwise presence only.
pub fn read_quote(quote: &str) -> (&'static str, Value) {
    match parse_age(quote) {
        Ok(reading) => (
            "age",
            json!({"min": reading.min, "max": reading.max, "rule": reading.rule}),
        ),
        Err(_) => ("presence", json!({})),
    }
}

/// Decides one fact against the live page. Pure.
pub fn evaluate_fact(
    page: &PageEvidence,
    quote: &str,
    baseline: Option<&str>,
    stored: Ages,
    adoption_open: bool,
) -> FactOutcome {
    if !split_fragments(quote).iter().all(|f| page.contains(f)) {
        return FactOutcome::QuoteMissing;
    }
    for fragment in split_fragments(quote) {
        if let Ok(reading) = parse_age(&fragment) {
            if (reading.min, reading.max) != (stored.min, stored.max) {
                return FactOutcome::FieldMismatch(format!(
                    "page reads age {:?}-{:?}, programme stores {:?}-{:?}",
                    reading.min, reading.max, stored.min, stored.max
                ));
            }
        }
    }
    let Some(window) = page.window_signature(quote) else {
        return FactOutcome::QuoteMissing;
    };
    match baseline {
        Some(previous) if super::values::same_values(previous, &window) => FactOutcome::Confirmed {
            window_signature: window,
            adopted: false,
        },
        Some(_) => FactOutcome::WindowChanged,
        None if adoption_open => FactOutcome::Confirmed {
            window_signature: window,
            adopted: true,
        },
        None => FactOutcome::AdoptionExpired,
    }
}

// ── verbatim validator for audit-backed data fixes ───────────────────────

static DIGITS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\d+").expect("valid"));
static SINGLE_QUOTED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"'([^']{2,300})'").expect("valid"));

fn digits_in(value: &str) -> Vec<String> {
    DIGITS
        .find_iter(value)
        .map(|m| m.as_str().to_string())
        .collect()
}

/// `anchor` must be verbatim page text recorded by the auditor: an exact
/// substring of the audit `quote`, or of a single-quoted page excerpt
/// inside the audit `page_says`.
pub fn validate_anchor(
    anchor: &str,
    audit_quote: &str,
    audit_page_says: &str,
) -> Result<(), String> {
    if anchor.trim().chars().count() < 2 {
        return Err("anchor is empty".into());
    }
    if !audit_quote.is_empty() && audit_quote.contains(anchor) {
        return Ok(());
    }
    if SINGLE_QUOTED
        .captures_iter(audit_page_says)
        .any(|c| c[1].contains(anchor))
    {
        return Ok(());
    }
    Err(format!("anchor {anchor:?} is not verbatim audit page text"))
}

/// New text written into a public field must be backed by its anchor: every
/// number it contains occurs in the anchor, and an age it states is read
/// identically from the anchor.
pub fn validate_replacement(new_text: &str, anchor: &str) -> Result<(), String> {
    let anchor_digits = digits_in(anchor);
    if let Some(missing) = digits_in(new_text)
        .into_iter()
        .find(|d| !anchor_digits.contains(d))
    {
        return Err(format!(
            "number {missing} in {new_text:?} is not in the anchor"
        ));
    }
    match (parse_age(new_text), parse_age(anchor)) {
        (Ok(new), Ok(source)) if (new.min, new.max) == (source.min, source.max) => Ok(()),
        (Ok(new), Ok(source)) => Err(format!(
            "age {:?}-{:?} differs from the anchor's {:?}-{:?}",
            new.min, new.max, source.min, source.max
        )),
        (Ok(_), Err(_)) => Err("the new text states an age the anchor does not".into()),
        (Err(_), _) => Ok(()),
    }
}

/// A structured age fix must equal what the parser reads from the anchor.
pub fn validate_age_fix(anchor: &str, min: Option<i32>, max: Option<i32>) -> Result<(), String> {
    let reading = parse_age(anchor).map_err(|e| format!("anchor has no readable age: {e}"))?;
    if (reading.min, reading.max) == (min, max) {
        Ok(())
    } else {
        Err(format!(
            "fix {min:?}-{max:?} differs from the anchor's {:?}-{:?}",
            reading.min, reading.max
        ))
    }
}

// ── audit fixture ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct AuditItem {
    pub id: Uuid,
    pub visible: bool,
    pub program_type: String,
    pub provider: String,
    pub title: String,
    pub official_url: Option<String>,
    pub min_age: Option<i32>,
    pub max_age: Option<i32>,
    #[serde(default)]
    pub school_scope: Value,
    pub verdict: String,
    #[serde(default)]
    pub fields_wrong: Vec<String>,
    #[serde(default)]
    pub page_says: Option<String>,
    #[serde(default)]
    pub quote: Option<String>,
    #[serde(default)]
    pub robots: Option<String>,
    #[serde(default)]
    pub http: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AuditFile {
    pub audited_at: NaiveDate,
    pub items: Vec<AuditItem>,
}

pub fn audit() -> &'static AuditFile {
    static AUDIT: LazyLock<AuditFile> = LazyLock::new(|| {
        serde_json::from_str(include_str!("audit_20261016.json")).expect("audit fixture parses")
    });
    &AUDIT
}

pub fn audit_adopt_before() -> DateTime<Utc> {
    let day = audit().audited_at + chrono::Duration::days(AUDIT_ADOPTION_DAYS);
    Utc.from_utc_datetime(&day.and_hms_opt(0, 0, 0).expect("midnight"))
}

/// Evidence fragments of one audit item, or why it carries none.
pub fn audit_fragments(item: &AuditItem) -> Result<Vec<String>, &'static str> {
    if !matches!(item.verdict.as_str(), "CORRECT" | "WRONG") {
        return Err("verdict_without_evidence");
    }
    let quote = item.quote.as_deref().unwrap_or("").trim();
    if quote.is_empty() {
        return Err("no_quote");
    }
    let fragments = split_fragments(quote);
    if fragments.is_empty()
        || fragments
            .iter()
            .any(|f| compact(f).chars().count() < MIN_FRAGMENT_CHARS || f.chars().count() > 300)
    {
        return Err("fragment_out_of_range");
    }
    Ok(fragments)
}

fn quote_key(program_id: Uuid, fragment: &str) -> String {
    hex(&Sha256::digest(
        format!("{program_id}:{}", compact(fragment)).as_bytes(),
    ))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Debug, Default)]
pub struct SeedSummary {
    pub items_seeded: usize,
    pub facts_inserted: u64,
    pub skipped: BTreeMap<&'static str, usize>,
}

/// Idempotent: inserts the audit's verbatim quotes as evidence for the
/// programmes the audit labelled CORRECT, and for WRONG programmes only
/// once their audited data fix has been applied. A programme is matched by
/// its id, or (fresh databases) by its exact official URL and title.
pub async fn seed_audit_evidence(pool: &PgPool) -> Result<SeedSummary> {
    let mut summary = SeedSummary::default();
    let adopt_before = audit_adopt_before();
    for item in &audit().items {
        let fragments = match audit_fragments(item) {
            Ok(fragments) => fragments,
            Err(reason) => {
                *summary.skipped.entry(reason).or_default() += 1;
                continue;
            }
        };
        let program_id: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM programs WHERE id = $1
             UNION ALL
             SELECT id FROM programs
             WHERE NOT EXISTS (SELECT 1 FROM programs WHERE id = $1)
               AND official_url = $2 AND title = $3
               AND (SELECT count(*) FROM programs WHERE official_url = $2 AND title = $3) = 1
             LIMIT 1",
        )
        .bind(item.id)
        .bind(item.official_url.as_deref())
        .bind(&item.title)
        .fetch_optional(pool)
        .await?;
        let Some(program_id) = program_id else {
            *summary.skipped.entry("program_not_found").or_default() += 1;
            continue;
        };
        // Retired, closed and opted-out programmes never get evidence: a
        // seed must not become the way back for an offer that was withdrawn.
        let excluded: Option<&'static str> = sqlx::query_scalar(
            "SELECT CASE
                 WHEN p.retirement_reason IS NOT NULL OR p.program_status = 'closed' THEN 'program_retired'
                 WHEN EXISTS (SELECT 1 FROM program_source_holds h
                              WHERE h.program_id = p.id AND h.resolved_at IS NULL AND h.hold_reason = 'publisher_optout')
                     THEN 'opted_out'
                 WHEN EXISTS (
                     SELECT 1 FROM benefit_source_optouts o
                     JOIN benefit_source_candidates c ON c.domain_id = ANY(o.domain_ids)
                     JOIN program_source_monitors m ON m.candidate_id = c.id AND m.program_id = p.id
                     WHERE o.lifted_at IS NULL AND o.effective_at IS NOT NULL
                       AND (o.url_prefix IS NULL OR starts_with(c.url, o.url_prefix))
                 ) THEN 'opted_out'
             END
             FROM programs p WHERE p.id = $1",
        )
        .bind(program_id)
        .fetch_one(pool)
        .await
        .map(|reason: Option<String>| reason.map(|r| if r == "opted_out" { "opted_out" } else { "program_retired" }))?;
        if let Some(reason) = excluded {
            *summary.skipped.entry(reason).or_default() += 1;
            continue;
        }
        if item.verdict == "WRONG" {
            let fixed: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM corporate_audit_fix_backup WHERE program_id = $1)",
            )
            .bind(program_id)
            .fetch_one(pool)
            .await?;
            if !fixed {
                *summary.skipped.entry("wrong_not_fixed").or_default() += 1;
                continue;
            }
        }
        let mut inserted = 0;
        for fragment in &fragments {
            inserted += insert_seed_fragment(pool, program_id, fragment, adopt_before).await?;
        }
        summary.facts_inserted += inserted;
        summary.items_seeded += 1;
    }
    Ok(summary)
}

/// Issuer-paid offers reviewed by hand against the issuer's own page
/// (`20261107000002_issuer_seeds_skt_youth.sql`): their visible statements
/// become evidence the way the audit's do, under the same adoption window.
#[derive(Debug, Clone, Deserialize)]
pub struct IssuerSeedFile {
    pub reviewed_at: NaiveDate,
    pub source: String,
    pub items: Vec<IssuerSeedItem>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct IssuerSeedItem {
    pub id: Uuid,
    pub title: String,
    pub official_url: String,
    pub quote: String,
}

pub fn issuer_seeds() -> &'static IssuerSeedFile {
    static SEEDS: LazyLock<IssuerSeedFile> = LazyLock::new(|| {
        serde_json::from_str(include_str!("issuer_seeds_20261106.json"))
            .expect("issuer seed file parses")
    });
    &SEEDS
}

/// Idempotent: the issuer seeds' fragments as evidence of their programmes,
/// matched by id and official URL. A retired, closed or opted-out programme
/// gets none (as for the audit).
pub async fn seed_issuer_evidence(pool: &PgPool) -> Result<u64> {
    let seeds = issuer_seeds();
    let day = seeds.reviewed_at + chrono::Duration::days(AUDIT_ADOPTION_DAYS);
    let adopt_before = Utc.from_utc_datetime(&day.and_hms_opt(0, 0, 0).expect("midnight"));
    let mut inserted = 0;
    for item in &seeds.items {
        let eligible: bool = sqlx::query_scalar(
            "SELECT EXISTS (
                 SELECT 1 FROM programs p
                 WHERE p.id = $1 AND p.official_url = $2
                   AND p.retirement_reason IS NULL AND p.program_status <> 'closed'
                   AND NOT EXISTS (SELECT 1 FROM program_source_holds h
                                   WHERE h.program_id = p.id AND h.resolved_at IS NULL
                                     AND h.hold_reason = 'publisher_optout'))",
        )
        .bind(item.id)
        .bind(&item.official_url)
        .fetch_one(pool)
        .await?;
        if !eligible {
            continue;
        }
        for fragment in split_fragments(&item.quote) {
            inserted +=
                insert_seed_fragment_from(pool, item.id, &fragment, adopt_before, &seeds.source)
                    .await?;
        }
    }
    Ok(inserted)
}

/// Inserts one audit fragment as evidence unless it is already there or a
/// live relocation or applied update replaced it (it is then in some fact's
/// `anchors.origin_quote` / `previous_quotes`): re-inserting it would bring
/// back a quote the page no longer has and hide the offer again.
async fn insert_seed_fragment(
    pool: &PgPool,
    program_id: Uuid,
    fragment: &str,
    adopt_before: DateTime<Utc>,
) -> Result<u64> {
    insert_seed_fragment_from(pool, program_id, fragment, adopt_before, AUDIT_SOURCE).await
}

async fn insert_seed_fragment_from(
    pool: &PgPool,
    program_id: Uuid,
    fragment: &str,
    adopt_before: DateTime<Utc>,
    source: &str,
) -> Result<u64> {
    let (fact_type, value) = read_quote(fragment);
    Ok(sqlx::query(
        "INSERT INTO corporate_offer_facts (
                     program_id, fact_type, value, quote, quote_key, channel,
                     evidence_source, pass_b, status, adopt_before
                 )
                 SELECT $1, $2, $3, $4, $5, 'page', $6, $7, 'published', $8
                 WHERE NOT EXISTS (
                     SELECT 1 FROM corporate_offer_facts f
                     WHERE f.program_id = $1
                       AND (f.anchors->>'origin_quote' = $4 OR f.anchors->'previous_quotes' ? $4)
                 )
                 ON CONFLICT (program_id, quote_key) DO NOTHING",
    )
    .bind(program_id)
    .bind(fact_type)
    .bind(&value)
    .bind(fragment)
    .bind(quote_key(program_id, fragment))
    .bind(source)
    .bind(json!({"parser": crate::ai_extraction::crosscheck::PARSER_VERSION, "reading": value}))
    .bind(adopt_before)
    .execute(pool)
    .await?
    .rows_affected())
}

/// Moves a candidate's monitors to `evidence_auto` when every programme on
/// it carries evidence, its domain passes [`policy::ELIGIBLE_DOMAIN_SQL`]
/// and no census verdict ([`policy::census_verdicts_sql`], at most
/// [`CENSUS_MAX_AGE_DAYS`] old) rates its URL other than direct HTML. With
/// no verdict the evidence job rates the page itself before it reads
/// anything ([`fetch_target`]), so promotion never waits for a census that
/// has not seen the URL. Fully automatic; never demotes.
pub async fn promote_monitors(pool: &PgPool) -> Result<u64> {
    Ok(sqlx::query(&format!(
        "WITH refused AS (
             SELECT entry.candidate_id FROM ({verdicts}) entry WHERE NOT {census_direct}
         ), eligible AS (
             SELECT monitor.candidate_id
             FROM program_source_monitors monitor
             LEFT JOIN (SELECT DISTINCT program_id FROM corporate_offer_facts WHERE status <> 'retired') fact
               ON fact.program_id = monitor.program_id
             WHERE monitor.verification_state <> 'retired'
             GROUP BY monitor.candidate_id
             HAVING bool_and(fact.program_id IS NOT NULL)
         )
         UPDATE program_source_monitors monitor
         SET verification_mode = 'evidence_auto',
             -- A current monitor is shown under a live review deadline:
             -- evidence taking it over starts its grace window, so a first
             -- 5xx does not hide it before evidence ever read it.
             evidence_verified_at = CASE WHEN monitor.verification_state = 'current'
                                         THEN COALESCE(monitor.evidence_verified_at, now())
                                         ELSE monitor.evidence_verified_at END,
             updated_at = now()
         FROM eligible
         JOIN benefit_source_candidates candidate ON candidate.id = eligible.candidate_id
         JOIN benefit_source_domains d ON d.id = candidate.domain_id
         WHERE monitor.candidate_id = eligible.candidate_id
           AND NOT EXISTS (SELECT 1 FROM refused WHERE refused.candidate_id = eligible.candidate_id)
           AND {eligible_domain}
           -- A hub's list page is read only by the door job (review A-M2).
           AND NOT {hub}
           AND monitor.verification_state <> 'retired'
           AND monitor.verification_mode = 'human'",
        census_direct = policy::CENSUS_DIRECT_SQL,
        verdicts = policy::census_verdicts_sql("$1"),
        eligible_domain = policy::ELIGIBLE_DOMAIN_SQL,
        hub = policy::hub_owned("candidate"),
    ))
    .bind(CENSUS_MAX_AGE_DAYS as i32)
    .execute(pool)
    .await
    .context("failed to promote evidence monitors")?
    .rows_affected())
}

// ── baselines and the reader that wrote them ──────────────────────────────

/// The version of the value-window reader ([`PageEvidence::window_signature`]
/// with [`super::values::same_values`]) whose readings are stored as
/// baselines. Bump it with any change to how a window is read; the release
/// gate (test `reader_versions_are_bumped_with_the_reader`) fails until it
/// is bumped. A baseline of
/// another version is re-baselined on the same page bytes, otherwise held
/// for fresh extraction ([`assess`]). A window stored before versions were
/// written is read as this version's: the reader is unchanged since.
pub const WINDOW_READER: &str = "w1:";

/// A window reading as stored: with this reader's version.
pub fn stored_window(window: &str) -> String {
    format!(
        "{WINDOW_READER}{}",
        window_baseline(Some(window))
            .ok()
            .flatten()
            .unwrap_or(window)
    )
}

/// A baseline another reader version wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OtherReader;

/// A stored window as this reader reads it: `Ok(Some(reading))` for this
/// version (or none written), `Ok(None)` for no baseline, [`OtherReader`]
/// for a window another reader version wrote.
pub fn window_baseline(stored: Option<&str>) -> Result<Option<&str>, OtherReader> {
    static VERSIONED: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^w[0-9]+:").expect("valid version pattern"));
    match stored {
        None => Ok(None),
        Some(window) => match window.strip_prefix(WINDOW_READER) {
            Some(reading) => Ok(Some(reading)),
            None if VERSIONED.is_match(window) => Err(OtherReader),
            None => Ok(Some(window)),
        },
    }
}

// ── the job ──────────────────────────────────────────────────────────────

#[derive(Debug, Default, Clone)]
pub struct VerifySummary {
    pub targets: usize,
    pub decisions: BTreeMap<String, usize>,
    pub applied: usize,
    pub requests: u32,
    /// Programme decisions by class, with the grace counts.
    pub health: RunHealth,
}

#[derive(Debug, FromRow)]
struct Target {
    candidate_id: Uuid,
    url: String,
    domain_id: Uuid,
    gray_robots_missing: bool,
    gray_crawl_delay_min: bool,
    min_request_interval_ms: i32,
    max_pages_per_day: i32,
    consecutive_failures: i32,
    first_gone_at: Option<DateTime<Utc>>,
    last_outcome: Option<String>,
    last_checked_at: Option<DateTime<Utc>>,
    census_direct: bool,
    /// A census verdict ([`policy::census_verdicts_sql`]) exists for the
    /// URL, direct or not. Without one the reader rates the page itself.
    census_rated: bool,
    /// The domain's terms signal from its latest census, if one read it.
    census_tos: Option<String>,
    gray_generic_tos: bool,
    feed_active: bool,
    domain_eligible: bool,
}

#[derive(Debug, Clone, FromRow)]
struct FactRow {
    id: Uuid,
    program_id: Uuid,
    quote: String,
    anchors: Value,
    window_signature: Option<String>,
    shadow_window_signature: Option<String>,
    adopt_before: Option<DateTime<Utc>>,
    min_age: Option<i32>,
    max_age: Option<i32>,
}

/// Where an evidence page came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Origin {
    /// The source URL itself, or the page its redirect leads to: what a
    /// reader following the stored link sees. A confirmation counts at once,
    /// also while a redirect's new URL waits for adoption.
    Source,
    /// Another page of the site, found because robots closes the source
    /// page to us. It confirms only by exact quotes and only once its URL is
    /// adopted (two agreeing sightings 24 h apart); until then the offer
    /// waits out its grace window as a block.
    Fallback,
    /// Another page of the site, found after the source page went 404/410:
    /// the stored link is broken, so the offer is hidden until the new URL
    /// is adopted.
    Rediscovered,
}

/// Candidate-level result of one fetch.
#[derive(Debug, Clone, PartialEq)]
enum PageResult {
    Evidence {
        page: String,
        html: String,
        hash: String,
        http_status: u16,
        /// The URL the page was read from: the source URL, or a same-host
        /// redirect target or rediscovered URL awaiting adoption.
        final_url: String,
        via: &'static str,
        /// Where the page came from, which decides what a confirmation on
        /// it may do before a moved URL is adopted.
        origin: Origin,
    },
    Outcome {
        outcome: &'static str,
        http_status: Option<u16>,
        detail: String,
    },
}

/// Runs one bounded evidence pass. Shadow mode only writes
/// `corporate_evidence_checks` and its own schedule; live mode also hides,
/// restores and re-dates offers and resolves the matching source events.
pub async fn run(pool: &PgPool, flags: &Flags, http: &dyn PageHttp) -> Result<VerifySummary> {
    run_with_publish(
        pool,
        flags.verify_mode,
        flags.publish_enabled,
        flags.gray_generic_tos,
        http,
        None,
        None,
    )
    .await
}

/// `only` limits the pass to these sources. `test_spacing` replaces the
/// 10 s floor and the shared DB slot; only tests pass it.
#[cfg(test)]
pub(crate) async fn run_with(
    pool: &PgPool,
    mode: VerifyMode,
    http: &dyn PageHttp,
    only: Option<Vec<Uuid>>,
    test_spacing: Option<std::time::Duration>,
) -> Result<VerifySummary> {
    run_with_publish(
        pool,
        mode,
        mode == VerifyMode::Live,
        false,
        http,
        only,
        test_spacing,
    )
    .await
}

/// `publish`: whether live may adopt a moved page's URL
/// ([`Flags::publish_enabled`]); the reader's decisions apply either way.
/// `gray_generic_tos`: the global G1 switch ([`Flags::gray_generic_tos`]),
/// read with the domain's own when the reader rates a page itself.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_with_publish(
    pool: &PgPool,
    mode: VerifyMode,
    publish: bool,
    gray_generic_tos: bool,
    http: &dyn PageHttp,
    only: Option<Vec<Uuid>>,
    test_spacing: Option<std::time::Duration>,
) -> Result<VerifySummary> {
    let seeded = seed_audit_evidence(pool).await?;
    let issuer_facts = seed_issuer_evidence(pool).await?;
    if issuer_facts > 0 {
        info!(
            event = "corporate_evidence.issuer_seeded",
            issuer_facts, "issuer offers' reviewed statements seeded as evidence"
        );
    }
    let promoted = promote_monitors(pool).await?;
    if mode == VerifyMode::Live {
        let settled = super::events::settle_census_blocked(pool).await?;
        if settled > 0 {
            info!(
                event = "corporate_evidence.events_settled",
                settled, "source events of census-blocked hidden sources resolved"
            );
        }
    }
    let rescheduled = reschedule_stale(pool).await?;
    if rescheduled > 0 {
        info!(
            event = "corporate_evidence.rescheduled",
            rescheduled, "sources whose stored outcome is no longer a verdict are due now"
        );
    }
    if seeded.facts_inserted > 0 || promoted > 0 {
        info!(
            facts_inserted = seeded.facts_inserted,
            monitors_promoted = promoted,
            "corporate evidence seeded and promoted"
        );
    }
    sqlx::query(
        "INSERT INTO corporate_evidence_targets (candidate_id)
         SELECT DISTINCT candidate_id FROM program_source_monitors
         WHERE verification_mode = 'evidence_auto' AND verification_state <> 'retired'
         ON CONFLICT (candidate_id) DO NOTHING",
    )
    .execute(pool)
    .await?;

    let targets = sqlx::query_as::<_, Target>(&format!(
        "SELECT target.candidate_id, candidate.url, d.id AS domain_id,
                d.gray_robots_missing, d.gray_crawl_delay_min,
                d.min_request_interval_ms, d.max_pages_per_day,
                target.consecutive_failures, target.first_gone_at,
                target.last_outcome, target.last_checked_at,
                EXISTS (
                    SELECT 1 FROM ({verdicts}) entry
                    WHERE entry.candidate_id = target.candidate_id AND {census_direct}
                ) AS census_direct,
                EXISTS (
                    SELECT 1 FROM ({verdicts}) entry WHERE entry.candidate_id = target.candidate_id
                ) AS census_rated,
                (SELECT domain_entry.tos_signal FROM corporate_census_entries domain_entry
                 JOIN corporate_census_runs run ON run.id = domain_entry.run_id
                 WHERE domain_entry.domain_id = d.id AND domain_entry.subject_kind = 'domain'
                   AND run.status = 'completed' AND run.finished_at > now() - make_interval(days => $2)
                   AND domain_entry.tos_signal NOT IN ('unknown', 'unfetchable')
                 ORDER BY run.finished_at DESC LIMIT 1) AS census_tos,
                d.gray_generic_tos,
                EXISTS (SELECT 1 FROM benefit_feeds feed WHERE feed.domain_id = d.id AND feed.is_active
                        AND feed.authorized_until > now()) AS feed_active,
                {eligible} AS domain_eligible
         FROM corporate_evidence_targets target
         JOIN benefit_source_candidates candidate ON candidate.id = target.candidate_id
         JOIN benefit_source_domains d ON d.id = candidate.domain_id
         WHERE (target.next_check_at <= now() OR {deadline_due})
           AND ($3::uuid[] IS NULL OR target.candidate_id = ANY($3))
           AND NOT {hub}
           AND EXISTS (
               SELECT 1 FROM program_source_monitors monitor
               WHERE monitor.candidate_id = target.candidate_id
                 AND monitor.verification_mode = 'evidence_auto'
                 AND monitor.verification_state <> 'retired'
           )
         ORDER BY d.id, target.next_check_at
         LIMIT $1",
        eligible = policy::ELIGIBLE_DOMAIN_SQL,
        census_direct = policy::CENSUS_DIRECT_SQL,
        verdicts = policy::census_verdicts_sql("$2"),
        deadline_due = DEADLINE_DUE_SQL,
        hub = policy::hub_owned("candidate"),
    ))
    .bind(MAX_TARGETS_PER_RUN)
    .bind(CENSUS_MAX_AGE_DAYS as i32)
    .bind(only)
    .fetch_all(pool)
    .await
    .context("failed to select evidence targets")?;

    // Every daily schedule counts from the hour this run started, so a
    // target read late in the run is due at the same run the next day.
    // The database clock, which every check time is written with.
    let run_started: DateTime<Utc> = sqlx::query_scalar("SELECT now()")
        .fetch_one(pool)
        .await
        .context("failed to read the database clock")?;
    let mut summary = VerifySummary {
        targets: targets.len(),
        ..VerifySummary::default()
    };
    let mut index = 0;
    while index < targets.len() {
        let domain_id = targets[index].domain_id;
        let first = &targets[index];
        let shared_slot = test_spacing.is_none();
        let mut session = DomainSession::new(
            http,
            shared_slot.then_some(pool),
            shared_slot.then_some(domain_id),
            first.gray_crawl_delay_min,
            first.min_request_interval_ms as i64,
            first.max_pages_per_day.clamp(1, 200) as u32,
        );
        if let Some(spacing) = test_spacing {
            session = session.with_test_spacing(spacing);
        }
        while index < targets.len() && targets[index].domain_id == domain_id {
            let target = &targets[index];
            index += 1;
            let result = fetch_target(pool, &mut session, target, gray_generic_tos).await;
            // The weekly audit reads the page just fetched instead of
            // fetching it again, or records the outcome when there is no
            // page (measurement only; off unless its flag is on). It runs
            // in its own task and never affects this pass.
            if super::weekly_audit::hook_enabled() {
                let read = match &result {
                    PageResult::Evidence {
                        html, final_url, ..
                    } => super::weekly_audit::EvidenceRead::Page {
                        html: html.clone(),
                        final_url: final_url.clone(),
                    },
                    PageResult::Outcome { outcome, .. } => {
                        super::weekly_audit::EvidenceRead::Outcome(outcome.to_string())
                    }
                };
                super::weekly_audit::observe_evidence(pool, mode, target.candidate_id, read);
            }
            let decided = decide_and_record(
                pool,
                mode,
                publish,
                run_started,
                target,
                &result,
                &mut summary.health,
            )
            .await?;
            for (label, applied) in decided {
                *summary.decisions.entry(label).or_default() += 1;
                summary.applied += usize::from(applied);
            }
        }
        summary.requests += session.requests();
    }
    Ok(summary)
}

/// `reextract_reason` of a held source with several programmes: no
/// automatic exit, the door lifecycle or a person settles it.
pub(crate) const MULTI_OFFER_SOURCE: &str = "multi_offer_source";
/// `reextract_reason` of a held source that rereading republished
/// [`VOLATILE_REPUBLISHES`] times in 30 days: it is not queued again.
pub(crate) const VOLATILE_PAGE: &str = "volatile_page";
/// Republishes in 30 days after which a held source is volatile.
pub(crate) const VOLATILE_REPUBLISHES: i64 = 3;

/// Why a URL no census rated is not read: the census's own wait, written
/// before the reader rated pages itself. A source still parked on it is
/// due at once ([`reschedule_stale`]).
pub(crate) const LEGACY_CENSUS_WAIT_DETAIL: &str =
    "no completed census within 30 days rates this URL direct HTML";

/// One read of a target, under one decision for "may this reader read the
/// URL as direct HTML" (review A-H2): for a URL no census rated, what is
/// known without a request (a partner feed, a stored terms signal) refuses
/// before any request, and every page that becomes evidence, wherever it was
/// read, is rated by the census's rule ([`rate_own_read`]) first.
async fn fetch_target(
    pool: &PgPool,
    session: &mut DomainSession<'_>,
    target: &Target,
    gray_generic_tos: bool,
) -> PageResult {
    let result = read_target(pool, session, target, gray_generic_tos).await;
    let PageResult::Evidence {
        page,
        html,
        final_url,
        http_status,
        ..
    } = &result
    else {
        return result;
    };
    if target.census_rated {
        return result;
    }
    let Ok(read_url) = Url::parse(final_url) else {
        return PageResult::Outcome {
            outcome: "unsupported",
            http_status: Some(*http_status),
            detail: "invalid URL".into(),
        };
    };
    let state = PageState::Html {
        text_chars: page.chars().count(),
    };
    let (lane, reason) = rate_own_read(
        pool,
        session,
        target,
        gray_generic_tos,
        html,
        &read_url,
        &state,
    )
    .await;
    if (lane, reason) == (policy::Lane::Direct, policy::LaneReason::DirectHtml) {
        return result;
    }
    PageResult::Outcome {
        outcome: "policy_blocked",
        http_status: Some(*http_status),
        detail: format!(
            "this reader rates the page {}:{}",
            lane.as_str(),
            reason.as_str()
        ),
    }
}

/// [`fetch_target`] before the rating of the page read.
async fn read_target(
    pool: &PgPool,
    session: &mut DomainSession<'_>,
    target: &Target,
    gray_generic_tos: bool,
) -> PageResult {
    let outcome =
        |outcome: &'static str, http_status: Option<u16>, detail: &str| PageResult::Outcome {
            outcome,
            http_status,
            detail: detail.chars().take(300).collect(),
        };
    let Ok(url) = Url::parse(&target.url) else {
        return outcome("unsupported", None, "invalid URL");
    };
    match policy::opted_out(pool, &url).await {
        Ok(false) => {}
        Ok(true) => return outcome("opted_out", None, "publisher opt-out"),
        Err(error) => return outcome("fetch_failed", None, &error.to_string()),
    }
    if !target.domain_eligible {
        // Our own policy withdrew the source: refuted, hidden now.
        return outcome(
            "domain_withdrawn",
            None,
            "the domain is rejected, blocked, restricted or serves no corporate benefit",
        );
    }
    // One decision for "may this reader read the URL as direct HTML": a
    // census verdict that rates it otherwise stops it without a request;
    // with no verdict the reader rates the page it fetches by the same rule
    // ([`policy::rate_lane`]) before reading anything from it.
    if target.census_rated && !target.census_direct {
        return outcome(
            "policy_blocked",
            None,
            "the latest census verdict rates this URL other than direct HTML",
        );
    }
    if !target.census_rated {
        let stored_tos = target.census_tos.as_deref().and_then(tos_from_str);
        if let Some((lane, reason)) = policy::refused_without_request(
            target.feed_active,
            stored_tos,
            gray_generic_tos && target.gray_generic_tos,
        ) {
            return outcome(
                "policy_blocked",
                None,
                &format!(
                    "known before any request: {}:{}",
                    lane.as_str(),
                    reason.as_str()
                ),
            );
        }
    }
    let policy::Resolved {
        mut fetched,
        mut state,
        url: mut current,
    } = policy::fetch_resolving_same_host(
        session,
        &url,
        target.gray_robots_missing,
        Some(&policy::LiveOptOuts(pool)),
    )
    .await;
    // A redirect to another host of the same registrable domain (a brand's
    // own subdomain) is followed once under the same policy: robots for
    // that host, the opt-out check, the same polite session, for a URL a
    // census rated direct HTML on its own host. An unrated URL is not
    // followed off its host (the census never leaves the host). Off the
    // site it is not followed.
    if let PageState::Redirect {
        location,
        same_host: false,
    } = &state
    {
        if let Some(next) = current
            .join(location)
            .ok()
            .filter(|next| follows_off_host(target.census_rated, &url, next))
        {
            match target_domain_eligible(pool, &next).await {
                Ok(true) => {}
                Ok(false) => {
                    return outcome(
                        "redirected",
                        None,
                        "redirect to a host our domain policy does not cover",
                    )
                }
                Err(error) => return outcome("fetch_failed", None, &error.to_string()),
            }
            match policy::opted_out(pool, &next).await {
                Ok(false) => {}
                Ok(true) => {
                    return outcome(
                        "opted_out",
                        None,
                        "publisher opt-out on the redirect target",
                    )
                }
                // The check failed, not the publisher: retried, never held.
                Err(error) => return outcome("fetch_failed", None, &error.to_string()),
            }
            let resolved = policy::fetch_resolving_same_host(
                session,
                &next,
                target.gray_robots_missing,
                Some(&policy::LiveOptOuts(pool)),
            )
            .await;
            (fetched, state, current) = (resolved.fetched, resolved.state, resolved.url);
        }
    }
    let status = match &fetched {
        Fetched::Reply(reply) => Some(reply.status),
        _ => None,
    };
    match state {
        PageState::Html { .. } => {
            let Fetched::Reply(reply) = fetched else {
                return outcome("fetch_failed", None, "missing reply");
            };
            let html = match crate::sources::benefit_pages::decode_page(
                &reply.body,
                &reply.content_type,
            ) {
                Ok(html) => html,
                Err(error) => return outcome("unsupported", status, &error.to_string()),
            };
            let page = visible_text(&html);
            let hash = PageEvidence::from_visible_text(&page).hash();
            PageResult::Evidence {
                page,
                html,
                hash,
                http_status: reply.status,
                via: if current == url { "direct" } else { "redirect" },
                final_url: current.to_string(),
                origin: Origin::Source,
            }
        }
        // A loop is a misconfiguration we retry, not a statement.
        PageState::Redirect {
            same_host: true, ..
        } => outcome(
            "fetch_failed",
            status,
            "redirect loop or too many redirects",
        ),
        PageState::Redirect { .. } => outcome("redirected", status, "redirect off the site"),
        // The page is closed to us, not gone: the same offer on another
        // page of the site, found by its sitemap, still
        // confirms it.
        // Rediscovery finds again an offer a census-rated read confirmed;
        // an unrated URL has no such confirmation to look for.
        PageState::Disallowed if !target.census_rated => outcome(
            "robots_disallowed",
            None,
            "robots.txt disallows this path for MazimiBenefitBot",
        ),
        PageState::Disallowed => {
            match rediscover(pool, session, target, &current, Origin::Fallback).await {
                Some(found) => found,
                None => outcome(
                    "robots_disallowed",
                    None,
                    "robots.txt disallows this path for MazimiBenefitBot",
                ),
            }
        }
        PageState::BotWall => outcome(
            "blocked",
            status,
            "401/403 or challenge page; automated access stopped",
        ),
        PageState::RateLimited(_) => outcome("rate_limited", status, "HTTP 429; backing off"),
        PageState::Gone if !target.census_rated => outcome("gone", status, "HTTP 404/410"),
        PageState::Gone => {
            match rediscover(pool, session, target, &current, Origin::Rediscovered).await {
                Some(found) => found,
                None => outcome("gone", status, "HTTP 404/410"),
            }
        }
        PageState::Transient => outcome("fetch_failed", status, "transient failure"),
        PageState::Unsupported => outcome("unsupported", status, "unsupported response"),
        PageState::NotProbed => match fetched {
            Fetched::Stopped(reason)
                if reason.starts_with("robots_forbidden")
                    || reason.starts_with("robots_challenge")
                    || reason == "robots_html"
                    || reason == "robots_not_text"
                    || reason == "page_bot_wall" =>
            {
                outcome("blocked", None, reason)
            }
            Fetched::Stopped("opt_out") => outcome(
                "opted_out",
                None,
                "publisher opt-out on the redirect target",
            ),
            Fetched::Stopped("rate_limited") => {
                outcome("rate_limited", None, "domain rate limited")
            }
            Fetched::Stopped(reason) => outcome("fetch_failed", None, reason),
            Fetched::BudgetExhausted => {
                outcome("fetch_failed", None, "per-run request cap reached")
            }
            Fetched::Failed(message) => outcome("fetch_failed", None, &message),
            Fetched::Reply(_) => outcome("fetch_failed", None, "unexpected reply"),
        },
    }
}

/// The lane of a page this reader fetched for a URL no census rated, by
/// the census's rule ([`policy::rate_lane`]): the opt-out, a partner feed,
/// robots for our agent on the page's own path (cached in the session, no
/// new request), the domain's terms signal (its latest census reading, or
/// the terms page read now through the same session), the page's visible
/// length.
async fn rate_own_read(
    pool: &PgPool,
    session: &mut DomainSession<'_>,
    target: &Target,
    gray_generic_tos: bool,
    html: &str,
    page_url: &Url,
    state: &PageState,
) -> (policy::Lane, policy::LaneReason) {
    let optouts = policy::LiveOptOuts(pool);
    let opted_out = policy::OptOutCheck::covers(&optouts, page_url).await;
    let robots = session.robots(page_url).await;
    let tos = match target.census_tos.as_deref().and_then(tos_from_str) {
        Some(tos) => tos,
        None => {
            policy::terms_signal(
                session,
                html,
                page_url,
                target.gray_robots_missing,
                &optouts,
            )
            .await
        }
    };
    policy::rate_lane(&policy::LaneInput {
        opted_out,
        feed_active: target.feed_active,
        robots: &robots,
        path_allowed: robots.allows(&policy::robots_target(page_url), target.gray_robots_missing),
        page: state,
        tos,
        gray_generic_tos: gray_generic_tos && target.gray_generic_tos,
        gray_robots_missing: target.gray_robots_missing,
    })
}

/// A stored terms signal ([`policy::TosSignal::as_str`]).
fn tos_from_str(raw: &str) -> Option<policy::TosSignal> {
    use policy::TosSignal as T;
    [
        T::None,
        T::LoginOnly,
        T::GenericAutomated,
        T::SpecificCollection,
        T::Unfetchable,
        T::NotFound,
        T::Unknown,
    ]
    .into_iter()
    .find(|signal| signal.as_str() == raw)
}

/// Where a hidden or failing offer goes next: the one automatic exit of
/// its hold class, and the one queue that leads there.
///
/// | class | exit | queue |
/// |---|---|---|
/// | the page no longer states the fact as stored (quote gone, value window or field changed, section changed, a date or an ending read, an exclusion, a baseline of another reader, an adoption that ran out, no facts) | [`Exit::Reread`]: intake's held-source lane re-derives the offer from the page through the whole chain of a new offer (policy, readers A and B, scope, ending scan, 24 h stability) and republishes it; nothing is restored from the old fact | `corporate_evidence_targets.reextract_requested_at`, set here, cleared at the next full confirmation |
/// | we could not or may not read the page now (5xx, 429, robots, a wall, the lane, a domain withdrawn by our own policy, a 404 not yet confirmed, a URL move awaiting agreement) | [`Exit::Wait`]: this job reads it again on its own schedule; a confirmation restores it | `corporate_evidence_targets.next_check_at` |
/// | a publisher opt-out, a redirect off the site, a programme retired | [`Exit::Terminal`]: no automatic exit (the opt-out flow lifts its own hold; a moved brand needs its new domain) | none |
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// Confirmed: shown, nothing to leave.
    Shown,
    Reread,
    Wait,
    Terminal,
}

/// The exit of one fact's reading. Exhaustive over [`Resolution`] and
/// [`FactOutcome`]: a reading added later is given an exit before it
/// compiles. Rereading an ending or a date is safe: the chain must find a
/// current offer on the page, pass the ending scan and stand a day.
pub fn fact_exit(resolution: &Resolution) -> Exit {
    match resolution {
        Resolution::Exact(FactOutcome::Confirmed { .. })
        | Resolution::Fuzzy { .. }
        | Resolution::Relocated { .. } => Exit::Shown,
        Resolution::Exact(
            FactOutcome::QuoteMissing
            | FactOutcome::FieldMismatch(_)
            | FactOutcome::WindowChanged
            | FactOutcome::AdoptionExpired,
        )
        | Resolution::Proposed(_)
        | Resolution::Ambiguous(_) => Exit::Reread,
    }
}

/// Every candidate-level outcome this job writes ([`fetch_target`],
/// [`decide_and_record`]), for [`source_exit`]'s completeness test.
pub const SOURCE_OUTCOMES: &[&str] = &[
    "confirmed",
    "url_adopted",
    "url_move_proposed",
    "evidence_failed",
    "adoption_expired",
    "no_evidence",
    "fetch_failed",
    "rate_limited",
    "unsupported",
    "robots_disallowed",
    "blocked",
    "policy_blocked",
    "domain_withdrawn",
    "gone",
    "retired_gone",
    "opted_out",
    "redirected",
];

/// The exit of a source-level outcome (no page, or a programme with no
/// facts). An outcome not in [`SOURCE_OUTCOMES`] has no automatic exit
/// (fail closed).
pub fn source_exit(outcome: &str) -> Exit {
    match outcome {
        "confirmed" | "url_adopted" => Exit::Shown,
        // Per fact: [`fact_exit`] decides; the source's own wait meanwhile.
        "evidence_failed" | "adoption_expired" => Exit::Reread,
        "no_evidence" => Exit::Reread,
        "url_move_proposed" | "fetch_failed" | "rate_limited" | "unsupported"
        | "robots_disallowed" | "blocked" | "policy_blocked" | "domain_withdrawn" | "gone" => {
            Exit::Wait
        }
        _ => Exit::Terminal,
    }
}

/// Hold reason for a refuted candidate-level outcome, which hides its
/// programmes at once. Transient and blocked outcomes hide only once their
/// grace window ran out ([`reverify::grace_hold`]).
fn hide_reason(outcome: &str) -> Option<&'static str> {
    match outcome {
        // Lifted only by the opt-out flow once the publisher lifts it.
        "opted_out" => Some("publisher_optout"),
        // Our own domain policy withdrew the source.
        "domain_withdrawn" => Some("access_revoked"),
        "gone" => Some("not_found"),
        "redirected" => Some("redirected"),
        _ => None,
    }
}

/// A target is due by its review deadline as well as by its own schedule:
/// a current evidence_auto monitor whose deadline is less than a day away
/// (or passed) is checked at most hourly, so no monitor sits past its
/// deadline unchecked while the job runs. A policy outcome (robots, block,
/// opt-out) and a rate limit keep their own wait. Health counts against the same rule.
pub(crate) const DEADLINE_DUE_SQL: &str = "(
    EXISTS (SELECT 1 FROM program_source_monitors due
            WHERE due.candidate_id = target.candidate_id
              AND due.verification_mode = 'evidence_auto'
              AND due.verification_state = 'current'
              AND due.review_due_at <= now() + interval '1 day')
    AND (target.last_checked_at IS NULL OR target.last_checked_at <= now() - interval '1 hour')
    AND target.last_outcome IS DISTINCT FROM 'robots_disallowed'
    AND target.last_outcome IS DISTINCT FROM 'blocked'
    AND target.last_outcome IS DISTINCT FROM 'opted_out'
    AND target.last_outcome IS DISTINCT FROM 'policy_blocked'
    AND target.last_outcome IS DISTINCT FROM 'domain_withdrawn'
    AND target.last_outcome IS DISTINCT FROM 'rate_limited')";

/// When a target is due again: `delay` after the hour the run started (a
/// run lasts minutes, so this is 23 to 24 h after the check for a day),
/// never sliding later by the run's length day after day.
fn next_check_at(run_started: DateTime<Utc>, delay: chrono::Duration) -> DateTime<Utc> {
    use chrono::DurationRound;
    run_started
        .duration_trunc(chrono::Duration::hours(1))
        .unwrap_or(run_started)
        + delay
}

/// Makes a source due now when its stored outcome is no longer a verdict
/// under the current rules, so a wait set on a reading we now reject (or a
/// label whose class changed) never outlives the fix. Two cases, both
/// re-read every run and idempotent:
/// - a request-free outcome whose input now reads otherwise: `policy_blocked`
///   while the census verdict ([`policy::census_verdicts_sql`]) rates the
///   URL direct, `domain_withdrawn` while the domain policy admits it again
///   (production 2026-11-05: Apple Music held by a census pause, due a week
///   later);
/// - the class of the stored outcome ([`reverify::class_of`]) differs from
///   the class its schedule was set under (`schedule_class`). A schedule set
///   before classes were recorded (NULL) stands.
///
/// Returns how many sources were made due.
pub(crate) async fn reschedule_stale(pool: &PgPool) -> Result<u64> {
    let mut rescheduled = sqlx::query(&format!(
        "UPDATE corporate_evidence_targets target
         SET next_check_at = now(), updated_at = now()
         FROM benefit_source_candidates candidate
         JOIN benefit_source_domains d ON d.id = candidate.domain_id
         WHERE candidate.id = target.candidate_id AND target.next_check_at > now()
           AND ((target.last_outcome = 'policy_blocked'
                 AND (EXISTS (SELECT 1 FROM ({verdicts}) entry
                              WHERE entry.candidate_id = target.candidate_id AND {census_direct})
                      OR (NOT EXISTS (SELECT 1 FROM ({verdicts}) entry
                                      WHERE entry.candidate_id = target.candidate_id)
                          AND (SELECT check_row.details->>'detail' FROM corporate_evidence_checks check_row
                               WHERE check_row.candidate_id = target.candidate_id
                               ORDER BY check_row.checked_at DESC, check_row.id DESC LIMIT 1) = $2)))
                OR (target.last_outcome = 'domain_withdrawn' AND {eligible}))",
        verdicts = policy::census_verdicts_sql("$1"),
        census_direct = policy::CENSUS_DIRECT_SQL,
        eligible = policy::ELIGIBLE_DOMAIN_SQL,
    ))
    .bind(CENSUS_MAX_AGE_DAYS as i32)
    .bind(LEGACY_CENSUS_WAIT_DETAIL)
    .execute(pool)
    .await
    .context("failed to reschedule sources with a stale verdict")?
    .rows_affected();
    let scheduled: Vec<(Uuid, String, String)> = sqlx::query_as(
        "SELECT candidate_id, last_outcome, schedule_class FROM corporate_evidence_targets
         WHERE next_check_at > now() AND last_outcome IS NOT NULL AND schedule_class IS NOT NULL",
    )
    .fetch_all(pool)
    .await?;
    let changed: Vec<Uuid> = scheduled
        .into_iter()
        .filter(|(_, outcome, class)| reverify::class_of(outcome).as_str() != class)
        .map(|(id, _, _)| id)
        .collect();
    if !changed.is_empty() {
        rescheduled += sqlx::query(
            "UPDATE corporate_evidence_targets SET next_check_at = now(), updated_at = now()
             WHERE candidate_id = ANY($1) AND next_check_at > now()",
        )
        .bind(&changed)
        .execute(pool)
        .await?
        .rows_affected();
    }
    Ok(rescheduled)
}

/// The first run hour at or after `at`: a check scheduled for it reads
/// the grace window as passed.
fn hour_at_or_after(at: DateTime<Utc>) -> DateTime<Utc> {
    use chrono::DurationRound;
    let floor = at.duration_trunc(chrono::Duration::hours(1)).unwrap_or(at);
    if floor == at {
        at
    } else {
        floor + chrono::Duration::hours(1)
    }
}

/// The incident a source gets when an offer on it was hidden because its
/// grace window ran out unconfirmed; resolved by the next confirmation.
fn grace_incident(candidate_id: Uuid) -> String {
    format!("corporate_reverify:{candidate_id}")
}

/// Opens (or refreshes) the source's grace incident: one per source,
/// closed by its next confirmation.
async fn open_grace_incident(
    tx: &mut Transaction<'_, Postgres>,
    target: &Target,
    outcome: &str,
    class: Class,
    programs_hidden: usize,
) -> Result<()> {
    operations::record_event_tx(
        tx,
        &operations::EventCommand {
            fingerprint: &grace_incident(target.candidate_id),
            severity: operations::Severity::Warning,
            component: "corporate_auto",
            event_type: "corporate_reverify_grace_expired",
            subject_type: Some("benefit_source_candidate"),
            subject_id: Some(&target.candidate_id.to_string()),
            message: "An official benefit page could not be re-verified within its grace window; its offers are hidden until it confirms again",
            details: json!({"outcome": outcome, "class": class.as_str(),
                            "programs_hidden": programs_hidden,
                            "grace_hours": reverify::GRACE.num_hours()}),
        },
    )
    .await?;
    Ok(())
}

/// Closes the source's grace incident: nothing is left to wait for.
async fn resolve_grace_incident(
    tx: &mut Transaction<'_, Postgres>,
    target: &Target,
    why: &str,
) -> Result<()> {
    operations::resolve_event_tx(
        tx,
        &grace_incident(target.candidate_id),
        None,
        Some(&format!("Automatically resolved: {why}")),
    )
    .await?;
    Ok(())
}

/// A programme's last evidence confirmation and whether it is shown.
async fn shown_since(
    tx: &mut Transaction<'_, Postgres>,
    program_id: Uuid,
) -> Result<(Option<DateTime<Utc>>, bool)> {
    Ok(sqlx::query_as(
        "SELECT max(monitor.evidence_verified_at), bool_or(program.is_active)
         FROM program_source_monitors monitor JOIN programs program ON program.id = monitor.program_id
         WHERE monitor.program_id = $1 AND monitor.verification_mode = 'evidence_auto'",
    )
    .bind(program_id)
    .fetch_one(&mut **tx)
    .await
    .map(|(at, active): (Option<DateTime<Utc>>, Option<bool>)| (at, active.unwrap_or(false)))?)
}

/// `publish`: whether live may adopt a moved page's URL. `health` counts
/// every programme decision by class ([`reverify`]).
async fn decide_and_record(
    pool: &PgPool,
    mode: VerifyMode,
    publish: bool,
    run_started: DateTime<Utc>,
    target: &Target,
    result: &PageResult,
    health: &mut RunHealth,
) -> Result<Vec<(String, bool)>> {
    let live = mode == VerifyMode::Live;
    // hide_only: hides for the allowlisted reasons apply, nothing is ever
    // made visible.
    let hide_only = mode == VerifyMode::HideOnly;
    // Shadow reads the same allowlist: a page read with no allowlisted
    // reason keeps the review deadline moving in both modes.
    let allowed = if !live {
        super::hide_reasons_from_env()?
    } else {
        Vec::new()
    };
    let allowlisted = |key: &str| allowed.iter().any(|a| a == key);
    let mut tx = pool.begin().await?;
    let mut decisions = Vec::new();
    let candidate_outcome: &'static str;
    // Source events resolve on every outcome but an offer waiting out its
    // grace window, which is still shown.
    let settle_events;
    match result {
        PageResult::Evidence {
            page,
            html,
            hash,
            http_status,
            final_url,
            via,
            origin,
        } => {
            let facts = load_facts(&mut tx, target.candidate_id).await?;
            let (digest, assessed) = assess_all(html, page, final_url, &facts, live).await?;
            let moved = *via != "direct";
            let mut by_program: BTreeMap<Uuid, Vec<(Uuid, &'static str, bool)>> = BTreeMap::new();
            // hide_only: programmes with a fact hidden for an allowlisted
            // reason.
            let mut hide_now: std::collections::BTreeSet<Uuid> = Default::default();
            // Shadow and hide_only: programmes with a fact failing for an
            // allowlisted reason (hidden in hide_only, counted in shadow).
            let mut trusted_failure: std::collections::BTreeSet<Uuid> = Default::default();
            let mut fact_details: BTreeMap<Uuid, Vec<Value>> = BTreeMap::new();
            for (fact, item) in facts.iter().zip(&assessed) {
                let label = item.resolution.label();
                let confirms = item.resolution.confirms();
                let mut detail = match &item.resolution {
                    Resolution::Exact(FactOutcome::FieldMismatch(d)) => Value::from(d.as_str()),
                    Resolution::Ambiguous(reason) => Value::from(reason.as_str()),
                    Resolution::Relocated { score, .. } => json!({"score": score}),
                    _ => Value::Null,
                };
                if let Some(anchors) = &item.anchors {
                    sqlx::query(
                        "UPDATE corporate_offer_facts SET anchors = $2, anchors_updated_at = now() WHERE id = $1",
                    )
                    .bind(fact.id)
                    .bind(serde_json::to_value(anchors)?)
                    .execute(&mut *tx)
                    .await?;
                }
                // Shadow metric only: what the value matcher reads on a
                // changed page. Nothing is proposed or applied from it.
                if let Some(update) = &item.shadow_update {
                    detail = json!({"reason": detail, "shadow_update": {
                        "old": update.old_signature, "new": update.new_signature,
                        "extractors_agree": update.extractors_agree}});
                }
                let trusted_key = !confirms
                    && match &item.resolution {
                        Resolution::Ambiguous(reason) => {
                            resilience::reason_key(reason).is_some_and(allowlisted)
                        }
                        _ => false,
                    };
                if trusted_key {
                    trusted_failure.insert(fact.program_id);
                }
                if hide_only && trusted_key {
                    sqlx::query(
                        "UPDATE corporate_offer_facts
                         SET last_checked_at = now(), last_outcome = $2, status = 'hidden', updated_at = now()
                         WHERE id = $1",
                    )
                    .bind(fact.id)
                    .bind(label)
                    .execute(&mut *tx)
                    .await?;
                    hide_now.insert(fact.program_id);
                }
                if live {
                    // An expired adoption decides nothing: the fact keeps its
                    // status (never published on its account).
                    let status = if confirms {
                        Some("published")
                    } else if label == "adoption_expired" {
                        None
                    } else {
                        Some("hidden")
                    };
                    let window = match &item.resolution {
                        Resolution::Exact(FactOutcome::Confirmed {
                            window_signature, ..
                        })
                        | Resolution::Fuzzy { window_signature } => {
                            Some(stored_window(window_signature))
                        }
                        _ => None,
                    };
                    // An adoption sets the baseline, also over one stored
                    // before pages were digested.
                    let adopted = matches!(
                        item.resolution,
                        Resolution::Exact(FactOutcome::Confirmed { adopted: true, .. })
                    );
                    sqlx::query(
                        "UPDATE corporate_offer_facts
                         SET last_checked_at = now(), last_outcome = $2, status = COALESCE($3, status),
                             window_signature = CASE WHEN $6 THEN $4 ELSE COALESCE(window_signature, $4) END,
                             last_confirmed_at = CASE WHEN $5 THEN now() ELSE last_confirmed_at END,
                             updated_at = now()
                         WHERE id = $1",
                    )
                    .bind(fact.id)
                    .bind(label)
                    .bind(status)
                    .bind(window)
                    .bind(confirms)
                    .bind(adopted)
                    .execute(&mut *tx)
                    .await?;
                    if let Resolution::Relocated {
                        quote,
                        window_signature,
                        ..
                    } = &item.resolution
                    {
                        // The relocated sentence becomes the evidence; the
                        // old quote is kept among the anchors.
                        sqlx::query(
                            "UPDATE corporate_offer_facts f
                             SET quote = $2, quote_key = $3, window_signature = $4, updated_at = now()
                             WHERE f.id = $1
                               AND NOT EXISTS (SELECT 1 FROM corporate_offer_facts o
                                               WHERE o.program_id = f.program_id AND o.quote_key = $3 AND o.id <> f.id)",
                        )
                        .bind(fact.id)
                        .bind(quote)
                        .bind(quote_key(fact.program_id, quote))
                        .bind(stored_window(window_signature))
                        .execute(&mut *tx)
                        .await?;
                    }
                }
                if !live {
                    if let Resolution::Exact(FactOutcome::Confirmed {
                        window_signature,
                        adopted: true,
                    }) = &item.resolution
                    {
                        sqlx::query(
                            "UPDATE corporate_offer_facts
                             SET shadow_window_signature = $2,
                                 shadow_baseline_at = now(),
                                 updated_at = now()
                             WHERE id = $1",
                        )
                        .bind(fact.id)
                        .bind(stored_window(window_signature))
                        .execute(&mut *tx)
                        .await?;
                    }
                }
                by_program
                    .entry(fact.program_id)
                    .or_default()
                    .push((fact.id, label, confirms));
                fact_details
                    .entry(fact.program_id)
                    .or_default()
                    .push(json!({"fact_id": fact.id, "outcome": label, "detail": detail}));
            }

            // A moved page is adopted only through two agreeing fetches.
            let all_confirm = by_program.values().flatten().all(|(_, _, c)| *c);
            let mut url_state: Option<&'static str> = None;
            if moved {
                if all_confirm && !by_program.is_empty() {
                    let (_, live_agreed) =
                        record_url_proposal(&mut tx, target, final_url, via, live).await?;
                    // A new URL is a new public value: adopted only when
                    // publishing is enabled, else left as the proposal.
                    if live
                        && publish
                        && live_agreed
                        && adopt_url(&mut tx, target, final_url).await?
                    {
                        url_state = Some("url_adopted");
                    }
                }
            } else if all_confirm {
                sqlx::query(
                    "UPDATE corporate_change_proposals SET status = 'superseded'
                     WHERE status = 'pending' AND kind = 'url' AND candidate_id = $1",
                )
                .bind(target.candidate_id)
                .execute(&mut *tx)
                .await?;
            }

            let programs_without_facts: Vec<Uuid> = sqlx::query_scalar(
                "SELECT monitor.program_id FROM program_source_monitors monitor
                 WHERE monitor.candidate_id = $1 AND monitor.verification_mode = 'evidence_auto'
                   AND monitor.verification_state <> 'retired'
                   AND NOT EXISTS (SELECT 1 FROM corporate_offer_facts fact
                                   WHERE fact.program_id = monitor.program_id AND fact.status <> 'retired')",
            )
            .bind(target.candidate_id)
            .fetch_all(&mut *tx)
            .await?;
            let mut any_confirmed = false;
            for program_id in programs_without_facts {
                record_check(
                    &mut tx,
                    target.candidate_id,
                    Some(program_id),
                    mode,
                    "no_evidence",
                    false,
                    Some(*http_status),
                    json!({}),
                )
                .await?;
                if live {
                    hide_program(
                        &mut tx,
                        target.candidate_id,
                        program_id,
                        "evidence_failed",
                        "no_evidence",
                    )
                    .await?;
                }
                decisions.push(("no_evidence".to_string(), live));
                health.count(Class::Refuted);
            }
            // A programme with no evidence is never confirmed.
            let mut every_program_confirmed = decisions.is_empty();
            // Grace is read on the database clock every check time is
            // written with.
            let now = run_started;
            let mut grace_end: Option<DateTime<Utc>> = None;
            let mut hidden_after_grace = 0;
            // A refutation hid an offer: the grace incident has nothing
            // left to wait for.
            let mut refuted_hide = false;
            for (program_id, outcomes) in &by_program {
                let program_confirms = outcomes.iter().all(|(_, _, c)| *c);
                // The worst fact decides; among confirmations the most
                // notable path is reported.
                let mut label = outcomes
                    .iter()
                    .map(|(_, l, _)| *l)
                    .max_by_key(|l| label_severity(l))
                    .unwrap_or("confirmed");
                let mut confirmed = program_confirms;
                // On a moved page the new URL waits for adoption; failures
                // keep their own label ([`Origin`] says what counts before).
                let adopted = url_state == Some("url_adopted");
                if moved && program_confirms {
                    label = if adopted {
                        "url_adopted"
                    } else {
                        "url_move_proposed"
                    };
                    confirmed = match origin {
                        Origin::Source => true,
                        Origin::Rediscovered => adopted,
                        Origin::Fallback => {
                            adopted && outcomes.iter().all(|(_, l, _)| *l == "confirmed")
                        }
                    };
                }
                // A fallback page that has not confirmed the offer says
                // nothing about it: the source is still closed to us, so the
                // offer waits out its grace window as a block.
                let mut grace_hold: Option<&'static str> = None;
                let mut waiting = false;
                if *origin == Origin::Fallback && !confirmed {
                    let (confirmed_at, active) = shown_since(&mut tx, *program_id).await?;
                    match reverify::wait(Class::Blocked, active, confirmed_at, now) {
                        reverify::Wait::Shown(end) => {
                            waiting = true;
                            health.in_grace += 1;
                            grace_end = Some(grace_end.map_or(end, |e| e.min(end)));
                        }
                        reverify::Wait::Hide(hold) => {
                            grace_hold = Some(hold);
                            if active {
                                hidden_after_grace += 1;
                            }
                        }
                    }
                }
                every_program_confirmed &= confirmed;
                health.count(if confirmed {
                    Class::Confirmed
                } else if *origin == Origin::Fallback {
                    Class::Blocked
                } else {
                    reverify::class_of(label)
                });
                let mut details = json!({
                    "facts": fact_details.remove(program_id).unwrap_or_default(),
                    "page_hash": hash,
                    "page_digest": digest,
                    "via": via,
                    "url": final_url,
                });
                // Telemetry: the page reads otherwise than when a fact of
                // this offer was last confirmed. Never a reason to hide.
                details["page_changed"] = json!(facts
                    .iter()
                    .zip(&assessed)
                    .any(|(f, a)| f.program_id == *program_id && a.page_changed));
                // An expired adoption is not a decision: nothing is shown or
                // hidden on its account. hide_only applies only its hides.
                let applies = (live && label != "adoption_expired" && !waiting)
                    || (hide_only && !confirmed && hide_now.contains(program_id));
                record_check(
                    &mut tx,
                    target.candidate_id,
                    Some(*program_id),
                    mode,
                    label,
                    applies,
                    Some(*http_status),
                    details,
                )
                .await?;
                if applies {
                    if confirmed && live {
                        confirm_program(&mut tx, target.candidate_id, *program_id, hash).await?;
                        any_confirmed = true;
                    } else if !confirmed {
                        hide_program(
                            &mut tx,
                            target.candidate_id,
                            *program_id,
                            grace_hold.unwrap_or("evidence_failed"),
                            label,
                        )
                        .await?;
                        refuted_hide |= grace_hold.is_none();
                    }
                }
                if !live && confirmed && !trusted_failure.contains(program_id) {
                    // Shadow and hide_only: every fact of the offer
                    // confirmed on this page (a moved page only once
                    // adopted), so the deadline moves as on a live
                    // confirmation. Anything else (a quote gone, an ending
                    // read but not trusted, an HTTP error) leaves it to the
                    // overdue sweep, fail closed.
                    extend_review(&mut tx, target.candidate_id, *program_id).await?;
                }
                decisions.push((label.to_string(), applies));
            }
            if live && any_confirmed {
                restore_confirmed(&mut tx, target.candidate_id).await?;
            }
            candidate_outcome = if every_program_confirmed {
                "confirmed"
            } else if decisions
                .iter()
                .any(|(l, applied)| *applied && reverify::class_of(l) != Class::Confirmed)
            {
                "evidence_failed"
            } else if decisions.iter().any(|(l, _)| l == "adoption_expired") {
                "adoption_expired"
            } else {
                "evidence_failed"
            };
            // A fact that cannot be confirmed on this page (its text changed,
            // the page reads ended, the quote is gone, the window or a field
            // differs) is not re-derived from the old fact: the source is
            // flagged for fresh extraction, which validates from scratch
            // before anything is published. Cleared once every fact
            // confirms again. A pending URL move is not a failure.
            // The one queue of the rereading exit ([`fact_exit`]).
            // The worst reading names the request ([`label_severity`]).
            let reread_reason = assessed
                .iter()
                .map(|a| &a.resolution)
                .filter(|r| fact_exit(r) == Exit::Reread)
                .map(Resolution::label)
                .max_by_key(|label| label_severity(label))
                .or_else(|| {
                    decisions
                        .iter()
                        .any(|(l, _)| source_exit(l) == Exit::Reread)
                        .then_some("no_evidence")
                });
            // A source with several programmes has no reread exit: intake
            // re-derives one offer per source and publish refuses a source
            // with more than one programme. It is stated as such, not queued
            // for a lane that never takes it (review A-M3), and counted.
            // A source rereading has republished VOLATILE_REPUBLISHES times
            // in 30 days and that is held again flaps (every cycle passes
            // every gate, it never settles): no more rereading, stated and
            // counted (review A-M4).
            let republished: i64 = if reread_reason.is_some() {
                sqlx::query_scalar(
                    "SELECT count(*) FROM benefit_source_checks
                     WHERE candidate_id = $1 AND check_kind = 'unchanged'
                       AND error_summary = $2 AND detected_at > now() - interval '30 days'",
                )
                .bind(target.candidate_id)
                .bind(super::publish::REPUBLISHED_SUMMARY)
                .fetch_one(&mut *tx)
                .await?
            } else {
                0
            };
            let (reextract, reread_reason) = match reread_reason {
                Some(_) if decisions.len() > 1 => {
                    health.multi_offer_held += 1;
                    (false, Some(MULTI_OFFER_SOURCE))
                }
                Some(_) if republished >= VOLATILE_REPUBLISHES => {
                    health.volatile_held += 1;
                    (false, Some(VOLATILE_PAGE))
                }
                reason => (reason.is_some(), reason),
            };
            sqlx::query(
                "UPDATE corporate_evidence_targets
                 SET reextract_requested_at = CASE WHEN $2 THEN COALESCE(reextract_requested_at, now()) END,
                     reextract_reason = $3
                 WHERE candidate_id = $1",
            )
            .bind(target.candidate_id)
            .bind(reextract)
            .bind(reread_reason)
            .execute(&mut *tx)
            .await?;
            // A page found again after a 404 keeps the gone clock running
            // until its new URL is adopted.
            let keep_gone_clock =
                moved && *origin == Origin::Rediscovered && url_state != Some("url_adopted");
            health.hidden_after_grace += hidden_after_grace;
            if live && hidden_after_grace > 0 {
                open_grace_incident(
                    &mut tx,
                    target,
                    "robots_disallowed",
                    Class::Blocked,
                    hidden_after_grace,
                )
                .await?;
            }
            settle_events = grace_end.is_none();
            if live && candidate_outcome == "confirmed" {
                resolve_grace_incident(
                    &mut tx,
                    target,
                    "the official page confirmed the offer again",
                )
                .await?;
            } else if live && refuted_hide && hidden_after_grace == 0 {
                resolve_grace_incident(
                    &mut tx,
                    target,
                    "the official page refuted the offer; it stays hidden on that account",
                )
                .await?;
            }
            sqlx::query(
                "UPDATE corporate_evidence_targets
                 SET last_checked_at = now(), last_success_at = now(), last_outcome = $2,
                     consecutive_failures = 0,
                     first_gone_at = CASE WHEN $3 THEN COALESCE(first_gone_at, now()) END,
                     next_check_at = $4, schedule_class = $5, updated_at = now()
                 WHERE candidate_id = $1",
            )
            .bind(target.candidate_id)
            .bind(candidate_outcome)
            .bind(keep_gone_clock)
            .bind({
                let next = next_check_at(run_started, chrono::Duration::days(1));
                grace_end.map_or(next, |end| next.min(hour_at_or_after(end)))
            })
            .bind(reverify::class_of(candidate_outcome).as_str())
            .execute(&mut *tx)
            .await?;
        }
        PageResult::Outcome {
            outcome,
            http_status,
            detail,
        } => {
            let mut outcome: &'static str = outcome;
            let class = reverify::class_of(outcome);
            let failures = if class.waits() {
                target.consecutive_failures + 1
            } else {
                0
            };
            // Gone twice at least 24 h apart retires the offer.
            let retire = outcome == "gone"
                && target
                    .first_gone_at
                    .is_some_and(|first| Utc::now() - first >= chrono::Duration::hours(24));
            if retire {
                outcome = "retired_gone";
            }
            // A 404/410 counts only once a re-fetch at least an hour later
            // reads it too: two blips in the same scheduled hour never start
            // the gone clock. The first one only schedules that re-fetch.
            let gone_confirmed = outcome == "gone"
                && target.first_gone_at.is_none()
                && target.last_outcome.as_deref() == Some("gone")
                && target
                    .last_checked_at
                    .is_some_and(|at| Utc::now() - at >= chrono::Duration::hours(1));
            let gone_probe = outcome == "gone" && target.first_gone_at.is_none() && !gone_confirmed;
            // Refuted hides at once; transient and blocked wait out each
            // programme's grace window from its last confirmation.
            let refuted_hold = (class == Class::Refuted)
                .then(|| hide_reason(outcome))
                .flatten();
            // hide_only: only a source gone for good (404/410 twice, 24 h
            // apart) hides, and only when allowlisted; it is held, not
            // retired.
            let hide_only_gone = hide_only && retire && allowlisted("source_gone");
            let programs: Vec<(Uuid, Option<DateTime<Utc>>, bool)> = sqlx::query_as(
                "SELECT monitor.program_id, monitor.evidence_verified_at, program.is_active
                 FROM program_source_monitors monitor JOIN programs program ON program.id = monitor.program_id
                 WHERE monitor.candidate_id = $1 AND monitor.verification_mode = 'evidence_auto'
                   AND monitor.verification_state <> 'retired'",
            )
            .bind(target.candidate_id)
            .fetch_all(&mut *tx)
            .await?;
            let now = run_started;
            // The earliest end of a grace window still running.
            let mut grace_end: Option<DateTime<Utc>> = None;
            let mut hidden_after_grace = 0;
            for (program_id, confirmed_at, active) in &programs {
                let (waiting, grace_hold) = if class.waits() {
                    match reverify::wait(class, *active, *confirmed_at, now) {
                        reverify::Wait::Shown(end) => {
                            grace_end = Some(grace_end.map_or(end, |e| e.min(end)));
                            (true, None)
                        }
                        reverify::Wait::Hide(hold) => (false, Some(hold)),
                    }
                } else {
                    (false, None)
                };
                let hold = refuted_hold.or(grace_hold);
                let applies = (live && (hold.is_some() || retire)) || hide_only_gone;
                record_check(
                    &mut tx,
                    target.candidate_id,
                    Some(*program_id),
                    mode,
                    outcome,
                    applies,
                    *http_status,
                    json!({"detail": detail, "consecutive_failures": failures,
                           "class": class.as_str(), "in_grace": waiting}),
                )
                .await?;
                if hide_only_gone {
                    hide_program(
                        &mut tx,
                        target.candidate_id,
                        *program_id,
                        "not_found",
                        outcome,
                    )
                    .await?;
                } else if applies {
                    if retire {
                        retire_program(&mut tx, target.candidate_id, *program_id).await?;
                    } else if let Some(reason) = hold {
                        hide_program(&mut tx, target.candidate_id, *program_id, reason, outcome)
                            .await?;
                        if class.waits() && *active {
                            hidden_after_grace += 1;
                        }
                    }
                }
                if waiting {
                    health.in_grace += 1;
                }
                health.count(class);
                decisions.push((outcome.to_string(), applies));
            }
            health.hidden_after_grace += hidden_after_grace;
            if live && hidden_after_grace > 0 {
                open_grace_incident(&mut tx, target, outcome, class, hidden_after_grace).await?;
            } else if live && class == Class::Refuted {
                resolve_grace_incident(
                    &mut tx,
                    target,
                    if retire {
                        "the offer was retired"
                    } else {
                        "the source was refuted; its offers stay hidden on that account"
                    },
                )
                .await?;
            }
            settle_events = grace_end.is_none();
            candidate_outcome = outcome;
            // The re-fetch of a first 404 is an hour after this one; every
            // other wait counts from the hour the run started, and an offer
            // in grace is looked at again by the end of its window.
            // (`None`: an hour after this check, by the database clock.)
            let next_at = (!gone_probe).then(|| {
                let delay = reverify::retry_delay(
                    outcome,
                    failures,
                    grace_end.is_none(),
                    target.candidate_id,
                );
                let next = next_check_at(run_started, delay);
                grace_end.map_or(next, |end| next.min(hour_at_or_after(end)))
            });
            sqlx::query(
                "UPDATE corporate_evidence_targets
                 SET last_checked_at = now(), last_outcome = $2,
                     consecutive_failures = $3,
                     first_gone_at = CASE WHEN $2 = 'gone'
                                          THEN COALESCE(first_gone_at, CASE WHEN $5 THEN now() END)
                                          ELSE NULL END,
                     next_check_at = COALESCE($4, now() + interval '1 hour'),
                     schedule_class = $6, updated_at = now()
                 WHERE candidate_id = $1",
            )
            .bind(target.candidate_id)
            .bind(outcome)
            .bind(failures.min(100))
            .bind(next_at)
            .bind(gone_confirmed)
            .bind(class.as_str())
            .execute(&mut *tx)
            .await?;
        }
    }
    if live && settle_events {
        super::events::resolve_for_candidate(&mut tx, target.candidate_id, candidate_outcome)
            .await?;
    }
    tx.commit().await?;
    Ok(decisions)
}

/// Higher is worse; among confirmations the more notable path ranks higher.
fn label_severity(label: &str) -> u8 {
    match label {
        "confirmed" => 0,
        "confirmed_fuzzy" => 1,
        "relocated" => 2,
        "adoption_expired" => 4,
        "window_changed" => 5,
        "field_mismatch" => 7,
        "ambiguous" => 8,
        _ => 9,
    }
}

async fn load_facts(
    connection: &mut sqlx::PgConnection,
    candidate_id: Uuid,
) -> Result<Vec<FactRow>> {
    Ok(sqlx::query_as::<_, FactRow>(
        "SELECT fact.id, fact.program_id, fact.quote, fact.anchors, fact.window_signature,
                fact.shadow_window_signature, fact.adopt_before,
                program.min_age, program.max_age
         FROM corporate_offer_facts fact
         JOIN program_source_monitors monitor
           ON monitor.program_id = fact.program_id AND monitor.candidate_id = $1
          AND monitor.verification_mode = 'evidence_auto' AND monitor.verification_state <> 'retired'
         JOIN programs program ON program.id = fact.program_id
         WHERE fact.status <> 'retired'
         ORDER BY fact.program_id, fact.id",
    )
    .bind(candidate_id)
    .fetch_all(connection)
    .await?)
}

// ── re-baseline from fresh extraction (P2's exit) ─────────────────────────
//
// A fact held because its own section changed ([`resilience::SECTION_CHANGED`])
// returns only through fresh extraction: when an independent re-read derives
// the same fact (the same quote, a second reader agreeing on its value, the
// same reading for at least [`FRESH_STABLE_HOURS`]), the fact takes that page
// as its new baseline and the next evidence pass confirms it as usual. Nothing
// else re-baselines a fact; nothing here shows an offer by itself.

/// What fresh extraction re-derived for one stored fact.
#[derive(Debug, Clone)]
pub struct FreshExtraction<'a> {
    /// The page fresh extraction read, and the URL it read it from.
    pub html: &'a str,
    pub url: &'a str,
    /// The fact's quote as fresh extraction re-derived it from that page.
    pub quote: &'a str,
    /// An independent second reader read the same value from that quote.
    pub readers_agree: bool,
    /// The first and the latest of the agreeing reads of this page.
    pub first_read_at: DateTime<Utc>,
    pub last_read_at: DateTime<Utc>,
}

/// What [`rebaseline_from_fresh`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FreshOutcome {
    /// The fact takes the page as its baseline; the next evidence pass
    /// confirms it and restores the offer.
    Rebaselined,
    /// Why the fact stays hidden.
    Refused(&'static str),
}

/// The agreeing reads must span at least this long.
pub const FRESH_STABLE_HOURS: i64 = 24;

/// The way back from a hold in the evidence pass ([`assess`]): off unless
/// set (release review 2026-10-28). Off, a held fact stays held until fresh
/// extraction calls [`rebaseline_from_fresh`].
pub(crate) const WAY_BACK_FLAG: &str = "CORPORATE_EVIDENCE_WAY_BACK_ENABLED";

/// Whether `fresh` re-derives the stored `fact`, and its anchors on that
/// page. Pure. The page must state the fact as stored: the quote verbatim,
/// its value window equal to the stored baseline, its own dates and
/// structured data current, and no ending in the text added to its
/// section since the last confirmation ([`resilience::new_text_reason`]).
pub fn fresh_anchors(
    fact: &StoredFact<'_>,
    fresh: &FreshExtraction<'_>,
) -> Result<Anchors, &'static str> {
    if canonical(fresh.quote) != canonical(fact.quote) {
        return Err("quote_differs");
    }
    if !fresh.readers_agree {
        return Err("readers_disagree");
    }
    if fresh.last_read_at - fresh.first_read_at < chrono::Duration::hours(FRESH_STABLE_HOURS) {
        return Err("not_stable");
    }
    let kst = chrono::FixedOffset::east_opt(9 * 3_600).expect("KST offset");
    let view = PageView::at(
        fresh.html,
        &visible_text(fresh.html),
        fresh.last_read_at.with_timezone(&kst).naive_local(),
    )
    .with_url(fresh.url);
    let first_sight = StoredFact {
        anchors: &Anchors::default(),
        ..fact.clone()
    };
    match resilience::resolve(&view, &first_sight) {
        // Text added since the last confirmation is read for an ending;
        // the text judged then is not read again.
        Resolution::Exact(FactOutcome::Confirmed { .. })
            if resilience::new_text_reason(&view, fact).is_some() =>
        {
            Err("not_current")
        }
        Resolution::Exact(FactOutcome::Confirmed { .. }) => Ok(Anchors {
            fingerprint: resilience::capture(&view, fact.quote),
            pending_section: None,
            ..fact.anchors.clone()
        }),
        Resolution::Exact(FactOutcome::WindowChanged | FactOutcome::FieldMismatch(_)) => {
            Err("value_differs")
        }
        Resolution::Ambiguous(_) => Err("not_current"),
        _ => Err("quote_missing"),
    }
}

/// P2's entry point: re-baselines one stored fact from a fresh extraction
/// ([`fresh_anchors`]). Writes only the fact's anchors; the offer returns
/// on the next evidence pass, through the normal confirmation and restore.
pub async fn rebaseline_from_fresh(
    pool: &PgPool,
    fact_id: Uuid,
    fresh: &FreshExtraction<'_>,
) -> Result<FreshOutcome> {
    let Some(fact) = sqlx::query_as::<_, FactRow>(
        "SELECT fact.id, fact.program_id, fact.quote, fact.anchors, fact.window_signature,
                fact.shadow_window_signature, fact.adopt_before, program.min_age, program.max_age
         FROM corporate_offer_facts fact JOIN programs program ON program.id = fact.program_id
         WHERE fact.id = $1 AND fact.status <> 'retired'",
    )
    .bind(fact_id)
    .fetch_optional(pool)
    .await?
    else {
        return Ok(FreshOutcome::Refused("fact_not_found"));
    };
    let stored: Anchors = serde_json::from_value(fact.anchors.clone()).unwrap_or_default();
    let Ok(baseline) = window_baseline(
        fact.window_signature
            .as_deref()
            .or(fact.shadow_window_signature.as_deref()),
    ) else {
        return Ok(FreshOutcome::Refused("baseline_unreadable"));
    };
    let read = fresh_anchors(
        &StoredFact {
            quote: &fact.quote,
            baseline,
            ages: Ages {
                min: fact.min_age,
                max: fact.max_age,
            },
            adoption_open: false,
            anchors: &stored,
        },
        fresh,
    );
    let anchors = match read {
        Ok(anchors) => anchors,
        Err(reason) => return Ok(FreshOutcome::Refused(reason)),
    };
    sqlx::query(
        "UPDATE corporate_offer_facts SET anchors = $2, anchors_updated_at = now()
         WHERE id = $1 AND quote = $3",
    )
    .bind(fact.id)
    .bind(serde_json::to_value(anchors)?)
    .bind(&fact.quote)
    .execute(pool)
    .await?;
    info!(event = "corporate_evidence.rebaselined_from_fresh", fact_id = %fact.id,
          program_id = %fact.program_id, "a held fact takes a fresh extraction's page as its baseline");
    Ok(FreshOutcome::Rebaselined)
}

/// One fact resolved against one page, with what to store.
struct Assessed {
    resolution: Resolution,
    /// Refreshed locators after a confirmation.
    anchors: Option<Anchors>,
    /// Shadow metric: the value change the matcher reads on a changed page.
    shadow_update: Option<Proposal>,
    /// Telemetry: the page's digest differs from the one stored at the
    /// fact's last confirmation.
    page_changed: bool,
}

/// Parses the page and resolves every fact on a blocking thread: scoring
/// every element against a fingerprint is CPU work that must not stall the
/// async worker. Returns the page's digest with the facts.
async fn assess_all(
    html: &str,
    page: &str,
    url: &str,
    facts: &[FactRow],
    live: bool,
) -> Result<(String, Vec<Assessed>)> {
    let (html, page, url, facts) = (
        html.to_string(),
        page.to_string(),
        url.to_string(),
        facts.to_vec(),
    );
    let way_back = super::flag(WAY_BACK_FLAG);
    tokio::task::spawn_blocking(move || {
        let view = PageView::at(&html, &page, super::health::kst_now()).with_url(&url);
        let assessed = facts
            .iter()
            .map(|fact| assess(&view, &html, &url, fact, live, live && way_back))
            .collect();
        (view.page_digest().to_string(), assessed)
    })
    .await
    .context("evidence assessment task failed")
}

/// `way_back`: the way back from a hold runs (live only, and only with
/// [`WAY_BACK_FLAG`] set).
fn assess(
    view: &PageView,
    html: &str,
    url: &str,
    fact: &FactRow,
    live: bool,
    way_back: bool,
) -> Assessed {
    let open = fact.adopt_before.is_none_or(|limit| Utc::now() < limit);
    // Shadow keeps its own baseline, set on first sight inside the adoption
    // window, so window changes are measurable before live. Live adopts the
    // shadow baseline when it has none: a change since shadow started is a
    // change.
    let stored_baseline = if live {
        fact.window_signature
            .as_deref()
            .or(fact.shadow_window_signature.as_deref())
    } else {
        fact.shadow_window_signature.as_deref()
    };
    // A window another reader version wrote reads nothing to this one.
    let (baseline, other_reader) = match window_baseline(stored_baseline) {
        Ok(baseline) => (baseline, false),
        Err(OtherReader) => (None, true),
    };
    let mut stored: Anchors = serde_json::from_value(fact.anchors.clone()).unwrap_or_default();
    fn as_stored<'a>(
        fact: &'a FactRow,
        baseline: Option<&'a str>,
        open: bool,
        anchors: &'a Anchors,
    ) -> StoredFact<'a> {
        StoredFact {
            quote: &fact.quote,
            baseline,
            ages: Ages {
                min: fact.min_age,
                max: fact.max_age,
            },
            adoption_open: open,
            anchors,
        }
    }
    let unreadable = || Resolution::Ambiguous(resilience::BASELINE_UNREADABLE.into());
    let mut resolution = if other_reader {
        unreadable()
    } else {
        resilience::resolve(view, &as_stored(fact, baseline, open, &stored))
    };
    let mut baseline = baseline;
    // A baseline (window or section) of another reader version: on the very
    // bytes the fact was last confirmed on, this reader reads the page as on
    // first sight and takes its own reading as the baseline. The page is
    // the page confirmed, so nothing new is confirmed. On other bytes the
    // fact is held for fresh extraction; the new reading is never adopted.
    if resolution == unreadable() {
        let same_bytes = stored
            .fingerprint
            .as_ref()
            .and_then(|f| f.bytes.as_deref())
            .is_some_and(|bytes| bytes == view.bytes());
        if same_bytes {
            stored = Anchors {
                fingerprint: None,
                pending_section: None,
                ..stored.clone()
            };
            baseline = None;
            resolution = resilience::resolve(view, &as_stored(fact, None, true, &stored));
            info!(event = "corporate_evidence.reader_rebaselined", fact_id = %fact.id,
                  program_id = %fact.program_id, confirms = resolution.confirms(),
                  "a baseline of another reader version was re-read on the same page bytes");
        }
    }
    // The way back from a hold: a section changed since the last
    // confirmation holds the fact; once the same new section has stood a
    // day, the second reader agrees on the quote and the text added says no
    // ending, the fact takes the page as its baseline and confirms on it.
    let mut pending: Option<Anchors> = None;
    if way_back && resolution == Resolution::Ambiguous(resilience::SECTION_CHANGED.into()) {
        if let Some(digest) =
            resilience::held_section(view, &as_stored(fact, baseline, open, &stored))
        {
            let now = Utc::now();
            let since = stored
                .pending_section
                .as_ref()
                .filter(|p| p.digest == digest)
                .map_or(now, |p| p.since);
            let fresh = FreshExtraction {
                html,
                url,
                quote: &fact.quote,
                readers_agree: resilience::extractors_agree(&fact.quote),
                first_read_at: since,
                last_read_at: now,
            };
            let rebaselined = fresh_anchors(&as_stored(fact, baseline, open, &stored), &fresh);
            match rebaselined {
                Ok(rebased) => {
                    info!(event = "corporate_evidence.rebaselined", fact_id = %fact.id,
                          program_id = %fact.program_id, "a held fact's new section stood a day: re-baselined");
                    stored = rebased;
                    resolution =
                        resilience::resolve(view, &as_stored(fact, baseline, open, &stored));
                }
                Err(_) => {
                    pending = Some(Anchors {
                        pending_section: Some(resilience::PendingSection { digest, since }),
                        ..stored.clone()
                    });
                }
            }
        }
    }
    // New locators; the quote history grows only where the quote is really
    // replaced (live relocation, applied update), and the first quote is
    // kept for good so the audit seed never re-inserts it.
    let with_fingerprint = |mut fingerprint: resilience::Fingerprint, replaces_quote: bool| {
        // The first sight is kept while the page reads the same.
        if let Some(previous) = stored
            .fingerprint
            .as_ref()
            .filter(|previous| previous.page == fingerprint.page)
        {
            fingerprint.seen = previous.seen.or(fingerprint.seen);
        }
        let mut next = Anchors {
            fingerprint: Some(fingerprint),
            pending_section: None,
            ..stored.clone()
        };
        if replaces_quote {
            next.previous_quotes.insert(0, fact.quote.clone());
            next.previous_quotes.truncate(3);
            next.origin_quote.get_or_insert_with(|| fact.quote.clone());
        }
        next
    };
    // Shadow stores locators only from an exact confirmation, so a wrong
    // shadow relocation never becomes the first live run's starting point.
    let anchors = if resolution.confirms()
        && (live || matches!(resolution, Resolution::Exact(FactOutcome::Confirmed { .. })))
    {
        let (located, replaced) = match &resolution {
            Resolution::Relocated { quote, .. } => (quote.as_str(), live && *quote != fact.quote),
            _ => (fact.quote.as_str(), false),
        };
        resilience::capture(view, located).map(|fp| with_fingerprint(fp, replaced))
    } else {
        pending
    };
    let shadow_update = (!resolution.confirms())
        .then(|| {
            resilience::shadow_update(
                view,
                &StoredFact {
                    quote: &fact.quote,
                    baseline,
                    ages: Ages {
                        min: fact.min_age,
                        max: fact.max_age,
                    },
                    adoption_open: open,
                    anchors: &stored,
                },
            )
        })
        .flatten();
    let page_changed = stored
        .fingerprint
        .as_ref()
        .is_some_and(|f| f.page != view.page_digest());
    Assessed {
        resolution,
        anchors,
        shadow_update,
        page_changed,
    }
}

fn change_key(parts: &str) -> String {
    hex(&Sha256::digest(parts.as_bytes()))
}

/// What the result of a SQL sighting upsert means: agreement in any mode,
/// and agreement between two live sightings at least 24 h apart (the only
/// one that applies a change).
const AGREEMENT_RETURNING: &str = "RETURNING agreed_at IS NOT NULL AND extractors_agree,
             agreed_at IS NOT NULL AND extractors_agree AND first_live_at IS NOT NULL
             AND now() - first_live_at >= interval '24 hours'";

/// Records one sighting of a moved page whose every fact confirmed there.
/// Agreed once a second sighting at least 24 h after the first agrees;
/// applicable only once two live sightings are 24 h apart.
async fn record_url_proposal(
    tx: &mut Transaction<'_, Postgres>,
    target: &Target,
    final_url: &str,
    via: &str,
    live: bool,
) -> Result<(bool, bool)> {
    let key = change_key(&format!("url|{final_url}"));
    sqlx::query(
        "UPDATE corporate_change_proposals SET status = 'superseded'
         WHERE status = 'pending' AND kind = 'url' AND candidate_id = $1 AND change_key <> $2",
    )
    .bind(target.candidate_id)
    .bind(&key)
    .execute(&mut **tx)
    .await?;
    Ok(sqlx::query_as(&format!(
        "INSERT INTO corporate_change_proposals
             (kind, candidate_id, change_key, old_value, new_value, extractors_agree, details, first_live_at)
         VALUES ('url', $1, $2, $3, $4, true, $5, CASE WHEN $6 THEN now() END)
         ON CONFLICT (candidate_id, change_key) WHERE status = 'pending' DO UPDATE SET
             sightings = corporate_change_proposals.sightings + 1,
             last_seen_at = now(),
             first_live_at = COALESCE(corporate_change_proposals.first_live_at, EXCLUDED.first_live_at),
             agreed_at = CASE
                 WHEN corporate_change_proposals.agreed_at IS NULL
                      AND now() - corporate_change_proposals.first_seen_at >= interval '24 hours'
                 THEN now() ELSE corporate_change_proposals.agreed_at END
         {AGREEMENT_RETURNING}"
    ))
    .bind(target.candidate_id)
    .bind(&key)
    .bind(&target.url)
    .bind(final_url)
    .bind(json!({"via": via}))
    .bind(live)
    .fetch_one(&mut **tx)
    .await?)
}

/// Moves the source to its new URL (live, after agreement). The programme's
/// own link follows when it pointed at the old URL. False when another
/// source already owns the new URL.
async fn adopt_url(
    tx: &mut Transaction<'_, Postgres>,
    target: &Target,
    final_url: &str,
) -> Result<bool> {
    let mut canonical = Url::parse(final_url)?;
    canonical.set_fragment(None);
    let canonical = canonical.to_string();
    let taken: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM benefit_source_candidates WHERE canonical_url = $1 AND id <> $2)",
    )
    .bind(&canonical)
    .bind(target.candidate_id)
    .fetch_one(&mut **tx)
    .await?;
    if taken {
        return Ok(false);
    }
    sqlx::query("UPDATE benefit_source_candidates SET url = $2, canonical_url = $3, updated_at = now() WHERE id = $1")
        .bind(target.candidate_id)
        .bind(final_url)
        .bind(&canonical)
        .execute(&mut **tx)
        .await?;
    sqlx::query(
        "UPDATE programs p SET official_url = $2, updated_at = now()
         WHERE p.official_url = $3
           AND EXISTS (SELECT 1 FROM program_source_monitors m WHERE m.program_id = p.id AND m.candidate_id = $1)",
    )
    .bind(target.candidate_id)
    .bind(final_url)
    .bind(&target.url)
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "UPDATE corporate_change_proposals SET status = 'applied', applied_at = now()
         WHERE status = 'pending' AND kind = 'url' AND candidate_id = $1",
    )
    .bind(target.candidate_id)
    .execute(&mut **tx)
    .await?;
    info!(event = "corporate_evidence.url_adopted", candidate_id = %target.candidate_id,
          "moved source URL adopted after two agreeing fetches");
    Ok(true)
}

static SITEMAP_LOC: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)<loc>\s*([^<\s]+)\s*</loc>").expect("valid"));
const MAX_SITEMAP_URLS: usize = 5_000;
const REDISCOVERY_FETCHES: usize = 3;

/// `<loc>` entries of the host's sitemaps (robots-checked): the same-host
/// `Sitemap:` lines of robots.txt first, then `/sitemap.xml`, following at
/// most three child sitemaps of a sitemap index on the same host.
pub(crate) async fn sitemap_urls(
    session: &mut DomainSession<'_>,
    origin: &Url,
    gray_robots_missing: bool,
) -> Vec<Url> {
    let mut root = origin.clone();
    root.set_path("/sitemap.xml");
    root.set_query(None);
    root.set_fragment(None);
    let mut queue = vec![root.clone()];
    if let policy::RobotsState::Ok { body, .. } = session.robots(origin).await {
        for line in body.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            let Some((field, value)) = line.split_once(':') else {
                continue;
            };
            if !field.trim().eq_ignore_ascii_case("sitemap") {
                continue;
            }
            if let Ok(url) = Url::parse(value.trim()) {
                if url.host_str() == origin.host_str()
                    && url.scheme() == origin.scheme()
                    && url != root
                    && !queue.contains(&url)
                    && queue.len() < 4
                {
                    // Popped last-first: robots' own sitemaps are read first.
                    queue.push(url);
                }
            }
        }
    }
    let mut urls = Vec::new();
    let mut fetched = 0;
    while let Some(next) = queue.pop() {
        fetched += 1;
        if fetched > 4 {
            break;
        }
        let Fetched::Reply(reply) = session
            .text_file(&next, MAX_PAGE_BYTES, gray_robots_missing)
            .await
        else {
            break;
        };
        if reply.status != 200 {
            continue;
        }
        let body = String::from_utf8_lossy(&reply.body);
        let index = body.contains("<sitemapindex");
        for capture in SITEMAP_LOC.captures_iter(&body) {
            let Ok(url) = Url::parse(capture[1].trim()) else {
                continue;
            };
            if url.host_str() != origin.host_str() {
                continue;
            }
            if index {
                if queue.len() < 3 {
                    queue.push(url);
                }
            } else if urls.len() < MAX_SITEMAP_URLS {
                urls.push(url);
            }
        }
    }
    urls
}

/// Both URLs on one registrable domain (a brand's own subdomains), over
/// the same scheme; never across tenants of a shared host.
/// Both URLs on one registrable domain by the Public Suffix List (a
/// brand's own subdomains), over the same scheme; never across tenants of a
/// shared host on either side.
/// Whether a redirect from `from` to another host `next` is followed: only
/// for a URL a census rated direct HTML (an unrated URL is never read off
/// its host, review A-H2.3) and only within one registrable domain. A
/// redirect not followed answers `redirected`.
fn follows_off_host(census_rated: bool, from: &Url, next: &Url) -> bool {
    census_rated && same_registrable(next, from)
}

fn same_registrable(a: &Url, b: &Url) -> bool {
    use mazimi_core::source_policy::is_shared_hosting;
    let (Some(x), Some(y)) = (a.host_str(), b.host_str()) else {
        return false;
    };
    let registrable = |host: &str| {
        psl::domain(host.as_bytes())
            .filter(|d| d.suffix().is_known())
            .map(|d| d.as_bytes().to_ascii_lowercase())
    };
    a.scheme() == b.scheme()
        && !is_shared_hosting(x)
        && !is_shared_hosting(y)
        && registrable(x).is_some_and(|d| registrable(y) == Some(d))
}

/// The redirect target's host belongs to a verified source domain that our
/// domain policy lets us read ([`policy::ELIGIBLE_DOMAIN_SQL`]).
async fn target_domain_eligible(pool: &PgPool, url: &Url) -> Result<bool> {
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    Ok(sqlx::query_scalar(&format!(
        "SELECT EXISTS (SELECT 1 FROM benefit_source_domains d
                        WHERE ($1 = lower(d.domain) OR $1 LIKE '%.' || lower(d.domain)) AND {eligible})",
        eligible = policy::ELIGIBLE_DOMAIN_SQL
    ))
    .bind(host)
    .fetch_one(pool)
    .await?)
}

fn same_site(a: &Url, b: &Url) -> bool {
    let strip = |u: &Url| {
        u.host_str()
            .map(|h| h.trim_start_matches("www.").to_string())
    };
    a.scheme() == b.scheme()
        && a.port_or_known_default() == b.port_or_known_default()
        && strip(a).is_some()
        && strip(a) == strip(b)
}

/// After a 404/410, or a robots disallow on the page: looks for the page
/// on the same verified domain (its sitemap; no search engine, whose terms
/// forbid storing or processing its results), fetches the closest few under the same policy
/// and returns the first page on which every fact of the source confirms
/// as stored (its section included, so another page counts only where it
/// states the offer the same way). Adoption still needs agreement.
/// `origin`: [`Origin::Fallback`] (robots) takes exact quotes only,
/// [`Origin::Rediscovered`] (404/410) the redesign-tolerant reads too.
async fn rediscover(
    pool: &PgPool,
    session: &mut DomainSession<'_>,
    target: &Target,
    gone: &Url,
    origin: Origin,
) -> Option<PageResult> {
    let mut connection = pool.acquire().await.ok()?;
    let facts = load_facts(&mut connection, target.candidate_id)
        .await
        .ok()?;
    drop(connection);
    if facts.is_empty() {
        return None;
    }
    let found: Vec<(Url, &'static str)> = sitemap_urls(session, gone, target.gray_robots_missing)
        .await
        .into_iter()
        .map(|u| (u, "sitemap"))
        .collect();
    let mut seen = HashSet::new();
    let mut ranked: Vec<(f64, Url, &'static str)> = found
        .into_iter()
        .filter(|(u, _)| same_site(u, gone) && u != gone && seen.insert(u.to_string()))
        .map(|(u, via)| {
            let closeness = resilience::similarity(gone.path(), u.path());
            (closeness, u, via)
        })
        .collect();
    ranked.sort_by(|a, b| b.0.total_cmp(&a.0));
    for (_, url, via) in ranked.into_iter().take(REDISCOVERY_FETCHES) {
        if policy::opted_out(pool, &url).await.unwrap_or(true) {
            continue;
        }
        let (fetched, state) = session.page(&url, target.gray_robots_missing).await;
        let Fetched::Reply(reply) = fetched else {
            if session.stopped().is_some() {
                break;
            }
            continue;
        };
        if !matches!(state, PageState::Html { .. }) {
            continue;
        }
        let Ok(html) = crate::sources::benefit_pages::decode_page(&reply.body, &reply.content_type)
        else {
            continue;
        };
        let page = visible_text(&html);
        let Ok((_, assessed)) = assess_all(&html, &page, url.as_str(), &facts, true).await else {
            continue;
        };
        let matches = assessed.iter().all(|item| match origin {
            Origin::Fallback => matches!(
                item.resolution,
                Resolution::Exact(FactOutcome::Confirmed { .. })
            ),
            _ => item.resolution.confirms(),
        });
        if matches {
            let hash = PageEvidence::from_visible_text(&page).hash();
            return Some(PageResult::Evidence {
                page,
                html,
                hash,
                http_status: reply.status,
                final_url: url.to_string(),
                via,
                origin,
            });
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
async fn record_check(
    tx: &mut Transaction<'_, Postgres>,
    candidate_id: Uuid,
    program_id: Option<Uuid>,
    mode: VerifyMode,
    outcome: &str,
    applied: bool,
    http_status: Option<u16>,
    details: Value,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO corporate_evidence_checks (candidate_id, program_id, mode, outcome, applied, http_status, details)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(candidate_id)
    .bind(program_id)
    .bind(mode.as_str())
    .bind(outcome)
    .bind(applied)
    .bind(http_status.map(i32::from))
    .bind(details)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn source_check(
    tx: &mut Transaction<'_, Postgres>,
    candidate_id: Uuid,
    kind: &str,
    hash: Option<&str>,
    note: &str,
) -> Result<Uuid> {
    Ok(sqlx::query_scalar(
        "INSERT INTO benefit_source_checks (candidate_id, check_kind, content_hash, error_summary, review_status)
         VALUES ($1, $2, $3, $4, 'not_required') RETURNING id",
    )
    .bind(candidate_id)
    .bind(kind)
    .bind(hash)
    .bind(note)
    .fetch_one(&mut **tx)
    .await?)
}

/// Shadow and hide_only: an offer whose every fact confirmed moves its
/// review deadline by [`EVIDENCE_MAX_AGE_DAYS`], as a live confirmation
/// does, and nothing else: the monitor state, holds and the candidate are
/// left as they are, so nothing is made visible.
async fn extend_review(
    tx: &mut Transaction<'_, Postgres>,
    candidate_id: Uuid,
    program_id: Uuid,
) -> Result<()> {
    sqlx::query(
        "UPDATE program_source_monitors
         SET review_due_at = GREATEST(review_due_at, now() + make_interval(days => $3)),
             updated_at = now()
         WHERE program_id = $1 AND candidate_id = $2 AND verification_state = 'current'",
    )
    .bind(program_id)
    .bind(candidate_id)
    .bind(EVIDENCE_MAX_AGE_DAYS as i32)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Live: the monitor becomes current for [`EVIDENCE_MAX_AGE_DAYS`]. The
/// evidence page hash is kept apart from the hash path's verified hash, so
/// returning a source to the hash path does not see every page as changed. If the
/// job stops, the existing overdue sweep hides the offer after that.
async fn confirm_program(
    tx: &mut Transaction<'_, Postgres>,
    candidate_id: Uuid,
    program_id: Uuid,
    hash: &str,
) -> Result<()> {
    let keys: Vec<String> = sqlx::query_scalar(
        "SELECT quote_key FROM corporate_offer_facts WHERE program_id = $1 AND status <> 'retired' ORDER BY quote_key",
    )
    .bind(program_id)
    .fetch_all(&mut **tx)
    .await?;
    let set_hash = hex(&Sha256::digest(keys.join(",").as_bytes()));
    source_check(
        tx,
        candidate_id,
        "unchanged",
        Some(hash),
        "evidence confirmed",
    )
    .await?;
    sqlx::query(
        "UPDATE program_source_monitors
         SET verification_state = 'current', evidence_page_hash = $3, evidence_verified_at = now(),
             verified_at = now(),
             review_due_at = now() + make_interval(days => $5), evidence_set_hash = $4,
             change_detected_at = NULL, last_error = NULL, updated_at = now()
         WHERE program_id = $1 AND candidate_id = $2",
    )
    .bind(program_id)
    .bind(candidate_id)
    .bind(hash)
    .bind(set_hash)
    .bind(EVIDENCE_MAX_AGE_DAYS as i32)
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "UPDATE benefit_source_candidates
         SET status = 'approved', last_successful_check_at = now(),
             last_checked_at = now(), consecutive_failures = 0, last_error = NULL,
             next_check_at = now() + make_interval(days => $2), updated_at = now()
         WHERE id = $1 AND status IN ('approved', 'change_review', 'unreachable_review', 'verification_due', 'fetched', 'error')",
    )
    .bind(candidate_id)
    .bind(EVIDENCE_MAX_AGE_DAYS as i32)
    .execute(&mut **tx)
    .await?;
    crate::source_monitoring::refresh_program_health(tx, candidate_id).await
}

/// Releases every open hold of programmes whose monitors are all current
/// again, restoring the publication state captured when they were held.
/// Retired, expired and manually closed programmes never come back.
///
/// A confirmation is a verbatim re-read of the official page under robots,
/// the opt-out check and the domain policy, so it also lifts holds for
/// revoked access (robots, a wall, a grace window run out) and for a page
/// gone: the read itself proves both over. Only a publisher opt-out waits
/// for its own flow ([`super::optout`]).
async fn restore_confirmed(tx: &mut Transaction<'_, Postgres>, candidate_id: Uuid) -> Result<()> {
    sqlx::query(
        "WITH recoverable AS (
             SELECT p.id, bool_or(h.previous_is_active) AS active,
                    min(h.previous_program_status) FILTER (WHERE h.previous_is_active) AS status
             FROM programs p
             JOIN program_source_holds h ON h.program_id = p.id AND h.resolved_at IS NULL
             WHERE p.source_health = 'current' AND p.program_status = 'review_required'
               AND p.retirement_reason IS NULL
               AND (LEAST(p.benefit_valid_until, p.application_end_at, p.deadline_at) IS NULL
                    OR LEAST(p.benefit_valid_until, p.application_end_at, p.deadline_at) > now())
               AND EXISTS (SELECT 1 FROM program_source_monitors m WHERE m.program_id = p.id AND m.candidate_id = $1)
               AND NOT EXISTS (SELECT 1 FROM program_source_monitors m
                               WHERE m.program_id = p.id AND m.verification_state NOT IN ('current', 'retired'))
               -- reverify::ONLY_OWN_FLOW_LIFTS (a publisher opt-out).
               AND NOT EXISTS (SELECT 1 FROM program_source_holds o
                               WHERE o.program_id = p.id AND o.resolved_at IS NULL
                                 AND o.hold_reason = ANY($2))
             GROUP BY p.id
         )
         UPDATE programs p
         SET is_active = r.active, program_status = COALESCE(r.status, 'review_required'), updated_at = now()
         FROM recoverable r WHERE p.id = r.id AND r.active",
    )
    .bind(candidate_id)
    .bind(reverify::ONLY_OWN_FLOW_LIFTS)
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "UPDATE program_source_holds h
         SET resolved_at = now(), resolution = 'confirmed_unchanged',
             resolution_note = 'Automatically restored: verbatim evidence re-verified on the live official page'
         FROM programs p
         WHERE p.id = h.program_id AND h.resolved_at IS NULL AND p.is_active AND p.source_health = 'current'
           AND h.hold_reason <> ALL($2)
           AND EXISTS (SELECT 1 FROM program_source_monitors m WHERE m.program_id = p.id AND m.candidate_id = $1)",
    )
    .bind(candidate_id)
    .bind(reverify::ONLY_OWN_FLOW_LIFTS)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Live: hides one programme with a reversible hold. No operator event is
/// raised; the daily digest lists it instead.
async fn hide_program(
    tx: &mut Transaction<'_, Postgres>,
    candidate_id: Uuid,
    program_id: Uuid,
    reason: &str,
    outcome: &str,
) -> Result<()> {
    let check_id = source_check(
        tx,
        candidate_id,
        "changed",
        None,
        &format!("evidence: {outcome}"),
    )
    .await?;
    sqlx::query(
        "INSERT INTO program_source_holds (program_id, candidate_id, source_check_id, hold_reason, previous_is_active, previous_program_status)
         SELECT p.id, $2, $3, $4, p.is_active, p.program_status FROM programs p WHERE p.id = $1
         ON CONFLICT (program_id, candidate_id) WHERE resolved_at IS NULL DO NOTHING",
    )
    .bind(program_id)
    .bind(candidate_id)
    .bind(check_id)
    .bind(reason)
    .execute(&mut **tx)
    .await?;
    let state = match reason {
        "source_unreachable" => "unreachable",
        _ => "change_detected",
    };
    sqlx::query(
        "UPDATE program_source_monitors
         SET verification_state = $3, change_detected_at = COALESCE(change_detected_at, now()),
             last_error = $4, updated_at = now()
         WHERE program_id = $1 AND candidate_id = $2 AND verification_state <> 'retired'",
    )
    .bind(program_id)
    .bind(candidate_id)
    .bind(state)
    .bind(format!("evidence: {outcome}"))
    .execute(&mut **tx)
    .await?;
    crate::source_monitoring::refresh_program_health(tx, candidate_id).await
}

/// Live: 404/410 on two fetches at least 24 h apart.
async fn retire_program(
    tx: &mut Transaction<'_, Postgres>,
    candidate_id: Uuid,
    program_id: Uuid,
) -> Result<()> {
    source_check(
        tx,
        candidate_id,
        "retired",
        None,
        "evidence: page gone twice 24 h apart",
    )
    .await?;
    sqlx::query(
        "UPDATE program_source_monitors SET verification_state = 'retired', last_error = 'page gone', updated_at = now()
         WHERE program_id = $1 AND candidate_id = $2",
    )
    .bind(program_id)
    .bind(candidate_id)
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "UPDATE programs SET is_active = false, program_status = 'closed', retired_at = COALESCE(retired_at, now()),
             retirement_reason = 'provider_withdrawn', updated_at = now()
         WHERE id = $1",
    )
    .bind(program_id)
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "UPDATE program_source_holds SET resolved_at = now(), resolution = 'retired',
             resolution_note = 'Automatically retired: the official page returned 404/410 twice at least 24 h apart'
         WHERE program_id = $1 AND resolved_at IS NULL",
    )
    .bind(program_id)
    .execute(&mut **tx)
    .await?;
    sqlx::query("UPDATE corporate_offer_facts SET status = 'retired', updated_at = now() WHERE program_id = $1")
        .bind(program_id)
        .execute(&mut **tx)
        .await?;
    crate::source_monitoring::refresh_program_health(tx, candidate_id).await
}

/// Whether live evidence verification is on.
pub fn live_enabled() -> bool {
    Flags::from_env()
        .is_ok_and(|flags| flags.verify_enabled && flags.verify_mode == VerifyMode::Live)
}

/// Whether evidence verification owns `evidence_auto` monitors: on, in any
/// mode but shadow (`hide_only` included). The hash fetcher and the
/// upcoming-review warning step aside for them, so a hash recheck can never
/// release a hold evidence placed.
pub fn owns_sources() -> bool {
    Flags::from_env()
        .is_ok_and(|flags| flags.verify_enabled && flags.verify_mode != VerifyMode::Shadow)
}

/// Worker entry point; logs instead of failing the scheduler.
pub async fn run_scheduled(pool: &PgPool) {
    static RUNNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if RUNNING.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    struct Release;
    impl Drop for Release {
        fn drop(&mut self) {
            RUNNING.store(false, std::sync::atomic::Ordering::SeqCst);
        }
    }
    let _release = Release;
    run_scheduled_once(pool).await;
}

async fn run_scheduled_once(pool: &PgPool) {
    let flags = match Flags::from_env() {
        Ok(flags) if flags.verify_enabled => flags,
        Ok(_) => return,
        Err(error) => {
            warn!(event = "corporate_evidence.config_invalid", error = %error, "corporate evidence verification misconfigured");
            return;
        }
    };
    let Some(user_agent) = flags.user_agent() else {
        return;
    };
    let http = match super::http::PinnedHttp::new(user_agent, std::time::Duration::from_secs(20)) {
        Ok(http) => http,
        Err(error) => {
            warn!(event = "corporate_evidence.config_invalid", error = %error, "corporate evidence transport rejected");
            return;
        }
    };
    match run(pool, &flags, &http).await {
        Ok(summary) => {
            let health = &summary.health;
            info!(event = "corporate_evidence.run", mode = flags.verify_mode.as_str(), targets = summary.targets,
                  applied = summary.applied, requests = summary.requests, decisions = ?summary.decisions,
                  confirmed = health.confirmed, refuted = health.refuted, transient = health.transient,
                  blocked = health.blocked, in_grace = health.in_grace,
                  hidden_after_grace = health.hidden_after_grace,
                  multi_offer_held = health.multi_offer_held,
                  volatile_held = health.volatile_held,
                  unreadable_share = health.unreadable_share(),
                  "corporate evidence verification pass complete");
            if let Err(error) = report_health(pool, health).await {
                warn!(event = "corporate_evidence.health_alert_failed", error = %crate::error_safety::persisted_error(&error),
                      "could not record the re-verification health alert");
            }
            let _ = operations::resolve_event(
                pool,
                "job:corporate_evidence",
                None,
                Some("Run completed"),
            )
            .await;
        }
        Err(error) => {
            warn!(event = "corporate_evidence.run_failed", error = %crate::error_safety::persisted_error(&error), "corporate evidence verification failed");
            let _ = operations::record_event(
                pool,
                &operations::EventCommand {
                    fingerprint: "job:corporate_evidence",
                    severity: operations::Severity::Error,
                    component: "corporate_auto",
                    event_type: "corporate_evidence_failed",
                    subject_type: None,
                    subject_id: None,
                    message: "Corporate evidence verification run failed",
                    details: json!({}),
                },
            )
            .await;
        }
    }
}

/// The run-level alert: one open operational event while the share of
/// checks we could not read (transient or blocked) is over
/// [`reverify::ALERT_UNREADABLE_SHARE`], resolved by the first run under
/// it. A run with too few checks leaves it as it is.
pub(crate) const HEALTH_ALERT_FINGERPRINT: &str = "job:corporate_evidence_health";

pub(crate) async fn report_health(pool: &PgPool, health: &RunHealth) -> Result<()> {
    match health.alert() {
        Some(true) => {
            operations::record_event(
                pool,
                &operations::EventCommand {
                    fingerprint: HEALTH_ALERT_FINGERPRINT,
                    severity: operations::Severity::Warning,
                    component: "corporate_auto",
                    event_type: "corporate_reverify_degraded",
                    subject_type: None,
                    subject_id: None,
                    message: "Many official benefit pages could not be re-verified in the last evidence run",
                    details: json!({
                        "confirmed": health.confirmed, "refuted": health.refuted,
                        "transient": health.transient, "blocked": health.blocked,
                        "in_grace": health.in_grace, "hidden_after_grace": health.hidden_after_grace,
                        "multi_offer_held": health.multi_offer_held,
                        "volatile_held": health.volatile_held,
                        "unreadable_share": health.unreadable_share(),
                        "threshold": reverify::ALERT_UNREADABLE_SHARE,
                    }),
                },
            )
            .await?;
        }
        Some(false) => {
            operations::resolve_event(
                pool,
                HEALTH_ALERT_FINGERPRINT,
                None,
                Some("Automatically resolved: the share of unreadable checks fell under the threshold"),
            )
            .await?;
        }
        None => {}
    }
    Ok(())
}

#[cfg(test)]
#[path = "evidence_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "issuer_tests.rs"]
mod issuer_tests;
