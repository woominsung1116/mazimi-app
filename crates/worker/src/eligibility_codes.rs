//! Structured eligibility codes published by the sources themselves, mapped
//! to program fields. Nothing here reads free text: an official code either
//! maps to a field the profile can answer, or it is kept as a target flag for
//! display and later filters.
//!
//! - 보조금24 `supportConditions` (공공서비스 혜택정보, odcloud gov24 v3):
//!   per-service `JAxxxx` flags. Field labels are the provider's OpenAPI spec
//!   (`infuser.odcloud.kr/api/stages/44436/api-docs`). Within one group the
//!   flags are the characteristics the service accepts ("Y"), and the group's
//!   "해당사항없음" flag means a person with none of them is accepted too.
//! - 온통청년 policy codes: the provider's 코드정의서 (`API코드정보.xlsx`,
//!   linked from youthcenter.go.kr OPEN API 가이드).
//!
//! Every coded restriction leaves the worker here, as `ingestion:source_codes`
//! rule conditions (the one eligibility path, [`mazimi_core::rule_engine`]):
//! a code the profile answers maps to its field (or a view derived from
//! existing fields, [`mazimi_core::education`]); a group no profile answers
//! is a label on `target_group` (or `gender`), always `Unknown` with "지원
//! 대상" as the check; a code that restricts nobody is listed in
//! [`NOT_A_RESTRICTION`]. A profile that leaves a field empty is `Unknown`,
//! never `Fail`. The `programs.target_*` columns keep the mapped codes as the
//! source record; nothing reads them for eligibility.
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::BTreeSet;

// ── 보조금24 supportConditions ────────────────────────────────────────────────

/// Personal characteristics (JA03xx). `JA0322` is 해당사항없음.
const GOV_PERSONAL: &[(&str, &str)] = &[
    ("JA0301", "prospective_parent"),
    ("JA0302", "pregnant"),
    ("JA0303", "childbirth_adoption"),
    ("JA0313", "farmer"),
    ("JA0314", "fisher"),
    ("JA0315", "livestock_farmer"),
    ("JA0316", "forester"),
    ("JA0317", "elementary_student"),
    ("JA0318", "middle_school_student"),
    ("JA0319", "high_school_student"),
    ("JA0320", "university_student"),
    ("JA0326", "worker"),
    ("JA0327", "job_seeker"),
    ("JA0328", "disabled"),
    ("JA0329", "veteran"),
    ("JA0330", "patient"),
];
const GOV_PERSONAL_NONE: &str = "JA0322";

/// Household characteristics (JA04xx). `JA0410` is 해당사항없음.
const GOV_HOUSEHOLD: &[(&str, &str)] = &[
    ("JA0401", "multicultural"),
    ("JA0402", "north_korean_defector"),
    ("JA0403", "single_parent_or_grandparent"),
    ("JA0404", "single_person"),
    ("JA0411", "multi_child"),
    ("JA0412", "homeless_household"),
    ("JA0413", "new_resident"),
    ("JA0414", "extended_family"),
];
const GOV_HOUSEHOLD_NONE: &str = "JA0410";

/// 기준 중위소득 bands (JA020x).
const GOV_INCOME: &[(&str, &str)] = &[
    ("JA0201", "0-50"),
    ("JA0202", "51-75"),
    ("JA0203", "76-100"),
    ("JA0204", "101-200"),
    ("JA0205", "200+"),
];

/// Parent-facing personal flags: a child-only age band on such a service is
/// the child's age, not the applicant's.
const GOV_PARENT_FLAGS: &[&str] = &["JA0301", "JA0302", "JA0303"];

/// Pupil flags (초·중·고등학생): the pupil is a parent's child.
const GOV_PUPIL_FLAGS: &[&str] = &["JA0317", "JA0318", "JA0319"];

/// An upper age below this is a child's band whoever applies: nobody under
/// 13 applies for a public service themselves.
const CHILD_APPLIES_FROM: i32 = 13;

/// Applicant kinds from 보조금24 `사용자구분` ("개인||가구").
const GOV_APPLICANTS: &[(&str, &str)] = &[
    ("개인", "individual"),
    ("가구", "household"),
    ("소상공인", "small_business"),
    ("법인/시설/단체", "organization"),
];

/// What a 보조금24 record says about who may apply.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct GovConditions {
    pub min_age: Option<i32>,
    pub max_age: Option<i32>,
    pub applicant_types: Option<Vec<String>>,
    pub target_groups: Option<Vec<String>>,
    pub target_household_types: Option<Vec<String>>,
    pub target_gender: Option<String>,
    pub income_median_bands: Option<Vec<String>>,
    /// The age band is a child's ([`gov_conditions`]): the applicant is the
    /// parent, a named check on `children_count`.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub for_parents_of_children: bool,
}

fn flag(conditions: &Value, code: &str) -> bool {
    conditions[code]
        .as_str()
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("y"))
}

