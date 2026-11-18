//! The hub row reader (review C1, H1-H4, H8, M7): the one hub lane. A hub
//! list page (T 멤버십 partner brands, KB국민카드 brand discounts, PAYCO
//! coupons) is read from its rows' own visible text. Nothing is composed
//! from data attributes and no script anchor is followed.
//!
//! Generic, with no stored selector:
//! 1. **Rows** are the visible element children of one parent that share a
//!    tag (class tokens vary between rows: zebra stripes, `active`), at least
//!    [`MIN_ROWS`] of them. The page is a listing when the best such group
//!    has at least [`MIN_ROWS`] rows holding a value and they are at least
//!    half of it; otherwise the read is `listing_unrecognised` and nothing
//!    is accepted or hidden.
//! 2. **Brand**: a row binds a registry brand only when the whole visible
//!    text of one of its elements equals one of that brand's names
//!    ([`Registry::exact`]); two brands, or a logo `alt` naming another
//!    brand, reject it. The hub's own brand never binds.
//! 3. **Value**: each visible text holding a `%` or `원` value is one
//!    statement. Its parenthesised notes are restrictions, not values; what
//!    remains must hold exactly one value, bound to a discount cue after it
//!    (or to a cell whose whole text is the cue). A range, a per-amount
//!    rate ("1천원당 100원"), an 적립 or 캐시백 statement is not read.
//!    "최대 N" is kept as an upper bound and shown verbatim.
//! 4. **Tiers** (review H1): a hidden accessible label (`<span class=
//!    'blind'>G</span>`) groups the statements it sits in; it is never
//!    evidence text. Statements sharing a parent are one group, and the
//!    labels of every member are its tiers. Only what holds for every tier
//!    is accepted: each tier must have exactly one discount of the same kind
//!    and bound; the lowest is kept and labelled as such.
//! 5. **Period**: a stated period must parse with its year (a two-digit
//!    year only within [today − 1 y, today + 2 y]), have started and not
//!    ended (KST). The period never enters the offer key: an identical row
//!    with a new period continues the same offer (review H8).
//! 6. **Per brand**: two accepted rows of one brand with different values
//!    reject both (`conflicting_values`); identical ones are one offer.
//!
//! Pure. Only counts and accepted offers leave this module; no row text of
//! an unmatched row is kept.

use chrono::{Datelike, NaiveDate};
use ego_tree::iter::Edge;
use ego_tree::{NodeId, NodeRef};
use regex::Regex;
use scraper::{ElementRef, Html, Node};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;
use uuid::Uuid;

use super::registry::Registry;
use crate::corporate_auto::evidence::{canonical, element_hidden, strip_comments};
use crate::corporate_auto::values::value_spans;

/// A listing has at least this many rows, and this many holding a value.
pub const MIN_ROWS: usize = 5;
/// The longest row line shown (the ledger keeps 100 characters).
pub const MAX_LINE_CHARS: usize = 100;
/// The deepest element nesting read; a deeper page is not a listing
/// (security review M2: every walk below is iterative or bounded by it).
pub const MAX_DEPTH: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ValueKind {
    /// `N%` off.
    Percent,
    /// `N원` off.
    Won,
}

impl ValueKind {
    pub fn fact_kind(self) -> &'static str {
        match self {
            Self::Percent => "discount_percent",
            Self::Won => "krw",
        }
    }
}

/// How the accepted value relates to the hub's tiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum TierReading {
    /// The row states one value for everyone.
    Untiered,
    /// Every tier gets the same value.
    EveryTierSame,
    /// Tiers differ; the value is the lowest, which every tier gets.
    EveryTierMinimum,
}

/// One accepted row offer.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RowOffer {
    pub brand_id: Uuid,
    pub kind: ValueKind,
    /// Canonical number: `10`, `9.5` (percent) or `4000` (won).
    pub value: String,
    /// The hub states it as `최대 N`: an upper bound, shown as written.
    pub upper_bound: bool,
    pub tiers: TierReading,
    /// The row as a person reads it (review H-2, M-3, M-4): the brand as
    /// the row prints it, then the row's visible text without the period,
    /// link labels and the other tiers' statements. At most
    /// [`MAX_LINE_CHARS`] characters; a longer row is refused.
    pub quote: String,
    pub starts: Option<NaiveDate>,
    pub ends: Option<NaiveDate>,
    /// (grantor, brand, kind, value, bound): never the period.
    pub offer_key: String,
}

impl RowOffer {
    /// The value as displayed: `최대 4,000원`, `10%`.
    pub fn value_text(&self) -> String {
        let number = match self.kind {
            ValueKind::Percent => format!("{}%", self.value),
            ValueKind::Won => format!("{}원", group_thousands(&self.value)),
        };
        if self.upper_bound {
            format!("최대 {number}")
        } else {
            number
        }
    }

    /// The title: see [`title_for`].
    pub fn title(&self, grantor: &str) -> String {
        title_for(grantor, &self.quote, self.tiers)
    }
}

/// The hub offer title: `{grantor} · {row line}`, the line exactly as the
/// row shows it (its restriction — "특정품목", "가맹점 첫결제 고객",
/// "최대" — stays with the value), and `(등급별 최솟값)` when tiers differ
/// and the statement is the lowest tier's. Never starts with the redeem
/// brand (the issuer trigger, 20261107000001).
pub fn title_for(grantor: &str, line: &str, tiers: TierReading) -> String {
    match tiers {
        TierReading::EveryTierMinimum => format!("{grantor} · {line} (등급별 최솟값)"),
        _ => format!("{grantor} · {line}"),
    }
}

