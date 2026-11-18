//! Code overrides: provable widening per field type, validation, and the
//! serve-time gate.

use super::overrides::{
    admitting, applicable, contains, evaluate_replacement, strip_targets, structural_gate,
    target_rule, CodeColumns, CodeOverride, EXCEPTION_KIND,
};
use super::store::{validate_with, Rejection, ReviewInput};
use super::*;
use crate::models::{ExclusionReason, ProfileInput};
use crate::rule_engine::RuleVerdict;
use crate::structural::{CheckKind, StructuralProfile};
use serde_json::json;

fn widens(wide: Value, narrow: Value) -> bool {
    contains(&wide, &narrow)
}

fn age(op: &str, value: Value) -> Value {
    json!({"field": "age", "op": op, "value": value})
}

#[test]
fn integer_ranges_widen_only_outward() {
    let target = age("between", json!([18, 34]));
    for wide in [
        age("between", json!([18, 39])),
        age("between", json!([15, 34])),
        age("gte", json!(18)),
        age("lte", json!(39)),
        age("gt", json!(17)),
        age("lt", json!(35)),
        age("ne", json!(40)),
        age("not_in", json!([40, 50])),
        json!({"any": [age("between", json!([35, 39])), age("between", json!([18, 34]))]}),
    ] {
        assert!(widens(wide.clone(), target.clone()), "{wide}");
    }
    for narrow in [
        age("between", json!([19, 39])),
        age("between", json!([18, 33])),
        age("gte", json!(19)),
        age("gt", json!(18)),
        age("lt", json!(34)),
        age("eq", json!(30)),
        age("in", json!([18, 19, 20])),
        age("ne", json!(30)),
        // Same numbers, other field.
        json!({"field": "school_year", "op": "between", "value": [18, 39]}),
        // Two branches that together cover the band are not one that does.
        json!({"any": [age("between", json!([18, 25])), age("between", json!([26, 39]))]}),
    ] {
        assert!(!widens(narrow.clone(), target.clone()), "{narrow}");
    }
    // A lower bound only (S-age gte 29) needs an open top.
    let min_only = age("gte", json!(29));
    assert!(widens(age("gte", json!(15)), min_only.clone()));
    assert!(!widens(age("between", json!([15, 100])), min_only));
    // A listed set inside a range.
    assert!(widens(
        age("in", json!([18, 19, 20])),
        age("between", json!([18, 20]))
    ));
    assert!(!widens(
        age("in", json!([18, 20])),
        age("between", json!([18, 20]))
    ));
}

#[test]
fn enum_sets_widen_only_by_adding_values() {
    let job =
        |op: &str, value: Value| json!({"field": "employment_status", "op": op, "value": value});
    let target = job("in", json!(["unemployed", "preparing", "student"]));
    assert!(widens(
        job(
            "in",
            json!(["unemployed", "preparing", "student", "employed"])
        ),
        target.clone()
    ));
    assert!(widens(job("ne", json!("employed")), target.clone()));
    assert!(widens(
        job("not_in", json!(["employed", "new_hire"])),
        target.clone()
    ));
    assert!(!widens(
        job("in", json!(["unemployed", "preparing"])),
        target.clone()
    ));
    assert!(!widens(job("not_in", json!(["student"])), target.clone()));
    assert!(!widens(job("eq", json!("unemployed")), target.clone()));
    // An all-but set is contained only by a smaller all-but set.
    assert!(widens(
        job("ne", json!("employed")),
        job("not_in", json!(["employed", "other"]))
    ));
    assert!(!widens(
        job("in", json!(["employed"])),
        job("ne", json!("other"))
    ));

    let region = |values: Value| json!({"field": "region_code", "op": "in", "value": values});
    assert!(widens(
        region(json!(["chungbuk", "gyeonggi"])),
        region(json!(["chungbuk"]))
    ));
    assert!(!widens(
        region(json!(["gyeonggi"])),
        region(json!(["chungbuk"]))
    ));
    // An unresolved scope admits nobody: an explicit list widens it, an
    // all-but set does not.
    assert!(widens(
        region(json!(["seoul", "busan"])),
        region(json!(["unknown"]))
    ));
    assert!(!widens(
        json!({"field": "region_code", "op": "ne", "value": "jeju"}),
        region(json!(["unknown"]))
    ));
    // `region` and `region_code` are one field.
    assert!(widens(
        json!({"field": "region", "op": "eq", "value": "chungbuk"}),
        region(json!(["chungbuk"]))
    ));
}