fn int(conditions: &Value, code: &str) -> Option<i32> {
    match &conditions[code] {
        Value::Number(n) => n.as_i64().and_then(|v| i32::try_from(v).ok()),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// The accepted set of one group, or `None` when the group does not restrict
/// anyone: its 해당사항없음 flag is set, or nothing in it is set at all.
fn restricted_group(
    conditions: &Value,
    table: &[(&str, &'static str)],
    none_code: &str,
) -> Option<Vec<String>> {
    if flag(conditions, none_code) {
        return None;
    }
    let set: Vec<String> = table
        .iter()
        .filter(|(code, _)| flag(conditions, code))
        .map(|(_, key)| (*key).to_string())
        .collect();
    (!set.is_empty()).then_some(set)
}

/// Map a 보조금24 record: `사용자구분` from `serviceList`, the rest from the
/// `supportConditions` row merged into the payload by the source.
pub fn gov_conditions(payload: &Value) -> GovConditions {
    let mut out = GovConditions {
        applicant_types: applicant_types(payload["사용자구분"].as_str()),
        ..GovConditions::default()
    };
    let conditions = &payload[crate::sources::gov_benefits::SUPPORT_CONDITIONS_KEY];
    if !conditions.is_object() {
        return out;
    }

    // Age: 0 as a lower bound and anything from 100 up are "no limit".
    let min = int(conditions, "JA0110").filter(|age| (1..100).contains(age));
    let max = int(conditions, "JA0111").filter(|age| (0..100).contains(age));
    let parent_facing = GOV_PARENT_FLAGS.iter().any(|code| flag(conditions, code));
    let pupils = GOV_PUPIL_FLAGS.iter().any(|code| flag(conditions, code));
    let inverted = min.zip(max).is_some_and(|(lo, hi)| lo > hi);
    // A child-only band describes the child and a parent applies: on a
    // pregnancy/birth service, on a pupil group (초·중·고등학생), or under 13
    // (유아학비 3–5), an age nobody applies at for themselves. A 13–18 band
    // with none of these is the applicant's own (청소년 교통비, audit M7).
    let childs_band = max.is_some_and(|hi| hi < 19)
        && (parent_facing || pupils || max.is_some_and(|hi| hi < CHILD_APPLIES_FROM));
    if !inverted && !childs_band {
        out.min_age = min;
        out.max_age = max;
    }
    // The parent of a child in the band: a named check on having children.
    // Pregnancy and birth services name their parent group instead.
    out.for_parents_of_children = !inverted && childs_band && !parent_facing;

    out.target_gender = match (flag(conditions, "JA0101"), flag(conditions, "JA0102")) {
        (true, false) => Some("male".into()),
        (false, true) => Some("female".into()),
        _ => None,
    };
    let bands: Vec<String> = GOV_INCOME
        .iter()
        .filter(|(code, _)| flag(conditions, code))
        .map(|(_, band)| (*band).to_string())
        .collect();
    out.income_median_bands =
        (!bands.is_empty() && bands.len() < GOV_INCOME.len()).then_some(bands);
    out.target_groups = restricted_group(conditions, GOV_PERSONAL, GOV_PERSONAL_NONE);
    out.target_household_types = restricted_group(conditions, GOV_HOUSEHOLD, GOV_HOUSEHOLD_NONE);
    out
}

/// `사용자구분` ("개인||가구") as canonical applicant kinds; `None` when absent
/// or when any part is unrecognised (a new provider label is not guessed).
fn applicant_types(text: Option<&str>) -> Option<Vec<String>> {
    let text = text?.trim();
    if text.is_empty() {
        return None;
    }
    let mut out = BTreeSet::new();
    for part in text.split("||").map(str::trim).filter(|p| !p.is_empty()) {
        let (_, key) = GOV_APPLICANTS.iter().find(|(label, _)| *label == part)?;
        out.insert((*key).to_string());
    }
    (!out.is_empty()).then(|| out.into_iter().collect())
}

// ── 온통청년 policy codes ─────────────────────────────────────────────────────

/// What an 온통청년 record's codes say, in profile vocabulary where one exists.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct YouthConditions {
    /// `jobCd` labels, verbatim from the code table (display and audit).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub employment_requirements: Option<Vec<String>>,
    /// Canonical `employment_status` values that satisfy `jobCd`, when every
    /// listed code maps to one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub employment_statuses: Option<Vec<String>>,
    /// `schoolCd` labels, verbatim.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub education_requirements: Option<Vec<String>>,
    /// Canonical `enrollment_status` values that satisfy `schoolCd`, when the
    /// code set is limited to university-level statuses.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enrollment_statuses: Option<Vec<String>>,
    /// `education_stage` values that satisfy `schoolCd`, for every set
    /// without 기타 (0049009); see [`mazimi_core::education`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub education_stages: Option<Vec<String>>,
    /// `sbizCd` as target-group keys (same vocabulary as 보조금24 where shared).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_groups: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_gender: Option<String>,
    /// `mrgSttsCd`: "married" or "unmarried".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub marital_status: Option<String>,
    /// `plcyMajorCd` labels, verbatim.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub major_requirements: Option<Vec<String>>,
    /// `major_category` values that satisfy `plcyMajorCd`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub major_categories: Option<Vec<String>>,
    /// `earnCndSeCd` 0043002: annual income bounds in 만원.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub income_annual_min_manwon: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub income_annual_max_manwon: Option<i64>,
    /// `earnCndSeCd` 0043003: the free-text income condition (`earnEtcCn`).
    /// It is for review and AI extraction, never a rule.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub income_condition_text: Option<String>,
}

const JOB_CODES: &[(&str, &str)] = &[
    ("0013001", "재직자"),
    ("0013002", "자영업자"),
    ("0013003", "미취업자"),
    ("0013004", "프리랜서"),
    ("0013005", "일용근로자"),
    ("0013006", "(예비)창업자"),
    ("0013007", "단기근로자"),
    ("0013008", "영농종사자"),
    ("0013009", "기타"),
    ("0013010", "제한없음"),
];

/// Profile `employment_status` values each `jobCd` admits. `None`: the code
/// cannot be answered from the profile ((예비)창업자 may be anyone, 기타 is
/// unspecified), so a set containing it produces no rule. 미취업자 admits
/// students and job seekers as well as the unemployed.
fn job_statuses(code: &str) -> Option<&'static [&'static str]> {
    match code {
        "0013001" => Some(&["employed", "new_hire"]),
        "0013002" | "0013004" => Some(&["self_employed"]),
        "0013003" => Some(&["unemployed", "preparing", "student"]),
        "0013005" | "0013007" => Some(&["employed", "new_hire", "other"]),
        "0013008" => Some(&["self_employed", "employed", "other"]),
        _ => None,
    }
}

const SCHOOL_CODES: &[(&str, &str)] = &[
    ("0049001", "고졸 미만"),
    ("0049002", "고교 재학"),
    ("0049003", "고졸 예정"),
    ("0049004", "고교 졸업"),
    ("0049005", "대학 재학"),
    ("0049006", "대졸 예정"),
    ("0049007", "대학 졸업"),
    ("0049008", "석·박사"),
    ("0049009", "기타"),
    ("0049010", "제한없음"),
];

/// Profile `enrollment_status` values for university-level `schoolCd`. The
/// profile field describes higher education only, so a set that admits any
/// school-level or unspecified code yields no restriction. 대학 재학 admits
/// 휴학 too: excluding a student on leave would hide programmes many of them
/// qualify for, and a missed exclusion only costs precision.
fn school_statuses(code: &str) -> Option<&'static [&'static str]> {
    match code {
        "0049005" => Some(&["enrolled", "on_leave"]),
        "0049006" => Some(&["enrolled"]),
        "0049007" => Some(&["graduated"]),
        "0049008" => Some(&["enrolled", "on_leave", "graduated", "research_student"]),
        _ => None,
    }
}

/// `schoolCd` → `education_stage`. 기타 and 제한없음 have none.
fn school_stage(code: &str) -> Option<&'static str> {
    Some(match code {
        "0049001" => "hs_below",
        "0049002" => "hs_enrolled",
        "0049003" => "hs_expected",
        "0049004" => "hs_graduate",
        "0049005" => "univ_enrolled",
        "0049006" => "univ_expected",
        "0049007" => "univ_graduate",
        "0049008" => "postgrad",
        _ => return None,
    })
}

const SBIZ_CODES: &[(&str, &str)] = &[
    ("0014001", "sme_worker"),
    ("0014002", "women"),
    ("0014003", "basic_livelihood_recipient"),
    ("0014004", "single_parent_or_grandparent"),
    ("0014005", "disabled"),
    ("0014006", "farmer"),
    ("0014007", "soldier"),
    ("0014008", "local_talent"),
];

/// `plcyMajorCd` → `major_category`. 기타 and 제한없음 have none.
fn major_category(code: &str) -> Option<&'static str> {
    Some(match code {
        "0011001" => "humanities",
        "0011002" => "social_science",
        "0011003" => "business",
        "0011004" => "natural_science",
        "0011005" => "engineering",
        "0011006" => "arts_sports",
        "0011007" => "agriculture",
        _ => return None,
    })
}

const MAJOR_CODES: &[(&str, &str)] = &[
    ("0011001", "인문계열"),
    ("0011002", "사회계열"),
    ("0011003", "상경계열"),
    ("0011004", "이학계열"),
    ("0011005", "공학계열"),
    ("0011006", "예체능계열"),
    ("0011007", "농산업계열"),
    ("0011008", "기타"),
];

/// Codes of a comma-separated code field. `None` when the field is missing or
/// blank, or when it contains the group's 제한없음 code or an unknown code.
fn code_set<'a>(
    payload: &'a Value,
    field: &str,
    table: &[(&str, &str)],
    unrestricted: &str,
) -> Option<Vec<&'a str>> {
    let codes: Vec<&str> = payload[field]
        .as_str()?
        .split(',')
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .collect();
    if codes.is_empty() || codes.contains(&unrestricted) {
        return None;
    }
    let unknown: Vec<&str> = codes
        .iter()
        .copied()
        .filter(|c| *c != unrestricted && !table.iter().any(|(code, _)| code == c))
        .collect();
    if !unknown.is_empty() {
        log_unknown_codes(field, &unknown);
        return None;
    }
    Some(codes)
}

/// Logs a code the table does not know, once per field and code for the
/// life of the process (audit M10): a new restrictive code is seen, the set
/// still gives no rule.
fn log_unknown_codes(field: &str, codes: &[&str]) {
    static SEEN: std::sync::OnceLock<std::sync::Mutex<BTreeSet<String>>> =
        std::sync::OnceLock::new();
    let seen = SEEN.get_or_init(Default::default);
    let Ok(mut seen) = seen.lock() else {
        return;
    };
    for code in codes {
        if seen.len() < 1000 && seen.insert(format!("{field}:{code}")) {
            tracing::warn!(
                field,
                code,
                "unknown youth_center code; its set gives no rule"
            );
        }
    }
}

fn labels(codes: &[&str], table: &[(&str, &str)]) -> Vec<String> {
    codes
        .iter()
        .filter_map(|c| table.iter().find(|(code, _)| code == c))
        .map(|(_, label)| (*label).to_string())
        .collect()
}

/// The admitted profile values when every code maps; `None` otherwise.
fn statuses(
    codes: &[&str],
    map: fn(&str) -> Option<&'static [&'static str]>,
) -> Option<Vec<String>> {
    let mut out = BTreeSet::new();
    for code in codes {
        out.extend(map(code)?.iter().map(|s| (*s).to_string()));
    }
    Some(out.into_iter().collect())
}