/// `12000` as `12,000`.
pub(crate) fn group_thousands(digits: &str) -> String {
    let bytes: Vec<char> = digits.chars().collect();
    let mut out = String::new();
    for (i, c) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(*c);
    }
    out
}

/// What one read of a hub list page found.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct HubRead {
    pub listing: bool,
    pub rows_seen: usize,
    /// Rows that bound one registry brand.
    pub rows_matched: usize,
    pub offers: Vec<RowOffer>,
    /// Rejections of matched rows, by reason.
    pub rejected: BTreeMap<&'static str, usize>,
    /// Rejections with their brand, for the per-brand metrics.
    pub rejected_brands: Vec<(Uuid, &'static str)>,
    /// Rows the hub states in tiers (H1: measured in shadow).
    pub tiered_rows: usize,
}

impl HubRead {
    pub fn verdict(&self) -> &'static str {
        if self.listing {
            "listing"
        } else {
            "listing_unrecognised"
        }
    }

    pub fn matched_brands(&self) -> BTreeSet<Uuid> {
        self.offers
            .iter()
            .map(|o| o.brand_id)
            .chain(self.rejected_brands.iter().map(|(b, _)| *b))
            .collect()
    }
}

static PARENS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\([^()]*\)").expect("valid"));
static RANGE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\d\s*[~∼]\s*\d[\d,.]*\s*(?:%|원|만\s*원|천\s*원)").expect("valid")
});
static PER_AMOUNT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:\d|천|만)\s*원\s*당|\d+\s*P\s*당").expect("valid"));
static DISCOUNT_AFTER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(?:[가-힣]{1,4}\s*)?할인").expect("valid"));
static EARN_AFTER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(?:[가-힣]{1,4}\s*)?적립").expect("valid"));
static OTHER_AFTER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*(?:[가-힣]{1,4}\s*)?(?:캐시백|증정|페이백|쿠폰|상품권)").expect("valid")
});
/// An upper bound (review M-5): "최대 N", "최고 N", "~N" before the value,
/// "N까지" after it.
static UPPER_BEFORE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:최대|최고|[~∼])\s*$").expect("valid"));
static UPPER_AFTER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*까지").expect("valid"));

// Date tokens (review H-1): every date-like token of a row must be consumed
// by the period read from it.
static FULL_DATE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(20\d{2})\s*(?:[./-]|년)\s*(\d{1,2})\s*(?:[./-]|월)\s*(\d{1,2})(?:\s*일)?")
        .expect("valid")
});
static SHORT_YEAR_DATE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(\d{2})\s*[./-]\s*(\d{1,2})\s*[./-]\s*(\d{1,2})").expect("valid")
});
static MONTH_DAY_WORDS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(\d{1,2})\s*월\s*(\d{1,2})\s*일").expect("valid"));
static MONTH_DAY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(\d{1,2})\s*[./]\s*(\d{1,2})").expect("valid"));
static WEEKDAY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\(\s*[월화수목금토일]\s*\)").expect("valid"));
static WEEKDAY_HEAD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*\(\s*[월화수목금토일]\s*\)").expect("valid"));
const PERIOD_SEPARATORS: &[&str] = &["~", "∼", "〜", "～", "-", "–", "—", "−"];

/// A cell whose whole text is the kind of benefit (PAYCO's 유형 column).
const DISCOUNT_CELL: &str = "할인";
/// Cells naming another kind of benefit: a row that has one beside a bare
/// "할인" cell is a badge row (T 멤버십 "할인/적립/사용"), where "할인"
/// does not say what a bare amount is (review M-6).
const OTHER_KIND_CELLS: &[&str] = &["적립", "사용", "캐시백", "증정", "페이백", "포인트"];

const BLOCK_TAGS: &[&str] = &[
    "div", "p", "li", "dd", "dt", "td", "th", "tr", "dl", "ul", "ol", "section", "article", "h1",
    "h2", "h3", "h4", "h5", "h6", "table", "tbody",
];

/// True when `html` nests elements deeper than [`MAX_DEPTH`]: such a page
/// is read by no reader of the door job.
pub fn nested_too_deep(html: &str) -> bool {
    too_deep(&Html::parse_document(&strip_comments(html)))
}