/// Verifier gaps 5, 6: the overrides read districts through the structural
/// rule (official parents, current codes), never a digit prefix.
#[test]
fn override_districts_read_the_structural_rule() {
    let city = |values: Value| json!({"field": "city_code", "op": "in", "value": values});
    // 영동군 43740 is not the parent of 증평군 43745; 청주시 is of 상당구.
    assert!(!widens(city(json!(["43740"])), city(json!(["43745"]))));
    assert!(widens(city(json!(["43110"])), city(json!(["43111"]))));
    // S-district lists the current codes only: a name is no leaf, and the
    // official 12xxx numbering reads as the client code.
    let rule = |listed: &[&str], regions: &[&str]| {
        target_rule(
            "S-district",
            &CodeColumns {
                regions: Some(regions.iter().map(|r| (*r).to_owned()).collect()),
                region_districts: Some(listed.iter().map(|d| (*d).to_owned()).collect()),
                ..CodeColumns::default()
            },
        )
    };
    assert_eq!(
        rule(&["수영구", "26500"], &["busan"]),
        Some(city(json!(["26500"])))
    );
    assert_eq!(rule(&["12210"], &["gwangju"]), Some(city(json!(["29110"]))));
    assert_eq!(rule(&["수영구"], &["busan"]), None);
}

#[test]
fn districts_widen_by_city_or_sido() {
    let city = |values: Value| json!({"field": "city_code", "op": "in", "value": values});
    let region = |values: Value| json!({"field": "region_code", "op": "in", "value": values});
    // 용인 처인·기흥·수지구.
    let target = city(json!(["41461", "41463", "41465"]));
    assert!(
        widens(city(json!(["41460"])), target.clone()),
        "a 시 covers its 구"
    );
    assert!(widens(
        city(json!(["41461", "41463", "41465", "41110"])),
        target.clone()
    ));
    assert!(widens(region(json!(["gyeonggi"])), target.clone()));
    assert!(widens(
        json!({"any": [city(json!(["41461", "41463", "41465"])),
                       {"field": "enrollment_status", "op": "in", "value": ["enrolled", "on_leave"]}]}),
        target.clone()
    ));
    assert!(!widens(city(json!(["41461"])), target.clone()));
    assert!(!widens(region(json!(["seoul"])), target.clone()));
    // A 구 does not cover its 시.
    assert!(!widens(city(json!(["41461"])), city(json!(["41460"]))));

    // 부산 전체 + 김해시: the sido-wide part needs a region branch.
    let columns = CodeColumns {
        regions: Some(vec!["busan".into(), "gyeongnam".into()]),
        region_districts: Some(vec!["48250".into()]),
        ..CodeColumns::default()
    };
    let target = target_rule("S-district", &columns).unwrap();
    assert!(!widens(city(json!(["48250", "48120"])), target.clone()));
    assert!(widens(
        json!({"any": [region(json!(["busan"])), city(json!(["48250", "48120"]))]}),
        target.clone()
    ));
    assert!(widens(region(json!(["busan", "gyeongnam"])), target));
}

#[test]
fn bool_groups_and_labels() {
    let flag = |field: &str| json!({"field": field, "op": "is_true"});
    let target = json!({"any": [flag("has_disability"), flag("is_low_income_household")],
                        "origin": "source_codes"});
    assert!(widens(
        json!({"any": [flag("has_disability"), flag("is_low_income_household"), flag("veteran_family")]}),
        target.clone()
    ));
    assert!(!widens(flag("has_disability"), target.clone()));
    assert!(!widens(
        json!({"field": "has_disability", "op": "is_false"}),
        flag("has_disability")
    ));
    // A label leaf never hides: nothing hard is wider than it.
    let label = json!({"field": "major_category", "op": "in", "value": ["engineering"],
                       "severity": "label", "origin": "source_codes"});
    assert!(!widens(
        json!({"field": "major_category", "op": "in",
               "value": ["engineering", "natural_science", "humanities"]}),
        label
    ));
}

