//! Validation and writes of `program_condition_reviews` (importer, admin
//! routes and the rolling-window confirmation job). Every write is checked
//! against the programme's CURRENT `raw_payload` and logged.

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use super::overrides::{self, Action, CodeColumns, CodeOverride};
use super::{
    code_fields, evidence_found, is_check_kind, text_fields, text_hash, ReviewStatus, Window,
    WindowKind, YOUTH_CLOSED_CODE,
};
use crate::rule_engine::{self, FieldType};

/// Largest coverage map stored per review (the curation output for one
/// notice is a few kilobytes).
pub const MAX_COVERAGE_BYTES: usize = 1_048_576;
const MAX_RULE_NODES: usize = 64;
const MAX_ANY_CHILDREN: usize = 32;
const MAX_LIST_VALUES: usize = 64;

/// One review as written by the importer or an admin. This is the pilot's
/// `reviews.jsonl` record shape (see [`super::pilot`]); other keys are
/// annotations for people and are ignored.
#[derive(Debug, Clone, Deserialize)]
pub struct ReviewInput {
    pub source_type: String,
    pub source_id: String,
    /// [`super::text_hash`] of the text the reviewers read. When given, a
    /// write is rejected if the current text hashes differently.
    #[serde(default)]
    pub text_hash: Option<String>,
    pub status: String,
    /// Reviewed rule nodes: a leaf `{id, field, op, value?, seg_id, evidence}`
    /// or an any-group `{id, any: [{field, op, value?}, …], seg_id, evidence}`.
    /// Top level is AND.
    #[serde(default)]
    pub rules: Vec<Value>,
    /// The code-derived rules the curators were shown. `source_codes` leaves
    /// are stored with the review (and evaluated with it); `structural`
    /// ones are re-derived from the programme's columns at serve.
    #[serde(default)]
    pub code_rules: Vec<Value>,
    pub window: Value,
    #[serde(default)]
    pub remaining_checks: Vec<String>,
    /// Coverage map (segment labels, reasons, agreement); stored for audit.
    #[serde(default)]
    pub coverage: Option<Value>,
    pub reviewer: String,
    /// Overrides that widen (`widen` + `replacement`) or drop (`drop`) a
    /// code rule named in `code_rules`: `{target, action, replacement?,
    /// check_kind, seg_id?, evidence}`. See [`super::overrides`].
    #[serde(default)]
    pub code_overrides: Vec<Value>,
}

/// Why a review was not written. No variant carries source text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Rejection {
    UnknownStatus,
    UnknownCheckKind,
    InconsistentStatus,
    InvalidReviewer,
    InvalidSource,
    MissingTextHash,
    TextChanged,
    UnsupportedSource,
    ProgramNotFound,
    InvalidRule,
    EvidenceNotFound,
    InvalidWindow,
    InvalidCoverage,
    InvalidOverride,
    OverrideTargetMissing,
    OverrideNotWider,
    OverrideIncomplete,
    CoverageIncomplete,
}

