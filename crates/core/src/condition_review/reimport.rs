//! Re-import lines rebuilt from stored condition reviews.
//!
//! A `complete` row stored before the current [`super::coverage::VERSION`]
//! is served as incomplete ([`super::coverage::served_status`]) until it is
//! written again through [`super::store::upsert`], the one path that runs
//! the coverage check. This converter turns such stored rows
//! (`row_to_json(program_condition_reviews)`, one per line) back into the
//! [`super::store::ReviewInput`] lines the importer reads, so the existing
//! `import-condition-reviews` re-checks them against the programme as it is
//! now: a changed text is `text_changed`, a structural reference the
//! programme lacks is `coverage_incomplete` (review B7). Nothing here
//! decides a status.
//!
//! What a stored row keeps of its input:
//! - reviewed rules: the `origin: review` nodes of `rule_json`, as written;
//! - the code rules the curators were shown: `coverage_json.code_rules`
//!   (the pilot converter copies them there). `rule_json` keeps only the
//!   compiled `source_codes` leaves, without their `from`;
//! - the reviewed window: `window_json.reviewed` once the confirmation job
//!   replaced it, else `window_json` (the importer then keeps the
//!   confirmation, as for any re-import of the same window);
//! - overrides, remaining checks, coverage, reviewer and `text_hash` as
//!   stored.
//!
//! A row is skipped, with a reason, when that cannot be rebuilt without
//! inventing anything: no `code_rules` in its coverage, a `source_codes`
//! leaf the shown code rules do not list, or a node of another origin.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::{json, Map, Value};

/// Keys of a stored `origin: review` node that are its input.
const REVIEW_NODE_KEYS: [&str; 7] = ["id", "field", "op", "value", "any", "seg_id", "evidence"];

#[derive(Debug, Default, Serialize)]
pub struct Conversion {
    /// Import lines, one per rebuilt row.
    pub lines: Vec<Value>,
    /// (source id, reason) of rows left out.
    pub skipped: Vec<(String, String)>,
    /// Rows left out by reason.
    pub skipped_by_reason: BTreeMap<String, usize>,
}

/// Convert stored rows (JSON lines). Only `complete` rows written before
/// `version` are rebuilt; any other row is skipped as `not_awaiting`.
pub fn convert(text: &str, version: i16) -> Conversion {
    let mut out = Conversion::default();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let (source_id, result) = match serde_json::from_str::<Value>(line) {
            Ok(row) => (
                row.get("source_id")
                    .and_then(Value::as_str)
                    .map_or_else(|| format!("line {}", index + 1), str::to_owned),
                input_line(&row, version),
            ),
            Err(_) => (format!("line {}", index + 1), Err("invalid_json")),
        };
        match result {
            Ok(line) => out.lines.push(line),
            Err(reason) => {
                *out.skipped_by_reason.entry(reason.to_owned()).or_default() += 1;
                out.skipped.push((source_id, reason.to_owned()));
            }
        }
    }
    out
}

/// The import line of one stored row, or why it cannot be rebuilt.
pub fn input_line(row: &Value, version: i16) -> Result<Value, &'static str> {
    let row = row.as_object().ok_or("invalid_row")?;
    let text = |key: &str| row.get(key).and_then(Value::as_str);
    let stored_version = row.get("coverage_version").and_then(Value::as_i64);
    if text("status") != Some("complete") || stored_version.is_some_and(|v| v >= i64::from(version))
    {
        return Err("not_awaiting");
    }
    let coverage = row
        .get("coverage_json")
        .and_then(Value::as_object)
        .ok_or("no_coverage")?;
    let code_rules = coverage
        .get("code_rules")
        .and_then(Value::as_array)
        .ok_or("no_code_rules")?;
    let shown = |id: &str| {
        code_rules
            .iter()
            .any(|c| c.get("id").and_then(Value::as_str) == Some(id))
    };

    let mut rules = Vec::new();
    let nodes = match row.get("rule_json") {
        None | Some(Value::Null) => Vec::new(),
        Some(tree) => tree
            .get("all")
            .and_then(Value::as_array)
            .cloned()
            .ok_or("invalid_rule_json")?,
    };
    for node in &nodes {
        let id = node.get("id").and_then(Value::as_str).unwrap_or_default();
        match node.get("origin").and_then(Value::as_str) {
            Some("review") => {
                let object = node.as_object().ok_or("invalid_rule_json")?;
                let input: Map<String, Value> = REVIEW_NODE_KEYS
                    .iter()
                    .filter_map(|key| object.get(*key).map(|v| ((*key).to_owned(), v.clone())))
                    .collect();
                rules.push(Value::Object(input));
            }
            // Rebuilt from the shown code rules, which carry `from`.
            Some(crate::rule_engine::SOURCE_ORIGIN) if shown(id) => {}
            Some(crate::rule_engine::SOURCE_ORIGIN) => return Err("code_rule_not_shown"),
            _ => return Err("unknown_rule_origin"),
        }
    }

    let window = row.get("window_json").ok_or("no_window")?;
    let window = window.get("reviewed").unwrap_or(window).clone();
    if window.get("source").and_then(Value::as_str) == Some("official_recheck") {
        return Err("no_reviewed_window");
    }

    let required = |key: &str| text(key).ok_or("missing_field");
    Ok(json!({
        "source_type": required("source_type")?,
        "source_id": required("source_id")?,
        "text_hash": required("text_hash")?,
        "status": "complete",
        "rules": rules,
        "code_rules": code_rules,
        "window": window,
        "remaining_checks": row.get("remaining_checks").cloned().unwrap_or(json!([])),
        "code_overrides": match row.get("code_overrides") {
            None | Some(Value::Null) => json!([]),
            Some(overrides) => overrides.clone(),
        },
        "coverage": Value::Object(coverage.clone()),
        "reviewer": required("reviewer")?,
    }))
}