/// True when the document nests elements deeper than [`MAX_DEPTH`].
fn too_deep(document: &Html) -> bool {
    let mut depth = 0usize;
    for edge in document.tree.root().traverse() {
        match edge {
            Edge::Open(node) if node.value().is_element() => {
                depth += 1;
                if depth > MAX_DEPTH {
                    return true;
                }
            }
            Edge::Close(node) if node.value().is_element() => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    false
}

fn hidden_or_under_hidden(element: ElementRef<'_>, stop: Option<ElementRef<'_>>) -> bool {
    let mut node = Some(*element);
    while let Some(current) = node {
        if let Some(e) = ElementRef::wrap(current) {
            if element_hidden(e.value()) {
                return true;
            }
            if stop.is_some_and(|s| s.id() == e.id()) {
                return false;
            }
        }
        node = current.parent();
    }
    false
}

/// The text a person sees, without control and format characters (security
/// review L1), one space between pieces, case kept.
fn display(text: &str) -> String {
    let format_char = |c: char| {
        matches!(c, '\u{00AD}' | '\u{061C}' | '\u{180E}' | '\u{FEFF}')
            || ('\u{200B}'..='\u{200F}').contains(&c)
            || ('\u{202A}'..='\u{202E}').contains(&c)
            || ('\u{2060}'..='\u{206F}').contains(&c)
    };
    let cleaned: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .filter(|c| !format_char(*c))
        .collect();
    cleaned.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Visible text of `node`, skipping hidden subtrees and whatever `skip`
/// says, in document order. Iterative (security review M2).
fn collect_visible(node: NodeRef<'_, Node>, skip: &dyn Fn(NodeRef<'_, Node>) -> bool) -> String {
    let mut out = String::new();
    let mut skipping: Option<NodeId> = None;
    for edge in node.traverse() {
        match edge {
            Edge::Open(n) => {
                if skipping.is_some() {
                    continue;
                }
                match n.value() {
                    Node::Text(text) => {
                        out.push_str(text);
                        out.push(' ');
                    }
                    Node::Element(element) if element_hidden(element) || skip(n) => {
                        skipping = Some(n.id());
                    }
                    _ => {}
                }
            }
            Edge::Close(n) => {
                if skipping == Some(n.id()) {
                    skipping = None;
                }
            }
        }
    }
    out
}

/// Visible text of `element` as shown (case kept).
fn shown(element: ElementRef<'_>) -> String {
    display(&collect_visible(*element, &|_| false))
}

/// Visible text of `element`, canonical form (for reading values).
fn visible(element: ElementRef<'_>) -> String {
    canonical(&shown(element))
}

/// The hidden accessible labels inside `element` ("V", "G", "S"): short
/// texts of hidden elements, never scripts or templates.
fn hidden_labels(element: ElementRef<'_>) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for node in element.descendants() {
        let Some(e) = ElementRef::wrap(node) else {
            continue;
        };
        if matches!(
            e.value().name(),
            "script" | "style" | "template" | "noscript" | "svg"
        ) {
            continue;
        }
        if element_hidden(e.value()) {
            let text: String = e.text().collect::<String>().trim().to_string();
            let chars = text.chars().count();
            if (1..=4).contains(&chars) && !text.chars().any(|c| c.is_ascii_digit()) {
                out.insert(text);
            }
        }
    }
    out
}

/// The row signature: the tag alone. Class tokens are presentation and
/// vary between rows of one list (PAYCO alternates `bg-a`/`bg-b`, a list
/// marks one item `active`), so they never split a list.
fn signature(element: ElementRef<'_>) -> String {
    element.value().name().to_string()
}

fn has_value(text: &str) -> bool {
    value_spans(text)
        .iter()
        .any(|(_, _, token)| token.ends_with('%') || token.ends_with('원'))
}

/// The element path from `row` to `element`: element-child indices.
fn path_in(row: ElementRef<'_>, element: ElementRef<'_>) -> Vec<usize> {
    let mut path = Vec::new();
    let mut current = *element;
    while current.id() != row.id() {
        let index = current
            .prev_siblings()
            .filter(|s| s.value().is_element())
            .count();
        path.push(index);
        match current.parent() {
            Some(parent) => current = parent,
            None => break,
        }
    }
    path.reverse();
    path
}

/// One element of a row whose whole visible text is a registry brand name.
#[derive(Debug, Clone)]
struct BrandCell {
    path: Vec<usize>,
    brand: Uuid,
    element: NodeId,
}

fn brand_cells(row: ElementRef<'_>, registry: &Registry, grantor: Uuid) -> Vec<BrandCell> {
    let mut out = Vec::new();
    for node in row.descendants() {
        let Some(e) = ElementRef::wrap(node) else {
            continue;
        };
        if hidden_or_under_hidden(e, Some(row)) {
            continue;
        }
        if let Some(brand) = registry.exact(&shown(e)) {
            if brand.brand_id != grantor {
                out.push(BrandCell {
                    path: path_in(row, e),
                    brand: brand.brand_id,
                    element: e.id(),
                });
            }
        }
    }
    out
}

/// The rows of the page (step 1): the sibling group with the most rows that
/// name a registry brand (review H-7), at least [`MIN_ROWS`] of them holding
/// a value and at least half. A group naming no brand is never a listing.
fn find_rows<'a>(
    document: &'a Html,
    registry: &Registry,
    grantor: Uuid,
) -> Option<Vec<ElementRef<'a>>> {
    let mut best: Option<((usize, usize, u64), Vec<ElementRef<'a>>)> = None;
    for node in document.root_element().descendants() {
        let Some(parent) = ElementRef::wrap(node) else {
            continue;
        };
        if element_hidden(parent.value()) {
            continue;
        }
        let mut groups: BTreeMap<String, Vec<ElementRef<'_>>> = BTreeMap::new();
        for child in parent.children().filter_map(ElementRef::wrap) {
            if !element_hidden(child.value()) {
                groups.entry(signature(child)).or_default().push(child);
            }
        }
        for (_, rows) in groups {
            if rows.len() < MIN_ROWS || hidden_or_under_hidden(parent, None) {
                continue;
            }
            let with_value = rows.iter().filter(|r| has_value(&visible(**r))).count();
            if with_value < MIN_ROWS || with_value * 2 < rows.len() {
                continue;
            }
            let bound = rows
                .iter()
                .filter(|r| !brand_cells(**r, registry, grantor).is_empty())
                .count();
            if bound == 0 {
                continue;
            }
            // Share as parts per million, so the key is totally ordered.
            let share = (with_value as u64 * 1_000_000) / rows.len() as u64;
            let key = (bound, with_value, share);
            if best.as_ref().is_none_or(|(k, _)| key > *k) {
                best = Some((key, rows));
            }
        }
    }
    best.map(|(_, rows)| rows)
}