impl Rejection {
    pub fn message(self) -> &'static str {
        match self {
            Self::UnknownStatus => "status must be one of complete, incomplete, conflict",
            Self::UnknownCheckKind => "remaining_checks must name contract check kinds",
            Self::InconsistentStatus => "a complete review cannot have remaining_checks",
            Self::InvalidReviewer => "reviewer must be 1-200 characters without control characters",
            Self::InvalidSource => "source_type must be 1-100 and source_id 1-200 characters",
            Self::MissingTextHash => "text_hash of the text the reviewers read is required",
            Self::TextChanged => "the programme text changed since it was reviewed (text_hash differs); review the current text",
            Self::UnsupportedSource => "this source has no reviewed field set",
            Self::ProgramNotFound => "no programme with this source_type and source_id",
            Self::InvalidRule => "every rule needs an id and a supported field, operator and value",
            Self::EvidenceNotFound => "every rule and window evidence must be a verbatim quote (5-2000 characters) of one reviewed field of the current text",
            Self::InvalidWindow => "window kind must be dated, rolling, recurring, event, closed or unknown, with valid dates",
            Self::InvalidCoverage => "coverage must be a JSON object of at most 1 MB",
            Self::InvalidOverride => "each code override needs a known target (once), action widen (with a replacement leaf or any-group) or drop (without one), a contract check_kind and a text quote",
            Self::OverrideTargetMissing => "a code override names a code rule the programme does not currently have",
            Self::OverrideNotWider => "a widened code rule must provably admit everyone the code rule admits",
            Self::OverrideIncomplete => "overriding S-region of a programme with S-district needs an S-district override too",
            Self::CoverageIncomplete => "a complete review needs a coverage label for every segment of the current text, no uncheckable segment, a rule for every condition segment and at least one restrictive rule",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Written {
    Inserted,
    Updated,
    Unchanged,
}

/// A review ready to store.
#[derive(Debug, Clone, PartialEq)]
pub struct Validated {
    pub status: ReviewStatus,
    pub rule_json: Option<Value>,
    pub window_json: Value,
    pub remaining_checks: Vec<String>,
    pub coverage_json: Value,
    pub evidence: Value,
    pub text_hash: String,
    pub reviewer: String,
    /// Validated [`CodeOverride`]s; `None` when there are none.
    pub code_overrides: Option<Value>,
    /// The [`super::coverage::VERSION`] the status was checked with.
    pub coverage_version: i16,
}

/// Checks that need no programme text.
fn precheck(input: &ReviewInput) -> Result<ReviewStatus, Rejection> {
    let status = ReviewStatus::parse(input.status.trim()).ok_or(Rejection::UnknownStatus)?;
    if !input.remaining_checks.iter().all(|k| is_check_kind(k)) {
        return Err(Rejection::UnknownCheckKind);
    }
    if status == ReviewStatus::Complete && !input.remaining_checks.is_empty() {
        return Err(Rejection::InconsistentStatus);
    }
    let reviewer = input.reviewer.trim();
    if !(1..=200).contains(&reviewer.chars().count()) || reviewer.chars().any(char::is_control) {
        return Err(Rejection::InvalidReviewer);
    }
    if !(1..=100).contains(&input.source_type.trim().chars().count())
        || !(1..=200).contains(&input.source_id.trim().chars().count())
    {
        return Err(Rejection::InvalidSource);
    }
    if text_fields(input.source_type.trim()).is_none() {
        return Err(Rejection::UnsupportedSource);
    }
    Ok(status)
}

/// Validate `input` against the programme's current `raw_payload`, with no
/// programme columns: every code override is `override_target_missing`.
/// The pilot replay uses it; writes go through [`validate_with`].
pub fn validate(
    input: &ReviewInput,
    raw_payload: &Value,
    require_text_hash: bool,
) -> Result<Validated, Rejection> {
    validate_with(
        input,
        raw_payload,
        require_text_hash,
        &CodeColumns::default(),
    )
}

/// Validate `input` against the programme's current `raw_payload` and the
/// columns and compiled rule its code rules come from. Pure: the importer
/// and the admin route share it through [`upsert`].
pub fn validate_with(
    input: &ReviewInput,
    raw_payload: &Value,
    require_text_hash: bool,
    columns: &CodeColumns,
) -> Result<Validated, Rejection> {
    let status = precheck(input)?;
    let source_type = input.source_type.trim();
    let read_hash = input
        .text_hash
        .as_deref()
        .map(|hash| hash.trim().to_ascii_lowercase());
    if require_text_hash && read_hash.is_none() {
        return Err(Rejection::MissingTextHash);
    }
    let hash = text_hash(source_type, raw_payload).ok_or(Rejection::UnsupportedSource)?;
    if read_hash.is_some_and(|read| read != hash) {
        return Err(Rejection::TextChanged);
    }

    let mut evidence = Vec::new();
    let code_overrides = code_overrides(input, source_type, raw_payload, columns, &mut evidence)?;
    let mut ids = HashSet::new();
    let mut nodes = Vec::new();
    for code in &input.code_rules {
        // An overridden code rule is replaced by the override; its copy is
        // not kept with the review.
        let id = code.get("id").and_then(Value::as_str).map(str::trim);
        if id.is_some_and(|id| code_overrides.iter().any(|o| o.target == id)) {
            continue;
        }
        if let Some(node) = code_node(code, source_type, raw_payload, &mut ids)? {
            nodes.push(node);
        }
    }
    for node in &input.rules {
        nodes.push(review_node(
            node,
            source_type,
            raw_payload,
            &mut ids,
            &mut evidence,
        )?);
    }
    if nodes.len() > MAX_RULE_NODES {
        return Err(Rejection::InvalidRule);
    }
    let nodes_count = nodes.len();
    let rule_json = if nodes.is_empty() {
        None
    } else {
        let tree = json!({ "all": nodes });
        rule_engine::parse_rule_json(&tree).map_err(|_| Rejection::InvalidRule)?;
        Some(tree)
    };

    let window = window(&input.window, source_type, raw_payload, &mut evidence)?;
    let coverage_json = match &input.coverage {
        None | Some(Value::Null) => json!({}),
        Some(value @ Value::Object(_)) => value.clone(),
        Some(_) => return Err(Rejection::InvalidCoverage),
    };
    if serde_json::to_vec(&coverage_json).map_or(true, |bytes| bytes.len() > MAX_COVERAGE_BYTES) {
        return Err(Rejection::InvalidCoverage);
    }
    // Only the coverage check decides `complete` (audit H1): a claim it does
    // not back is rejected, never stored. Rules a condition label may name:
    // the validated ones, every code rule shown and every overridden code
    // rule. A structural rule is re-derived from the columns at serve, so
    // it backs a sentence only when the programme has it now
    // ([`overrides::target_rule`], as for an override target): an id the
    // input merely names is not evidence (review B7).
    if status == ReviewStatus::Complete {
        let structural_now = |id: &str| overrides::target_rule(id, columns).is_some();
        let mut structural = 0;
        let mut code_ids = Vec::new();
        for code in &input.code_rules {
            let Some(id) = code.get("id").and_then(Value::as_str).map(str::trim) else {
                continue;
            };
            if code.get("origin").and_then(Value::as_str) == Some("structural") {
                if !structural_now(id) {
                    continue;
                }
                structural += 1;
            }
            code_ids.push(id.to_owned());
        }
        let mut refs = ids;
        refs.extend(code_ids);
        refs.extend(code_overrides.iter().map(|o| o.target.clone()));
        let restrictive = nodes_count + structural + code_overrides.len();
        if !super::coverage::check(source_type, raw_payload, &coverage_json, &refs, restrictive)
            .is_empty()
        {
            return Err(Rejection::CoverageIncomplete);
        }
    }
    Ok(Validated {
        coverage_version: super::coverage::VERSION,
        status,
        rule_json,
        window_json: serde_json::to_value(window).expect("window serialises"),
        remaining_checks: input.remaining_checks.clone(),
        coverage_json,
        evidence: Value::Array(evidence),
        text_hash: hash,
        reviewer: input.reviewer.trim().to_owned(),
        code_overrides: (!code_overrides.is_empty())
            .then(|| serde_json::to_value(&code_overrides).expect("overrides serialise")),
    })
}

/// The review's code overrides, each checked against the programme's
/// current code rule (`target`) and text (`evidence`).
fn code_overrides(
    input: &ReviewInput,
    source_type: &str,
    raw_payload: &Value,
    columns: &CodeColumns,
    evidence: &mut Vec<Value>,
) -> Result<Vec<CodeOverride>, Rejection> {
    let mut out: Vec<CodeOverride> = Vec::new();
    for value in &input.code_overrides {
        let parsed = code_override(value, source_type, raw_payload, columns)?;
        if out.iter().any(|o| o.target == parsed.target) {
            return Err(Rejection::InvalidOverride);
        }
        evidence
            .push(json!({"for": format!("override:{}", parsed.target), "quote": parsed.evidence}));
        out.push(parsed);
    }
    // A district limit fails users of every other sido: widening the
    // region alone would change nothing.
    let has = |target: &str| out.iter().any(|o| o.target == target);
    if has("S-region")
        && !has("S-district")
        && overrides::target_rule("S-district", columns).is_some()
    {
        return Err(Rejection::OverrideIncomplete);
    }
    Ok(out)
}

fn code_override(
    value: &Value,
    source_type: &str,
    raw_payload: &Value,
    columns: &CodeColumns,
) -> Result<CodeOverride, Rejection> {
    let object = value.as_object().ok_or(Rejection::InvalidOverride)?;
    let text = |key: &str| object.get(key).and_then(Value::as_str).map(str::trim);
    let target = text("target")
        .filter(|t| overrides::TARGETS.contains(t))
        .ok_or(Rejection::InvalidOverride)?;
    let action = match text("action") {
        Some("widen") => Action::Widen,
        Some("drop") => Action::Drop,
        _ => return Err(Rejection::InvalidOverride),
    };
    let check_kind = text("check_kind")
        .filter(|k| is_check_kind(k))
        .ok_or(Rejection::InvalidOverride)?;
    let replacement = match (action, object.get("replacement")) {
        (Action::Drop, None | Some(Value::Null)) => None,
        (Action::Widen, Some(node)) => Some(replacement(node)?),
        _ => return Err(Rejection::InvalidOverride),
    };
    // The text that admits more people than the code, never the code itself.
    let quote = text("evidence")
        .filter(|q| !q.starts_with("code:"))
        .ok_or(Rejection::EvidenceNotFound)?;
    if !evidence_found(source_type, raw_payload, quote) {
        return Err(Rejection::EvidenceNotFound);
    }
    let current =
        overrides::target_rule(target, columns).ok_or(Rejection::OverrideTargetMissing)?;
    // An unresolved region scope is widened only to an explicit region list.
    if overrides::unresolved_scope(&current) {
        let explicit = replacement.as_ref().is_some_and(|node| {
            matches!(node["field"].as_str(), Some("region_code" | "region"))
                && matches!(node["op"].as_str(), Some("in" | "eq"))
        });
        if !explicit {
            return Err(Rejection::InvalidOverride);
        }
    }
    if let Some(node) = &replacement {
        if !overrides::contains(node, &current) {
            return Err(Rejection::OverrideNotWider);
        }
    }
    Ok(CodeOverride {
        target: target.to_owned(),
        action,
        replacement,
        check_kind: check_kind.to_owned(),
        seg_id: seg_id(text("seg_id")),
        evidence: quote.to_owned(),
    })
}

/// A replacement: one leaf or an any-group of leaves in the review
/// vocabulary, without `severity`. A `city_code` leaf lists districts
/// (`in`/`eq`): it is read as district coverage.
fn replacement(node: &Value) -> Result<Value, Rejection> {
    let invalid = |_| Rejection::InvalidOverride;
    let object = node.as_object().ok_or(Rejection::InvalidOverride)?;
    let checked = |child: &Map<String, Value>| -> Result<Value, Rejection> {
        if child.contains_key("severity") || child.contains_key("any") || child.contains_key("all")
        {
            return Err(Rejection::InvalidOverride);
        }
        let out = leaf(child).map_err(invalid)?;
        if out["field"] == "city_code" && !matches!(out["op"].as_str(), Some("in" | "eq")) {
            return Err(Rejection::InvalidOverride);
        }
        Ok(Value::Object(out))
    };
    match object.get("any") {
        Some(any) => {
            if object.len() != 1 {
                return Err(Rejection::InvalidOverride);
            }
            let children = any
                .as_array()
                .filter(|children| (2..=MAX_ANY_CHILDREN).contains(&children.len()))
                .ok_or(Rejection::InvalidOverride)?;
            let leaves = children
                .iter()
                .map(|c| {
                    c.as_object()
                        .ok_or(Rejection::InvalidOverride)
                        .and_then(checked)
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(json!({ "any": leaves }))
        }
        None => checked(object),
    }
}

fn rule_id(object: &Map<String, Value>, ids: &mut HashSet<String>) -> Result<String, Rejection> {
    let id = object
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| (1..=40).contains(&id.chars().count()) && !id.chars().any(char::is_control))
        .ok_or(Rejection::InvalidRule)?;
    if !ids.insert(id.to_owned()) {
        return Err(Rejection::InvalidRule);
    }
    Ok(id.to_owned())
}

/// A source-code leaf (`origin: source_codes`) kept with the review;
/// `structural` leaves are skipped (re-derived from columns at serve). Its
/// `from` must name code fields the current text actually carries. Such a
/// leaf can only withhold `verified`, never hide a row ([`super::ServeReview`]).
fn code_node(
    code: &Value,
    source_type: &str,
    raw_payload: &Value,
    ids: &mut HashSet<String>,
) -> Result<Option<Value>, Rejection> {
    let object = code.as_object().ok_or(Rejection::InvalidRule)?;
    match object.get("origin").and_then(Value::as_str) {
        Some("structural") => Ok(None),
        Some("source_codes") => {
            if object.contains_key("any") {
                return Err(Rejection::InvalidRule);
            }
            let fields = code_fields(source_type).unwrap_or_default();
            let from = object
                .get("from")
                .and_then(Value::as_array)
                .filter(|from| !from.is_empty())
                .ok_or(Rejection::InvalidRule)?;
            for name in from {
                let name = name.as_str().ok_or(Rejection::InvalidRule)?;
                if !fields.contains(&name)
                    || crate::applicant_review::field_text(raw_payload, name)
                        .trim()
                        .is_empty()
                {
                    return Err(Rejection::InvalidRule);
                }
            }
            let mut leaf = leaf(object)?;
            // A `label` code leaf (a loose provider flag) keeps its severity:
            // a contradiction reads as Unknown, which still withholds
            // `verified` (zero unknowns).
            match object.get("severity").and_then(Value::as_str) {
                None | Some("hard") => {}
                Some("label") => {
                    leaf.insert("severity".into(), Value::String("label".into()));
                }
                Some(_) => return Err(Rejection::InvalidRule),
            }
            leaf.insert("id".into(), Value::String(rule_id(object, ids)?));
            leaf.insert(
                rule_engine::SOURCE_ORIGIN_KEY.into(),
                Value::String(rule_engine::SOURCE_ORIGIN.into()),
            );
            Ok(Some(Value::Object(leaf)))
        }
        _ => Err(Rejection::InvalidRule),
    }
}

/// A reviewed leaf or any-group with its quote checked.
fn review_node(
    node: &Value,
    source_type: &str,
    raw_payload: &Value,
    ids: &mut HashSet<String>,
    evidence: &mut Vec<Value>,
) -> Result<Value, Rejection> {
    let object = node.as_object().ok_or(Rejection::InvalidRule)?;
    // `severity` belongs to copied code leaves only; on a reviewed leaf it
    // would silently become a hard condition.
    let has_severity = |o: &Map<String, Value>| o.contains_key("severity");
    if has_severity(object)
        || object
            .get("any")
            .and_then(Value::as_array)
            .is_some_and(|children| {
                children
                    .iter()
                    .any(|c| c.as_object().is_some_and(has_severity))
            })
    {
        return Err(Rejection::InvalidRule);
    }
    let id = rule_id(object, ids)?;
    let mut out = match object.get("any") {
        Some(any) => {
            let children = any
                .as_array()
                .filter(|children| (2..=MAX_ANY_CHILDREN).contains(&children.len()))
                .ok_or(Rejection::InvalidRule)?;
            let leaves = children
                .iter()
                .map(|child| {
                    child
                        .as_object()
                        .ok_or(Rejection::InvalidRule)
                        .and_then(leaf)
                        .map(Value::Object)
                })
                .collect::<Result<Vec<_>, _>>()?;
            let mut group = Map::new();
            group.insert("any".into(), Value::Array(leaves));
            group
        }
        None => leaf(object)?,
    };
    let quote = object
        .get("evidence")
        .and_then(Value::as_str)
        .map(str::trim)
        .ok_or(Rejection::EvidenceNotFound)?;
    if !evidence_found(source_type, raw_payload, quote) {
        return Err(Rejection::EvidenceNotFound);
    }
    out.insert("id".into(), Value::String(id.clone()));
    out.insert("origin".into(), Value::String("review".into()));
    out.insert("evidence".into(), Value::String(quote.to_owned()));
    if let Some(seg_id) = seg_id(object.get("seg_id").and_then(Value::as_str)) {
        out.insert("seg_id".into(), Value::String(seg_id));
    }
    evidence.push(json!({"for": format!("rule:{id}"), "quote": quote}));
    Ok(Value::Object(out))
}

fn seg_id(value: Option<&str>) -> Option<String> {
    value
        .filter(|s| (1..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(str::to_owned)
}

/// Whether reviews may write rules on `field`: every field the rule engine
/// reads ([`rule_engine::FIELDS`]).
pub fn is_review_field(field: &str) -> bool {
    rule_engine::field_type(field).is_some()
}

/// `{field, op, value?}` checked against the curator vocabulary and the
/// rule engine's parser. Curators never use `is_null`, `is_not_null` or
/// `contains`; a value outside a field's enum or range is rejected (a typo
/// would otherwise hide rows on a condition no notice states).
fn leaf(object: &Map<String, Value>) -> Result<Map<String, Value>, Rejection> {
    let field = object
        .get("field")
        .and_then(Value::as_str)
        .ok_or(Rejection::InvalidRule)?;
    let op = object
        .get("op")
        .and_then(Value::as_str)
        .ok_or(Rejection::InvalidRule)?;
    let value = object.get("value").cloned().unwrap_or(Value::Null);
    let kind = rule_engine::field_type(field).ok_or(Rejection::InvalidRule)?;
    let scalar_ok = |v: &Value| match &kind {
        FieldType::Int(min, max) => v.as_i64().is_some_and(|n| (*min..=*max).contains(&n)),
        FieldType::Enum(values) => v.as_str().is_some_and(|s| values.contains(&s)),
        FieldType::Text(check) => v.as_str().is_some_and(check),
        FieldType::Bool => false,
    };
    let valid = match (&kind, op) {
        (FieldType::Bool, "is_true" | "is_false") => matches!(value, Value::Null | Value::Bool(_)),
        (FieldType::Bool, _) => false,
        (_, "in" | "not_in") => value.as_array().is_some_and(|items| {
            (1..=MAX_LIST_VALUES).contains(&items.len()) && items.iter().all(scalar_ok)
        }),
        (FieldType::Int(..), "between") => value.as_array().is_some_and(|items| {
            items.len() == 2
                && items.iter().all(scalar_ok)
                && items[0].as_i64() <= items[1].as_i64()
        }),
        (FieldType::Int(..), "gte" | "lte" | "gt" | "lt") => scalar_ok(&value),
        (_, "eq" | "ne") => scalar_ok(&value),
        _ => false,
    };
    if !valid {
        return Err(Rejection::InvalidRule);
    }
    // A review states its set exactly (review R-M7): the widening of
    // `education::admitted_education_stages` is for loose source codes and
    // stays in the worker's code compiler.
    let mut out = Map::new();
    out.insert("field".into(), Value::String(field.to_owned()));
    out.insert("op".into(), Value::String(op.to_owned()));
    if !value.is_null() {
        out.insert("value".into(), value);
    }
    rule_engine::parse_rule_json(&Value::Object(out.clone()))
        .map_err(|_| Rejection::InvalidRule)?;
    Ok(out)
}

fn window(
    value: &Value,
    source_type: &str,
    raw_payload: &Value,
    evidence: &mut Vec<Value>,
) -> Result<Window, Rejection> {
    let mut window: Window =
        serde_json::from_value(value.clone()).map_err(|_| Rejection::InvalidWindow)?;
    // Only the confirmation job writes this.
    window.reviewed = None;
    if !matches!(window.source.as_deref(), None | Some("code" | "review")) {
        return Err(Rejection::InvalidWindow);
    }
    match (window.kind, window.opens_at, window.closes_at) {
        (WindowKind::Dated, Some(opens), Some(closes)) if opens <= closes => {}
        (WindowKind::Dated, _, _) => return Err(Rejection::InvalidWindow),
        (_, Some(opens), Some(closes)) if opens > closes => return Err(Rejection::InvalidWindow),
        _ => {}
    }
    if window
        .recurrence
        .as_deref()
        .is_some_and(|r| r.chars().count() > 200 || r.chars().any(char::is_control))
    {
        return Err(Rejection::InvalidWindow);
    }
    window.seg_id = seg_id(window.seg_id.as_deref());
    window.evidence = window
        .evidence
        .map(|quote| quote.trim().to_owned())
        .filter(|quote| !quote.is_empty());
    if let Some(quote) = &window.evidence {
        if !evidence_found(source_type, raw_payload, quote) {
            return Err(Rejection::EvidenceNotFound);
        }
        evidence.push(json!({"for": "window", "quote": quote}));
    }
    // A window that can open or hide a row rests on proof: the source's
    // current structured codes, or a verbatim quote.
    if window.source.as_deref() == Some("code") {
        if !code_window_matches(&window, raw_payload) {
            return Err(Rejection::InvalidWindow);
        }
    } else if window.kind != WindowKind::Unknown && window.evidence.is_none() {
        return Err(Rejection::EvidenceNotFound);
    }
    Ok(window)
}

/// A `source: code` window equals the current 온통청년 codes: `dated` =
/// `aplyYmd` "YYYYMMDD ~ YYYYMMDD", `closed` = `aplyPrdSeCd` 0057003.
fn code_window_matches(window: &Window, raw_payload: &Value) -> bool {
    let code = |field| crate::applicant_review::field_text(raw_payload, field);
    match window.kind {
        WindowKind::Dated => {
            let ymd = code("aplyYmd");
            let mut dates = ymd
                .split('~')
                .map(|part| chrono::NaiveDate::parse_from_str(part.trim(), "%Y%m%d").ok());
            match (dates.next().flatten(), dates.next().flatten(), dates.next()) {
                (Some(opens), Some(closes), None) => {
                    window.opens_at == Some(opens) && window.closes_at == Some(closes)
                }
                _ => false,
            }
        }
        WindowKind::Closed => code("aplyPrdSeCd").trim() == YOUTH_CLOSED_CODE,
        _ => false,
    }
}

type Stored = (
    String,
    Option<Value>,
    Value,
    Vec<String>,
    Value,
    Value,
    String,
    String,
    Option<Value>,
    Option<i16>,
);

/// `raw_payload`, then the columns code rules come from.
type ProgramColumns = (
    Option<Value>,
    Option<i32>,
    Option<i32>,
    Option<Vec<String>>,
    Option<Vec<String>>,
    Option<Value>,
    Option<Value>,
);

/// Validate `input` against the programme's current `raw_payload` and
/// upsert it with a log row naming `actor`. With `require_text_hash` a
/// review without [`ReviewInput::text_hash`] is rejected. Runs in a
/// savepoint of the caller's transaction (or its own transaction).
pub async fn upsert(
    connection: &mut sqlx::PgConnection,
    input: &ReviewInput,
    actor: &str,
    require_text_hash: bool,
) -> Result<Result<Written, Rejection>, sqlx::Error> {
    use sqlx::Connection;
    if let Err(rejection) = precheck(input) {
        return Ok(Err(rejection));
    }
    if require_text_hash && input.text_hash.is_none() {
        return Ok(Err(Rejection::MissingTextHash));
    }
    let source_type = input.source_type.trim();
    let source_id = input.source_id.trim();
    let mut tx = connection.begin().await?;
    let row: Option<ProgramColumns> = sqlx::query_as(
        "SELECT p.raw_payload, p.min_age, p.max_age, p.regions, p.region_districts,
                p.normalized_payload->'enrollment_statuses',
                (SELECT e.hard_filter_json FROM eligibility_rules e
                 WHERE e.program_id = p.id ORDER BY e.version DESC LIMIT 1)
         FROM programs p WHERE p.source_type = $1 AND p.source_id = $2",
    )
    .bind(source_type)
    .bind(source_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((raw, min_age, max_age, regions, region_districts, enrollment, hard_filter)) = row
    else {
        return Ok(Err(Rejection::ProgramNotFound));
    };
    let raw = raw.unwrap_or(Value::Null);
    // The same reading as the list (`structural::enrollment_statuses`).
    let enrollment_statuses = enrollment
        .as_ref()
        .and_then(Value::as_array)
        .filter(|a| !a.is_empty())
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        });
    let columns = CodeColumns {
        min_age,
        max_age,
        regions,
        region_districts,
        enrollment_statuses,
        hard_filter,
    };
    let valid = match validate_with(input, &raw, require_text_hash, &columns) {
        Ok(valid) => valid,
        Err(rejection) => return Ok(Err(rejection)),
    };
    let previous: Option<Stored> = sqlx::query_as(
        "SELECT status, rule_json, window_json, remaining_checks, coverage_json, evidence,
                text_hash, reviewer, code_overrides, coverage_version
         FROM program_condition_reviews
         WHERE source_type = $1 AND source_id = $2 FOR UPDATE",
    )
    .bind(source_type)
    .bind(source_id)
    .fetch_optional(&mut *tx)
    .await?;
    // The confirmation job replaces a rolling/unknown window and keeps the
    // reviewed one under `reviewed`. Re-importing that same reviewed window
    // keeps the confirmed window and its time; a new window drops both.
    let keep_confirmation = previous.as_ref().is_some_and(|p| {
        p.2.get("reviewed")
            .is_some_and(|reviewed| reviewed == &valid.window_json)
    });
    let window_json = match &previous {
        Some(p) if keep_confirmation => p.2.clone(),
        _ => valid.window_json.clone(),
    };
    let next: Stored = (
        valid.status.as_str().to_owned(),
        valid.rule_json.clone(),
        window_json.clone(),
        valid.remaining_checks.clone(),
        valid.coverage_json.clone(),
        valid.evidence.clone(),
        valid.text_hash.clone(),
        valid.reviewer.clone(),
        valid.code_overrides.clone(),
        Some(valid.coverage_version),
    );
    if previous.as_ref() == Some(&next) {
        tx.commit().await?;
        return Ok(Ok(Written::Unchanged));
    }
    sqlx::query(
        "INSERT INTO program_condition_reviews
           (source_type, source_id, status, rule_json, window_json, remaining_checks,
            coverage_json, evidence, text_hash, reviewer, code_overrides, coverage_version)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
         ON CONFLICT (source_type, source_id) DO UPDATE SET
           status = EXCLUDED.status, rule_json = EXCLUDED.rule_json,
           window_confirmed_at = CASE
             WHEN program_condition_reviews.window_json = EXCLUDED.window_json
             THEN program_condition_reviews.window_confirmed_at END,
           window_json = EXCLUDED.window_json, remaining_checks = EXCLUDED.remaining_checks,
           coverage_json = EXCLUDED.coverage_json, evidence = EXCLUDED.evidence,
           text_hash = EXCLUDED.text_hash, reviewer = EXCLUDED.reviewer,
           code_overrides = EXCLUDED.code_overrides,
           coverage_version = EXCLUDED.coverage_version, updated_at = now()",
    )
    .bind(source_type)
    .bind(source_id)
    .bind(valid.status.as_str())
    .bind(&valid.rule_json)
    .bind(&window_json)
    .bind(&valid.remaining_checks)
    .bind(&valid.coverage_json)
    .bind(&valid.evidence)
    .bind(&valid.text_hash)
    .bind(&valid.reviewer)
    .bind(&valid.code_overrides)
    .bind(valid.coverage_version)
    .execute(&mut *tx)
    .await?;
    let coverage_sha256 = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&valid.coverage_json).unwrap_or_default())
    );
    let snapshot = json!({
        "status": valid.status.as_str(),
        "rule_json": valid.rule_json,
        "window_json": window_json,
        "coverage_sha256": coverage_sha256,
        "remaining_checks": valid.remaining_checks,
        "evidence": valid.evidence,
        "reviewer": valid.reviewer,
        "code_overrides": valid.code_overrides,
    });
    log(
        &mut tx,
        source_type,
        source_id,
        "upsert",
        Some(valid.status.as_str()),
        previous.as_ref().map(|p| p.0.as_str()),
        Some(&valid.text_hash),
        Some(&snapshot),
        actor,
    )
    .await?;
    tx.commit().await?;
    Ok(Ok(if previous.is_some() {
        Written::Updated
    } else {
        Written::Inserted
    }))
}

#[allow(clippy::too_many_arguments)]
async fn log(
    connection: &mut sqlx::PgConnection,
    source_type: &str,
    source_id: &str,
    action: &str,
    status: Option<&str>,
    previous_status: Option<&str>,
    text_hash: Option<&str>,
    snapshot: Option<&Value>,
    actor: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO program_condition_review_log
           (source_type, source_id, action, status, previous_status, text_hash, snapshot, actor)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(source_type)
    .bind(source_id)
    .bind(action)
    .bind(status)
    .bind(previous_status)
    .bind(text_hash)
    .bind(snapshot)
    .bind(actor)
    .execute(connection)
    .await?;
    Ok(())
}

/// Delete a review with a log row. `false` = there was none.
pub async fn delete(
    connection: &mut sqlx::PgConnection,
    source_type: &str,
    source_id: &str,
    actor: &str,
) -> Result<bool, sqlx::Error> {
    use sqlx::Connection;
    let mut tx = connection.begin().await?;
    let previous: Option<String> = sqlx::query_scalar(
        "DELETE FROM program_condition_reviews WHERE source_type = $1 AND source_id = $2
         RETURNING status",
    )
    .bind(source_type)
    .bind(source_id)
    .fetch_optional(&mut *tx)
    .await?;
    if previous.is_some() {
        log(
            &mut tx,
            source_type,
            source_id,
            "delete",
            None,
            previous.as_deref(),
            None,
            None,
            actor,
        )
        .await?;
    }
    tx.commit().await?;
    Ok(previous.is_some())
}

/// What the official record said about a rolling or unknown window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Confirmation {
    /// Still 상시: the window is rolling, confirmed now.
    StillRolling,
    /// Now 마감: the window is closed.
    Closed,
}

