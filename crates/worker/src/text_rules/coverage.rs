//! Source codes that state a condition no compiled rule checks.
//!
//! A programme's text can be fully structured while its codes still
//! restrict who qualifies (a 보조금24 gender flag, an 온통청년 marriage code).
//! Those codes become rules only where [`crate::eligibility_codes`] compiles
//! them; any other restricting code is a condition the list cannot check, so
//! the programme is not fully structured. Ages, regions and districts are
//! served columns checked structurally and need nothing here.

use serde_json::Value;

use crate::eligibility_codes::{gov_conditions, gov_rule_conditions, youth_conditions};

/// Codes that restrict applicants without a compiled rule, as fixed names.
pub fn uncovered_codes(source_type: &str, payload: &Value) -> Vec<&'static str> {
    match source_type {
        "gov_benefits" => gov_uncovered(payload),
        "youth_center" => youth_uncovered(payload),
        "scholarship" => scholarship_uncovered(payload),
        _ => vec!["unsupported_source"],
    }
}

fn gov_uncovered(payload: &Value) -> Vec<&'static str> {
    let c = gov_conditions(payload);
    let mut out = Vec::new();
    match c.applicant_types.as_deref() {
        // A person (or their household) applies.
        Some(kinds) if kinds.iter().all(|k| k == "individual" || k == "household") => {}
        _ => out.push("applicant_type"),
    }
    if c.target_gender.is_some() {
        out.push("gender");
    }
    if c.income_median_bands.is_some() {
        out.push("income_median_bands");
    }
    // A group is covered only when its rule is compiled on fields a profile
    // answers (a label on target_group or gender names a check, it checks
    // nothing). Compile each group alone to see which ones are.
    let compiled = |only: crate::eligibility_codes::GovConditions| {
        let rules = gov_rule_conditions(&only);
        !rules.is_empty() && rules.iter().all(answerable)
    };
    if c.target_groups.is_some()
        && !compiled(crate::eligibility_codes::GovConditions {
            target_groups: c.target_groups.clone(),
            applicant_types: c.applicant_types.clone(),
            ..Default::default()
        })
    {
        out.push("target_groups");
    }
    if c.target_household_types.is_some()
        && !compiled(crate::eligibility_codes::GovConditions {
            target_household_types: c.target_household_types.clone(),
            ..Default::default()
        })
    {
        out.push("target_household_types");
    }
    out
}

/// Whether every leaf of a compiled condition is on a field some profile
/// answers ([`mazimi_core::rule_engine::NO_PROFILE_ANSWER`]).
fn answerable(rule: &Value) -> bool {
    match rule {
        Value::Object(map) => match map.get("field").and_then(Value::as_str) {
            Some(field) => !mazimi_core::rule_engine::NO_PROFILE_ANSWER.contains(&field),
            None => map.values().all(answerable),
        },
        Value::Array(items) => items.iter().all(answerable),
        _ => true,
    }
}

fn youth_uncovered(payload: &Value) -> Vec<&'static str> {
    let c = youth_conditions(payload);
    let mut out = Vec::new();
    if c.employment_requirements.is_some() && c.employment_statuses.is_none() {
        out.push("jobCd");
    }
    if c.education_requirements.is_some() && c.education_stages.is_none() {
        out.push("schoolCd");
    }
    // Only 장애인 and 기초생활수급자 sets compile.
    if let Some(groups) = &c.target_groups {
        if !groups
            .iter()
            .all(|g| g == "disabled" || g == "basic_livelihood_recipient")
        {
            out.push("sbizCd");
        }
    }
    if c.target_gender.is_some() {
        out.push("gender");
    }
    if c.marital_status.is_some() {
        out.push("mrgSttsCd");
    }
    if c.major_requirements.is_some() && c.major_categories.is_none() {
        out.push("plcyMajorCd");
    }
    if c.income_annual_min_manwon.is_some() || c.income_annual_max_manwon.is_some() {
        out.push("earnMinAmt/earnMaxAmt");
    }
    // 0043003 (기타) points at earnEtcCn, which the text rules read.
    out
}

/// 대학구분·학년구분·학과구분 restrict unless they say 제한없음; a
/// recommendation requirement is never a profile answer.
fn scholarship_uncovered(payload: &Value) -> Vec<&'static str> {
    let text = |key: &str| {
        payload
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default()
            .to_owned()
    };
    let mut out = Vec::new();
    for (key, name) in [
        ("대학구분", "대학구분"),
        ("학년구분", "학년구분"),
        ("학과구분", "학과구분"),
    ] {
        let value = text(key);
        if !value.is_empty() && !value.replace(' ', "").contains("제한없음") {
            out.push(name);
        }
    }
    let recommendation = text("추천필요여부 상세내용").replace(' ', "");
    if !recommendation.is_empty()
        && !matches!(
            recommendation.as_str(),
            "해당없음" | "없음" | "-" | "해당사항없음" | "필요없음"
        )
    {
        out.push("추천필요여부");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn restricting_codes_without_a_rule_are_uncovered() {
        // 개인, no flags: nothing beyond the text.
        assert!(uncovered_codes("gov_benefits", &json!({"사용자구분": "개인"})).is_empty());
        assert_eq!(
            uncovered_codes("gov_benefits", &json!({"사용자구분": "법인/시설/단체"})),
            vec!["applicant_type"]
        );
        assert_eq!(
            uncovered_codes("gov_benefits", &json!({})),
            vec!["applicant_type"]
        );
        let female = json!({"사용자구분": "개인",
            "supportConditions": {"JA0101": "N", "JA0102": "Y", "JA0201": "Y"}});
        assert_eq!(
            uncovered_codes("gov_benefits", &female),
            vec!["gender", "income_median_bands"]
        );
        // 장애인 compiles to a rule; 농업인 does not.
        let disabled = json!({"사용자구분": "개인", "supportConditions": {"JA0328": "Y"}});
        assert!(uncovered_codes("gov_benefits", &disabled).is_empty());
        let farmer = json!({"사용자구분": "개인", "supportConditions": {"JA0313": "Y"}});
        assert_eq!(
            uncovered_codes("gov_benefits", &farmer),
            vec!["target_groups"]
        );

        assert!(uncovered_codes("youth_center", &json!({"jobCd": "0013010"})).is_empty());
        assert_eq!(
            uncovered_codes("youth_center", &json!({"mrgSttsCd": "0055002"})),
            vec!["mrgSttsCd"]
        );
        assert_eq!(
            uncovered_codes("youth_center", &json!({"jobCd": "0013006"})),
            vec!["jobCd"]
        );
        assert_eq!(
            uncovered_codes(
                "scholarship",
                &json!({"학년구분": "대학 1학년", "대학구분": "제한없음"})
            ),
            vec!["학년구분"]
        );
        assert_eq!(
            uncovered_codes("local_welfare", &json!({})),
            vec!["unsupported_source"]
        );
    }
}