/// One value statement of a row.
#[derive(Debug, Clone)]
struct Statement {
    /// The element whose own text holds the value; its nearest block gives
    /// the labels and the group.
    owner: NodeId,
    read: Result<Value, &'static str>,
}

#[derive(Debug, Clone, PartialEq)]
struct Value {
    kind: ValueKind,
    number: String,
    upper: bool,
}

/// Reads one statement (step 3). `Ok(None)` = states no value.
fn read_statement(text: &str, discount_cell: bool) -> Result<Option<Value>, &'static str> {
    let head = PARENS.replace_all(text, " ").to_string();
    if RANGE.is_match(&head) {
        return Err("range_value");
    }
    let spans: Vec<(usize, usize, String)> = value_spans(&head)
        .into_iter()
        .filter(|(_, _, t)| t.ends_with('%') || t.ends_with('원'))
        .collect();
    let Some((start, end, token)) = spans.first().cloned() else {
        return Ok(None);
    };
    if PER_AMOUNT.is_match(&head) {
        return Err("rate_per_amount");
    }
    if spans.len() > 1 {
        return Err("multi_value_statement");
    }
    let after = &head[end..];
    let cue = if DISCOUNT_AFTER.is_match(after) {
        true
    } else if EARN_AFTER.is_match(after) {
        return Err("earn_not_read");
    } else if OTHER_AFTER.is_match(after) {
        return Err("kind_not_read");
    } else {
        discount_cell && after.trim().is_empty()
    };
    if !cue {
        return Err("no_discount_cue");
    }
    let (kind, number) = if let Some(n) = token.strip_suffix('%') {
        (ValueKind::Percent, n.to_string())
    } else {
        (ValueKind::Won, token.trim_end_matches('원').to_string())
    };
    let valid = match kind {
        ValueKind::Percent => number.parse::<f64>().is_ok_and(|n| n > 0.0 && n <= 90.0),
        ValueKind::Won => number
            .parse::<u64>()
            .is_ok_and(|n| (100..=10_000_000).contains(&n)),
    };
    if !valid {
        return Err("value_out_of_range");
    }
    Ok(Some(Value {
        kind,
        number,
        upper: UPPER_BEFORE.is_match(&head[..start]) || UPPER_AFTER.is_match(after),
    }))
}

fn nearest_block<'a>(element: ElementRef<'a>, row: ElementRef<'a>) -> ElementRef<'a> {
    let mut current = element;
    loop {
        if current.id() == row.id() || BLOCK_TAGS.contains(&current.value().name()) {
            return current;
        }
        match current.parent().and_then(ElementRef::wrap) {
            Some(parent) => current = parent,
            None => return row,
        }
    }
}

/// The value statements of a row: every visible text node holding a value,
/// read in its own element's visible text. `Err` when a value sits under a
/// hidden element of the row (review M-1: a hidden tier would otherwise
/// give the visible tier's value to everyone).
fn statements(row: ElementRef<'_>, discount_cell: bool) -> Result<Vec<Statement>, &'static str> {
    let mut out: Vec<Statement> = Vec::new();
    for node in row.descendants() {
        let Node::Text(text) = node.value() else {
            continue;
        };
        if !has_value(&canonical(text)) {
            continue;
        }
        let Some(owner) = node.parent().and_then(ElementRef::wrap) else {
            continue;
        };
        if matches!(
            owner.value().name(),
            "script" | "style" | "template" | "noscript"
        ) {
            continue;
        }
        if hidden_or_under_hidden(owner, Some(row)) {
            return Err("hidden_value");
        }
        if out.iter().any(|s| s.owner == owner.id()) {
            continue;
        }
        let read = match read_statement(&visible(owner), discount_cell) {
            Ok(Some(value)) => Ok(value),
            Ok(None) => continue,
            Err(reason) => Err(reason),
        };
        out.push(Statement {
            owner: owner.id(),
            read,
        });
    }
    Ok(out)
}

/// One group's reading (step 4).
#[derive(Debug, Clone, PartialEq)]
struct GroupOffer {
    value: Value,
    tiers: TierReading,
    /// The statement shown: the lowest tier's, or the only one.
    owner: NodeId,
}

fn element_by_id(document: &Html, id: NodeId) -> Option<ElementRef<'_>> {
    document.tree.get(id).and_then(ElementRef::wrap)
}

