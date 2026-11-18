//! Rule-engine views derived from existing profile answers: the education
//! stage (from `school_type`, `enrollment_status` and `school_year`) and the
//! major category (from `major_group`). They add no profile field; they let
//! source codes such as 온통청년 `schoolCd` and `plcyMajorCd` be checked
//! against what the user already told us.
//!
//! Both return every value the answers allow. The rule engine passes a
//! condition only when all of them pass and fails it only when none does,
//! so an answer that cannot tell two stages apart stays `Unknown`.

/// 온통청년 `schoolCd` 0049001–0049008 in order: the highest education stage
/// a person is at now. `univ_on_leave` (대학 휴학) has no code: no source
/// set lists it, so a student on leave is never proven 재학 by a code; a
/// review may name it.
pub const EDUCATION_STAGES: &[&str] = &[
    "hs_below",
    "hs_enrolled",
    "hs_expected",
    "hs_graduate",
    "univ_enrolled",
    "univ_on_leave",
    "univ_expected",
    "univ_graduate",
    "postgrad",
];

/// Korean label for an education stage code.
pub fn education_stage_label(stage: &str) -> &'static str {
    match stage.trim_matches('"') {
        "hs_below" => "고졸 미만",
        "hs_enrolled" => "고교 재학",
        "hs_expected" => "고졸 예정",
        "hs_graduate" => "고교 졸업",
        "univ_enrolled" => "대학 재학",
        "univ_on_leave" => "대학 휴학",
        "univ_expected" => "대졸 예정",
        "univ_graduate" => "대학 졸업",
        "postgrad" => "석·박사",
        _ => "기타",
    }
}

/// Canonical `school_type`. `None` means unrecognised.
pub fn canonical_school_type(value: &str) -> Option<&'static str> {
    match value.trim().trim_matches('"').to_lowercase().as_str() {
        "high_school" | "고등학교" | "고교" | "고등학생" => Some("high_school"),
        "university" | "대학교" | "4년제" | "4년제 대학" | "4년제대학" => {
            Some("university")
        }
        "college" | "전문대" | "전문대학" => Some("college"),
        "graduate" | "graduate_school" | "대학원" | "대학원생" => Some("graduate"),
        _ => None,
    }
}

/// Every school type a stored `school_type` answer may mean. The app asks
/// "대학 (전문대 포함)" and stores `university`, so that answer cannot tell
/// 4년제 from 전문대: a condition on that split stays Unknown for it, and
/// only undergraduate vs 대학원 is decided. An unrecognised value is kept
/// as given.
pub fn school_type_candidates(value: &str) -> Vec<&str> {
    match canonical_school_type(value) {
        Some("university") => vec!["university", "college"],
        Some(code) => vec![code],
        None => vec![value],
    }
}

/// Korean label for a `school_type` value (code or alias); "기타" when unrecognised.
pub fn school_type_label(value: &str) -> &'static str {
    match canonical_school_type(value) {
        Some("high_school") => "고등학교",
        Some("university") => "대학교",
        Some("college") => "전문대학",
        Some("graduate") => "대학원",
        _ => "기타",
    }
}

#[derive(Clone, Copy)]
enum Level {
    HighSchool,
    /// A degree course whose final year is this one or later.
    Degree(i32),
    Graduate,
}

/// The stages a person at `level` may be at. 졸업 예정 is told apart from 재학
/// only by the school year: a year before the final one is never 졸업 예정,
/// anything else (or no year) may be either.
fn level_stages(level: Level, enrollment: Option<&str>, year: Option<i32>) -> Vec<&'static str> {
    // A high school has no on-leave stage: 휴학 there reads as 재학.
    let (enrolled, on_leave, expected, graduate, final_year) = match level {
        Level::HighSchool => ("hs_enrolled", None, "hs_expected", "hs_graduate", 3),
        Level::Degree(final_year) => (
            "univ_enrolled",
            Some("univ_on_leave"),
            "univ_expected",
            "univ_graduate",
            final_year,
        ),
        // 대학원 students also hold a first degree: both readings.
        Level::Graduate => return vec!["univ_graduate", "postgrad"],
    };
    match enrollment {
        None => vec![enrolled, expected, graduate],
        Some("enrolled") => match year {
            Some(y) if y < final_year => vec![enrolled],
            _ => vec![enrolled, expected],
        },
        // On leave: not finishing this term, and not 재학 for a notice that
        // excludes 휴학생: both readings.
        Some("on_leave") => [Some(enrolled), on_leave].into_iter().flatten().collect(),
        Some("graduated") => vec![graduate],
        // 수료 후 연구생 is a graduate-school status.
        _ => Vec::new(),
    }
}