/// Record an official re-check of a review's window. Applies only while
/// the stored `text_hash` is `expected_text_hash` (the review the job read).
/// `false` = no such review any more. Logged as `confirm_window`.
pub async fn confirm_window(
    connection: &mut sqlx::PgConnection,
    source_type: &str,
    source_id: &str,
    expected_text_hash: &str,
    outcome: Confirmation,
    now: DateTime<Utc>,
    actor: &str,
) -> Result<bool, sqlx::Error> {
    use sqlx::Connection;
    let mut tx = connection.begin().await?;
    let row: Option<(String, Value)> = sqlx::query_as(
        "SELECT status, window_json FROM program_condition_reviews
         WHERE source_type = $1 AND source_id = $2 AND text_hash = $3 FOR UPDATE",
    )
    .bind(source_type)
    .bind(source_id)
    .bind(expected_text_hash)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((status, window_json)) = row else {
        return Ok(false);
    };
    let Ok(previous) = serde_json::from_value::<Window>(window_json) else {
        return Ok(false);
    };
    let window = Window {
        kind: match outcome {
            Confirmation::StillRolling => WindowKind::Rolling,
            Confirmation::Closed => WindowKind::Closed,
        },
        opens_at: None,
        closes_at: None,
        recurrence: None,
        evidence: previous.evidence.clone(),
        seg_id: previous.seg_id.clone(),
        source: Some("official_recheck".into()),
        reviewed: previous
            .reviewed
            .clone()
            .or_else(|| Some(Box::new(previous.clone()))),
    };
    let window_json = serde_json::to_value(&window).expect("window serialises");
    sqlx::query(
        "UPDATE program_condition_reviews
         SET window_json = $3, window_confirmed_at = $4, updated_at = now()
         WHERE source_type = $1 AND source_id = $2",
    )
    .bind(source_type)
    .bind(source_id)
    .bind(&window_json)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    let snapshot =
        json!({"window_json": window_json, "window_confirmed_at": now, "outcome": outcome});
    log(
        &mut tx,
        source_type,
        source_id,
        "confirm_window",
        Some(&status),
        Some(&status),
        Some(expected_text_hash),
        Some(&snapshot),
        actor,
    )
    .await?;
    tx.commit().await?;
    Ok(true)
}