/// Groups the statements of a row and reads each group (step 4).
fn read_groups(
    document: &Html,
    row: ElementRef<'_>,
    found: &[Statement],
) -> (Vec<GroupOffer>, Vec<&'static str>, bool) {
    let mut offers = Vec::new();
    let mut reasons = Vec::new();
    let mut tiered = false;
    // group key: the parent of the statement's block (or the row).
    let mut groups: BTreeMap<NodeId, Vec<&Statement>> = BTreeMap::new();
    for statement in found {
        let Some(owner) = element_by_id(document, statement.owner) else {
            continue;
        };
        let block = nearest_block(owner, row);
        let key = if block.id() == row.id() {
            row.id()
        } else {
            block
                .parent()
                .and_then(ElementRef::wrap)
                .map_or(row.id(), |p| p.id())
        };
        groups.entry(key).or_default().push(statement);
    }
    for (key, members) in groups {
        let Some(parent) = element_by_id(document, key) else {
            continue;
        };
        // Every member block of the group with its labels (members with no
        // value count: their tiers are tiers too).
        let blocks: Vec<ElementRef<'_>> = if key == row.id() {
            vec![row]
        } else {
            parent
                .children()
                .filter_map(ElementRef::wrap)
                .filter(|c| !element_hidden(c.value()))
                .collect()
        };
        let universe: BTreeSet<String> = blocks.iter().flat_map(|b| hidden_labels(*b)).collect();
        let readings: Vec<(BTreeSet<String>, &Statement)> = members
            .iter()
            .map(|s| {
                let labels = element_by_id(document, s.owner)
                    .map(|owner| hidden_labels(nearest_block(owner, row)))
                    .unwrap_or_default();
                (labels, *s)
            })
            .collect();
        let discounts: Vec<(&BTreeSet<String>, &Value, &Statement)> = readings
            .iter()
            .filter_map(|(labels, s)| s.read.as_ref().ok().map(|v| (labels, v, *s)))
            .collect();
        if discounts.is_empty() {
            if let Some(reason) = readings.iter().find_map(|(_, s)| s.read.as_ref().err()) {
                reasons.push(*reason);
            }
            continue;
        }
        if universe.is_empty() {
            if discounts.len() > 1 {
                reasons.push("multi_value_row");
                continue;
            }
            let (_, value, statement) = discounts[0];
            offers.push(GroupOffer {
                value: value.clone(),
                tiers: TierReading::Untiered,
                owner: statement.owner,
            });
            continue;
        }
        tiered = true;
        let mut per_tier: Vec<(&Value, &Statement)> = Vec::new();
        let mut failed = None;
        for tier in &universe {
            let held: Vec<(&Value, &Statement)> = discounts
                .iter()
                .filter(|(labels, ..)| labels.contains(tier))
                .map(|(_, v, s)| (*v, *s))
                .collect();
            match held.len() {
                0 => failed = Some("not_every_tier"),
                1 => per_tier.push(held[0]),
                _ => failed = Some("tier_ambiguous"),
            }
        }
        if let Some(reason) = failed {
            reasons.push(reason);
            continue;
        }
        let first = per_tier[0].0;
        if per_tier
            .iter()
            .any(|(v, _)| v.kind != first.kind || v.upper != first.upper)
        {
            reasons.push("mixed_tier_values");
            continue;
        }
        let number = |v: &Value| v.number.parse::<f64>().unwrap_or(f64::MAX);
        let (lowest, statement) = per_tier
            .iter()
            .min_by(|a, b| number(a.0).total_cmp(&number(b.0)))
            .copied()
            .expect("non-empty");
        let all_same = per_tier.iter().all(|(v, _)| v.number == lowest.number);
        offers.push(GroupOffer {
            value: lowest.clone(),
            tiers: if all_same {
                TierReading::EveryTierSame
            } else {
                TierReading::EveryTierMinimum
            },
            owner: statement.owner,
        });
    }
    (offers, reasons, tiered)
}

/// A period a text states (step 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Period {
    pub starts: Option<NaiveDate>,
    pub ends: NaiveDate,
    /// The byte range of the text the period was read from.
    pub span: (usize, usize),
}

#[derive(Debug, Clone, Copy)]
struct DateToken {
    start: usize,
    end: usize,
    /// `Some((year, two_digit))`; `None` for a yearless token.
    year: Option<(i32, bool)>,
    month: u32,
    day: u32,
}

/// Every date-like token of `text`, in order: full dates, two-digit-year
/// dates, `N월 N일`, `M.D` / `M/D` (never a decimal value such as `9.5%`).
fn date_tokens(text: &str) -> Vec<DateToken> {
    let mut masked = text.to_string();
    let mut out: Vec<DateToken> = Vec::new();
    let free = |masked: &str, start: usize, end: usize| {
        let before = masked[..start].chars().next_back();
        let after = masked[end..].trim_start().chars().next();
        !before.is_some_and(|c| c.is_ascii_digit() || c == '.')
            && !masked[end..].starts_with(|c: char| c.is_ascii_digit() || c == '.')
            && !after.is_some_and(|c| matches!(c, '%' | '원' | '만' | '천' | '배' | 'P' | 'p'))
    };
    let patterns: [(&Regex, bool); 4] = [
        (&FULL_DATE, true),
        (&SHORT_YEAR_DATE, true),
        (&MONTH_DAY_WORDS, false),
        (&MONTH_DAY, false),
    ];
    for (pattern, with_year) in patterns {
        let found: Vec<(usize, usize, Vec<String>)> = pattern
            .captures_iter(&masked)
            .filter_map(|c| {
                let whole = c.get(0)?;
                let parts = c
                    .iter()
                    .skip(1)
                    .map(|m| m.map_or(String::new(), |m| m.as_str().to_string()))
                    .collect();
                Some((whole.start(), whole.end(), parts))
            })
            .collect();
        for (start, end, parts) in found {
            if !free(&masked, start, end) {
                continue;
            }
            let number = |i: usize| parts.get(i).and_then(|p| p.parse::<u32>().ok());
            let token = if with_year {
                let raw = &parts[0];
                let Some(year) = raw.parse::<i32>().ok() else {
                    continue;
                };
                DateToken {
                    start,
                    end,
                    year: Some((
                        if raw.len() == 2 { 2000 + year } else { year },
                        raw.len() == 2,
                    )),
                    month: number(1).unwrap_or(0),
                    day: number(2).unwrap_or(0),
                }
            } else {
                let (Some(month), Some(day)) = (number(0), number(1)) else {
                    continue;
                };
                if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
                    continue;
                }
                DateToken {
                    start,
                    end,
                    year: None,
                    month,
                    day,
                }
            };
            masked.replace_range(start..end, &" ".repeat(end - start));
            out.push(token);
        }
    }
    out.sort_by_key(|t| t.start);
    out
}

