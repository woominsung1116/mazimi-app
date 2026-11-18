use super::store::{validate, Rejection, ReviewInput};
use super::*;
use crate::models::ProfileInput;
use crate::rule_engine::RuleVerdict;
use crate::structural::{CheckKind, StructuralCheck};
use chrono::TimeZone;
use serde_json::json;

fn payload() -> Value {
    json!({
        "plcyNm": "청년 월세 지원",
        "plcyExplnCn": "부산 거주 무주택 청년에게 월세 지원",
        "plcySprtCn": "월 20만원",
        "etcMttrCn": null,
        "addAplyQlfcCndCn": " 만 19~34세",
        "ptcpPrpTrgtCn": "",
        "plcyAplyMthdCn": "온라인 신청",
        "sbmsnDcmntCn": "임대차계약서",
        "srngMthdCn": "소득 심사",
        "sprtTrgtMinAge": "19",
        "sprtTrgtMaxAge": "34",
        "zipCd": "26410,26440",
        "jobCd": "0013003",
        "schoolCd": "0049010",
        "sbizCd": "0014010",
        "plcyMajorCd": "0011009",
        "mrgSttsCd": "0055003",
        "earnCndSeCd": "0043002",
        "earnMinAmt": "0",
        "earnMaxAmt": "5000",
        "earnEtcCn": "",
        "aplyPrdSeCd": "0057001",
        "aplyYmd": "20260101 ~ 20261231",
        "plcyNo": "R1",
        "lastMdfcnDt": "2026-10-15"
    })
}

/// Golden value: changing the field list, order, separator or encoding
/// invalidates every stored review, so it must be a deliberate change.
/// Recompute with segment.py's text_hash over YOUTH_TEXT + YOUTH_HASH_CODE:
/// python3 -c "import hashlib;print(hashlib.sha256('\x1f'.join(VALUES).encode()).hexdigest())"
#[test]
fn text_hash_is_stable() {
    assert_eq!(
        text_hash("youth_center", &payload()).unwrap(),
        "e5fdfddc08bf7e157c83cefb80ed0aa7ca9810af8a23ca08ae02463adb6cd231"
    );
    assert_eq!(
        text_hash("youth_center", &json!({})).unwrap(),
        "5c186c9552441f84cc151f29061cb10d0e185297da752d2e412d0e43a0676ca2"
    );
}

#[test]
fn hash_covers_text_and_codes_only() {
    let base = text_hash("youth_center", &payload());
    let mut other = payload();
    other["lastMdfcnDt"] = json!("2026-10-16");
    other["inqCnt"] = json!("99");
    assert_eq!(text_hash("youth_center", &other), base);
    for (field, value) in [
        ("aplyPrdSeCd", "0057002"),
        ("aplyYmd", ""),
        ("jobCd", "0013001"),
        ("zipCd", "26410"),
        ("earnMaxAmt", "4000"),
        ("addAplyQlfcCndCn", "만 19~39세"),
    ] {
        let mut changed = payload();
        changed[field] = json!(value);
        assert_ne!(text_hash("youth_center", &changed), base, "{field}");
    }
    // The applicant review reads a subset of the same text, so a current
    // condition hash implies a current applicant text.
    let hashed = hashed_fields("youth_center").unwrap();
    for field in crate::applicant_review::fields("youth_center").unwrap() {
        assert!(hashed.contains(field), "{field}");
    }
    assert_eq!(text_hash("gov_benefits", &payload()), None);
    let projection = hashed_projection_sql("p.");
    for field in hashed {
        assert!(projection.contains(&format!("'{field}', p.raw_payload->'{field}'")));
    }
}

#[test]
fn evidence_is_a_quote_of_one_text_field_or_a_current_code() {
    let mut p = payload();
    p["etcMttrCn"] = json!("문의: 청년정책과 051-888-1234 (평일)\n신청 문의는 담당자 홍길동");
    assert!(evidence_found("youth_center", &p, "부산 거주 무주택 청년"));
    assert!(evidence_found("youth_center", &p, "  임대차계약서 "));
    // Masked quotes as the curators saw them.
    assert!(evidence_found(
        "youth_center",
        &p,
        "문의: 청년정책과 [전화번호 생략] (평일)"
    ));
    assert!(evidence_found(
        "youth_center",
        &p,
        "신청 문의는 담당자 [이름 생략]"
    ));
    assert!(
        !evidence_found("youth_center", &p, "담당자 [이름 생략]"),
        "too short"
    );
    assert!(!evidence_found(
        "youth_center",
        &p,
        "신청 문의는 담당자 홍길동[이름 생략]"
    ));
    // A mask cannot stand for a line break, nothing, or text that differs.
    assert!(!evidence_found(
        "youth_center",
        &p,
        "(평일)[전화번호 생략]신청 문의는"
    ));
    assert!(!evidence_found(
        "youth_center",
        &p,
        "문의: 다른과 [전화번호 생략] (평일)"
    ));
    // Code segments must equal the current codes.
    assert!(evidence_found("youth_center", &p, "code:jobCd=0013003"));
    assert!(evidence_found(
        "youth_center",
        &p,
        "code:earnCndSeCd=0043002;earnMaxAmt=5000"
    ));
    assert!(!evidence_found("youth_center", &p, "code:jobCd=0013001"));
    assert!(!evidence_found(
        "youth_center",
        &p,
        "code:plcyNm=청년 월세 지원"
    ));
    // Paraphrase, across fields, too short.
    assert!(!evidence_found("youth_center", &p, "무주택 부산 청년"));
    assert!(!evidence_found("youth_center", &p, "월 20만원\u{1f}만 19"));
    assert!(!evidence_found("youth_center", &p, "월세 지"));
    assert!(!evidence_found("youth_center", &p, "[이름 생략]"));
}