pub fn youth_conditions(p: &Value) -> YouthConditions {
    let mut out = YouthConditions::default();
    if let Some(codes) = code_set(p, "jobCd", JOB_CODES, "0013010") {
        out.employment_requirements = Some(labels(&codes, JOB_CODES));
        out.employment_statuses = statuses(&codes, job_statuses);
    }
    if let Some(codes) = code_set(p, "schoolCd", SCHOOL_CODES, "0049010") {
        out.education_requirements = Some(labels(&codes, SCHOOL_CODES));
        out.enrollment_statuses = statuses(&codes, school_statuses);
        // 고교 졸업 and 대학 졸업 read as "고졸 이상"/"대졸 이상" on many
        // notices (재학생 apply too), so a set topped by one says nothing
        // definite: no rule.
        let highest = codes.iter().copied().max();
        let stages: Option<Vec<&str>> = codes.iter().map(|c| school_stage(c)).collect();
        out.education_stages = stages
            .filter(|_| !matches!(highest, Some("0049004" | "0049007")))
            .map(|stages| {
                mazimi_core::education::admitted_education_stages(&stages)
                    .into_iter()
                    .map(String::from)
                    .collect()
            });
    }
    if let Some(codes) = code_set(
        p,
        "sbizCd",
        &[SBIZ_CODES, &[("0014009", "기타")]].concat(),
        "0014010",
    ) {
        // 기타 is unspecified: the set no longer says who is targeted.
        if !codes.contains(&"0014009") {
            let groups: Vec<String> = codes
                .iter()
                .filter_map(|c| SBIZ_CODES.iter().find(|(code, _)| code == c))
                .map(|(_, key)| (*key).to_string())
                .collect();
            if groups == ["women"] {
                out.target_gender = Some("female".into());
            }
            out.target_groups = Some(groups);
        }
    }
    out.marital_status = match p["mrgSttsCd"].as_str().map(str::trim) {
        Some("0055001") => Some("married".into()),
        Some("0055002") => Some("unmarried".into()),
        _ => None,
    };
    if let Some(codes) = code_set(
        p,
        "plcyMajorCd",
        &[MAJOR_CODES, &[("0011009", "제한없음")]].concat(),
        "0011009",
    ) {
        if !codes.contains(&"0011008") {
            out.major_requirements = Some(labels(&codes, MAJOR_CODES));
            let categories: Option<Vec<&str>> = codes.iter().map(|c| major_category(c)).collect();
            out.major_categories = categories.map(|categories| {
                mazimi_core::education::admitted_major_categories(&categories)
                    .into_iter()
                    .map(String::from)
                    .collect()
            });
        }
    }
    match p["earnCndSeCd"].as_str().map(str::trim) {
        Some("0043002") => {
            let amount = |field: &str| {
                p[field]
                    .as_str()
                    .and_then(|s| s.trim().parse::<i64>().ok())
                    .or_else(|| p[field].as_i64())
                    .filter(|v| *v > 0)
            };
            out.income_annual_min_manwon = amount("earnMinAmt");
            out.income_annual_max_manwon = amount("earnMaxAmt");
        }
        Some("0043003") => {
            out.income_condition_text = p["earnEtcCn"]
                .as_str()
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(str::to_owned);
        }
        _ => {}
    }
    out
}

// ── 한국장학재단 학자금지원정보(대학생) ──────────────────────────────────────────

/// `대학구분` token → profile `school_type`. A token is read by the school
/// kind it starts with ("4년제(5~6년제포함)", "전문대(2~3년제)"); anything
/// else (해외대학, 원격대학 …) maps to none, and then the field gives no rule.
fn scholarship_school_type(token: &str) -> Option<&'static str> {
    if token.starts_with("4년제") {
        Some("university")
    } else if token.starts_with("전문대") {
        Some("college")
    } else if token.starts_with("대학원") {
        Some("graduate")
    } else {
        None
    }
}

/// `학년구분` token → the education stages it names. 신입생 is admitted
/// before enrolling (고3, 졸업 후 합격자); a 학기 is a current student.
fn scholarship_year_stages(token: &str) -> Option<&'static [&'static str]> {
    if token == "대학신입생" {
        return Some(&["hs_expected", "hs_graduate", "univ_enrolled"]);
    }
    let semester = token.strip_prefix("대학")?.strip_suffix("학기")?;
    semester
        .parse::<u8>()
        .ok()
        .filter(|n| (1..=12).contains(n))
        .map(|_| &["univ_enrolled"][..])
}

/// The comma-separated tokens of a field; `None` when absent, blank or
/// 제한없음.
fn scholarship_tokens<'a>(p: &'a Value, field: &str) -> Option<Vec<&'a str>> {
    let tokens: Vec<&str> = p[field]
        .as_str()?
        .split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .collect();
    let unrestricted = tokens
        .iter()
        .any(|t| t.replace(' ', "").contains("제한없음"));
    (!tokens.is_empty() && !unrestricted).then_some(tokens)
}

/// Conditions of a 학자금지원정보 row: `대학구분` as `school_type` and
/// `학년구분` as `education_stage`, each only when every token maps (an
/// unknown token is logged and gives no rule for its field). `school_type`
/// is hard for current students; with 신입생 among the years it is a label,
/// since an admitted 고3 is not yet at the school. The stage is a label:
/// the year a notice names is not always the year it restricts to.
pub fn scholarship_conditions(p: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    let years = scholarship_tokens(p, "학년구분");
    let stages: Option<Vec<&str>> = years.as_ref().and_then(|tokens| {
        let mut stages = Vec::new();
        for token in tokens {
            let Some(named) = scholarship_year_stages(token) else {
                tracing::warn!(
                    field = "학년구분",
                    token,
                    "unknown scholarship token; no rule"
                );
                return None;
            };
            stages.extend_from_slice(named);
        }
        Some(stages)
    });
    let freshers = years
        .as_ref()
        .is_some_and(|tokens| tokens.contains(&"대학신입생"));
    if let Some(tokens) = scholarship_tokens(p, "대학구분") {
        let types: Option<BTreeSet<&str>> = tokens
            .iter()
            .map(|token| {
                let mapped = scholarship_school_type(token);
                if mapped.is_none() {
                    tracing::warn!(
                        field = "대학구분",
                        token,
                        "unknown scholarship token; no rule"
                    );
                }
                mapped
            })
            .collect();
        if let Some(types) = types {
            let types: Vec<&str> = types.into_iter().collect();
            out.push(if freshers {
                label_condition("school_type", "in", json!(types))
            } else {
                source_condition("school_type", "in", json!(types))
            });
        }
    }
    if let Some(stages) = stages {
        let mut admitted = mazimi_core::education::admitted_education_stages(&stages);
        // A 신입생-only row is for 고3 and new students: 대학 재학 admits
        // 대학원생 elsewhere, not here (review L8).
        if years.as_deref() == Some(&["대학신입생"][..]) {
            admitted.retain(|stage| *stage != "postgrad");
        }
        out.push(label_condition("education_stage", "in", json!(admitted)));
    }
    out
}

/// Whether a 학자금지원정보 row is a loan (`상품구분` 학자금대출), not a grant.
pub fn scholarship_is_loan(p: &Value) -> bool {
    p["상품구분"]
        .as_str()
        .is_some_and(|kind| kind.contains("대출"))
}

// ── Rules ────────────────────────────────────────────────────────────────────

/// See [`mazimi_core::rule_engine::SOURCE_ORIGIN_KEY`].
pub const RULE_ORIGIN_KEY: &str = mazimi_core::rule_engine::SOURCE_ORIGIN_KEY;
pub const RULE_ORIGIN: &str = mazimi_core::rule_engine::SOURCE_ORIGIN;