/// The period `text` states (step 5): `Ok(None)` when it holds no date
/// token. Every date token must belong to the period read (review H-1):
/// `A ~ B` (a weekday in parentheses allowed after each), `~ B` or
/// `B까지`; anything else is `period_unparsed`, a yearless token
/// `period_without_year`.
pub fn row_period(text: &str, today: NaiveDate) -> Result<Option<Period>, &'static str> {
    let tokens = date_tokens(text);
    if tokens.is_empty() {
        return Ok(None);
    }
    if tokens.iter().any(|t| t.year.is_none()) {
        return Err("period_without_year");
    }
    let date = |t: &DateToken| -> Result<NaiveDate, &'static str> {
        let (year, two_digit) = t.year.expect("checked");
        if two_digit && !(today.year() - 1..=today.year() + 2).contains(&year) {
            return Err("period_year_implausible");
        }
        NaiveDate::from_ymd_opt(year, t.month, t.day).ok_or("period_unparsed")
    };
    let weekday_after = |end: usize| {
        WEEKDAY_HEAD
            .find(&text[end..])
            .map_or(end, |m| end + m.end())
    };
    match tokens.as_slice() {
        [only] => {
            let ends = date(only)?;
            let before = text[..only.start].trim_end();
            let after_at = weekday_after(only.end);
            let after = &text[after_at..];
            if let Some(separator) = ["~", "∼", "〜", "～"]
                .iter()
                .find(|s| before.ends_with(**s))
            {
                let start = before.len() - separator.len();
                return Ok(Some(Period {
                    starts: None,
                    ends,
                    span: (start, after_at),
                }));
            }
            let trimmed = after.trim_start();
            if trimmed.starts_with("까지") {
                let end = after_at + (after.len() - trimmed.len()) + "까지".len();
                return Ok(Some(Period {
                    starts: None,
                    ends,
                    span: (only.start, end),
                }));
            }
            Err("period_unparsed")
        }
        [first, second] => {
            let between = WEEKDAY.replace_all(&text[first.end..second.start], "");
            if !PERIOD_SEPARATORS.contains(&between.trim()) {
                return Err("period_unparsed");
            }
            let starts = date(first)?;
            let ends = date(second)?;
            if starts > ends {
                return Err("period_unparsed");
            }
            Ok(Some(Period {
                starts: Some(starts),
                ends,
                span: (first.start, weekday_after(second.end)),
            }))
        }
        _ => Err("period_unparsed"),
    }
}