#[test]
fn targets_read_the_current_columns_and_compiled_rule() {
    let columns = CodeColumns {
        min_age: Some(18),
        max_age: Some(34),
        regions: Some(vec!["chungbuk".into()]),
        region_districts: Some(vec!["43111".into(), "4311200000".into(), "bogus".into()]),
        enrollment_statuses: Some(vec!["enrolled".into(), "재학".into()]),
        hard_filter: Some(json!({"all": [
            {"field": "employment_status", "op": "in", "value": ["unemployed"],
             "origin": "source_codes", "weight": 20},
            {"field": "employment_status", "op": "in", "value": ["unemployed"]},
            {"field": "education_stage", "op": "in", "value": ["univ_enrolled"],
             "origin": "source_codes"}
        ]})),
    };
    assert_eq!(
        target_rule("S-age", &columns),
        Some(age("between", json!([18, 34])))
    );
    assert_eq!(
        target_rule("S-district", &columns),
        Some(json!({"field": "city_code", "op": "in", "value": ["43111", "43112"]}))
    );
    assert_eq!(
        target_rule("S-enrollment", &columns).unwrap()["value"],
        json!(["enrolled"]),
        "aliases admit nobody"
    );
    // Only the compiled (source-code) condition is the target.
    assert_eq!(
        target_rule("C-job", &columns),
        Some(
            json!({"field": "employment_status", "op": "in", "value": ["unemployed"],
                    "origin": "source_codes", "weight": 20})
        )
    );
    assert!(target_rule("C-sbiz", &columns).is_none());
    assert!(target_rule("S-age", &CodeColumns::default()).is_none());

    // Stripping a C- target keeps every other condition.
    let job = CodeOverride {
        target: "C-job".into(),
        action: overrides::Action::Drop,
        replacement: None,
        check_kind: "employer_or_job_detail".into(),
        seg_id: None,
        evidence: "x".into(),
    };
    let stripped = strip_targets(columns.hard_filter.as_ref(), &[&job]).unwrap();
    assert_eq!(stripped["all"].as_array().unwrap().len(), 2);
    assert!(stripped["all"]
        .as_array()
        .unwrap()
        .iter()
        .all(|c| !(c["origin"] == "source_codes" && c["field"] == "employment_status")));
    let only = json!({"all": [{"field": "employment_status", "op": "in", "value": ["unemployed"],
                               "origin": "source_codes"}]});
    assert_eq!(strip_targets(Some(&only), &[&job]), None);
}

// ── Validation ─────────────────────────────────────────────────────────────

fn veteran_payload() -> Value {
    json!({
        "plcyNm": "청년도전지원사업",
        "plcyExplnCn": "구직단념청년의 구직의욕 고취",
        "addAplyQlfcCndCn": "※ 군 경력자는 복무기간에 비례하여 참여 제한 연령을 연동하여 적용(최고 만 39세로 한정)",
        "plcyAplyMthdCn": "온라인 신청",
        "sprtTrgtMinAge": "18", "sprtTrgtMaxAge": "34", "zipCd": "41461,41463,41465",
        "jobCd": "0013003", "aplyPrdSeCd": "0057002", "aplyYmd": ""
    })
}

fn veteran_columns() -> CodeColumns {
    CodeColumns {
        min_age: Some(18),
        max_age: Some(34),
        regions: Some(vec!["gyeonggi".into()]),
        region_districts: Some(vec!["41461".into(), "41463".into(), "41465".into()]),
        enrollment_statuses: None,
        hard_filter: Some(json!({"all": [{"field": "employment_status", "op": "in",
            "value": ["preparing", "student", "unemployed"], "origin": "source_codes",
            "weight": 20}]})),
    }
}

fn override_input(code_overrides: Value) -> ReviewInput {
    let raw = veteran_payload();
    serde_json::from_value(json!({
        "source_type": "youth_center", "source_id": "V1",
        "text_hash": text_hash("youth_center", &raw).unwrap(),
        "status": "incomplete",
        "rules": [],
        "code_rules": [
            {"id": "C-job", "field": "employment_status", "op": "in",
             "value": ["preparing", "student", "unemployed"], "origin": "source_codes",
             "from": ["jobCd"]},
            {"id": "S-age", "field": "age", "op": "between", "value": [18, 34],
             "origin": "structural"}
        ],
        "window": {"kind": "unknown"},
        "remaining_checks": ["military_service"],
        "reviewer": "curator:A+B",
        "code_overrides": code_overrides
    }))
    .unwrap()
}