/// Every education stage the answers allow, in [`EDUCATION_STAGES`] order;
/// `None` when nothing is known or the answers contradict each other. An
/// unrecognised `school_type` counts as not answered.
pub fn education_stage_candidates(
    school_type: Option<&str>,
    enrollment_status: Option<&str>,
    school_year: Option<i32>,
) -> Option<Vec<&'static str>> {
    let enrollment = enrollment_status.and_then(crate::canonical_enrollment_status);
    let year = school_year.filter(|y| (1..=10).contains(y));
    let levels: Vec<Level> = match school_type.and_then(canonical_school_type) {
        Some("high_school") => vec![Level::HighSchool],
        Some("university") => vec![Level::Degree(4)],
        Some("college") => vec![Level::Degree(2)],
        Some(_) => vec![Level::Graduate],
        // "재학 중" covers 고등학교, 대학 and 대학원 alike.
        None if enrollment == Some("research_student") => vec![Level::Graduate],
        None if enrollment.is_some() => vec![
            Level::HighSchool,
            Level::Degree(2),
            Level::Degree(4),
            Level::Graduate,
        ],
        None => return None,
    };
    let mut stages: Vec<&'static str> = levels
        .into_iter()
        .flat_map(|level| level_stages(level, enrollment, year))
        .collect();
    stages.sort_by_key(|s| EDUCATION_STAGES.iter().position(|x| x == s));
    stages.dedup();
    (!stages.is_empty()).then_some(stages)
}

/// The stages a source's accepted set really admits. 졸업 예정 sits between
/// 재학 and 졸업 and is admitted by either: a programme open to 대학 재학 is
/// open to a final-year student, and one coded 대학 졸업 is usually for
/// "졸업(예정)자".
/// 대학 재학 also admits 대학원생: notices coded 대학 재학 often say
/// "대학생·대학원생" (부산 천원의 아침밥, `20260930005400213691`).
pub fn admitted_education_stages(accepted: &[&str]) -> Vec<&'static str> {
    EDUCATION_STAGES
        .iter()
        .copied()
        .filter(|stage| {
            accepted.contains(stage)
                || (*stage == "univ_expected"
                    && (accepted.contains(&"univ_enrolled") || accepted.contains(&"univ_graduate")))
                || (*stage == "hs_expected"
                    && (accepted.contains(&"hs_enrolled") || accepted.contains(&"hs_graduate")))
                || (*stage == "postgrad" && accepted.contains(&"univ_enrolled"))
        })
        .collect()
}

/// 온통청년 `plcyMajorCd` 계열 (0011001–0011007).
pub const MAJOR_CATEGORIES: &[&str] = &[
    "humanities",
    "social_science",
    "business",
    "natural_science",
    "engineering",
    "arts_sports",
    "agriculture",
];

pub fn major_category_label(category: &str) -> &'static str {
    match category.trim_matches('"') {
        "humanities" => "인문계열",
        "social_science" => "사회계열",
        "business" => "상경계열",
        "natural_science" => "이학계열",
        "engineering" => "공학계열",
        "arts_sports" => "예체능계열",
        "agriculture" => "농산업계열",
        _ => "기타",
    }
}

/// The 계열 a free-form `major_group` answer names; `None` when it names no
/// single one (a department name, "it", "stem", 의약, 교육 …).
pub fn canonical_major_category(value: &str) -> Option<&'static str> {
    match value.trim().trim_matches('"').to_lowercase().as_str() {
        "humanities" | "인문" | "인문계열" => Some("humanities"),
        "social_science" | "사회" | "사회계열" | "사회과학" => Some("social_science"),
        "business" | "상경" | "상경계열" => Some("business"),
        "natural_science" | "science" | "자연" | "자연계열" | "자연과학" | "이학" | "이학계열" => {
            Some("natural_science")
        }
        "engineering" | "공학" | "공학계열" => Some("engineering"),
        "arts_sports" | "예체능" | "예체능계열" => Some("arts_sports"),
        "agriculture" | "농업" | "농산업" | "농산업계열" => Some("agriculture"),
        _ => None,
    }
}

