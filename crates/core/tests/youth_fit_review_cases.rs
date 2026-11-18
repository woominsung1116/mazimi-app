//! Cases from the independent review of youth_fit (2026-10-21): each was
//! wrong before the fix. Behaviour only, through the public API.

use mazimi_core::youth_fit::{self, ProgramFacts, Verdict};
use serde_json::{json, Value};

fn gov(title: &str, raw: Value) -> youth_fit::Decision {
    gov_aged(title, raw, None, None)
}

fn gov_aged(
    title: &str,
    raw: Value,
    min_age: Option<i32>,
    max_age: Option<i32>,
) -> youth_fit::Decision {
    youth_fit::classify(&ProgramFacts {
        source_type: "gov_benefits",
        title,
        summary: None,
        min_age,
        max_age,
        curated_youth: false,
        raw: &raw,
    })
}

#[test]
fn org_only_facility_rows_are_not_kept_by_their_young_users() {
    for (title, target) in [
        (
            "청소년 수련시설 운영 지원",
            "○ 공공 청소년수련관, 청소년문화의집 등",
        ),
        (
            "한국청소년상담복지개발원 운영 지원",
            "○ 만 9~24세 청소년, 청소년 상담 관련 자격증 취득자 및 예정자, 지도자 등",
        ),
    ] {
        let d = gov(
            title,
            json!({"사용자구분": "법인/시설/단체", "지원대상": target}),
        );
        assert_eq!((d.verdict, d.rule), (Verdict::OffTarget, "S1"), "{title}");
    }
}

#[test]
fn individual_returning_farmer_benefits_are_kept() {
    let d = gov(
        "귀농인 정착장려금 지원",
        json!({"사용자구분": "개인", "지원대상": "○ 전입한 귀농인",
               "supportConditions": {"JA0313": "Y", "JA0110": 18, "JA0111": 65}}),
    );
    assert_eq!(d.verdict, Verdict::Fit);
    // An age floor above 39 still demotes.
    let d = gov(
        "고령 귀농인 영농 지원",
        json!({"사용자구분": "개인", "supportConditions": {"JA0313": "Y", "JA0110": 60}}),
    );
    assert!(d.verdict.demoted());
}

#[test]
fn veteran_rows_get_the_veteran_tier() {
    for title in ["보훈명예수당", "참전유공자 사망위로금"] {
        let d = gov(
            title,
            json!({"사용자구분": "개인", "지원대상": "○ 관내 거주 참전유공자"}),
        );
        assert_eq!((d.verdict, d.rule), (Verdict::VeteranOnly, "V1"), "{title}");
    }
}

#[test]
fn structured_age_floors_and_ceilings_demote_without_title_words() {
    let d = gov(
        "치매 조기검진 지원",
        json!({"사용자구분": "개인", "지원대상": "○ 관내 주민",
               "supportConditions": {"JA0110": 60, "JA0111": 120, "JA0322": "Y"}}),
    );
    assert_eq!((d.verdict, d.rule), (Verdict::OffTarget, "S3"));
    let d = gov(
        "아토피 치료비 지원",
        json!({"사용자구분": "개인", "지원대상": "○ 관내 아토피 환자",
               "supportConditions": {"JA0110": 0, "JA0111": 17, "JA0322": "Y"}}),
    );
    assert_eq!((d.verdict, d.rule), (Verdict::ChildOnly, "S4c"));
    let d = gov(
        "진폐재해자 위로금",
        json!({"사용자구분": "개인", "지원대상": "○ 진폐 재해자"}),
    );
    assert_eq!((d.verdict, d.rule), (Verdict::OffTarget, "S5"));
    // Youth recall is untouched: a youth band still wins.
    let d = gov(
        "청년 마음건강 지원",
        json!({"사용자구분": "개인", "supportConditions": {"JA0110": 19, "JA0111": 39}}),
    );
    assert_eq!(d.verdict, Verdict::Fit);
}

#[test]
fn the_list_predicate_exempts_the_same_tiers_as_recommendations() {
    // Audit M5: an anonymous viewer has answered no flag, so the disability
    // and veteran tiers are not demoted for them; the others are.
    let anon = youth_fit::demoted_sql("p.id", youth_fit::GateProfile::default());
    assert!(
        anon.contains("NOT IN ('fit', 'disability_only', 'veteran_only')"),
        "{anon}"
    );
    let answered_no = youth_fit::demoted_sql(
        "p.id",
        youth_fit::GateProfile {
            has_disability: Some(false),
            veteran_family: Some(false),
            ..Default::default()
        },
    );
    assert!(answered_no.contains("NOT IN ('fit')"), "{answered_no}");
    let parent = youth_fit::demoted_sql(
        "p.id",
        youth_fit::GateProfile {
            household_size: Some(3),
            ..Default::default()
        },
    );
    assert!(
        parent.contains("'child_only'") && !parent.contains("'business_only'"),
        "{parent}"
    );
    let owner = youth_fit::demoted_sql(
        "p.id",
        youth_fit::GateProfile {
            self_employed: true,
            veteran_family: Some(true),
            ..Default::default()
        },
    );
    assert!(
        owner.contains("'business_only'") && owner.contains("'veteran_only'"),
        "{owner}"
    );
    assert!(!owner.contains("'off_target'"));
}

#[test]
fn stored_version_tracks_every_word_list() {
    let version = youth_fit::classifier_version();
    assert!(
        version.starts_with(youth_fit::CLASSIFIER_VERSION),
        "{version}"
    );
    assert_eq!(version.len(), youth_fit::CLASSIFIER_VERSION.len() + 13);
    assert!(version
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'));
}