const VETERAN_QUOTE: &str =
    "군 경력자는 복무기간에 비례하여 참여 제한 연령을 연동하여 적용(최고 만 39세로 한정)";

fn veteran_override() -> Value {
    json!({"target": "S-age", "action": "widen",
           "replacement": {"field": "age", "op": "between", "value": [18, 39]},
           "check_kind": "military_service", "seg_id": "0123456789ab",
           "evidence": VETERAN_QUOTE})
}

#[test]
fn validate_stores_a_wider_override_and_drops_the_overridden_code_copy() {
    let job_drop = json!({"target": "C-job", "action": "drop", "check_kind": "employer_or_job_detail",
                          "evidence": "구직단념청년의 구직의욕 고취"});
    let valid = validate_with(
        &override_input(json!([veteran_override(), job_drop])),
        &veteran_payload(),
        true,
        &veteran_columns(),
    )
    .unwrap();
    let stored: Vec<CodeOverride> =
        serde_json::from_value(valid.code_overrides.clone().unwrap()).unwrap();
    assert_eq!(stored.len(), 2);
    assert_eq!(stored[0].target, "S-age");
    assert_eq!(stored[0].replacement, Some(age("between", json!([18, 39]))));
    assert_eq!(stored[0].seg_id.as_deref(), Some("0123456789ab"));
    assert_eq!(stored[1].replacement, None);
    // The copied C-job leaf is not kept: the override replaces it.
    assert_eq!(valid.rule_json, None);
    assert_eq!(
        valid.evidence,
        json!([{"for": "override:S-age", "quote": VETERAN_QUOTE},
               {"for": "override:C-job", "quote": "구직단념청년의 구직의욕 고취"}])
    );
    // No override: stored as NULL.
    let none = validate_with(
        &override_input(json!([])),
        &veteran_payload(),
        true,
        &veteran_columns(),
    )
    .unwrap();
    assert_eq!(none.code_overrides, None);
    assert!(none.rule_json.is_some(), "the C-job copy stays");
}

#[test]
fn validate_rejects_narrow_unprovable_and_malformed_overrides() {
    let reject = |overrides: Value| {
        validate_with(
            &override_input(overrides),
            &veteran_payload(),
            true,
            &veteran_columns(),
        )
        .unwrap_err()
    };
    let with = |patch: Value| {
        let mut o = veteran_override();
        for (k, v) in patch.as_object().unwrap() {
            if v.is_null() {
                o.as_object_mut().unwrap().remove(k);
            } else {
                o[k] = v.clone();
            }
        }
        json!([o])
    };
    for (patch, expected) in [
        // Narrower or not provable.
        (
            json!({"replacement": age("between", json!([19, 39]))}),
            Rejection::OverrideNotWider,
        ),
        (
            json!({"replacement": age("between", json!([18, 30]))}),
            Rejection::OverrideNotWider,
        ),
        (
            json!({"replacement": {"field": "region_code", "op": "in", "value": ["gyeonggi"]}}),
            Rejection::OverrideNotWider,
        ),
        // Shape.
        (json!({"target": "S-height"}), Rejection::InvalidOverride),
        (json!({"target": "C-marital"}), Rejection::InvalidOverride),
        (json!({"action": "replace"}), Rejection::InvalidOverride),
        (json!({"replacement": null}), Rejection::InvalidOverride),
        (json!({"action": "drop"}), Rejection::InvalidOverride),
        (json!({"check_kind": null}), Rejection::InvalidOverride),
        (
            json!({"check_kind": "shoe_size"}),
            Rejection::InvalidOverride,
        ),
        (
            json!({"replacement": {"field": "age", "op": "between", "value": [18, 39],
                                   "severity": "label"}}),
            Rejection::InvalidOverride,
        ),
        (
            json!({"replacement": {"field": "shoe_size", "op": "eq", "value": 1}}),
            Rejection::InvalidOverride,
        ),
        (
            json!({"replacement": {"any": [age("between", json!([18, 39]))]}}),
            Rejection::InvalidOverride,
        ),
        (
            json!({"replacement": {"any": [age("between", json!([18, 39])),
                                           {"any": [age("eq", json!(1)), age("eq", json!(2))]}]}}),
            Rejection::InvalidOverride,
        ),
        // Evidence: verbatim text, never a code quote.
        (
            json!({"evidence": "군 경력자는 최고 만 39세"}),
            Rejection::EvidenceNotFound,
        ),
        (
            json!({"evidence": "code:sprtTrgtMaxAge=34"}),
            Rejection::EvidenceNotFound,
        ),
        (json!({"evidence": null}), Rejection::EvidenceNotFound),
    ] {
        assert_eq!(reject(with(patch.clone())), expected, "{patch}");
    }
    // Twice the same target.
    assert_eq!(
        reject(json!([veteran_override(), veteran_override()])),
        Rejection::InvalidOverride
    );
    // A district replacement must list districts.
    assert_eq!(
        reject(json!([{"target": "S-district", "action": "widen",
                       "replacement": {"field": "city_code", "op": "not_in", "value": ["11110"]},
                       "check_kind": "other", "evidence": VETERAN_QUOTE}])),
        Rejection::InvalidOverride
    );
    // A code rule the programme does not have.
    assert_eq!(
        reject(
            json!([{"target": "S-enrollment", "action": "drop", "check_kind": "other",
                       "evidence": VETERAN_QUOTE}])
        ),
        Rejection::OverrideTargetMissing
    );
    // Widening the region alone leaves the district limit hiding.
    assert_eq!(
        reject(json!([{"target": "S-region", "action": "widen",
                       "replacement": {"field": "region_code", "op": "in",
                                       "value": ["gyeonggi", "seoul"]},
                       "check_kind": "other", "evidence": VETERAN_QUOTE}])),
        Rejection::OverrideIncomplete
    );
    // Without programme columns (pilot replay) every override is targetless.
    assert_eq!(
        store::validate(
            &override_input(json!([veteran_override()])),
            &veteran_payload(),
            true
        )
        .unwrap_err(),
        Rejection::OverrideTargetMissing
    );
}