fn at(date: &str) -> DateTime<Utc> {
    let d = NaiveDate::parse_from_str(date, "%Y-%m-%d").unwrap();
    // 12:00 KST.
    Utc.from_utc_datetime(&d.and_hms_opt(3, 0, 0).unwrap())
}

fn window(kind: WindowKind, opens: Option<&str>, closes: Option<&str>) -> Window {
    let date = |d: Option<&str>| d.map(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").unwrap());
    Window {
        kind,
        opens_at: date(opens),
        closes_at: date(closes),
        recurrence: None,
        evidence: None,
        seg_id: None,
        source: Some("review".into()),
        reviewed: None,
    }
}

#[test]
fn window_state_is_open_only_on_reviewed_dates_or_a_fresh_confirmation() {
    use WindowKind::*;
    use WindowState::*;
    let dated = window(Dated, Some("2026-10-15"), Some("2026-10-31"));
    assert_eq!(window_state(&dated, None, at("2026-10-15")), Open);
    assert_eq!(window_state(&dated, None, at("2026-10-31")), Open);
    assert_eq!(window_state(&dated, None, at("2026-11-01")), NotOpen);
    assert_eq!(window_state(&dated, None, at("2026-10-14")), Unverified);
    // 23:30 KST on the closing day is still that day.
    let late = Utc.with_ymd_and_hms(2026, 10, 31, 14, 30, 0).unwrap();
    assert_eq!(window_state(&dated, None, late), Open);

    let rolling = window(Rolling, None, None);
    let now = at("2026-10-15");
    assert_eq!(window_state(&rolling, None, now), Unverified);
    assert_eq!(
        window_state(&rolling, Some(now - chrono::Duration::days(6)), now),
        Open
    );
    assert_eq!(
        window_state(&rolling, Some(now - chrono::Duration::days(7)), now),
        Open
    );
    assert_eq!(
        window_state(
            &rolling,
            Some(now - chrono::Duration::days(7) - chrono::Duration::minutes(1)),
            now
        ),
        Unverified,
        "rolling past 7 days"
    );
    assert_eq!(
        window_state(&rolling, Some(now + chrono::Duration::hours(1)), now),
        Unverified,
        "a confirmation from the future is not trusted"
    );
    for kind in [Recurring, Unknown, Event] {
        assert_eq!(
            window_state(&window(kind, None, None), Some(now), now),
            Unverified
        );
    }
    let event = window(Event, Some("2026-10-20"), Some("2026-10-20"));
    assert_eq!(window_state(&event, None, at("2026-10-15")), Unverified);
    assert_eq!(window_state(&event, None, at("2026-10-21")), NotOpen);
    assert_eq!(
        window_state(&window(Closed, None, None), None, now),
        NotOpen
    );
}

// ── assess ──────────────────────────────────────────────────────────────

fn profile(extra: Value) -> ProfileInput {
    let mut base = json!({"birth_year": 2000, "region_code": "busan",
        "employment_status": "unemployed"});
    base.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    serde_json::from_value(base).unwrap()
}

fn review(rule: Option<Value>) -> ServeReview {
    let window = window(WindowKind::Dated, Some("2026-10-01"), Some("2026-10-31"));
    ServeReview::from_columns(
        "complete",
        rule.as_ref(),
        &serde_json::to_value(window).unwrap(),
        Vec::new(),
        None,
        true,
    )
}

fn employment_rule() -> Value {
    json!({"all": [{"id": "R1", "field": "employment_status", "op": "in",
        "value": ["unemployed", "preparing"], "origin": "review", "evidence": "미취업 청년"}]})
}

fn pass_check(kind: CheckKind) -> StructuralCheck {
    StructuralCheck {
        kind,
        status: RuleVerdict::Pass,
        missing_field: None,
        unresolved_scope: false,
    }
}

struct Case {
    review: Option<ServeReview>,
    applicant: Option<ApplicantClass>,
    program_type: &'static str,
    structural: Vec<StructuralCheck>,
    source: Vec<RuleEvalResult>,
    now: DateTime<Utc>,
}

impl Case {
    fn good() -> Self {
        Self {
            review: Some(review(Some(employment_rule()))),
            applicant: Some(ApplicantClass::Individual),
            program_type: "policy",
            structural: vec![pass_check(CheckKind::Age), pass_check(CheckKind::Region)],
            source: Vec::new(),
            now: at("2026-10-15"),
        }
    }

    fn run(&self, p: &ProfileInput) -> Assessment {
        let source: Vec<&RuleEvalResult> = self.source.iter().collect();
        assess(
            p,
            &ServeFacts {
                review: self.review.as_ref(),
                applicant: self.applicant,
                program_type: self.program_type,
                structural: &self.structural,
                source_rules: &source,
                source_rule_error: false,
                override_checks: &[],
                override_exceptions: &[],
                age_basis: &crate::rule_engine::AgeBasis::today(),
                list_checks: &[],
                now: self.now,
            },
        )
    }
}

#[test]
fn verified_only_when_every_clause_holds() {
    let p = profile(json!({}));
    let good = Case::good().run(&p);
    assert!(good.verified, "{:?}", good.blockers);
    assert!(good.remaining_checks.is_empty());
    assert_eq!(good.hide, None);
    assert!(good.window.as_ref().unwrap().open_now);
    // Explain trace: the reviewed leaf is recorded as matched.
    assert_eq!(
        good.reviewed_eval.as_ref().unwrap().matched_rules[0].field,
        "employment_status"
    );

    let not_verified = |case: Case, expect: &str| {
        let result = case.run(&p);
        assert!(!result.verified, "{expect}: {:?}", result.blockers);
        assert_eq!(result.hide, None, "{expect} never hides");
        assert!(
            result.remaining_checks.iter().any(|c| c.kind == expect),
            "{expect}: {:?}",
            result.remaining_checks
        );
    };

    // 1. No review, a stale hash, an incomplete or conflicting review.
    not_verified(
        Case {
            review: None,
            ..Case::good()
        },
        "conditions_unreviewed",
    );
    let mut stale = review(Some(employment_rule()));
    stale.current = false;
    not_verified(
        Case {
            review: Some(stale),
            ..Case::good()
        },
        "conditions_changed",
    );
    let incomplete = ServeReview::from_columns(
        "incomplete",
        Some(&employment_rule()),
        &serde_json::to_value(window(
            WindowKind::Dated,
            Some("2026-10-01"),
            Some("2026-10-31"),
        ))
        .unwrap(),
        vec!["duplicate_benefit".into()],
        None,
        true,
    );
    not_verified(
        Case {
            review: Some(incomplete),
            ..Case::good()
        },
        "duplicate_benefit",
    );
    let mut conflict = review(Some(employment_rule()));
    conflict.status = ReviewStatus::Conflict;
    not_verified(
        Case {
            review: Some(conflict),
            ..Case::good()
        },
        "conflict",
    );
    let broken = ServeReview::from_columns(
        "complete",
        Some(&json!({"all": [{"field": "shoe_size", "op": "eq", "value": 1}]})),
        &json!({"kind": "dated", "opens_at": "2026-10-01", "closes_at": "2026-10-31"}),
        Vec::new(),
        None,
        true,
    );
    not_verified(
        Case {
            review: Some(broken),
            ..Case::good()
        },
        "rule_unavailable",
    );

    // 2. Applicant: indirect, unclear or missing.
    not_verified(
        Case {
            applicant: Some(ApplicantClass::Indirect),
            ..Case::good()
        },
        "applicant_indirect",
    );
    for applicant in [Some(ApplicantClass::Unclear), None] {
        not_verified(
            Case {
                applicant,
                ..Case::good()
            },
            "applicant",
        );
    }

    // 3. Window: rolling past 7 days, recurring, not yet open.
    let mut rolling = review(Some(employment_rule()));
    rolling.window = window(WindowKind::Rolling, None, None);
    rolling.window_confirmed_at = Some(at("2026-10-07"));
    not_verified(
        Case {
            review: Some(rolling.clone()),
            ..Case::good()
        },
        "window",
    );
    rolling.window_confirmed_at = Some(at("2026-10-09"));
    assert!(
        Case {
            review: Some(rolling),
            ..Case::good()
        }
        .run(&p)
        .verified
    );
    let mut recurring = review(Some(employment_rule()));
    recurring.window = window(WindowKind::Recurring, None, None);
    not_verified(
        Case {
            review: Some(recurring),
            ..Case::good()
        },
        "window",
    );

    // 4. An unknown leaf: the reviewed rule asks what the profile left out.
    let unknown = profile(json!({"employment_status": null}));
    let result = Case::good().run(&unknown);
    assert!(!result.verified);
    assert_eq!(result.hide, None);
    let question = result
        .remaining_checks
        .iter()
        .find(|c| c.kind == "profile")
        .unwrap();
    assert_eq!(question.question_id.as_deref(), Some("employment_status"));
    assert_eq!(question.label, "취업 상태 확인 필요");

    // Boundary age: the structural age check is Unknown.
    let mut boundary = Case::good();
    boundary.structural[0] = StructuralCheck {
        kind: CheckKind::Age,
        status: RuleVerdict::Unknown,
        missing_field: Some("birth_year"),
        unresolved_scope: false,
    };
    not_verified(boundary, "profile");

    // A source-code rule with an unknown leaf.
    let source_unknown = rule_engine::evaluate(
        &p,
        &rule_engine::parse_rule_json(&json!({"field": "income_bracket", "op": "lte", "value": 5}))
            .unwrap(),
        &crate::rule_engine::AgeBasis::today(),
    );
    not_verified(
        Case {
            source: vec![source_unknown],
            ..Case::good()
        },
        "profile",
    );

    // 5. Corporate benefits are never verified.
    not_verified(
        Case {
            program_type: "corporate_benefit",
            ..Case::good()
        },
        "provider_terms",
    );
}

#[test]
fn only_reviewed_facts_of_a_current_complete_review_hide() {
    let p = profile(json!({"employment_status": "employed"}));
    let fail = Case::good().run(&p);
    assert!(!fail.verified);
    assert_eq!(fail.hide, Some(ExclusionReason::HardRule));
    assert_eq!(
        fail.reviewed_eval.as_ref().unwrap().failed_rules[0].field,
        "employment_status"
    );

    let past = Case {
        now: at("2026-11-01"),
        ..Case::good()
    }
    .run(&profile(json!({})));
    assert_eq!(past.hide, Some(ExclusionReason::NotOpen));
    let mut closed = review(None);
    closed.window = window(WindowKind::Closed, None, None);
    assert_eq!(
        Case {
            review: Some(closed),
            ..Case::good()
        }
        .run(&profile(json!({})))
        .hide,
        Some(ExclusionReason::NotOpen)
    );

    // The same facts in a stale, incomplete or conflicting review hide nothing.
    for (status, current) in [
        ("complete", false),
        ("incomplete", true),
        ("conflict", true),
    ] {
        let r = ServeReview::from_columns(
            status,
            Some(&employment_rule()),
            &json!({"kind": "closed"}),
            Vec::new(),
            None,
            current,
        );
        let result = Case {
            review: Some(r),
            ..Case::good()
        }
        .run(&p);
        assert_eq!(result.hide, None, "{status} current={current}");
        assert!(!result.verified);
    }
    // Missing data never hides.
    let result = Case {
        review: None,
        applicant: None,
        ..Case::good()
    }
    .run(&profile(json!({"employment_status": null})));
    assert_eq!(result.hide, None);
}

#[test]
fn remaining_checks_use_fixed_labels_in_order() {
    let p = profile(json!({"employment_status": null}));
    let incomplete = ServeReview::from_columns(
        "incomplete",
        Some(&employment_rule()),
        &json!({"kind": "recurring", "recurrence": "매년 7~8월"}),
        vec!["housing_detail".into(), "other".into()],
        None,
        true,
    );
    let result = Case {
        review: Some(incomplete),
        applicant: None,
        ..Case::good()
    }
    .run(&p);
    let kinds: Vec<&str> = result
        .remaining_checks
        .iter()
        .map(|c| c.kind.as_str())
        .collect();
    assert_eq!(
        kinds,
        ["applicant", "window", "profile", "housing_detail", "other"]
    );
    for check in &result.remaining_checks {
        assert!(!check.label.contains("7~8월"), "no curator text served");
    }
    assert_eq!(
        result.remaining_checks[3].label,
        "주택·임대차 조건 확인이 필요해요"
    );
    // The window view carries no curator text either.
    let view = serde_json::to_value(result.window.unwrap()).unwrap();
    assert_eq!(
        view,
        json!({"kind": "recurring", "opens_at": null, "closes_at": null,
               "confirmed_at": null, "open_now": false})
    );
}

// ── validate ────────────────────────────────────────────────────────────

/// A coverage map labelling every segment of [`payload`]'s text (the text
/// fields every test payload shares) with `label`.
fn labelled(label: Value) -> Value {
    let labels: serde_json::Map<String, Value> =
        super::coverage::segments("youth_center", &payload())
            .unwrap()
            .into_iter()
            .map(|segment| (segment.seg_id, label.clone()))
            .collect();
    json!({ "labels": labels })
}

fn input(extra: Value) -> ReviewInput {
    let mut base = json!({
        "source_type": "youth_center", "source_id": "R1",
        "text_hash": text_hash("youth_center", &payload()).unwrap(),
        "status": "complete",
        "rules": [{"id": "R1", "field": "housing_type", "op": "in", "value": ["monthly_rent"],
                   "seg_id": "0123456789ab", "evidence": "무주택 청년에게 월세 지원"}],
        "code_rules": [
            {"id": "C-job", "field": "employment_status", "op": "in",
             "value": ["unemployed", "preparing", "student"], "origin": "source_codes",
             "from": ["jobCd"]},
            {"id": "S-age", "field": "age", "op": "between", "value": [19, 34], "origin": "structural"}
        ],
        "window": {"kind": "dated", "opens_at": "2026-01-01", "closes_at": "2026-12-31",
                   "source": "code"},
        "remaining_checks": [],
        "coverage": labelled(json!({"label": "non_condition"})),
        "reviewer": "curator:A+B"
    });
    base.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    serde_json::from_value(base).unwrap()
}

#[test]
fn validate_accepts_a_pilot_shaped_review() {
    let valid = validate(&input(json!({})), &payload(), true).unwrap();
    assert_eq!(valid.status, ReviewStatus::Complete);
    let rule = valid.rule_json.unwrap();
    let all = rule["all"].as_array().unwrap();
    assert_eq!(all.len(), 2, "structural leaves are re-derived, not stored");
    assert_eq!(all[0]["origin"], "source_codes");
    assert_eq!(all[1]["origin"], "review");
    assert_eq!(all[1]["evidence"], "무주택 청년에게 월세 지원");
    rule_engine::parse_rule_json(&rule).unwrap();
    assert_eq!(
        valid.evidence,
        json!([{"for": "rule:R1", "quote": "무주택 청년에게 월세 지원"}])
    );
    // No rule at all is stored as NULL (an incomplete set; a complete one
    // needs a restrictive rule, below).
    let none = validate(
        &input(json!({"rules": [], "code_rules": [], "status": "incomplete"})),
        &payload(),
        true,
    )
    .unwrap();
    assert_eq!(none.rule_json, None);
}

/// Audit H1: only the coverage check decides `complete`. A claim it does
/// not back is rejected: no coverage, a segment without a label, an
/// uncheckable segment, a condition naming a rule the review lacks, or no
/// restrictive rule at all. The same set claimed incomplete is stored.
#[test]
fn a_complete_claim_the_coverage_does_not_back_is_rejected() {
    let reject = |extra: Value| validate(&input(extra), &payload(), true).unwrap_err();
    for extra in [
        json!({"rules": [], "code_rules": [], "coverage": null}),
        json!({"coverage": null}),
        json!({"coverage": {"labels": {}}}),
        json!({"coverage": labelled(json!({"label": "uncheckable", "check_kind": "housing_detail"}))}),
        json!({"coverage": labelled(json!({"label": "condition", "rule_refs": ["R9"]}))}),
        json!({"coverage": labelled(json!({"label": "condition"}))}),
        json!({"coverage": labelled(json!({"label": "maybe"}))}),
        json!({"rules": [], "code_rules": []}),
    ] {
        assert_eq!(
            reject(extra.clone()),
            Rejection::CoverageIncomplete,
            "{extra}"
        );
        let mut incomplete = extra.clone();
        incomplete["status"] = json!("incomplete");
        let valid = validate(&input(incomplete), &payload(), true).unwrap();
        assert_eq!(valid.status, ReviewStatus::Incomplete, "{extra}");
    }
    // Conditions backed by the review's own rule, a code rule or a
    // structural rule the programme has (its age band): complete.
    let aged = super::overrides::CodeColumns {
        min_age: Some(19),
        max_age: Some(34),
        ..Default::default()
    };
    for refs in [json!(["R1"]), json!(["C-job"]), json!(["S-age", "R1"])] {
        let line =
            input(json!({"coverage": labelled(json!({"label": "condition", "rule_refs": refs}))}));
        assert_eq!(
            super::store::validate_with(&line, &payload(), true, &aged)
                .unwrap()
                .status,
            ReviewStatus::Complete
        );
    }
}

/// Review B7: a structural rule backs a condition sentence only when the
/// programme has that rule now (its columns), never because the input names
/// one. A made-up id, or `S-age` on a programme with no age band, backs
/// nothing, so `complete` is rejected.
#[test]
fn a_structural_reference_backs_a_condition_only_when_the_programme_has_it() {
    let claim = |id: &str| {
        input(json!({
            "rules": [],
            "code_rules": [{"id": id, "origin": "structural"}],
            "coverage": labelled(json!({"label": "condition", "rule_refs": [id]}))
        }))
    };
    let none = super::overrides::CodeColumns::default();
    for id in ["S-made-up", "S-age"] {
        assert_eq!(
            super::store::validate_with(&claim(id), &payload(), true, &none).unwrap_err(),
            Rejection::CoverageIncomplete,
            "{id}"
        );
    }
    let aged = super::overrides::CodeColumns {
        min_age: Some(19),
        max_age: Some(34),
        ..Default::default()
    };
    assert_eq!(
        super::store::validate_with(&claim("S-age"), &payload(), true, &aged)
            .unwrap()
            .status,
        ReviewStatus::Complete
    );
}

#[test]
fn validate_rejects_stale_text_bad_rules_non_quotes_and_bad_enums() {
    let reject = |extra: Value| validate(&input(extra), &payload(), true).unwrap_err();
    assert_eq!(
        reject(json!({"text_hash": "0".repeat(64)})),
        Rejection::TextChanged
    );
    assert_eq!(
        validate(&input(json!({"text_hash": null})), &payload(), true).unwrap_err(),
        Rejection::MissingTextHash
    );
    for rules in [
        json!([{"id": "R1", "field": "marital_status", "op": "in", "value": ["single"],
                "evidence": "무주택 청년에게 월세 지원"}]),
        json!([{"id": "R1", "field": "housing_type", "op": "contains", "value": "rent",
                "evidence": "무주택 청년에게 월세 지원"}]),
        json!([{"id": "R1", "field": "region_code", "op": "in", "value": ["Busan"],
                "evidence": "무주택 청년에게 월세 지원"}]),
        json!([{"id": "R1", "field": "age", "op": "between", "value": [34, 19],
                "evidence": "무주택 청년에게 월세 지원"}]),
        json!([{"id": "R1", "field": "has_disability", "op": "eq", "value": true,
                "evidence": "무주택 청년에게 월세 지원"}]),
        json!([{"field": "housing_type", "op": "eq", "value": "jeonse",
                "evidence": "무주택 청년에게 월세 지원"}]),
        json!([{"id": "R1", "any": [{"field": "housing_type", "op": "eq", "value": "jeonse"}],
                "evidence": "무주택 청년에게 월세 지원"}]),
        json!([{"id": "C-job", "field": "housing_type", "op": "eq", "value": "jeonse",
                "evidence": "무주택 청년에게 월세 지원"}]),
    ] {
        assert_eq!(
            reject(json!({"rules": rules})),
            Rejection::InvalidRule,
            "{rules}"
        );
    }
    assert_eq!(
        reject(
            json!({"code_rules": [{"id": "C-x", "field": "is_farmer", "op": "is_true",
                                      "origin": "source_codes"}]})
        ),
        Rejection::InvalidRule
    );
    assert_eq!(
        reject(
            json!({"rules": [{"id": "R1", "field": "housing_type", "op": "eq",
            "value": "monthly_rent", "evidence": "월세 사는 청년"}]})
        ),
        Rejection::EvidenceNotFound
    );
    assert_eq!(
        reject(json!({"window": {"kind": "rolling", "evidence": "상시 모집합니다"}})),
        Rejection::EvidenceNotFound
    );
    assert_eq!(
        reject(json!({"status": "verified"})),
        Rejection::UnknownStatus
    );
    assert_eq!(
        reject(json!({"status": "incomplete", "remaining_checks": ["shoe_size"]})),
        Rejection::UnknownCheckKind
    );
    assert_eq!(
        reject(json!({"remaining_checks": ["assets"]})),
        Rejection::InconsistentStatus
    );
    for window in [
        json!({"kind": "soon"}),
        json!({"kind": "dated", "opens_at": "2026-01-01"}),
        json!({"kind": "dated", "opens_at": "2026-12-31", "closes_at": "2026-01-01"}),
        json!({"kind": "dated", "opens_at": "2026-13-01", "closes_at": "2026-12-31"}),
        json!({"kind": "rolling", "source": "official_recheck"}),
    ] {
        assert_eq!(
            reject(json!({"window": window})),
            Rejection::InvalidWindow,
            "{window}"
        );
    }
    assert_eq!(reject(json!({"coverage": [1]})), Rejection::InvalidCoverage);
    assert_eq!(reject(json!({"reviewer": " "})), Rejection::InvalidReviewer);
    assert_eq!(
        reject(json!({"source_type": "gov_benefits"})),
        Rejection::UnsupportedSource
    );
}

#[test]
fn pilot_converter_maps_records_and_skips_what_cannot_import() {
    let good = json!({
        "program_id": "p-good", "source_type": "youth_center", "source_id": "S1",
        "text_hash": "a".repeat(64), "agreement": "agreed", "chosen": "A",
        "status": "complete", "reasons": [], "conflict_fields": [], "remaining_checks": [],
        "uncheckable_kinds": [], "labels": {"x": {"label": "non_condition"}},
        "code_rules": [{"id": "S-age", "field": "age", "op": "between", "value": [19, 39],
                        "origin": "structural"}],
        "rules": [], "window": {"kind": "dated", "opens_at": "2026-01-01",
                                "closes_at": "2026-12-31", "source": "code"},
        "applicant": {"class": "individual", "seg_id": "x", "evidence": "홈페이지에서 신청"}
    });
    let farmer = json!({"program_id": "p-farm", "source_type": "youth_center", "source_id": "S2",
        "status": "complete", "rules": [],
        "code_rules": [{"id": "C-farm", "field": "is_farmer", "op": "is_true", "origin": "source_codes"}],
        "applicant": {"class": "individual", "evidence": "홈페이지에서 신청"}});
    let gov = json!({"program_id": "p-gov", "source_type": "gov_benefits", "source_id": "G1",
        "status": "complete"});
    let masked = json!({"program_id": "p-mask", "source_type": "youth_center", "source_id": "S3",
        "text_hash": "b".repeat(64), "status": "incomplete", "rules": [],
        "remaining_checks": ["other"], "window": {"kind": "unknown"},
        "applicant": {"class": "individual", "evidence": "문의 [전화번호 생략] 로 신청"}});
    let text = [good, farmer, gov, masked, json!("x")]
        .map(|v| v.to_string())
        .join("\n");
    let hashes: HashMap<String, String> = [
        ("p-good".to_owned(), "c".repeat(64)),
        ("p-mask".to_owned(), "d".repeat(64)),
    ]
    .into();
    let out = pilot::convert(&text, "pilot:verified-p0", &hashes);
    assert_eq!(out.conditions.len(), 2);
    assert_eq!(
        out.conditions[0]["reviewer"],
        "pilot:verified-p0 (agreed/A)"
    );
    assert_eq!(out.conditions[0]["coverage"]["agreement"], "agreed");
    assert_eq!(
        out.conditions[0]["coverage"]["labels"]["x"]["label"],
        "non_condition"
    );
    assert_eq!(out.conditions[1]["reviewer"], "pilot:verified-p0 (/none)");
    let skipped: Vec<&str> = out.skipped.iter().map(|(_, r)| r.as_str()).collect();
    assert_eq!(
        skipped,
        [
            "unsupported_field:is_farmer",
            "unsupported_source:gov_benefits",
            "invalid_json"
        ]
    );
    assert_eq!(out.applicants.len(), 1);
    assert_eq!(out.applicants[0]["text_hash"], "c".repeat(64));
    assert_eq!(
        out.applicant_skipped,
        [("p-mask".to_owned(), "masked_evidence".to_owned())]
    );
    // The converted line is a valid import line shape.
    let line: ReviewInput = serde_json::from_value(out.conditions[0].clone()).unwrap();
    assert_eq!(line.status, "complete");
}

use std::collections::HashMap;

/// Review H1: a source-code leaf copied into a review withholds `verified`
/// but never hides; only a quoted reviewed leaf does.
#[test]
fn code_leaves_withhold_verified_but_never_hide() {
    let code_only = json!({"all": [{"id": "C-job", "field": "employment_status", "op": "in",
        "value": ["unemployed"], "origin": "source_codes"}]});
    let r = ServeReview::from_columns(
        "complete",
        Some(&code_only),
        &json!({"kind": "dated", "opens_at": "2026-10-01", "closes_at": "2026-10-31"}),
        Vec::new(),
        None,
        true,
    );
    assert!(r.rule.is_none() && r.code_rule.is_some());
    let employed = profile(json!({"employment_status": "employed"}));
    let result = Case {
        review: Some(r.clone()),
        ..Case::good()
    }
    .run(&employed);
    assert!(!result.verified);
    assert_eq!(result.hide, None, "a code leaf never hides");
    assert!(result
        .remaining_checks
        .iter()
        .any(|c| c.kind == "rule_mismatch"));
    assert!(
        Case {
            review: Some(r),
            ..Case::good()
        }
        .run(&profile(json!({})))
        .verified
    );
}

/// Review SecM2: a window that can open or hide a row rests on proof.
#[test]
fn windows_rest_on_current_codes_or_a_quote() {
    let reject = |extra: Value| validate(&input(extra), &payload(), true).unwrap_err();
    // The code window must be the current aplyYmd / aplyPrdSeCd.
    assert_eq!(
        reject(json!({"window": {"kind": "dated", "opens_at": "2026-01-01",
                                 "closes_at": "2026-11-30", "source": "code"}})),
        Rejection::InvalidWindow
    );
    assert_eq!(
        reject(json!({"window": {"kind": "closed", "source": "code"}})),
        Rejection::InvalidWindow
    );
    let mut closed = payload();
    closed["aplyPrdSeCd"] = json!("0057003");
    let line = input(json!({
        "text_hash": text_hash("youth_center", &closed).unwrap(),
        "window": {"kind": "closed", "source": "code"}
    }));
    assert!(validate(&line, &closed, true).is_ok());
    // A reviewed window other than unknown needs a quote.
    for kind in ["closed", "rolling", "recurring", "event"] {
        assert_eq!(
            reject(json!({"window": {"kind": kind, "source": "review"}})),
            Rejection::EvidenceNotFound,
            "{kind}"
        );
    }
    assert!(validate(
        &input(json!({"window": {"kind": "event", "opens_at": "2026-10-20",
            "closes_at": "2026-10-20", "evidence": "온라인 신청"}})),
        &payload(),
        true
    )
    .is_ok());
    assert!(validate(
        &input(json!({"window": {"kind": "unknown"}})),
        &payload(),
        true
    )
    .is_ok());
}

#[test]
fn code_leaves_name_present_codes_and_lists_are_bounded() {
    let reject = |extra: Value| validate(&input(extra), &payload(), true).unwrap_err();
    for code_rules in [
        json!([{"id": "C-job", "field": "employment_status", "op": "in",
                "value": ["unemployed"], "origin": "source_codes"}]),
        json!([{"id": "C-job", "field": "employment_status", "op": "in",
                "value": ["unemployed"], "origin": "source_codes", "from": ["plcyNm"]}]),
        json!([{"id": "C-edu", "field": "employment_status", "op": "in",
                "value": ["unemployed"], "origin": "source_codes", "from": ["schoolCd2"]}]),
        json!([{"id": "C-x", "field": "employment_status", "op": "in",
                "value": ["unemployed"], "origin": "source_codes", "from": ["earnEtcCn"]}]),
    ] {
        assert_eq!(
            reject(json!({"code_rules": code_rules})),
            Rejection::InvalidRule,
            "{code_rules}"
        );
    }
    let many: Vec<Value> = (0..33)
        .map(|_| json!({"field": "housing_type", "op": "eq", "value": "jeonse"}))
        .collect();
    assert_eq!(
        reject(
            json!({"rules": [{"id": "R1", "any": many, "evidence": "무주택 청년에게 월세 지원"}]})
        ),
        Rejection::InvalidRule
    );
    let codes: Vec<String> = (0..65).map(|i| format!("{:05}", 26000 + i)).collect();
    assert_eq!(
        reject(
            json!({"rules": [{"id": "R1", "field": "city_code", "op": "in", "value": codes,
                                  "evidence": "무주택 청년에게 월세 지원"}]})
        ),
        Rejection::InvalidRule
    );
}

/// Review M2: a mask stands only for text of the kind it replaced, never
/// for a condition.
#[test]
fn a_mask_never_stands_for_a_condition() {
    let mut p = payload();
    p["etcMttrCn"] =
        json!("만 19세 이상 39세 이하 청년\n문의처 담당자 김민수 주무관 연락처 051-888-1234");
    for quote in [
        "만 19세 [이름 생략] 이하 청년",
        "만 19세 [전화번호 생략] 이하 청년",
        "만 19세 이상[전화번호 생략]세 이하 청년",
    ] {
        assert!(!evidence_found("youth_center", &p, quote), "{quote}");
    }
    assert!(evidence_found(
        "youth_center",
        &p,
        "문의처 담당자 [이름 생략] 주무관 연락처 [전화번호 생략]"
    ));
    // A name mask only follows "담당자 " / "담당 ", where the mask put one.
    assert!(!evidence_found(
        "youth_center",
        &p,
        "[이름 생략] 주무관 연락처 051-888-1234"
    ));
    assert!(!evidence_found(
        "youth_center",
        &p,
        "만 19세 [이름 생략] 39세 이하"
    ));
    // A leading mask must border text it could stand for.
    assert!(!evidence_found(
        "youth_center",
        &p,
        "[전화번호 생략] 이하 청년"
    ));
}

/// A `severity: label` source-code leaf (a loose provider flag) never hides;
/// contradicted it reads as Unknown, and Unknown withholds `verified`.
#[test]
fn label_code_leaves_withhold_verified_when_unknown() {
    let line = input(
        json!({"code_rules": [{"id": "C-job", "field": "employment_status",
        "op": "in", "value": ["unemployed"], "origin": "source_codes", "from": ["jobCd"],
        "severity": "label"}]}),
    );
    let valid = validate(&line, &payload(), true).unwrap();
    assert_eq!(
        valid.rule_json.as_ref().unwrap()["all"][0]["severity"],
        "label"
    );
    let review = ServeReview::from_columns(
        "complete",
        valid.rule_json.as_ref(),
        &json!({"kind": "dated", "opens_at": "2026-10-01", "closes_at": "2026-10-31"}),
        Vec::new(),
        None,
        true,
    );
    let case = Case {
        review: Some(review),
        ..Case::good()
    };
    // The reviewed rule (housing_type) is answered; the label leaf decides.
    let renter =
        |job: &str| profile(json!({"employment_status": job, "housing_type": "monthly_rent"}));
    let contradicted = case.run(&renter("employed"));
    assert!(!contradicted.verified);
    assert_eq!(contradicted.hide, None, "a label leaf never hides");
    // The answered field is named as a check, never asked again: the same
    // answer cannot change the result (verify V-M1).
    assert!(contradicted.remaining_checks.iter().any(|c| c
        .label
        .contains(rule_engine::field_label("employment_status"))
        && c.question_id.is_none()));
    assert!(case.run(&renter("unemployed")).verified);
    assert_eq!(
        validate(
            &input(
                json!({"code_rules": [{"id": "C-job", "field": "employment_status",
                "op": "in", "value": ["unemployed"], "origin": "source_codes",
                "from": ["jobCd"], "severity": "soft"}]})
            ),
            &payload(),
            true
        )
        .unwrap_err(),
        Rejection::InvalidRule
    );
}

#[test]
fn derived_education_fields_are_review_fields() {
    for (field, good, bad) in [
        ("education_stage", "univ_expected", "college_grad"),
        ("major_category", "engineering", "medical"),
    ] {
        let rule = |value: &str| {
            input(
                json!({"rules": [{"id": "R1", "field": field, "op": "in", "value": [value],
                "evidence": "무주택 청년에게 월세 지원"}]}),
            )
        };
        assert!(validate(&rule(good), &payload(), true).is_ok(), "{field}");
        assert_eq!(
            validate(&rule(bad), &payload(), true).unwrap_err(),
            Rejection::InvalidRule,
            "{field}"
        );
    }
}

/// Review R-M7 (replaces merge review M1): a review states its set
/// exactly. The widening of `education::admitted_education_stages`
/// compensates for loose source codes and stays in the worker's code
/// compiler; a reviewer who read "졸업자만" or "대학생·대학원생" writes that set.
#[test]
fn reviewed_education_leaves_keep_the_set_the_reviewer_wrote() {
    let line = input(
        json!({"rules": [{"id": "R1", "field": "education_stage", "op": "in",
        "value": ["univ_enrolled"], "evidence": "무주택 청년에게 월세 지원"}]}),
    );
    let valid = validate(&line, &payload(), true).unwrap();
    assert_eq!(
        valid.rule_json.as_ref().unwrap()["all"][1]["value"],
        json!(["univ_enrolled"])
    );
    let review = ServeReview::from_columns(
        "complete",
        valid.rule_json.as_ref(),
        &json!({"kind": "dated", "opens_at": "2026-10-01", "closes_at": "2026-10-31"}),
        Vec::new(),
        None,
        true,
    );
    let case = Case {
        review: Some(review),
        ..Case::good()
    };
    let student = |extra: Value| {
        let mut p = json!({"employment_status": "unemployed"});
        p.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        profile(p)
    };
    // A second-year student passes; a final-year one (재학 or 졸업 예정) is
    // not proven, so not verified and not hidden.
    let second_year = case.run(&student(json!({"school_type": "university",
        "enrollment_status": "enrolled", "school_year": 2})));
    assert!(second_year.verified, "{:?}", second_year.blockers);
    let final_year = case.run(&student(json!({"school_type": "university",
        "enrollment_status": "enrolled", "school_year": 4})));
    assert!(!final_year.verified);
    assert_eq!(final_year.hide, None);
    // A university graduate is contradicted.
    let alumnus = case.run(&student(json!({"school_type": "university",
        "enrollment_status": "graduated"})));
    assert_eq!(alumnus.hide, Some(ExclusionReason::HardRule));

    // A negated set is kept too: "졸업자 제외" stays not_in [univ_graduate].
    let excluded = validate(
        &input(
            json!({"rules": [{"id": "R1", "field": "education_stage", "op": "not_in",
            "value": ["univ_graduate"], "evidence": "무주택 청년에게 월세 지원"}]}),
        ),
        &payload(),
        true,
    )
    .unwrap();
    assert_eq!(
        excluded.rule_json.unwrap()["all"][1]["value"],
        json!(["univ_graduate"])
    );
}

/// Merge review L1: only copied code leaves may carry `severity`; on a
/// reviewed leaf it would silently become a hard condition.
#[test]
fn reviewed_leaves_carry_no_severity() {
    for rules in [
        json!([{"id": "R1", "field": "housing_type", "op": "eq", "value": "monthly_rent",
                "severity": "label", "evidence": "무주택 청년에게 월세 지원"}]),
        json!([{"id": "R1", "field": "housing_type", "op": "eq", "value": "monthly_rent",
                "severity": "hard", "evidence": "무주택 청년에게 월세 지원"}]),
        json!([{"id": "R1", "any": [
                    {"field": "housing_type", "op": "eq", "value": "monthly_rent", "severity": "label"},
                    {"field": "housing_type", "op": "eq", "value": "jeonse"}],
                "evidence": "무주택 청년에게 월세 지원"}]),
    ] {
        assert_eq!(
            validate(&input(json!({"rules": rules})), &payload(), true).unwrap_err(),
            Rejection::InvalidRule,
            "{rules}"
        );
    }
}

#[test]
fn an_unanswered_optional_answer_names_what_to_enter() {
    assert_eq!(
        profile_label("homeless_household"),
        "무주택 여부를 입력하면 확인할 수 있어요"
    );
    assert_eq!(
        profile_label("income_median_pct"),
        "기준 중위소득 구간을 입력하면 확인할 수 있어요"
    );
}

/// Audit M4: the review vocabulary is the rule engine's field table, so a
/// notice whose conditions the engine can check (무주택, 혼인, 자녀,
/// 중위소득) can get a complete review.
#[test]
fn every_rule_engine_field_is_a_review_field() {
    for field in [
        "homeless_household",
        "marital_status",
        "newlywed",
        "children_count",
        "income_median_pct",
    ] {
        assert!(super::store::is_review_field(field), "{field}");
    }
    for spec in crate::rule_engine::FIELDS {
        assert!(super::store::is_review_field(spec.name), "{}", spec.name);
        assert_ne!(crate::rule_engine::field_label(spec.name), "추가 자격");
    }
}

/// Audit M4: the list reads applicant reviews through the condition
/// review's hashed projection, so every field the applicant hash reads must
/// be one of its fields.
#[test]
fn the_hashed_projection_carries_every_applicant_field() {
    for source in ["youth_center", "gov_benefits"] {
        let Some(fields) = crate::applicant_review::fields(source) else {
            continue;
        };
        let hashed =
            super::hashed_fields(source).expect("an applicant source is a reviewed source");
        for field in fields {
            assert!(hashed.contains(field), "{source}: {field}");
        }
    }
}

/// Every field no profile answers stands for a named check with no
/// question: a new one added to `rule_engine::NO_PROFILE_ANSWER` fails here
/// until it is given its check.
#[test]
fn every_unanswered_field_names_a_check_and_asks_nothing() {
    for field in crate::rule_engine::NO_PROFILE_ANSWER {
        let (kind, label) = unanswered_check(field).unwrap_or_else(|| panic!("{field}"));
        let check = RemainingCheck::profile(field);
        assert_eq!((check.kind.as_str(), check.label.as_str()), (kind, label));
        assert_eq!(check.question_id, None, "{field}");
        assert_eq!(profile_label(field), label, "{field}");
        assert_ne!(check.kind, "profile", "{field}");
    }
}