pub fn offer_key(grantor: Uuid, brand: Uuid, kind: ValueKind, value: &str, upper: bool) -> String {
    let mut hasher = Sha256::new();
    hasher.update(format!(
        "hub\x1f{grantor}\x1f{brand}\x1f{}\x1f{value}\x1f{upper}",
        kind.fact_kind()
    ));
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The row line (review H-2, M-2, M-3, M-4): the brand as the row prints
/// it, then the chosen statement with every piece of the row around it
/// that says something about it. Climbing from the statement to the row,
/// each sibling at each level is kept when it holds no value, no date, no
/// brand cell, no other statement and is not a link or button: a column
/// such as PAYCO's 사용대상 ("가맹점 첫결제 고객") or 유형 ("할인"), a
/// membership type heading ("할인형"). Kept in document order.
fn row_line(
    document: &Html,
    row: ElementRef<'_>,
    brand: NodeId,
    keep: NodeId,
    owners: &BTreeSet<NodeId>,
    today: NaiveDate,
) -> Result<String, &'static str> {
    let Some(keep_node) = document.tree.get(keep) else {
        return Err("no_value");
    };
    let holds = |outer: NodeRef<'_, Node>, inner: NodeId| {
        document
            .tree
            .get(inner)
            .is_some_and(|n| n.ancestors().any(|a| a.id() == outer.id()) || n.id() == outer.id())
    };
    // A link, a form control, or an element styled or announced as a
    // button ("자세히 보기" in `<span class="btn-round">`).
    let control = |n: NodeRef<'_, Node>| {
        n.value().as_element().is_some_and(|e| {
            matches!(e.name(), "a" | "button" | "select" | "option" | "input")
                || matches!(e.attr("role"), Some("button" | "link"))
                || e.attr("class").is_some_and(|class| {
                    class.split_ascii_whitespace().any(|token| {
                        let token = token.to_ascii_lowercase();
                        token == "btn" || token.starts_with("btn-") || token.starts_with("button")
                    })
                })
        })
    };
    let mut roots: BTreeSet<NodeId> = BTreeSet::from([keep]);
    let mut current = keep_node;
    while current.id() != row.id() {
        let Some(parent) = current.parent() else {
            break;
        };
        for sibling in parent.children().filter(|c| c.id() != current.id()) {
            let qualifies = match sibling.value() {
                Node::Text(text) => {
                    let text = display(text);
                    !text.is_empty()
                        && !has_value(&canonical(&text))
                        && date_tokens(&text).is_empty()
                }
                Node::Element(element) => {
                    let text = display(&collect_visible(sibling, &|_| false));
                    !element_hidden(element)
                        && !control(sibling)
                        && !holds(sibling, brand)
                        && !owners.iter().any(|o| holds(sibling, *o))
                        && !text.is_empty()
                        && !has_value(&canonical(&text))
                        && date_tokens(&text).is_empty()
                }
                _ => false,
            };
            if qualifies {
                roots.insert(sibling.id());
            }
        }
        current = parent;
    }
    let mut pieces = String::new();
    for node in row.descendants() {
        let Node::Text(text) = node.value() else {
            continue;
        };
        let mut inside = false;
        let mut shown_here = true;
        for at in std::iter::once(node).chain(node.ancestors()) {
            if at.id() == row.id() {
                break;
            }
            if roots.contains(&at.id()) {
                inside = true;
            }
            if let Some(element) = at.value().as_element() {
                if element_hidden(element) || (control(at) && !holds(at, keep)) {
                    shown_here = false;
                }
            }
        }
        if inside && shown_here {
            pieces.push_str(text);
            pieces.push(' ');
        }
    }
    let mut statement = display(&pieces);
    if let Some(period) = row_period(&statement, today)? {
        statement.replace_range(period.span.0..period.span.1, " ");
        statement = display(&statement);
    }
    let printed = element_by_id(document, brand)
        .map(shown)
        .unwrap_or_default();
    let line = display(&format!("{printed} {statement}"));
    if line.chars().count() > MAX_LINE_CHARS {
        return Err("statement_too_long");
    }
    Ok(line)
}

/// Reads one hub list page (steps 1-6).
pub fn read_hub(html: &str, registry: &Registry, grantor: Uuid, today: NaiveDate) -> HubRead {
    let document = Html::parse_document(&strip_comments(html));
    let mut read = HubRead::default();
    if too_deep(&document) {
        return read;
    }
    let Some(rows) = find_rows(&document, registry, grantor) else {
        return read;
    };
    read.listing = true;
    read.rows_seen = rows.len();
    // Step 2 (review H-5): the brand binds at the position (element path
    // within the row) where most rows name a brand, computed per read.
    let cells: Vec<Vec<BrandCell>> = rows
        .iter()
        .map(|r| brand_cells(*r, registry, grantor))
        .collect();
    let mut votes: BTreeMap<&[usize], usize> = BTreeMap::new();
    for row_cells in &cells {
        let paths: BTreeSet<&[usize]> = row_cells.iter().map(|c| c.path.as_slice()).collect();
        for path in paths {
            *votes.entry(path).or_default() += 1;
        }
    }
    let top = votes.values().copied().max().unwrap_or(0);
    let positions: BTreeSet<&[usize]> = votes
        .iter()
        .filter(|(_, n)| **n == top)
        .map(|(p, _)| *p)
        .collect();
    let mut accepted: Vec<RowOffer> = Vec::new();
    let reject = |read: &mut HubRead, brand: Uuid, reason: &'static str| {
        *read.rejected.entry(reason).or_default() += 1;
        read.rejected_brands.push((brand, reason));
    };
    for (row, row_cells) in rows.iter().copied().zip(&cells) {
        let at: Vec<&BrandCell> = row_cells
            .iter()
            .filter(|c| positions.contains(c.path.as_slice()))
            .collect();
        let named: BTreeSet<Uuid> = at.iter().map(|c| c.brand).collect();
        let (brand, brand_element) = match named.len() {
            0 => continue,
            1 => (at[0].brand, at[0].element),
            _ => {
                read.rows_matched += 1;
                *read.rejected.entry("ambiguous_brand").or_default() += 1;
                continue;
            }
        };
        read.rows_matched += 1;
        let alts: BTreeSet<Uuid> = row
            .descendants()
            .filter_map(ElementRef::wrap)
            .filter(|e| !hidden_or_under_hidden(*e, Some(row)))
            .filter_map(|e| e.value().attr("alt"))
            .filter_map(|alt| registry.exact(alt))
            .map(|b| b.brand_id)
            .filter(|b| *b != grantor)
            .collect();
        if alts.iter().any(|a| *a != brand) {
            reject(&mut read, brand, "logo_disagrees");
            continue;
        }
        let cell_texts: BTreeSet<String> = row
            .descendants()
            .filter_map(ElementRef::wrap)
            .filter(|e| !hidden_or_under_hidden(*e, Some(row)))
            .map(visible)
            .filter(|t| t.chars().count() <= 4)
            .collect();
        let discount_cell = cell_texts.contains(DISCOUNT_CELL)
            && !OTHER_KIND_CELLS.iter().any(|k| cell_texts.contains(*k));
        let found = match statements(row, discount_cell) {
            Ok(found) => found,
            Err(reason) => {
                reject(&mut read, brand, reason);
                continue;
            }
        };
        let (offers, reasons, tiered) = read_groups(&document, row, &found);
        if tiered {
            read.tiered_rows += 1;
        }
        // Identical group readings are one offer (a hub restating the same
        // value under two membership types holds for both).
        let mut distinct: Vec<GroupOffer> = Vec::new();
        for offer in offers {
            if !distinct.iter().any(|d| d.value == offer.value) {
                distinct.push(offer);
            }
        }
        let offer = match distinct.len() {
            0 => {
                reject(
                    &mut read,
                    brand,
                    reasons.first().copied().unwrap_or("no_value"),
                );
                continue;
            }
            1 => distinct.remove(0),
            _ => {
                reject(&mut read, brand, "multi_offer_row");
                continue;
            }
        };
        let (starts, ends) = match row_period(&shown(row), today) {
            Ok(Some(period)) => {
                if period.ends < today {
                    reject(&mut read, brand, "period_past");
                    continue;
                }
                if period.starts.is_some_and(|s| s > today) {
                    reject(&mut read, brand, "not_started");
                    continue;
                }
                (period.starts, Some(period.ends))
            }
            Ok(None) => (None, None),
            Err(reason) => {
                reject(&mut read, brand, reason);
                continue;
            }
        };
        let owners: BTreeSet<NodeId> = found.iter().map(|s| s.owner).collect();
        let quote = match row_line(&document, row, brand_element, offer.owner, &owners, today) {
            Ok(line) => line,
            Err(reason) => {
                reject(&mut read, brand, reason);
                continue;
            }
        };
        accepted.push(RowOffer {
            brand_id: brand,
            kind: offer.value.kind,
            offer_key: offer_key(
                grantor,
                brand,
                offer.value.kind,
                &offer.value.number,
                offer.value.upper,
            ),
            value: offer.value.number,
            upper_bound: offer.value.upper,
            tiers: offer.tiers,
            quote,
            starts,
            ends,
        });
    }
    // Step 6: per brand, one value.
    let mut by_brand: BTreeMap<Uuid, Vec<RowOffer>> = BTreeMap::new();
    for offer in accepted {
        by_brand.entry(offer.brand_id).or_default().push(offer);
    }
    for (brand, offers) in by_brand {
        let keys: BTreeSet<&str> = offers.iter().map(|o| o.offer_key.as_str()).collect();
        if keys.len() > 1 {
            for _ in &offers {
                reject(&mut read, brand, "conflicting_values");
            }
            continue;
        }
        // The same offer twice: the one whose period runs longest.
        let kept = offers
            .into_iter()
            .max_by_key(|o| o.ends.unwrap_or(NaiveDate::MAX))
            .expect("non-empty");
        read.offers.push(kept);
    }
    read
}