// ── Serve ──────────────────────────────────────────────────────────────────

fn person(age: i32, extra: Value) -> ProfileInput {
    let mut base = json!({"birth_year": crate::seoul_year(chrono::Utc::now()) - age,
                          "region_code": "gyeonggi"});
    base.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    serde_json::from_value(base).unwrap()
}

fn structural(p: &ProfileInput) -> StructuralProfile<'_> {
    StructuralProfile {
        birth_year: Some(p.birth_year),
        birth_date: p.birth_date,
        region_code: Some(&p.region_code),
        enrollment_status: p.enrollment_status.as_deref(),
        university_id: None,
        city_code: p.city_code.as_deref(),
    }
}

fn programme() -> crate::models::Program {
    let mut p = crate::structural::test_program("policy");
    p.min_age = Some(18);
    p.max_age = Some(34);
    p.regions = Some(vec!["gyeonggi".into()]);
    p.region_districts = Some(vec!["41461".into(), "41463".into(), "41465".into()]);
    p
}

#[test]
fn the_gate_replaces_only_applied_structural_checks() {
    let p = programme();
    let veteran: CodeOverride = serde_json::from_value(veteran_override()).unwrap();
    let older = person(38, json!({"city_code": "41463"}));
    let gate = |o: &[&CodeOverride], who: &ProfileInput| {
        structural_gate(
            &p,
            None,
            &structural(who),
            who,
            o,
            crate::seoul_date(chrono::Utc::now()),
        )
    };
    assert_eq!(gate(&[], &older).unwrap_err(), ExclusionReason::Age);
    let (checks, evals) = gate(&[&veteran], &older).unwrap();
    assert!(checks.iter().all(|c| c.kind != CheckKind::Age));
    assert_eq!(evals[0].verdict, RuleVerdict::Pass);
    // The replacement still hides outside it, as an age exclusion.
    assert_eq!(
        gate(&[&veteran], &person(45, json!({"city_code": "41463"}))).unwrap_err(),
        ExclusionReason::Age
    );
    // Other structural checks still apply.
    assert_eq!(
        gate(&[&veteran], &person(38, json!({"city_code": "41110"}))).unwrap_err(),
        ExclusionReason::Region
    );

    // Applied only while the target exists and is still contained.
    let columns = CodeColumns::of(&p, None);
    assert_eq!(
        applicable(std::slice::from_ref(&veteran), &columns).len(),
        1
    );
    let mut narrowed = p.clone();
    narrowed.min_age = Some(15);
    assert!(applicable(
        std::slice::from_ref(&veteran),
        &CodeColumns::of(&narrowed, None)
    )
    .is_empty());
    narrowed.min_age = None;
    narrowed.max_age = None;
    assert!(applicable(
        std::slice::from_ref(&veteran),
        &CodeColumns::of(&narrowed, None)
    )
    .is_empty());
}