#[cfg(test)]
mod tests {
    use super::super::overrides::CodeColumns;
    use super::super::store::{validate_with, Rejection, ReviewInput};
    use super::super::{coverage, text_hash};
    use super::*;

    fn payload() -> Value {
        json!({
            "plcyNm": "청년 월세 지원",
            "plcyExplnCn": "부산 거주 무주택 청년에게 월세 지원",
            "plcySprtCn": "월 20만원",
            "addAplyQlfcCndCn": " 만 19~34세",
            "jobCd": "0013003",
            "aplyPrdSeCd": "0057002",
        })
    }

    fn aged() -> CodeColumns {
        CodeColumns {
            min_age: Some(19),
            max_age: Some(34),
            ..Default::default()
        }
    }

    /// The curators' line: a reviewed rule, a code rule and the age band,
    /// every segment a condition backed by `S-age` and the reviewed rule.
    fn original() -> Value {
        let labels: Map<String, Value> = coverage::segments("youth_center", &payload())
            .unwrap()
            .into_iter()
            .map(|s| {
                (
                    s.seg_id,
                    json!({"label": "condition", "rule_refs": ["S-age", "R1"]}),
                )
            })
            .collect();
        let code_rules = json!([
            {"id": "C-job", "field": "employment_status", "op": "in",
             "value": ["unemployed", "preparing", "student"], "origin": "source_codes",
             "from": ["jobCd"]},
            {"id": "S-age", "field": "age", "op": "between", "value": [19, 34],
             "origin": "structural", "from": ["min_age", "max_age"]}
        ]);
        json!({
            "source_type": "youth_center", "source_id": "R1",
            "text_hash": text_hash("youth_center", &payload()).unwrap(),
            "status": "complete",
            "rules": [{"id": "R1", "field": "housing_type", "op": "in", "value": ["monthly_rent"],
                       "seg_id": "0123456789ab", "evidence": "무주택 청년에게 월세 지원"}],
            "code_rules": code_rules,
            "window": {"kind": "unknown", "source": "review"},
            "remaining_checks": [],
            "coverage": {"labels": labels, "code_rules": code_rules, "agreement": "agreed"},
            "reviewer": "curator (agreed/A)"
        })
    }

    /// The row the importer stored for `line`, as `row_to_json` exports it,
    /// before the coverage version existed, with a confirmed window.
    fn stored(line: &Value) -> Value {
        stored_with(line, &aged())
    }

    fn stored_with(line: &Value, columns: &CodeColumns) -> Value {
        let input: ReviewInput = serde_json::from_value(line.clone()).unwrap();
        let valid = validate_with(&input, &payload(), true, columns).unwrap();
        json!({
            "source_type": "youth_center", "source_id": "R1",
            "status": valid.status.as_str(),
            "rule_json": valid.rule_json,
            "window_json": {"kind": "rolling", "source": "official_recheck",
                            "opens_at": null, "closes_at": null,
                            "reviewed": valid.window_json},
            "remaining_checks": valid.remaining_checks,
            "coverage_json": valid.coverage_json,
            "evidence": valid.evidence,
            "text_hash": valid.text_hash,
            "reviewer": valid.reviewer,
            "code_overrides": valid.code_overrides,
            "coverage_version": null,
        })
    }

    fn validate_line(line: &Value, columns: &CodeColumns) -> Result<Value, Rejection> {
        let input: ReviewInput = serde_json::from_value(line.clone()).unwrap();
        validate_with(&input, &payload(), true, columns).map(|v| {
            json!({
                "status": v.status.as_str(), "rule_json": v.rule_json,
                "window_json": v.window_json, "coverage_json": v.coverage_json,
                "evidence": v.evidence, "text_hash": v.text_hash, "reviewer": v.reviewer,
                "code_overrides": v.code_overrides, "coverage_version": v.coverage_version,
            })
        })
    }

