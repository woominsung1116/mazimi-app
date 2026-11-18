//! rule_v1 facts as profile conditions, and whether a programme is fully
//! structured.
//!
//! Only kinds in [`ENABLED_KINDS`] are kept: they reached at least 98%
//! precision on both held-out label sets (see [`super::eval`]). A fact of
//! any other kind is dropped and leaves its text unstructured, so the
//! programme is not fully structured.

use mazimi_core::text_rules::{EXTRACTOR_VERSION, ORIGIN};
use serde_json::{json, Value};

use super::extract::{Extraction, Fact, Role};

/// Kinds shipped in rule_v1, decided by the held-out measurement
/// ([`super::eval`]). `children` is extracted but disabled: no held-out set
/// has an emitted 자녀 수 fact, so its precision is unmeasured.
pub const ENABLED_KINDS: &[&str] = &[
    "age",
    "income_median",
    "income_kosaf",
    "major",
    "business",
    "prior_receipt",
    "homeless",
    "marital",
];

/// What the refresh stores for one programme.
#[derive(Debug, Clone, PartialEq)]
pub struct Compiled {
    pub has_text: bool,
    pub fully_structured: bool,
    pub evaluable: bool,
    /// Every kept fact, as stored (`program_rule_extractions.facts`).
    pub facts: Vec<Value>,
    /// The profile conditions the required facts would compile to (stored
    /// with the extraction for measurement; never served).
    pub leaves: Vec<Value>,
    pub blockers: Vec<String>,
}

/// The profile conditions of one fact, or `None` when no profile answer
/// decides it.
pub fn leaves(fact: &Fact) -> Option<Vec<Value>> {
    let leaf = |field: &str, op: &str, value: Value| {
        json!({
            "field": field, "op": op, "value": value, "weight": 1.0,
            "origin": ORIGIN, "kind": fact.kind, "quote": fact.quote,
            "extractor_version": EXTRACTOR_VERSION,
        })
    };
    let label = |mut v: Value| {
        v["severity"] = json!("label");
        v
    };
    let v = &fact.value;
    Some(match fact.kind {
        "age" => {
            // A 연 나이 band is not the 만나이 the profile reads.
            if v.get("basis").is_some() {
                return None;
            }
            vec![leaf("age", "between", json!([v["min"], v["max"]]))]
        }
        "income_median" => {
            let op = if v["strict"] == json!(true) {
                "lt"
            } else {
                "lte"
            };
            vec![leaf("income_median_pct", op, v["max_pct"].clone())]
        }
        "income_kosaf" => vec![leaf("kosaf_support_bracket", "lte", v["max"].clone())],
        "homeless" => match v["level"].as_str() {
            // The profile asks about the user's own household.
            Some("household") => vec![leaf("homeless_household", "is_true", Value::Null)],
            // A homeless household means a homeless person; a household
            // with a home does not say whether this person owns it.
            Some("person") => vec![label(leaf("homeless_household", "is_true", Value::Null))],
            _ => return None,
        },
        "marital" => match (v["status"].as_str(), v["newlywed_years"].as_i64()) {
            (Some("unmarried"), _) => vec![leaf("marital_status", "eq", json!("unmarried"))],
            // The profile's 신혼 is 혼인 7년 이내.
            (Some("married"), Some(7)) => vec![leaf("newlywed", "is_true", Value::Null)],
            (Some("married"), Some(years)) if years > 7 => vec![
                leaf("marital_status", "eq", json!("married")),
                label(leaf("newlywed", "is_true", Value::Null)),
            ],
            (Some("married"), None) if v.get("newlywed_years").is_none() => {
                vec![leaf("marital_status", "eq", json!("married"))]
            }
            // An unqualified 신혼부부 or a window under 7 years.
            _ => return None,
        },
        // 다자녀 rules usually count minor children only, which the
        // profile's 자녀 수 does not tell; major names a 학과, the profile a
        // 계열; business and prior receipt need answers the profile lacks.
        _ => return None,
    })
}