#[test]
fn a_city_replacement_reads_districts_like_the_structural_check() {
    let leaf = json!({"field": "city_code", "op": "in", "value": ["41460"]});
    let verdict = |who: ProfileInput| {
        evaluate_replacement(&who, &leaf, &crate::rule_engine::AgeBasis::today()).verdict
    };
    assert_eq!(
        verdict(person(25, json!({"city_code": "41463"}))),
        RuleVerdict::Pass
    );
    assert_eq!(verdict(person(25, json!({}))), RuleVerdict::Unknown);
    assert_eq!(
        verdict(person(25, json!({"city_code": "41110"}))),
        RuleVerdict::Fail
    );
    assert_eq!(
        verdict(person(
            25,
            json!({"region_code": "seoul", "city_code": "11110"})
        )),
        RuleVerdict::Fail,
        "no listed district in the user's sido"
    );
    // A 시 answer for a 구 programme cannot be told.
    let gu = json!({"field": "city_code", "op": "in", "value": ["41463"]});
    assert_eq!(
        evaluate_replacement(
            &person(25, json!({"city_code": "41460"})),
            &gu,
            &crate::rule_engine::AgeBasis::today()
        )
        .verdict,
        RuleVerdict::Unknown
    );
    // Any-group: a student outside the districts passes on the other branch.
    let either = json!({"any": [leaf, {"field": "enrollment_status", "op": "in",
                                       "value": ["enrolled", "on_leave"]}]});
    assert_eq!(
        evaluate_replacement(
            &person(
                25,
                json!({"city_code": "41110", "enrollment_status": "enrolled"})
            ),
            &either,
            &crate::rule_engine::AgeBasis::today()
        )
        .verdict,
        RuleVerdict::Pass
    );
    assert_eq!(
        evaluate_replacement(
            &person(25, json!({"city_code": "41110"})),
            &either,
            &crate::rule_engine::AgeBasis::today()
        )
        .verdict,
        RuleVerdict::Unknown
    );
}

#[test]
fn an_applied_override_is_never_verified_and_names_its_check() {
    let window = serde_json::to_value(Window {
        kind: WindowKind::Rolling,
        ..Window::unknown()
    })
    .unwrap();
    let review = ServeReview::from_columns(
        "complete",
        None,
        &window,
        Vec::new(),
        Some(chrono::Utc::now()),
        true,
    );
    let p = person(25, json!({}));
    let run = |checks: &[&str]| {
        assess(
            &p,
            &ServeFacts {
                review: Some(&review),
                applicant: Some(applicant_review::ApplicantClass::Individual),
                program_type: "policy",
                structural: &[],
                source_rules: &[],
                source_rule_error: false,
                override_checks: checks,
                override_exceptions: &[],
                age_basis: &crate::rule_engine::AgeBasis::today(),
                list_checks: &[],
                now: chrono::Utc::now(),
            },
        )
    };
    assert!(run(&[]).verified);
    let overridden = run(&["military_service"]);
    assert!(!overridden.verified);
    assert_eq!(overridden.hide, None);
    assert!(overridden.blockers.contains(&Blocker::CodeOverride));
    assert_eq!(overridden.remaining_checks[0].kind, "military_service");
    assert_eq!(
        overridden.remaining_checks[0].label,
        "병역 조건 확인이 필요해요"
    );

    // Malformed stored overrides apply nothing and withhold `verified`.
    let broken = review
        .clone()
        .with_code_overrides(Some(&json!([{"target": 1}])));
    assert!(broken.code_overrides.is_empty() && broken.rule_error);
}