/// Hard-filter conditions the existing evaluator understands, compiled from
/// youth codes. Only codes that map to a profile field produce a condition;
/// an unanswered field stays `unknown` and visible.
pub fn youth_rule_conditions(conditions: &YouthConditions) -> Vec<Value> {
    let mut out = Vec::new();
    if let Some(statuses) = &conditions.employment_statuses {
        out.push(source_condition("employment_status", "in", json!(statuses)));
    }
    if let Some(stages) = &conditions.education_stages {
        // A set with a school-level code (고졸 미만 … 고교 졸업) is often a
        // 고졸 예정 code on a notice that also admits 졸업자 or 일반인: a
        // named check, never a hide. University-level sets stay hard.
        if stages.iter().any(|s| s.starts_with("hs_")) {
            out.push(label_condition("education_stage", "in", json!(stages)));
        } else {
            out.push(source_condition("education_stage", "in", json!(stages)));
        }
    }
    if let Some(categories) = &conditions.major_categories {
        // 계열 sets miss related majors (CS under 이학, 사회복지 under 사회):
        // a named check, never a hide.
        out.push(label_condition("major_category", "in", json!(categories)));
    }
    // A group the profile answers is a hard condition; one it does not
    // (농업인, 군인) is the unanswered `target_group`, so a set with such a
    // member can no longer exclude anyone but still never passes unchecked.
    if let Some(rule) = conditions
        .target_groups
        .as_deref()
        .and_then(|groups| group_rule(groups, youth_group_condition))
    {
        out.push(rule);
    }
    // 기혼 is also how a notice open to 예비부부 (결혼 예정자) must be coded:
    // a named check. 미혼 is exact: a married user is outside it.
    match conditions.marital_status.as_deref() {
        Some("married") => out.push(label_condition("marital_status", "eq", json!("married"))),
        Some("unmarried") => out.push(source_condition("marital_status", "eq", json!("unmarried"))),
        _ => {}
    }
    out
}

/// 온통청년 `sbizCd` groups the profile answers. 여성 is the unanswered
/// `gender`.
fn youth_group_condition(group: &str) -> Option<Value> {
    match group {
        "disabled" => Some(source_condition("has_disability", "is_true", Value::Null)),
        "basic_livelihood_recipient" => Some(source_condition(
            "is_low_income_household",
            "is_true",
            Value::Null,
        )),
        "women" => Some(label_condition("gender", "eq", json!("female"))),
        _ => None,
    }
}

/// Coded groups that do not restrict the applicant, so a set holding one
/// gives no rule:
/// - 보조금24 근로자 (JA0326): the provider also sets it on 소상공인, 창업 and
///   근로자 자녀 장학금 rows.
/// - 보조금24 초·중·고등학생 (JA0317–0319): the pupil is a parent's child;
///   the child's age band is handled by [`gov_conditions`].
const NOT_A_RESTRICTION: &[&str] = &[
    "worker",
    "elementary_student",
    "middle_school_student",
    "high_school_student",
];

/// One coded "any of these groups" set as a rule: the members `answer` maps
/// to a profile condition, plus one label condition on the unanswered
/// `target_group` field for the rest. `None` for an empty set or one that
/// holds a [`NOT_A_RESTRICTION`] member.
fn group_rule(members: &[String], answer: impl Fn(&str) -> Option<Value>) -> Option<Value> {
    if members.is_empty()
        || members
            .iter()
            .any(|m| NOT_A_RESTRICTION.contains(&m.as_str()))
    {
        return None;
    }
    let mut mapped = Vec::new();
    let mut unanswered = Vec::new();
    for member in members {
        match answer(member) {
            Some(condition) => mapped.push(condition),
            None => unanswered.push(member.clone()),
        }
    }
    if !unanswered.is_empty() {
        mapped.push(label_condition("target_group", "in", json!(unanswered)));
    }
    match mapped.as_slice() {
        [single] => Some(single.clone()),
        _ => Some(json!({"any": mapped, RULE_ORIGIN_KEY: RULE_ORIGIN})),
    }
}

/// The upper age a record states, as a named check: for a record that also
/// extends it for military service, whose served band is widened instead
/// (see `pipeline::military_age_extension`).
pub fn stated_upper_age_label(max_age: i32) -> Value {
    label_condition("age", "lte", json!(max_age))
}

/// A stated lower age a widened band admits below: a named check.
pub fn stated_lower_age_label(min_age: i32) -> Value {
    label_condition("age", "gte", json!(min_age))
}

/// Eligibility the notice states as birth years: hard when the range is whole
/// calendar years (a birth year decides it), else a named check.
pub fn birth_year_condition(first: i32, last: i32, whole_years: bool) -> Value {
    let condition = source_condition("birth_year", "between", json!([first, last]));
    if whole_years {
        condition
    } else {
        let mut condition = condition;
        condition["severity"] = json!("label");
        condition
    }
}

/// A source condition: hard filter, marked as compiled from codes.
fn source_condition(field: &str, op: &str, value: Value) -> Value {
    json!({"field": field, "op": op, "value": value, "weight": 20, RULE_ORIGIN_KEY: RULE_ORIGIN})
}

/// A non-hiding source condition (`"severity": "label"`): a contradiction
/// reads as Unknown and names the check.
fn label_condition(field: &str, op: &str, value: Value) -> Value {
    let mut condition = source_condition(field, op, value);
    condition["severity"] = json!("label");
    condition
}

/// 보조금24 personal characteristics (JA03xx) the profile answers about the
/// applicant. 국가보훈대상자 is not the profile's 보훈 가족 flag, so it stays
/// an unanswered `target_group`.
fn gov_personal_condition(group: &str) -> Option<Value> {
    match group {
        "university_student" => Some(label_condition(
            "education_stage",
            "in",
            // 대학원생 apply as 대학생 for many loans and scholarships.
            json!(["univ_enrolled", "univ_expected", "postgrad"]),
        )),
        "job_seeker" => Some(label_condition(
            "employment_status",
            "in",
            json!(["preparing", "student", "unemployed"]),
        )),
        "disabled" => Some(label_condition("has_disability", "is_true", Value::Null)),
        _ => None,
    }
}

/// 보조금24 household characteristics (JA04xx) the profile answers.
fn gov_household_condition(kind: &str) -> Option<Value> {
    match kind {
        "multicultural" => Some(label_condition(
            "is_multicultural_family",
            "is_true",
            Value::Null,
        )),
        "single_person" => Some(label_condition("household_size", "eq", json!(1))),
        "homeless_household" => Some(label_condition(
            "homeless_household",
            "is_true",
            Value::Null,
        )),
        // 다자녀 is two or more children on some notices and three or more
        // on others: three always qualifies, fewer is a named check.
        "multi_child" => Some(label_condition("children_count", "gte", json!(3))),
        _ => None,
    }
}