    /// Rebuild `line`'s stored row and validate it again on `columns`: it
    /// gives exactly the stored columns, now with the current coverage
    /// version, and the reviewed window under the confirmation (which the
    /// importer keeps).
    fn assert_round_trip(line: &Value, columns: &CodeColumns) {
        let row = stored_with(line, columns);
        let rebuilt = input_line(&row, coverage::VERSION).unwrap();
        assert_eq!(rebuilt["window"], row["window_json"]["reviewed"]);
        let again = validate_line(&rebuilt, columns).unwrap();
        for key in [
            "status",
            "rule_json",
            "coverage_json",
            "evidence",
            "text_hash",
            "reviewer",
            "code_overrides",
        ] {
            assert_eq!(again[key], row[key], "{key}");
        }
        assert_eq!(again["window_json"], row["window_json"]["reviewed"]);
        assert_eq!(again["coverage_version"], json!(coverage::VERSION));
    }

    #[test]
    fn a_rebuilt_line_validates_to_the_stored_row() {
        assert_round_trip(&original(), &aged());
    }

    /// A stored override (as `CodeOverride` serialises it) and a reviewed
    /// any-group rebuild to the same stored columns.
    #[test]
    fn overrides_and_any_groups_round_trip() {
        let regional = CodeColumns {
            regions: Some(vec!["busan".into()]),
            ..aged()
        };
        let mut line = original();
        line["code_overrides"] = json!([{
            "target": "S-region", "action": "drop", "check_kind": "residence_duration",
            "seg_id": "0123456789ab", "evidence": "부산 거주 무주택 청년"
        }]);
        let row = stored_with(&line, &regional);
        assert_eq!(row["code_overrides"][0]["target"], "S-region");
        assert_round_trip(&line, &regional);

        let mut line = original();
        line["rules"] = json!([{
            "id": "R2", "seg_id": "0123456789ab", "evidence": "무주택 청년에게 월세 지원",
            "any": [
                {"field": "employment_status", "op": "eq", "value": "unemployed"},
                {"field": "employment_status", "op": "eq", "value": "student"}
            ]
        }]);
        for label in line["coverage"]["labels"]
            .as_object_mut()
            .unwrap()
            .values_mut()
        {
            label["rule_refs"] = json!(["S-age", "R2"]);
        }
        let row = stored_with(&line, &aged());
        assert!(row["rule_json"]["all"][1]["any"].is_array());
        assert_round_trip(&line, &aged());
    }

    /// Review B7 through the rebuilt line: a programme that lost its age
    /// band no longer backs the `S-age` sentences, and a changed text is
    /// stale. Neither is upgraded.
    #[test]
    fn a_rebuilt_line_is_rejected_when_the_programme_changed() {
        let line = input_line(&stored(&original()), coverage::VERSION).unwrap();
        assert_eq!(
            validate_line(&line, &CodeColumns::default()).unwrap_err(),
            Rejection::CoverageIncomplete
        );
        let mut changed = line.clone();
        changed["text_hash"] = json!("0".repeat(64));
        assert_eq!(
            validate_line(&changed, &aged()).unwrap_err(),
            Rejection::TextChanged
        );
    }

    #[test]
    fn rows_that_cannot_be_rebuilt_are_skipped_with_a_reason() {
        let row = stored(&original());
        let with = |key: &str, value: Value| {
            let mut r = row.clone();
            r[key] = value;
            r
        };
        let version = coverage::VERSION;
        let mut no_shown = row.clone();
        no_shown["coverage_json"]
            .as_object_mut()
            .unwrap()
            .remove("code_rules");
        let mut unlisted = row.clone();
        unlisted["coverage_json"]["code_rules"] = json!([]);
        let mut other = row.clone();
        other["rule_json"]["all"][0]["origin"] = json!("structural");
        let cases = [
            (with("status", json!("incomplete")), "not_awaiting"),
            (with("coverage_version", json!(version)), "not_awaiting"),
            (no_shown, "no_code_rules"),
            (unlisted, "code_rule_not_shown"),
            (other, "unknown_rule_origin"),
            (
                with(
                    "window_json",
                    json!({"kind": "rolling", "source": "official_recheck"}),
                ),
                "no_reviewed_window",
            ),
        ];
        for (row, reason) in cases {
            assert_eq!(input_line(&row, version).unwrap_err(), reason);
        }
        // An older version is awaiting; a row without rules rebuilds empty.
        assert!(input_line(&with("coverage_version", json!(version - 1)), version).is_ok());
        let line = input_line(&with("rule_json", Value::Null), version).unwrap();
        assert_eq!(line["rules"], json!([]));

        let text = format!(
            "{}\n\nnot json\n{}\n",
            row,
            with("status", json!("conflict"))
        );
        let out = convert(&text, version);
        assert_eq!(out.lines.len(), 1);
        assert_eq!(
            out.skipped,
            vec![
                ("line 3".to_owned(), "invalid_json".to_owned()),
                ("R1".to_owned(), "not_awaiting".to_owned())
            ]
        );
    }
}