/// Re-review L: an unresolved region scope ("unknown") is widened only to an
/// explicit region list; a drop, an all-but set or a group is rejected, and
/// a stored drop of it is never applied.
#[test]
fn an_unresolved_region_scope_is_widened_only_to_an_explicit_list() {
    let columns = CodeColumns {
        regions: Some(vec!["unknown".into()]),
        ..CodeColumns::default()
    };
    let check = |o: Value| {
        validate_with(
            &override_input(json!([o])),
            &veteran_payload(),
            true,
            &columns,
        )
        .map(|_| ())
    };
    let base = |extra: Value| {
        let mut o = json!({"target": "S-region", "check_kind": "other", "evidence": VETERAN_QUOTE});
        o.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        o
    };
    assert_eq!(
        check(base(json!({"action": "widen",
            "replacement": {"field": "region_code", "op": "in", "value": ["seoul", "busan"]}}))),
        Ok(())
    );
    for bad in [
        json!({"action": "drop"}),
        json!({"action": "widen",
               "replacement": {"field": "region_code", "op": "not_in", "value": ["jeju"]}}),
        json!({"action": "widen",
               "replacement": {"field": "region_code", "op": "ne", "value": "jeju"}}),
        json!({"action": "widen", "replacement": {"any": [
            {"field": "region_code", "op": "in", "value": ["seoul"]},
            {"field": "age", "op": "gte", "value": 0}]}}),
    ] {
        assert_eq!(
            check(base(bad.clone())),
            Err(Rejection::InvalidOverride),
            "{bad}"
        );
    }
    let stored_drop: CodeOverride =
        serde_json::from_value(base(json!({"action": "drop"}))).unwrap();
    assert!(applicable(std::slice::from_ref(&stored_drop), &columns).is_empty());
}

/// An override admits a profile only where the code rule it replaces would
/// hide it; a profile the code passes or leaves unknown only gains the
/// override's check. Admitted targets come first in the remaining checks.
#[test]
fn an_override_admits_only_profiles_the_code_rule_hides() {
    let p = programme();
    let job = json!({"all": [{"field": "employment_status", "op": "in",
        "value": ["unemployed", "preparing", "student"], "origin": "source_codes"}]});
    let columns = CodeColumns::of(&p, Some(&job));
    let parse = |v: Value| -> CodeOverride { serde_json::from_value(v).unwrap() };
    let veteran = parse(veteran_override());
    let district = parse(json!({"target": "S-district", "action": "widen",
        "replacement": {"any": [
            {"field": "city_code", "op": "in", "value": ["41461", "41463", "41465"]},
            {"field": "enrollment_status", "op": "in", "value": ["enrolled", "on_leave"]}]},
        "check_kind": "school_detail", "evidence": "x"}));
    let drop_job = parse(json!({"target": "C-job", "action": "drop",
        "check_kind": "employer_or_job_detail", "evidence": "x"}));
    let all = [&veteran, &district, &drop_job];
    assert_eq!(
        applicable(
            &[veteran.clone(), district.clone(), drop_job.clone()],
            &columns
        )
        .len(),
        3
    );
    let targets = |who: &ProfileInput| -> Vec<String> {
        admitting(
            &p,
            None,
            &structural(who),
            who,
            &all,
            &columns,
            crate::seoul_date(chrono::Utc::now()),
        )
        .iter()
        .map(|o| o.target.clone())
        .collect()
    };
    // Inside every code rule: nothing admitted.
    let inside = person(
        25,
        json!({"city_code": "41463", "employment_status": "unemployed"}),
    );
    assert!(targets(&inside).is_empty());
    // Unknown on the code rules (no city, no job): not admitted either.
    assert!(targets(&person(25, json!({}))).is_empty());
    // Each failed code rule is one admission.
    assert_eq!(
        targets(&person(
            37,
            json!({"city_code": "41110", "enrollment_status": "enrolled",
                                    "employment_status": "employed"})
        )),
        ["S-age", "S-district", "C-job"]
    );
    // An unresolved region scope hides everyone: a widening admits.
    let mut unresolved = p.clone();
    unresolved.regions = Some(vec!["unknown".into()]);
    unresolved.region_districts = None;
    let scope = parse(json!({"target": "S-region", "action": "widen",
        "replacement": {"field": "region_code", "op": "in", "value": ["gyeonggi"]},
        "check_kind": "other", "evidence": "x"}));
    assert_eq!(
        admitting(
            &unresolved,
            None,
            &structural(&inside),
            &inside,
            &[&scope],
            &CodeColumns::of(&unresolved, None),
            crate::seoul_date(chrono::Utc::now())
        )
        .len(),
        1
    );

    // The exception check leads, once per label (region and district share one).
    let window = serde_json::to_value(Window {
        kind: WindowKind::Rolling,
        ..Window::unknown()
    })
    .unwrap();
    let review = ServeReview::from_columns(
        "complete",
        None,
        &window,
        Vec::new(),
        Some(chrono::Utc::now()),
        true,
    );
    let assessment = assess(
        &inside,
        &ServeFacts {
            review: Some(&review),
            applicant: Some(applicant_review::ApplicantClass::Indirect),
            program_type: "policy",
            structural: &[],
            source_rules: &[],
            source_rule_error: false,
            override_checks: &["target_group_other"],
            override_exceptions: &["S-region", "S-district", "S-age"],
            age_basis: &crate::rule_engine::AgeBasis::today(),
            list_checks: &[],
            now: chrono::Utc::now(),
        },
    );
    let kinds: Vec<(&str, &str)> = assessment
        .remaining_checks
        .iter()
        .map(|c| (c.kind.as_str(), c.label.as_str()))
        .collect();
    assert_eq!(
        kinds[..3],
        [
            (
                EXCEPTION_KIND,
                "지역 조건 예외에 해당하는지 확인이 필요해요"
            ),
            (
                EXCEPTION_KIND,
                "연령 조건 예외에 해당하는지 확인이 필요해요"
            ),
            ("applicant_indirect", applicant_review::INDIRECT_NOTE),
        ]
    );
}

