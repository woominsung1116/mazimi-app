//! Converter from the Phase 0 pilot's merged review records
//! (`verified-p0/reviews.jsonl`, written by `coverage.py merge`) to the two
//! import formats: condition reviews ([`super::store::ReviewInput`]) and
//! applicant reviews ([`crate::applicant_review::Review`]).
//!
//! A pilot record already carries `source_type`, `source_id` and the
//! condition `text_hash`. The converter
//! - names the reviewer (`<reviewer> (<agreement>/<chosen>)`),
//! - moves the audit fields (labels, reasons, agreement, …) into `coverage`,
//! - emits an applicant line from the record's applicant class, with the
//!   applicant `text_hash` the pilot computed (`meta/<program_id>.json`).
//!
//! It skips, with a reason, what the importers would reject as a whole
//! file: sources without a reviewed field set, rule fields the rule engine
//! does not have yet (the remaining plan §2.2 additions, in rules, copied
//! code rules or code-override replacements) and pilot status `rejected`.
//! `code_overrides` pass through unchanged; the importer validates them
//! against the programme.
//! An applicant line is skipped for class `unclear`, a missing quote, a
//! quote containing a mask token (the applicant importer accepts plain
//! quotes only) or a missing applicant hash. Nothing is invented.

use std::collections::{BTreeMap, HashMap};

use serde::Serialize;
use serde_json::{json, Map, Value};

use super::{store::is_review_field, text_fields, MASK_TOKENS};

#[derive(Debug, Default, Serialize)]
pub struct Conversion {
    /// Condition review import lines.
    pub conditions: Vec<Value>,
    /// Applicant review import lines.
    pub applicants: Vec<Value>,
    /// (program_id, reason) of records left out of `conditions`.
    pub skipped: Vec<(String, String)>,
    /// (program_id, reason) of records with no applicant line.
    pub applicant_skipped: Vec<(String, String)>,
    pub by_status: BTreeMap<String, usize>,
    /// Code overrides of converted lines, by `<target>:<action>`.
    pub overrides_by_target: BTreeMap<String, usize>,
}

/// Audit keys of a pilot record copied into `coverage`.
const COVERAGE_KEYS: [&str; 12] = [
    "program_id",
    "agreement",
    "chosen",
    "reasons",
    "conflict_fields",
    "uncheckable_kinds",
    "labels",
    "code_rules",
    "spot_check",
    "condition_segments",
    "code_only_condition_segments",
    "no_restrictive_leaves",
];

/// Convert pilot JSON lines. `applicant_hashes` maps a program id to the
/// applicant `text_hash` the pilot computed for it.
pub fn convert(
    text: &str,
    reviewer: &str,
    applicant_hashes: &HashMap<String, String>,
) -> Conversion {
    let mut out = Conversion::default();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(Value::Object(record)) = serde_json::from_str::<Value>(line) else {
            out.skipped
                .push((format!("line {}", index + 1), "invalid_json".into()));
            continue;
        };
        let str_of = |key: &str| record.get(key).and_then(Value::as_str).unwrap_or_default();
        let program_id = str_of("program_id").to_owned();
        let source_type = str_of("source_type");
        if text_fields(source_type).is_none() {
            out.skipped
                .push((program_id, format!("unsupported_source:{source_type}")));
            continue;
        }
        let status = str_of("status");
        if !matches!(status, "complete" | "incomplete" | "conflict") {
            out.skipped.push((program_id, format!("status:{status}")));
            continue;
        }
        if let Some(field) = unsupported_field(&record) {
            out.skipped
                .push((program_id, format!("unsupported_field:{field}")));
            continue;
        }
        let tag = format!(
            "{reviewer} ({}/{})",
            str_of("agreement"),
            record
                .get("chosen")
                .and_then(Value::as_str)
                .unwrap_or("none")
        );
        let coverage: Map<String, Value> = COVERAGE_KEYS
            .iter()
            .filter_map(|key| record.get(*key).map(|v| ((*key).to_owned(), v.clone())))
            .collect();
        out.conditions.push(json!({
            "source_type": source_type,
            "source_id": str_of("source_id"),
            "text_hash": str_of("text_hash"),
            "status": status,
            "rules": record.get("rules").cloned().unwrap_or(json!([])),
            "code_rules": record.get("code_rules").cloned().unwrap_or(json!([])),
            "window": record.get("window").cloned().unwrap_or(json!({"kind": "unknown"})),
            "remaining_checks": record.get("remaining_checks").cloned().unwrap_or(json!([])),
            "code_overrides": record.get("code_overrides").cloned().unwrap_or(json!([])),
            "coverage": Value::Object(coverage),
            "reviewer": tag,
        }));
        *out.by_status.entry(status.to_owned()).or_default() += 1;
        for o in record
            .get("code_overrides")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let text = |key: &str| o.get(key).and_then(Value::as_str).unwrap_or_default();
            *out.overrides_by_target
                .entry(format!("{}:{}", text("target"), text("action")))
                .or_default() += 1;
        }

        let applicant = record.get("applicant").cloned().unwrap_or(Value::Null);
        let class = applicant
            .get("class")
            .and_then(Value::as_str)
            .unwrap_or("unclear");
        let evidence = applicant
            .get("evidence")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default();
        let reason = if class == "unclear" {
            Some("class_unclear")
        } else if evidence.is_empty() {
            Some("no_evidence")
        } else if MASK_TOKENS.iter().any(|t| evidence.contains(t)) {
            Some("masked_evidence")
        } else if !applicant_hashes.contains_key(&program_id) {
            Some("no_applicant_hash")
        } else {
            None
        };
        match reason {
            Some(reason) => out.applicant_skipped.push((program_id, reason.into())),
            None => out.applicants.push(json!({
                "source_type": source_type,
                "source_id": str_of("source_id"),
                "class": class,
                "evidence": evidence,
                "reviewer": tag,
                "text_hash": applicant_hashes[&program_id],
            })),
        }
    }
    out
}

/// The first rule field (reviewed or source-code) reviews cannot write yet.
fn unsupported_field(record: &Map<String, Value>) -> Option<String> {
    let mut fields = Vec::new();
    for node in record
        .get("rules")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        match node.get("any").and_then(Value::as_array) {
            Some(children) => fields.extend(children.iter().filter_map(|c| c.get("field"))),
            None => fields.extend(node.get("field")),
        }
    }
    for o in record
        .get("code_overrides")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(replacement) = o.get("replacement") {
            fields.extend(
                super::overrides::replacement_leaves(replacement)
                    .into_iter()
                    .filter_map(|leaf| leaf.get("field")),
            );
        }
    }
    for code in record
        .get("code_rules")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if code.get("origin").and_then(Value::as_str) == Some("source_codes") {
            fields.extend(code.get("field"));
        }
    }
    fields
        .into_iter()
        .map(|f| f.as_str().unwrap_or_default())
        .find(|f| !is_review_field(f))
        .map(str::to_owned)
}