/// The article links of a list page (a newsroom, an event list), review
/// H-4: a row is an article row only when it carries a date token and its
/// link's text is most of the row's text; the group with the most such rows
/// (at least [`MIN_ROWS`]) wins; the first such link of each row, at most
/// `limit`. A script anchor (`javascript:`) is never a link here: only an
/// `href` that resolves to the page's own scheme and host.
pub fn row_links(html: &str, page: &reqwest::Url, limit: usize) -> Vec<reqwest::Url> {
    let document = Html::parse_document(&strip_comments(html));
    if too_deep(&document) {
        return Vec::new();
    }
    let article_link = |row: ElementRef<'_>| -> Option<reqwest::Url> {
        let row_text = shown(row);
        if date_tokens(&row_text).is_empty() {
            return None;
        }
        // The row's text without its dates: what the link must mostly be.
        let mut undated = row_text.clone();
        for token in date_tokens(&row_text).iter().rev() {
            undated.replace_range(token.start..token.end, "");
        }
        let row_chars = undated.chars().filter(|c| !c.is_whitespace()).count();
        row.descendants()
            .filter_map(ElementRef::wrap)
            .filter(|e| e.value().name() == "a" && !hidden_or_under_hidden(*e, Some(row)))
            .filter(|e| {
                let link_chars = shown(*e).chars().filter(|c| !c.is_whitespace()).count();
                link_chars > 0 && link_chars * 2 >= row_chars
            })
            .filter_map(|e| e.value().attr("href"))
            .filter_map(|href| page.join(href.trim()).ok())
            .find(|url| {
                url.scheme() == page.scheme()
                    && url.host_str() == page.host_str()
                    && url.path() != page.path()
                    && !mazimi_core::source_policy::is_interactive_or_purchase_url(url)
            })
            .map(|mut url| {
                url.set_fragment(None);
                url
            })
    };
    let mut best: Vec<reqwest::Url> = Vec::new();
    for node in document.root_element().descendants() {
        let Some(parent) = ElementRef::wrap(node) else {
            continue;
        };
        if hidden_or_under_hidden(parent, None) {
            continue;
        }
        let mut groups: BTreeMap<String, Vec<reqwest::Url>> = BTreeMap::new();
        for child in parent.children().filter_map(ElementRef::wrap) {
            if element_hidden(child.value()) {
                continue;
            }
            if let Some(url) = article_link(child) {
                let group = groups.entry(signature(child)).or_default();
                if !group.contains(&url) {
                    group.push(url);
                }
            }
        }
        for (_, links) in groups {
            if links.len() >= MIN_ROWS && links.len() > best.len() {
                best = links;
            }
        }
    }
    best.truncate(limit);
    best
}
