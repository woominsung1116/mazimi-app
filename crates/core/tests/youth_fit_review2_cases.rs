//! Cases from the second re-review of youth_fit (2026-10-22): each was wrong
//! before the fix. Behaviour only, through the public API.

use mazimi_core::youth_fit::{self, ProgramFacts, Target, Verdict};
use serde_json::{json, Value};

fn gov(title: &str, raw: Value) -> youth_fit::Decision {
    youth_fit::classify(&ProgramFacts {
        source_type: "gov_benefits",
        title,
        summary: None,
        min_age: None,
        max_age: None,
        curated_youth: false,
        raw: &raw,
    })
}

/// A disability flag alone (D1) with only "장애인" in the conditions.
fn disabled_flag(target: &str) -> Value {
    json!({"사용자구분": "개인", "지원대상": target,
           "supportConditions": {"JA0110": 0, "JA0111": 120, "JA0328": "Y"}})
}

#[test]
fn a_disability_flag_does_not_demote_rows_that_name_other_people() {
    for (title, target) in [
        (
            "약국약제비 본인부담금 지원",
            "○ 계룡시 거주자로서 만 65세 이상인자, 장애인증서 소지자, 국민기초생활보장수급자, 국가유공자 및 유족 등",
        ),
        ("청각장애인 수어 교육", "○ 수어에 관심 있는 관내 주민"),
        (
            "장애인 보장구 수리 및 대여사업",
            "○ 보장구 수리 : 등록 장애인\n○ 보장구 무료대여 : 일반시민",
        ),
    ] {
        let d = gov(title, disabled_flag(target));
        assert_eq!(d.verdict, Verdict::Fit, "{title}: {d:?}");
    }
}

#[test]
fn a_disability_flag_still_demotes_disabled_residents_and_recipients() {
    for (title, target) in [
        ("수강료 감면(장애인)", "「장애인복지법」제2조의 장애인 중 성북구 주민"),
        ("장애인 동료 상담 지원", "○ 등록장애인(서대문구 주민 우선)"),
        ("보장구 구입비 지원", "○ 등록장애인 중 기초생활보장 의료급여 수급자"),
        (
            "발달재활서비스 바우처 지원",
            "○ 만 18세 미만의 등록한 장애인(지적, 자폐성)으로 국민 기초 생활 보장 수급자 및 차상위계층",
        ),
        ("수어통역센터 운영 지원", "○ 언어, 청각 장애인\n○ 주민등록상 관내 거주"),
    ] {
        let d = gov(title, disabled_flag(target));
        assert_eq!(
            (d.verdict, d.rule),
            (Verdict::DisabilityOnly, "D1"),
            "{title}"
        );
    }
}

#[test]
fn a_child_ceiling_does_not_demote_rows_that_name_adults() {
    let raw = |target: &str| {
        json!({"사용자구분": "개인", "지원대상": target,
               "supportConditions": {"JA0110": 7, "JA0111": 18, "JA0322": "Y"}})
    };
    for target in [
        "○ 아동(만7~18세), 청소년(만18세이하), 노인(만60세이상)",
        "○ 구강건강관리가 필요한 관내 주민",
        "○ 만 19세 이상 성인 및 아동",
    ] {
        let d = gov("지역사회서비스투자사업", raw(target));
        assert_eq!(d.verdict, Verdict::Fit, "{target}: {d:?}");
    }
    let d = gov(
        "북한이탈주민 자녀 학습지 지원",
        raw("○ 북한이탈주민 자녀(만 3세부터 초등학생까지)"),
    );
    assert_eq!((d.verdict, d.rule), (Verdict::ChildOnly, "S4c"));
}

#[test]
fn young_farmer_words_keep_a_row_only_from_its_title() {
    let farm = |title: &str, target: &str| {
        gov(
            title,
            json!({"사용자구분": "개인", "지원대상": target,
                   "supportConditions": {"JA0110": 18, "JA0111": 120, "JA0313": "Y"}}),
        )
    };
    let d = farm(
        "기본형 공익직불금",
        "○ (농가) 직불금 수령자, 정책대상자(후계농업경영인 등), 신규자",
    );
    assert_eq!((d.verdict, d.rule), (Verdict::OffTarget, "S2"));
    assert_eq!(farm("후계농업경영인 육성 지원", "○ 관내 농업인").rule, "K7");
    assert_eq!(farm("농업인 지원", "○ 귀농인 및 귀촌인").rule, "K7");
}

#[test]
fn occupational_disease_rows_are_not_labelled_elder() {
    let d = gov(
        "진폐재해자 위로금",
        json!({"사용자구분": "개인", "지원대상": "○ 진폐 재해자 및 유족"}),
    );
    assert_eq!(
        (d.verdict, d.target, d.rule),
        (Verdict::OffTarget, Some(Target::Other), "S5")
    );
}
