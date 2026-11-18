//! Shared vocabulary of the deterministic text rules (`rule_v1`).
//!
//! Measurement only. The worker reads each programme's eligibility text with
//! fixed rules (`mazimi_worker::text_rules`) and stores the facts, with their
//! verbatim quotes and the profile conditions they would compile to, in
//! `program_rule_extractions`. Nothing serves them: they never reach
//! `eligibility_rules`, ranking, labels, explanations or "조건 확인됨". The
//! measured precision was too thin to let them decide anything.

use serde_json::Value;

use crate::rule_engine;

/// `program_rule_extractions.extractor` and `eligibility_rules.created_by`.
pub const EXTRACTOR: &str = "rule_v1";
/// Bump on any change to the rules, the enabled kinds or the compiled
/// leaves: every stored extraction is then recomputed.
pub const EXTRACTOR_VERSION: i32 = 1;
/// `origin` of a compiled condition stored with an extraction
/// ([`rule_engine::SOURCE_ORIGIN_KEY`]).
pub const ORIGIN: &str = "rule_v1";

/// Sources whose eligibility text the rules read. Welfare list rows carry
/// only a purpose summary, so their conditions are not in our data.
pub const SOURCES: [&str; 3] = ["gov_benefits", "youth_center", "scholarship"];

/// `raw_payload` keys whose change makes a stored extraction stale: the text
/// fields the rules read and the source codes whose coverage they check.
pub fn hashed_keys(source_type: &str) -> &'static [&'static str] {
    match source_type {
        "gov_benefits" => &["지원대상", "선정기준", "사용자구분", "supportConditions"],
        "youth_center" => &[
            "addAplyQlfcCndCn",
            "plcyExplnCn",
            "plcySprtCn",
            "ptcpPrpTrgtCn",
            "earnEtcCn",
            "jobCd",
            "schoolCd",
            "sbizCd",
            "plcyMajorCd",
            "mrgSttsCd",
            "earnCndSeCd",
            "earnMinAmt",
            "earnMaxAmt",
        ],
        "scholarship" => &[
            "특정자격 상세내용",
            "자격제한 상세내용",
            "소득기준 상세내용",
            "성적기준 상세내용",
            "지역거주여부 상세내용",
            "추천필요여부 상세내용",
            "대학구분",
            "학년구분",
            "학과구분",
        ],
        _ => &[],
    }
}

/// SQL expression (table alias `alias`, e.g. `"p."`) for the hex SHA-256 of
/// the [`hashed_keys`] of a row's `raw_payload` (canonical `jsonb` text). The
/// worker stores it; a measurement compares it with the current payload.
pub fn input_hash_sql(alias: &str) -> String {
    let arms: Vec<String> = SOURCES
        .iter()
        .map(|source| {
            let pairs: Vec<String> = hashed_keys(source)
                .iter()
                .map(|key| format!("'{key}', {alias}raw_payload->'{key}'"))
                .collect();
            format!(
                "WHEN '{source}' THEN jsonb_build_object({})::text",
                pairs.join(", ")
            )
        })
        .collect();
    format!(
        "encode(sha256(convert_to({alias}source_type || ':' || CASE {alias}source_type {} ELSE '' END, 'UTF8')), 'hex')",
        arms.join(" ")
    )
}

/// A programme's stored extraction, for measurement.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TextRuleState {
    /// The text is fully structured and every fact is a profile condition.
    pub evaluable: bool,
    /// Stored for the current [`EXTRACTOR_VERSION`] and the current payload.
    pub current: bool,
    /// The leaves the extraction compiled.
    pub leaves: Vec<Value>,
}

impl TextRuleState {
    /// Whether the extraction would account for every condition of the text
    /// with `rule_json` carrying all of its leaves (`None` = no rule
    /// version). An extraction without leaves proves nothing: placeholder or
    /// empty text is missing data, not "no conditions".
    pub fn verifies(&self, rule_json: Option<&Value>) -> bool {
        if !(self.evaluable && self.current) || self.leaves.is_empty() {
            return false;
        }
        let active = rule_engine::top_level_conditions(rule_json);
        self.leaves.iter().all(|leaf| active.contains(&leaf))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn leaf(field: &str) -> Value {
        json!({"field": field, "op": "is_true", "origin": ORIGIN, "quote": "무주택 세대구성원"})
    }

    #[test]
    fn only_a_current_evaluable_extraction_with_every_leaf_active_verifies() {
        let rule = json!({"all": [leaf("homeless_household"),
            {"field": "region_code", "op": "in", "value": ["busan"], "origin": "source_codes"}]});
        let state = TextRuleState {
            evaluable: true,
            current: true,
            leaves: vec![leaf("homeless_household")],
        };
        assert!(state.verifies(Some(&rule)));
        assert!(!TextRuleState {
            current: false,
            ..state.clone()
        }
        .verifies(Some(&rule)));
        assert!(!TextRuleState {
            evaluable: false,
            ..state.clone()
        }
        .verifies(Some(&rule)));
        // A leaf another writer replaced is no longer checked by the text rules.
        let replaced = json!({"all": [{"field": "homeless_household", "op": "is_true"}]});
        assert!(!state.verifies(Some(&replaced)));
        assert!(!state.verifies(None));
        // No leaves proves nothing.
        let empty = TextRuleState {
            leaves: vec![],
            ..state
        };
        assert!(!empty.verifies(None));
    }

    #[test]
    fn the_hash_expression_reads_only_the_listed_keys() {
        let sql = input_hash_sql("p.");
        for source in SOURCES {
            for key in hashed_keys(source) {
                assert!(sql.contains(&format!("p.raw_payload->'{key}'")), "{key}");
            }
        }
        assert!(!sql.contains("raw_payload->>"));
    }
}