/// Conditions compiled from a 보조금24 record's codes: each restricted group
/// ([`group_rule`]) and a one-gender flag. Personal characteristics count
/// only when an individual, and not a household, applies: for a household
/// application they may be a household member's. Ages are served columns
/// (`min_age`/`max_age`), not rules; 중위소득 bands are read by
/// [`mazimi_core::income`].
///
/// Every condition here is a label (never hides). A review of prod rows
/// whose notice text was read found that 20–40% of each group's rows admit
/// people outside the flagged group ("노인·장애인·한부모 세대", "졸업 후 2년
/// 이내", "취업여부 무관"): the flag names who the notice mentions, not who
/// it restricts to.
pub fn gov_rule_conditions(conditions: &GovConditions) -> Vec<Value> {
    let mut out = Vec::new();
    let individual_only = conditions.applicant_types.as_ref().is_some_and(|kinds| {
        kinds.iter().any(|k| k == "individual") && !kinds.iter().any(|k| k == "household")
    });
    if individual_only {
        if let Some(rule) = conditions
            .target_groups
            .as_deref()
            .and_then(|groups| group_rule(groups, gov_personal_condition))
        {
            out.push(rule);
        }
    }
    if let Some(rule) = conditions
        .target_household_types
        .as_deref()
        .and_then(|kinds| group_rule(kinds, gov_household_condition))
    {
        out.push(rule);
    }
    if let Some(gender) = &conditions.target_gender {
        out.push(label_condition("gender", "eq", json!(gender)));
    }
    if conditions.for_parents_of_children {
        out.push(label_condition("children_count", "gte", json!(1)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The 8 probe personas' top-20 rows (prod, 2026-10-15) with their labels
    /// and the coded fields of each programme's stored payload. Rows are
    /// `persona#rank` keys.
    fn labelled_code_verdicts() -> Vec<(String, String, mazimi_core::rule_engine::RuleVerdict)> {
        use mazimi_core::rule_engine::{evaluate, parse_rule_json, RuleVerdict};
        let fixture: Value =
            serde_json::from_str(include_str!("../tests/fixtures/code_rules_labelled.json"))
                .expect("fixture is JSON");
        let mut out = Vec::new();
        for row in fixture["rows"].as_array().unwrap() {
            let persona = row["persona"].as_str().unwrap();
            let profile: mazimi_core::models::ProfileInput =
                serde_json::from_value(fixture["personas"][persona].clone()).unwrap();
            let programme = &fixture["programmes"][row["program_id"].as_str().unwrap()];
            let payload = &programme["payload"];
            let rules = match programme["source_type"].as_str().unwrap() {
                "youth_center" => youth_rule_conditions(&youth_conditions(payload)),
                "gov_benefits" => gov_rule_conditions(&gov_conditions(payload)),
                _ => Vec::new(),
            };
            let verdict = if rules.is_empty() {
                RuleVerdict::Pass
            } else {
                let tree = parse_rule_json(&json!({ "all": rules })).unwrap();
                evaluate(
                    &profile,
                    &tree,
                    &mazimi_core::rule_engine::AgeBasis::today(),
                )
                .verdict
            };
            let key = format!("{persona}#{}", row["rank"]);
            out.push((key, row["label"].as_str().unwrap().to_string(), verdict));
        }
        out
    }

    /// The independent review's false-hide examples: prod rows whose notice
    /// admits the profile that the first mapping hid. None may Fail.
    #[test]
    fn reviewer_false_hide_examples_never_fail() {
        use mazimi_core::rule_engine::{evaluate, parse_rule_json, RuleVerdict};
        let fixture: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/code_rules_review_cases.json"
        ))
        .expect("fixture is JSON");
        let cases = fixture["cases"].as_array().unwrap();
        assert_eq!(cases.len(), 57);
        let mut hidden = Vec::new();
        for case in cases {
            let payload = &case["payload"];
            let rules = match case["source_type"].as_str().unwrap() {
                "youth_center" => youth_rule_conditions(&youth_conditions(payload)),
                _ => gov_rule_conditions(&gov_conditions(payload)),
            };
            // 보조금24 groups and 계열 stay as named checks; 졸업-topped
            // school sets give no rule.
            let mapping = case["mapping"].as_str().unwrap();
            let checks_school = rules.iter().any(|r| r["field"] == "education_stage");
            if mapping == "schoolCd 0049004" {
                assert!(!checks_school, "{}", case["source_id"]);
            } else if mapping == "schoolCd school-level" {
                let school = rules.iter().find(|r| r["field"] == "education_stage");
                assert_eq!(
                    school.unwrap()["severity"],
                    "label",
                    "{}",
                    case["source_id"]
                );
            } else {
                assert!(!rules.is_empty(), "{mapping} {}", case["source_id"]);
            }
            if rules.is_empty() {
                continue;
            }
            let profile: mazimi_core::models::ProfileInput =
                serde_json::from_value(case["profile"].clone()).unwrap();
            let tree = parse_rule_json(&json!({ "all": rules })).unwrap();
            if evaluate(
                &profile,
                &tree,
                &mazimi_core::rule_engine::AgeBasis::today(),
            )
            .verdict
                == RuleVerdict::Fail
            {
                hidden.push(format!("{} {}", case["mapping"], case["source_id"]));
            }
        }
        assert!(hidden.is_empty(), "{} hidden: {hidden:?}", hidden.len());
    }

    /// Code rules alone must never hide a row a reviewer labelled correct,
    /// and they hide these labelled-wrong rows. The offline pilot found the
    /// first 15 (education stage and major); 보조금24 codes add the rest.
    #[test]
    fn labelled_rows_code_rules_hide_only_wrong_rows() {
        use mazimi_core::rule_engine::RuleVerdict;
        let rows = labelled_code_verdicts();
        assert_eq!(rows.len(), 160);
        let failed: Vec<String> = rows
            .iter()
            .filter(|(_, _, v)| *v == RuleVerdict::Fail)
            .map(|(key, label, _)| format!("{key} {label}"))
            .collect();
        let wrongly_hidden: Vec<&String> =
            failed.iter().filter(|r| r.ends_with(" correct")).collect();
        assert!(wrongly_hidden.is_empty(), "{wrongly_hidden:?}");
        // The pilot's 15 and the first 보조금24 mapping hid 18. After the
        // review, 보조금24 groups and plcyMajorCd are labels and sets topped
        // by a 졸업 code give no rule; what still hides is the 대졸 예정·대학
        // 졸업·석·박사 set (b6527c00) for a 휴학생 and a 고교생.
        let expected = ["P4#8 wrong-status", "P5#13 wrong-status"];
        let mut expected: Vec<String> = expected.iter().map(|r| r.to_string()).collect();
        let mut failed = failed;
        expected.sort();
        failed.sort();
        assert_eq!(failed, expected);
    }

    /// A 4th-year 재학생 may be 졸업 예정: a 대졸 예정 set hides a 2nd-year
    /// student but leaves a final-year or year-less student unknown.
    #[test]
    fn a_final_year_student_is_not_hidden_from_a_graduating_programme() {
        use mazimi_core::rule_engine::{evaluate, parse_rule_json, RuleVerdict};
        let c = youth_conditions(&json!({"schoolCd": "0049006"}));
        let tree = parse_rule_json(&json!({"all": youth_rule_conditions(&c)})).unwrap();
        let student = |year: Option<i32>| {
            let mut p: mazimi_core::models::ProfileInput = serde_json::from_value(json!({
                "birth_year": 2003, "region_code": "busan", "school_type": "university",
                "enrollment_status": "enrolled"}))
            .unwrap();
            p.school_year = year;
            evaluate(&p, &tree, &mazimi_core::rule_engine::AgeBasis::today()).verdict
        };
        assert_eq!(student(Some(2)), RuleVerdict::Fail);
        assert_eq!(student(Some(4)), RuleVerdict::Unknown);
        assert_eq!(student(None), RuleVerdict::Unknown);
        let rows = labelled_code_verdicts();
        let p1 = |key: &str| rows.iter().find(|(k, _, _)| k == key).unwrap().2;
        // 고졸 예정·고교 졸업·대졸 예정·대학 졸업 is topped by 대학 졸업: no rule.
        assert_eq!(p1("P1#2"), RuleVerdict::Pass);
    }

    #[test]
    fn school_codes_map_every_set_and_absent_answers_stay_unknown() {
        use mazimi_core::rule_engine::{evaluate, parse_rule_json, RuleVerdict};
        let verdict = |school_cd: &str, profile: Value| {
            let c = youth_conditions(&json!({ "schoolCd": school_cd }));
            let rules = youth_rule_conditions(&c);
            if rules.is_empty() {
                return None;
            }
            let tree = parse_rule_json(&json!({ "all": rules })).unwrap();
            let mut base = json!({"birth_year": 2000, "region_code": "seoul"});
            base.as_object_mut()
                .unwrap()
                .extend(profile.as_object().unwrap().clone());
            let p: mazimi_core::models::ProfileInput = serde_json::from_value(base).unwrap();
            Some(evaluate(&p, &tree, &mazimi_core::rule_engine::AgeBasis::today()).verdict)
        };
        let grad = json!({"school_type": "university", "enrollment_status": "graduated"});
        let hs = json!({"school_type": "high_school", "enrollment_status": "enrolled", "school_year": 2});
        // Sets with a school-level code are labels: named, never hidden
        // (review examples 20260923005400113587, 20251211005400211953 …).
        assert_eq!(
            verdict("0049002,0049005", grad.clone()),
            Some(RuleVerdict::Unknown)
        );
        let graduated = json!({"enrollment_status": "graduated"});
        for set in [
            "0049002,0049003",
            "0049003",
            "0049001,0049002,0049003,0049004,0049005,0049006",
        ] {
            assert_eq!(
                verdict(set, graduated.clone()),
                Some(RuleVerdict::Unknown),
                "{set}"
            );
        }
        // University-level sets stay hard; 대학 재학 admits 대학원생.
        assert_eq!(verdict("0049005", grad.clone()), Some(RuleVerdict::Fail));
        let grad_student = json!({"school_type": "graduate", "enrollment_status": "enrolled"});
        assert_eq!(
            verdict("0049005", grad_student.clone()),
            Some(RuleVerdict::Unknown)
        );
        assert_eq!(
            verdict("0049005,0049007,0049008", grad_student),
            Some(RuleVerdict::Pass)
        );
        assert_eq!(
            verdict("0049002,0049005", hs.clone()),
            Some(RuleVerdict::Pass)
        );
        // A set topped by 고교 졸업 or 대학 졸업 reads as "…이상" on many
        // notices: no rule at all (review examples 20251214005400211981 …).
        for set in [
            "0049004",
            "0049004,0049007",
            "0049007",
            "0049003,0049004",
            "0049005,0049007",
        ] {
            assert_eq!(verdict(set, hs.clone()), None, "{set}");
        }
        assert_eq!(
            verdict("0049004", json!({"enrollment_status": "enrolled"})),
            None
        );
        // 석·박사 on top keeps the rule.
        let leave_hs = json!({"school_type": "university", "enrollment_status": "on_leave"});
        assert_eq!(
            verdict("0049006,0049007,0049008", leave_hs),
            Some(RuleVerdict::Fail)
        );
        // 대학 재학 admits the final year. 휴학 is not proven 재학 (many
        // notices exclude 휴학생, audit M7): unknown, never hidden.
        let senior =
            json!({"school_type": "university", "enrollment_status": "enrolled", "school_year": 4});
        assert_eq!(verdict("0049005", senior), Some(RuleVerdict::Pass));
        let leave = json!({"school_type": "university", "enrollment_status": "on_leave"});
        assert_eq!(
            verdict("0049005", leave.clone()),
            Some(RuleVerdict::Unknown)
        );
        assert_eq!(verdict("0049006", leave), Some(RuleVerdict::Fail));
        // 대학원생 hold a first degree: 대학 졸업·석·박사 passes them.
        let postgrad = json!({"school_type": "graduate", "enrollment_status": "enrolled"});
        assert_eq!(
            verdict("0049007,0049008", postgrad),
            Some(RuleVerdict::Pass)
        );
        // Nothing known, or 재학 with no school type: never Fail on 재학 sets.
        assert_eq!(verdict("0049002", json!({})), Some(RuleVerdict::Unknown));
        let enrolled = json!({"enrollment_status": "enrolled"});
        assert_eq!(verdict("0049002", enrolled), Some(RuleVerdict::Unknown));
        // 기타, 제한없음, blank or an unknown code: no rule.
        for value in [
            "0049009",
            "0049005,0049009",
            "0049010",
            "",
            "0049099,0049005",
        ] {
            assert_eq!(verdict(value, json!({})), None, "{value}");
        }
    }

    #[test]
    fn major_codes_map_to_categories_with_overlaps_and_free_text_stays_unknown() {
        use mazimi_core::rule_engine::{evaluate, parse_rule_json, RuleVerdict};
        let verdict = |major_cd: &str, major_group: Option<&str>| {
            let c = youth_conditions(&json!({ "plcyMajorCd": major_cd }));
            let rules = youth_rule_conditions(&c);
            if rules.is_empty() {
                return None;
            }
            let tree = parse_rule_json(&json!({ "all": rules })).unwrap();
            let p: mazimi_core::models::ProfileInput = serde_json::from_value(
                json!({"birth_year": 2000, "region_code": "seoul", "major_group": major_group}),
            )
            .unwrap();
            Some(evaluate(&p, &tree, &mazimi_core::rule_engine::AgeBasis::today()).verdict)
        };
        // A label: a different 계열 is a named check, never a hide.
        assert_eq!(
            verdict("0011005", Some("humanities")),
            Some(RuleVerdict::Unknown)
        );
        let c = youth_conditions(&json!({ "plcyMajorCd": "0011005" }));
        assert_eq!(youth_rule_conditions(&c)[0]["severity"], "label");
        assert_eq!(verdict("0011005", Some("공학")), Some(RuleVerdict::Pass));
        assert_eq!(
            verdict("0011005", Some("컴퓨터공학과")),
            Some(RuleVerdict::Unknown)
        );
        assert_eq!(verdict("0011005", None), Some(RuleVerdict::Unknown));
        // 상경 sits under 사회 in the usual split; 농학 under 자연.
        assert_eq!(
            verdict("0011003", Some("social_science")),
            Some(RuleVerdict::Pass)
        );
        assert_eq!(
            verdict("0011007", Some("natural_science")),
            Some(RuleVerdict::Pass)
        );
        assert_eq!(
            verdict("0011007", Some("engineering")),
            Some(RuleVerdict::Unknown)
        );
        for value in ["0011008", "0011005,0011008", "0011009", "0011099"] {
            assert_eq!(verdict(value, Some("humanities")), None, "{value}");
        }
    }

    #[test]
    fn gov_groups_become_rules_only_when_every_member_maps() {
        use mazimi_core::rule_engine::{evaluate, parse_rule_json, RuleVerdict};
        let rules = |applicant: &str, flags: &[&str]| {
            let mut conditions = serde_json::Map::new();
            for flag in flags {
                conditions.insert((*flag).into(), json!("Y"));
            }
            gov_rule_conditions(&gov_conditions(
                &json!({"사용자구분": applicant, "supportConditions": conditions}),
            ))
        };
        let verdict = |rules: &[Value], profile: Value| {
            let tree = parse_rule_json(&json!({ "all": rules })).unwrap();
            let mut base = json!({"birth_year": 2000, "region_code": "seoul"});
            base.as_object_mut()
                .unwrap()
                .extend(profile.as_object().unwrap().clone());
            let p: mazimi_core::models::ProfileInput = serde_json::from_value(base).unwrap();
            evaluate(&p, &tree, &mazimi_core::rule_engine::AgeBasis::today()).verdict
        };
        // 대학생 only.
        let student = rules("개인", &["JA0320"]);
        assert_eq!(student.len(), 1);
        let employed_grad = json!({"school_type": "university", "enrollment_status": "graduated",
            "employment_status": "employed"});
        // Every 보조금24 condition is a label: a contradiction is Unknown.
        assert_eq!(
            verdict(&student, employed_grad.clone()),
            RuleVerdict::Unknown
        );
        assert_eq!(student[0]["severity"], "label");
        assert_eq!(
            verdict(&student, json!({"enrollment_status": "enrolled"})),
            RuleVerdict::Unknown
        );
        // 휴학 is not proven 재학 (audit M7).
        assert_eq!(
            verdict(
                &student,
                json!({"school_type": "university", "enrollment_status": "on_leave"})
            ),
            RuleVerdict::Unknown
        );
        assert_eq!(
            verdict(
                &student,
                json!({"school_type": "university", "enrollment_status": "enrolled", "school_year": 2})
            ),
            RuleVerdict::Pass
        );
        assert_eq!(
            verdict(
                &student,
                json!({"school_type": "graduate", "enrollment_status": "enrolled"})
            ),
            RuleVerdict::Unknown
        );
        // 구직자 or 장애인.
        let either = rules("개인", &["JA0327", "JA0328"]);
        assert_eq!(either.len(), 1);
        assert!(either[0]["any"].is_array());
        let disabled_worker = json!({"employment_status": "employed", "has_disability": true});
        assert_eq!(verdict(&either, disabled_worker), RuleVerdict::Pass);
        assert_eq!(
            verdict(&either, employed_grad.clone()),
            RuleVerdict::Unknown
        );
        let seeker = rules("개인", &["JA0327"]);
        assert_eq!(verdict(&seeker, employed_grad), RuleVerdict::Unknown);
        assert_eq!(verdict(&seeker, json!({})), RuleVerdict::Unknown);
        // An unmapped member (농업인, 국가보훈대상자) is the unanswered
        // target_group: never Pass on the mapped member's answer alone.
        let farmer_or_student = rules("개인", &["JA0320", "JA0313"]);
        assert_eq!(farmer_or_student.len(), 1);
        let enrolled =
            json!({"school_type": "university", "enrollment_status": "enrolled", "school_year": 2});
        assert_eq!(verdict(&farmer_or_student, enrolled), RuleVerdict::Pass);
        assert_eq!(verdict(&farmer_or_student, json!({})), RuleVerdict::Unknown);
        assert_eq!(
            verdict(&rules("개인", &["JA0329"]), json!({})),
            RuleVerdict::Unknown
        );
        // A member that restricts nobody (근로자, school pupils: a parent
        // applies), the none flag or a household application: no personal rule.
        assert!(rules("개인", &["JA0326"]).is_empty());
        assert!(rules("개인", &["JA0326", "JA0327"]).is_empty());
        assert!(rules("개인", &["JA0320", "JA0322"]).is_empty());
        assert!(rules("개인||가구", &["JA0320"]).is_empty());
        assert!(rules("가구", &["JA0327"]).is_empty());
        assert!(rules("개인", &["JA0319"]).is_empty());
        assert!(gov_rule_conditions(&gov_conditions(
            &json!({"supportConditions": {"JA0320": "Y"}})
        ))
        .is_empty());
        // Household kinds the profile answers, for any applicant.
        let single = rules("가구", &["JA0404"]);
        assert_eq!(
            verdict(&single, json!({"household_size": 1})),
            RuleVerdict::Pass
        );
        assert_eq!(
            verdict(&single, json!({"household_size": 3})),
            RuleVerdict::Unknown
        );
        assert_eq!(verdict(&single, json!({})), RuleVerdict::Unknown);
        let multicultural = rules("개인", &["JA0401", "JA0404"]);
        assert_eq!(
            verdict(
                &multicultural,
                json!({"household_size": 3, "is_multicultural_family": true})
            ),
            RuleVerdict::Pass
        );
        // 1인 가구 or 다자녀 (three children always qualify).
        let single_or_many = rules("개인", &["JA0404", "JA0411"]);
        assert_eq!(
            verdict(
                &single_or_many,
                json!({"household_size": 5, "children_count": 3})
            ),
            RuleVerdict::Pass
        );
        assert_eq!(
            verdict(
                &single_or_many,
                json!({"household_size": 4, "children_count": 2})
            ),
            RuleVerdict::Unknown
        );
        assert!(rules("개인", &["JA0404", "JA0410"]).is_empty());
    }

    /// Rows are verbatim `supportConditions` records from the provider (one
    /// throttled call, 2026-10-12), plus the matching `사용자구분`.
    #[test]
    fn real_support_condition_rows_map_to_reviewed_fields() {
        let table: Vec<Value> = serde_json::from_str(include_str!(
            "../tests/fixtures/gov_support_conditions.json"
        ))
        .expect("fixture is JSON");
        assert!(table.len() >= 3);
        for case in table {
            let payload = json!({
                "사용자구분": case["사용자구분"],
                "supportConditions": case["supportConditions"],
            });
            let actual = serde_json::to_value(gov_conditions(&payload)).unwrap();
            assert_eq!(actual, case["expected"], "{}", case["name"]);
        }
    }

    #[test]
    fn a_group_restricts_only_without_its_none_flag() {
        let only_disabled = json!({"사용자구분": "개인", "supportConditions": {
            "JA0328": "Y", "JA0322": null, "JA0110": 0, "JA0111": 150}});
        let mapped = gov_conditions(&only_disabled);
        assert_eq!(mapped.target_groups, Some(vec!["disabled".to_string()]));
        assert_eq!((mapped.min_age, mapped.max_age), (None, None));
        let anyone = json!({"supportConditions": {"JA0328": "Y", "JA0322": "Y"}});
        assert_eq!(gov_conditions(&anyone).target_groups, None);
        let nothing = json!({"supportConditions": {}});
        assert_eq!(gov_conditions(&nothing), GovConditions::default());
    }

    #[test]
    fn a_child_band_on_a_birth_service_is_not_the_applicants_age() {
        let birth =
            json!({"supportConditions": {"JA0302": "Y", "JA0303": "Y", "JA0110": 0, "JA0111": 1}});
        assert_eq!(gov_conditions(&birth).max_age, None);
        // A pupil's band is the child's: dropped, the parent is asked about
        // children (audit M7).
        let child = json!({"supportConditions": {"JA0317": "Y", "JA0110": 6, "JA0111": 12}});
        let mapped = gov_conditions(&child);
        assert_eq!((mapped.min_age, mapped.max_age), (None, None));
        assert!(mapped.for_parents_of_children);
        let inverted = json!({"supportConditions": {"JA0110": 40, "JA0111": 19}});
        assert_eq!(gov_conditions(&inverted).min_age, None);
    }

    /// Audit M7: a youth band (13–18) an individual applies for is the
    /// applicant's own; a band that is a child's (a pupil group, a
    /// parent-facing flag, or under 13) is dropped and the applicant is
    /// asked about children instead.
    #[test]
    fn only_a_childs_band_is_dropped_and_then_children_are_a_named_check() {
        use mazimi_core::rule_engine::RuleVerdict;
        let teen = json!({"사용자구분": "개인", "supportConditions": {"JA0110": 13, "JA0111": 18}});
        let mapped = gov_conditions(&teen);
        assert_eq!((mapped.min_age, mapped.max_age), (Some(13), Some(18)));
        assert!(gov_rule_conditions(&mapped).is_empty());
        for flags in [
            json!({"JA0319": "Y", "JA0110": 16, "JA0111": 18}),
            json!({"JA0110": 3, "JA0111": 5}),
        ] {
            let mapped = gov_conditions(&json!({"사용자구분": "개인", "supportConditions": flags}));
            assert_eq!((mapped.min_age, mapped.max_age), (None, None), "{flags}");
            let rules = gov_rule_conditions(&mapped);
            assert_eq!(
                verdict_of(&rules, json!({"children_count": 0})),
                RuleVerdict::Unknown
            );
            assert_eq!(
                verdict_of(&rules, json!({"children_count": 1})),
                RuleVerdict::Pass
            );
        }
    }

    /// Review M7: a child band on a programme an individual or household
    /// applies for is the child's age: 유아학비 (3–5) is applied for by a
    /// 30-year-old parent and must not read as the applicant's age.
    #[test]
    fn a_child_band_on_a_household_application_is_not_the_applicants_age() {
        let kindergarten =
            json!({"사용자구분": "개인", "supportConditions": {"JA0110": 3, "JA0111": 5}});
        let mapped = gov_conditions(&kindergarten);
        assert_eq!((mapped.min_age, mapped.max_age), (None, None));
        let household =
            json!({"사용자구분": "개인||가구", "supportConditions": {"JA0110": 6, "JA0111": 12}});
        assert_eq!(gov_conditions(&household).max_age, None);
        // An adult band is the applicant's own.
        let youth =
            json!({"사용자구분": "개인", "supportConditions": {"JA0110": 19, "JA0111": 34}});
        let mapped = gov_conditions(&youth);
        assert_eq!((mapped.min_age, mapped.max_age), (Some(19), Some(34)));
    }

    #[test]
    fn applicant_kinds_are_exact_or_unknown() {
        assert_eq!(
            applicant_types(Some("개인||가구")),
            Some(vec!["household".into(), "individual".into()])
        );
        assert_eq!(
            applicant_types(Some("법인/시설/단체")),
            Some(vec!["organization".into()])
        );
        assert_eq!(applicant_types(Some("개인||새유형")), None);
        assert_eq!(applicant_types(None), None);
    }

    /// Code sets as they occur in production snapshots (2026-10-12).
    #[test]
    fn youth_codes_map_only_what_the_profile_can_answer() {
        let p = json!({"jobCd": "0013003", "schoolCd": "0049005,0049006",
            "sbizCd": "0014005", "mrgSttsCd": "0055002", "plcyMajorCd": "0011009",
            "earnCndSeCd": "0043002", "earnMinAmt": "0", "earnMaxAmt": "5000"});
        let c = youth_conditions(&p);
        assert_eq!(
            c.employment_statuses,
            Some(vec![
                "preparing".into(),
                "student".into(),
                "unemployed".into()
            ])
        );
        assert_eq!(
            c.enrollment_statuses,
            Some(vec!["enrolled".into(), "on_leave".into()])
        );
        assert_eq!(c.target_groups, Some(vec!["disabled".into()]));
        assert_eq!(c.marital_status.as_deref(), Some("unmarried"));
        assert_eq!(c.major_requirements, None);
        assert_eq!(c.major_categories, None);
        assert_eq!(
            c.education_stages,
            Some(vec![
                "univ_enrolled".into(),
                "univ_expected".into(),
                "postgrad".into()
            ])
        );
        assert_eq!(
            (c.income_annual_min_manwon, c.income_annual_max_manwon),
            (None, Some(5000))
        );
        let rules = youth_rule_conditions(&c);
        // employment, education stage, 장애인, 미혼.
        assert_eq!(rules.len(), 4);
        for rule in &rules {
            mazimi_core::rule_engine::parse_rule_json(&json!({"all": [rule]})).unwrap();
        }

        // 제한없음, (예비)창업자, 기타 and school-level codes restrict nothing.
        for (field, value) in [
            ("jobCd", "0013010"),
            ("jobCd", "0013003,0013006"),
            ("jobCd", "0013001,0013009"),
            ("jobCd", ""),
            ("jobCd", "0013099"),
        ] {
            let c = youth_conditions(&json!({ field: value }));
            assert_eq!(c.employment_statuses, None, "{field}={value}");
        }
        for value in ["0049010", "0049002,0049005", "0049004", "0049005,0049009"] {
            let c = youth_conditions(&json!({ "schoolCd": value }));
            assert_eq!(c.enrollment_statuses, None, "{value}");
        }
        // 농업인 has no profile field: the set is 장애인 or the unanswered
        // target_group (see an_unanswerable_coded_group_is_unknown_never_pass).
        let c = youth_conditions(&json!({"sbizCd": "0014005,0014006"}));
        assert!(youth_rule_conditions(&c)[0]["any"].is_array());
        let c = youth_conditions(&json!({"sbizCd": "0014003,0014005"}));
        assert_eq!(youth_rule_conditions(&c).len(), 1);
        assert!(youth_rule_conditions(&c)[0]["any"].is_array());
        let c = youth_conditions(
            &json!({"earnCndSeCd": "0043003", "earnEtcCn": " 중위소득 150% 이하 "}),
        );
        assert_eq!(
            c.income_condition_text.as_deref(),
            Some("중위소득 150% 이하")
        );
    }

    #[test]
    fn employed_youth_fail_an_unemployed_only_rule_and_unknown_stays_visible() {
        use mazimi_core::rule_engine::{evaluate, parse_rule_json, RuleVerdict};
        let c = youth_conditions(&json!({"jobCd": "0013003"}));
        let tree = parse_rule_json(&json!({"all": youth_rule_conditions(&c)})).unwrap();
        let mut input: mazimi_core::models::ProfileInput =
            serde_json::from_value(json!({"birth_year": 1995, "region_code": "seoul"})).unwrap();
        assert_eq!(
            evaluate(&input, &tree, &mazimi_core::rule_engine::AgeBasis::today()).verdict,
            RuleVerdict::Unknown
        );
        input.employment_status = Some("employed".into());
        assert_eq!(
            evaluate(&input, &tree, &mazimi_core::rule_engine::AgeBasis::today()).verdict,
            RuleVerdict::Fail
        );
        input.employment_status = Some("unemployed".into());
        assert_eq!(
            evaluate(&input, &tree, &mazimi_core::rule_engine::AgeBasis::today()).verdict,
            RuleVerdict::Pass
        );
    }

    /// Audit H1: every coded restriction leaves the worker as a condition
    /// of the one eligibility path, or is explicitly not a restriction.
    fn verdict_of(rules: &[Value], profile: Value) -> mazimi_core::rule_engine::RuleVerdict {
        use mazimi_core::rule_engine::{evaluate, parse_rule_json};
        let tree = parse_rule_json(&json!({ "all": rules })).unwrap();
        let mut base = json!({"birth_year": 2000, "region_code": "seoul"});
        base.as_object_mut()
            .unwrap()
            .extend(profile.as_object().unwrap().clone());
        let p: mazimi_core::models::ProfileInput = serde_json::from_value(base).unwrap();
        evaluate(&p, &tree, &mazimi_core::rule_engine::AgeBasis::today()).verdict
    }

    #[test]
    fn coded_marital_status_becomes_a_condition() {
        use mazimi_core::rule_engine::RuleVerdict;
        let married = youth_rule_conditions(&youth_conditions(&json!({"mrgSttsCd": "0055001"})));
        assert!(
            married.iter().any(|r| r["field"] == "marital_status"),
            "{married:?}"
        );
        assert_eq!(
            verdict_of(&married, json!({"marital_status": "unmarried"})),
            RuleVerdict::Unknown
        );
        assert_eq!(
            verdict_of(&married, json!({"marital_status": "married"})),
            RuleVerdict::Pass
        );
        let single = youth_rule_conditions(&youth_conditions(&json!({"mrgSttsCd": "0055002"})));
        assert_eq!(
            verdict_of(&single, json!({"marital_status": "married"})),
            RuleVerdict::Fail
        );
        assert_eq!(verdict_of(&single, json!({})), RuleVerdict::Unknown);
        assert!(
            youth_rule_conditions(&youth_conditions(&json!({"mrgSttsCd": "0055003"}))).is_empty()
        );
    }

    #[test]
    fn an_unanswerable_coded_group_is_unknown_never_pass() {
        use mazimi_core::rule_engine::RuleVerdict;
        for code in [
            "0014007", "0014001", "0014008", "0014006", "0014004", "0014002",
        ] {
            let rules = youth_rule_conditions(&youth_conditions(&json!({ "sbizCd": code })));
            assert!(!rules.is_empty(), "{code}");
            assert_eq!(
                verdict_of(&rules, json!({})),
                RuleVerdict::Unknown,
                "{code}"
            );
        }
        // 장애인 or 농업인: a disabled user passes, nobody is hidden.
        let rules = youth_rule_conditions(&youth_conditions(&json!({"sbizCd": "0014005,0014006"})));
        assert_eq!(
            verdict_of(&rules, json!({"has_disability": true})),
            RuleVerdict::Pass
        );
        assert_eq!(
            verdict_of(&rules, json!({"has_disability": false})),
            RuleVerdict::Unknown
        );
        // 보조금24: women only, 농업인 only, 국가보훈대상자 only.
        for flags in [
            json!({"JA0102": "Y"}),
            json!({"JA0101": "Y"}),
            json!({"JA0313": "Y"}),
            json!({"JA0329": "Y"}),
            json!({"JA0402": "Y"}),
        ] {
            let rules = gov_rule_conditions(&gov_conditions(
                &json!({"사용자구분": "개인", "supportConditions": flags}),
            ));
            assert!(!rules.is_empty(), "{flags}");
            assert_eq!(
                verdict_of(&rules, json!({})),
                RuleVerdict::Unknown,
                "{flags}"
            );
        }
    }

    /// Every code of every group table compiles to a condition alone, or is
    /// listed in [`NOT_A_RESTRICTION`]; every key is in the core vocabulary.
    #[test]
    fn every_coded_group_is_a_condition_or_explicitly_not_a_restriction() {
        let vocabulary = mazimi_core::models::TARGET_GROUPS;
        let mut missing = Vec::new();
        for (code, key) in SBIZ_CODES {
            assert!(vocabulary.contains(key), "{key}");
            let rules = youth_rule_conditions(&youth_conditions(&json!({ "sbizCd": code })));
            if rules.is_empty() && !NOT_A_RESTRICTION.contains(key) {
                missing.push(*code);
            }
        }
        for (table, field) in [(GOV_PERSONAL, "개인"), (GOV_HOUSEHOLD, "가구")] {
            for (code, key) in table {
                assert!(vocabulary.contains(key), "{key}");
                let rules = gov_rule_conditions(&gov_conditions(
                    &json!({"사용자구분": "개인", "supportConditions": { *code: "Y" }}),
                ));
                if rules.is_empty() && !NOT_A_RESTRICTION.contains(key) {
                    missing.push(*code);
                }
                let _ = field;
            }
        }
        for code in ["JA0101", "JA0102"] {
            let rules = gov_rule_conditions(&gov_conditions(
                &json!({"supportConditions": { code: "Y" }}),
            ));
            if rules.is_empty() {
                missing.push(code);
            }
        }
        assert!(missing.is_empty(), "codes with no condition: {missing:?}");
        for rule in [
            youth_rule_conditions(&youth_conditions(&json!({"sbizCd": "0014001,0014007"}))),
            gov_rule_conditions(&gov_conditions(
                &json!({"사용자구분": "개인", "supportConditions": {"JA0313": "Y", "JA0102": "Y"}}),
            )),
        ] {
            mazimi_core::rule_engine::parse_rule_json(&json!({ "all": rule })).unwrap();
        }
    }
}