pub fn compile(extraction: &Extraction, uncovered: &[&str]) -> Compiled {
    let mut blockers: Vec<String> = Vec::new();
    if !extraction.has_text {
        blockers.push("no_text".into());
    } else if !extraction.complete {
        blockers.push("text_not_structured".into());
    }
    let (kept, dropped): (Vec<&Fact>, Vec<&Fact>) = extraction
        .facts
        .iter()
        .partition(|f| ENABLED_KINDS.contains(&f.kind));
    for f in &dropped {
        push_unique(&mut blockers, format!("disabled_kind:{}", f.kind));
    }
    for code in uncovered {
        push_unique(&mut blockers, format!("uncovered_code:{code}"));
    }
    let fully_structured = blockers.is_empty();
    let mut all_leaves = Vec::new();
    let mut evaluable = fully_structured;
    let mut facts = Vec::new();
    for f in &kept {
        let compiled = leaves(f);
        if f.role != Role::Req {
            push_unique(&mut blockers, format!("role:{}", f.role.as_str()));
            evaluable = false;
        }
        match &compiled {
            Some(leaves) if f.role == Role::Req => all_leaves.extend(leaves.iter().cloned()),
            Some(_) => {}
            None => {
                push_unique(&mut blockers, format!("not_evaluable:{}", f.kind));
                evaluable = false;
            }
        }
        facts.push(json!({
            "kind": f.kind, "value": f.value, "role": f.role.as_str(), "field": f.field,
            "quote": f.quote, "rule": f.rule, "evaluable": compiled.is_some(),
        }));
    }
    all_leaves.dedup();
    Compiled {
        has_text: extraction.has_text,
        fully_structured,
        evaluable,
        facts,
        // Leaves of required facts only: an alternative or an exclusion is
        // not a condition everyone must meet.
        leaves: all_leaves,
        blockers,
    }
}

fn push_unique(list: &mut Vec<String>, item: String) {
    if !list.contains(&item) {
        list.push(item);
    }
}

#[cfg(test)]
mod tests {
    use super::super::extract::extract;
    use super::*;
    use std::collections::BTreeMap;

    fn run(text: &str) -> Compiled {
        let e = extract(
            "gov_benefits",
            &BTreeMap::from([("지원대상".to_string(), text.to_string())]),
        );
        compile(&e, &[])
    }

    #[test]
    fn a_fully_structured_text_compiles_every_fact_to_a_leaf() {
        let c = run("기준 중위소득 150% 이하 무주택세대구성원");
        assert!(c.fully_structured && c.evaluable, "{c:?}");
        let fields: Vec<&str> = c
            .leaves
            .iter()
            .map(|l| l["field"].as_str().unwrap())
            .collect();
        assert_eq!(fields, vec!["income_median_pct", "homeless_household"]);
        for leaf in &c.leaves {
            assert_eq!(leaf["origin"], "rule_v1");
            assert!(leaf["quote"].as_str().is_some_and(|q| !q.is_empty()));
            mazimi_core::rule_engine::parse_rule_json(leaf).unwrap();
        }
    }

    #[test]
    fn a_fact_no_answer_decides_keeps_the_programme_unevaluable() {
        let c = run("신혼부부");
        assert!(c.fully_structured, "{c:?}");
        assert!(!c.evaluable);
        assert!(c.blockers.contains(&"not_evaluable:marital".to_string()));
        let c = run("기준 중위소득 100% 이하");
        let c2 = compile(
            &extract(
                "gov_benefits",
                &BTreeMap::from([(
                    "지원대상".to_string(),
                    "기준 중위소득 100% 이하".to_string(),
                )]),
            ),
            &["gender"],
        );
        assert!(c.evaluable);
        assert!(!c2.fully_structured && !c2.evaluable);
        assert_eq!(c2.blockers, vec!["uncovered_code:gender".to_string()]);
    }
}