/// Review2-verify gap 4: the override paths read what the list reads. An
/// `S-age` replacement counts 만 나이 on the notice window from the caller's
/// day (a birthday inside the window is unknown, not a pass or a fail), and
/// an `S-district` replacement reads a retired saved code as unanswered,
/// never as a mismatch.
#[test]
fn override_paths_read_the_notice_window_and_current_districts() {
    use chrono::TimeZone;
    let day = |y: i32, m: u32, d: u32| chrono::NaiveDate::from_ymd_opt(y, m, d).unwrap();
    let kst = |y: i32, m: u32, d: u32| {
        chrono::FixedOffset::east_opt(9 * 3600)
            .unwrap()
            .with_ymd_and_hms(y, m, d, 0, 0, 0)
            .unwrap()
            .with_timezone(&chrono::Utc)
    };
    let mut p = programme();
    p.region_districts = None;
    p.application_start_at = Some(kst(2026, 11, 1));
    p.application_end_at = Some(kst(2026, 12, 1));
    let parse = |v: Value| -> CodeOverride { serde_json::from_value(v).unwrap() };
    let widen = parse(json!({"target": "S-age", "action": "widen",
        "replacement": {"field": "age", "op": "lte", "value": 34},
        "check_kind": "other", "evidence": "x"}));
    // Born 1991-11-15: 34 when the window opens, 35 before it closes.
    let boundary = person(0, json!({"birth_year": 1991, "birth_date": "1991-11-15"}));
    let today = day(2026, 11, 5);
    let (_, evals) = structural_gate(
        &p,
        None,
        &structural(&boundary),
        &boundary,
        &[&widen],
        today,
    )
    .expect("a birthday inside the window does not hide");
    assert_eq!(evals[0].verdict, RuleVerdict::Unknown);
    // Well inside the band all window long: a pass.
    let young = person(0, json!({"birth_year": 1996, "birth_date": "1996-06-01"}));
    let (_, evals) =
        structural_gate(&p, None, &structural(&young), &young, &[&widen], today).unwrap();
    assert_eq!(evals[0].verdict, RuleVerdict::Pass);

    let district = parse(json!({"target": "S-district", "action": "widen",
        "replacement": {"field": "city_code", "op": "in", "value": ["28125"]},
        "check_kind": "other", "evidence": "x"}));
    let stale = person(25, json!({"region_code": "incheon", "city_code": "28110"}));
    let mut incheon = crate::structural::test_program("policy");
    incheon.regions = Some(vec!["incheon".into()]);
    incheon.region_districts = Some(vec!["28125".into()]);
    let (_, evals) = structural_gate(
        &incheon,
        None,
        &structural(&stale),
        &stale,
        &[&district],
        today,
    )
    .expect("a retired saved code is no mismatch");
    assert_eq!(evals[0].verdict, RuleVerdict::Unknown);
}