/// The categories a source's accepted set really admits. The usual 7계열
/// split files 상경 under 사회 and 농학 under 자연, so a student who says 사회
/// may be a 상경 student and vice versa: overlapping categories admit each
/// other rather than exclude someone who may qualify.
pub fn admitted_major_categories(accepted: &[&str]) -> Vec<&'static str> {
    const OVERLAPS: &[(&str, &str)] = &[
        ("business", "social_science"),
        ("social_science", "business"),
        ("agriculture", "natural_science"),
        ("natural_science", "agriculture"),
    ];
    MAJOR_CATEGORIES
        .iter()
        .copied()
        .filter(|category| {
            accepted.contains(category)
                || OVERLAPS
                    .iter()
                    .any(|(from, to)| to == category && accepted.contains(from))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stages(t: Option<&str>, e: Option<&str>, y: Option<i32>) -> Option<Vec<&'static str>> {
        education_stage_candidates(t, e, y)
    }

    /// Review B1 / verify V-L3: the app's "대학 (전문대 포함)" answer means
    /// either; every other answer means itself; an unknown value is kept.
    #[test]
    fn a_university_answer_means_4year_or_junior_college() {
        assert_eq!(
            school_type_candidates("university"),
            ["university", "college"]
        );
        assert_eq!(school_type_candidates("4년제"), ["university", "college"]);
        assert_eq!(school_type_candidates("전문대학"), ["college"]);
        assert_eq!(school_type_candidates("graduate"), ["graduate"]);
        assert_eq!(school_type_candidates("해외대학"), ["해외대학"]);
    }

    #[test]
    fn final_year_students_may_be_graduating_earlier_years_are_not() {
        let u = Some("university");
        assert_eq!(
            stages(u, Some("enrolled"), Some(2)),
            Some(vec!["univ_enrolled"])
        );
        assert_eq!(
            stages(u, Some("enrolled"), Some(4)),
            Some(vec!["univ_enrolled", "univ_expected"])
        );
        assert_eq!(
            stages(u, Some("enrolled"), None),
            Some(vec!["univ_enrolled", "univ_expected"])
        );
        // 5–6 year courses and 초과학기 stay ambiguous.
        assert_eq!(
            stages(u, Some("enrolled"), Some(6)),
            Some(vec!["univ_enrolled", "univ_expected"])
        );
        assert_eq!(
            stages(Some("college"), Some("enrolled"), Some(2)),
            Some(vec!["univ_enrolled", "univ_expected"])
        );
        assert_eq!(
            stages(Some("high_school"), Some("enrolled"), Some(3)),
            Some(vec!["hs_enrolled", "hs_expected"])
        );
        assert_eq!(
            stages(u, Some("on_leave"), Some(4)),
            Some(vec!["univ_enrolled", "univ_on_leave"])
        );
        assert_eq!(
            stages(u, Some("graduated"), None),
            Some(vec!["univ_graduate"])
        );
        assert_eq!(
            stages(Some("대학원"), Some("enrolled"), None),
            Some(vec!["univ_graduate", "postgrad"])
        );
    }

    /// Audit M7: many notices say "휴학생 제외", so 휴학 is not proof of
    /// 재학. A degree student on leave reads as either; a code set that
    /// admits 재학 alone leaves them unknown.
    #[test]
    fn a_student_on_leave_is_not_proven_enrolled() {
        assert_eq!(
            stages(Some("university"), Some("on_leave"), Some(4)),
            Some(vec!["univ_enrolled", "univ_on_leave"])
        );
        assert_eq!(
            stages(Some("college"), Some("on_leave"), None),
            Some(vec!["univ_enrolled", "univ_on_leave"])
        );
        assert!(!admitted_education_stages(&["univ_enrolled"]).contains(&"univ_on_leave"));
        // 졸업 예정 is admitted by a 졸업 set, at both levels (verifier gap 12).
        assert!(admitted_education_stages(&["univ_graduate"]).contains(&"univ_expected"));
        assert!(admitted_education_stages(&["hs_graduate"]).contains(&"hs_expected"));
        assert!(EDUCATION_STAGES.contains(&"univ_on_leave"));
        assert_eq!(education_stage_label("univ_on_leave"), "대학 휴학");
    }

    #[test]
    fn missing_or_unrecognised_answers_widen_never_narrow() {
        assert_eq!(stages(None, None, Some(2)), None);
        assert_eq!(
            stages(Some("university"), Some("garbage"), None),
            Some(vec!["univ_enrolled", "univ_expected", "univ_graduate"])
        );
        // "재학 중" with no school type: high school, degree or graduate school.
        assert_eq!(
            stages(None, Some("재학"), None),
            Some(vec![
                "hs_enrolled",
                "hs_expected",
                "univ_enrolled",
                "univ_expected",
                "univ_graduate",
                "postgrad"
            ])
        );
        assert_eq!(
            stages(Some("middle_school"), Some("graduated"), None),
            Some(vec!["hs_graduate", "univ_graduate", "postgrad"])
        );
        assert_eq!(
            stages(None, Some("research_student"), None),
            Some(vec!["univ_graduate", "postgrad"])
        );
        // Contradictory answers are not guessed at.
        assert_eq!(
            stages(Some("high_school"), Some("research_student"), None),
            None
        );
        // A nonsensical year is ignored.
        assert_eq!(
            stages(Some("university"), Some("enrolled"), Some(0)),
            Some(vec!["univ_enrolled", "univ_expected"])
        );
    }

    #[test]
    fn enrolled_sets_admit_the_graduating_and_majors_admit_overlaps() {
        assert_eq!(
            admitted_education_stages(&["univ_enrolled"]),
            vec!["univ_enrolled", "univ_expected", "postgrad"]
        );
        assert_eq!(
            admitted_education_stages(&["univ_expected", "hs_enrolled"]),
            vec!["hs_enrolled", "hs_expected", "univ_expected"]
        );
        assert_eq!(
            admitted_major_categories(&["engineering"]),
            vec!["engineering"]
        );
        assert_eq!(
            admitted_major_categories(&["business", "agriculture"]),
            vec![
                "social_science",
                "business",
                "natural_science",
                "agriculture"
            ]
        );
        assert_eq!(canonical_major_category("science"), Some("natural_science"));
        assert_eq!(canonical_major_category("컴퓨터공학과"), None);
        assert_eq!(canonical_major_category("stem"), None);
    }
}
