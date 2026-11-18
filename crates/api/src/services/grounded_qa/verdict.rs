//! The personalized verdict next to quoted passages. Every status comes from
//! [`structural::structural_checks`] (the function the recommendation list
//! filters with) or from the assistant's own assessment, never from text
//! matching, so a verdict cannot contradict the eligibility explanation.
//!
//! The question decides which checks are reported, each one it asks about
//! (worst status first decides the verdict): 휴학/재학/졸업 → 학적,
//! a region name → 지역, 나이/만 N세 → 연령, 학교 → 학교 (campus offers),
//! 소득/분위 → 소득 (the coded income check and the income leaves of the
//! hard rule, never the whole bundle), otherwise an eligibility question gets
//! the overall assessment. For 학적 and 지역 the value the question names is
//! also checked against the registered condition (`asked_status`), with the
//! same structural function; it describes the notice, not the user.
use super::text;
use crate::assistant_provider::Topic;
use mazimi_core::{
    models::Program,
    rule_engine::RuleVerdict,
    structural::{self, CheckKind, StructuralProfile},
};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerdictCheck {
    Overall,
    Age,
    Region,
    School,
    Enrollment,
    Income,
}

#[derive(Debug, Clone, Serialize)]
pub struct Verdict {
    pub check: VerdictCheck,
    /// For the saved profile.
    pub status: RuleVerdict,
    /// Whether the programme registers this condition at all. `false` is
    /// reported as `unknown`: no recorded limit is not a confirmed pass.
    pub registered: bool,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asked_value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asked_status: Option<RuleVerdict>,
    /// The quoted passage that states the condition, when one was found.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub passage_id: Option<String>,
}

/// The assistant's assessment the verdict reuses: overall status and
/// summary, and the status of its income conditions (`None` = none
/// registered; `Assessment::income`).
pub struct Overall<'a> {
    pub status: RuleVerdict,
    pub summary: &'a str,
    pub income: Option<RuleVerdict>,
    /// Structural checks a current review's code override replaced, with
    /// the card's status for them and the list's exception label when the
    /// profile is listed only through the override (`Assessment::overridden`).
    pub overridden: &'a [(CheckKind, RuleVerdict, Option<&'static str>)],
}

enum Asked {
    Enrollment(Option<&'static str>),
    Region(&'static str),
    Age,
    School,
    Income,
}

/// Every condition the question asks about, in this order: 학적, 지역,
/// 연령, 학교, 소득.
fn asked(question: &str, program: &Program) -> Vec<Asked> {
    let (compact, _) = text::compact(question);
    let words = text::words(question);
    let mut out = Vec::new();
    // The one status cue table (`intent::enrollment_cue`).
    if let Some(code) = super::intent::status_words(&words)
        .iter()
        .find_map(|w| super::intent::status_of(w))
    {
        out.push(Asked::Enrollment(Some(code)));
    } else if compact.contains("학적") {
        out.push(Asked::Enrollment(None));
    }
    if let Some(region) = words
        .iter()
        .find_map(|w| mazimi_core::canonical_region_code(text::stem(w)))
    {
        out.push(Asked::Region(region));
    }
    let digit_age = compact.char_indices().any(|(i, c)| {
        (c == '세' || c == '살') && compact[..i].ends_with(|d: char| d.is_ascii_digit())
    });
    if digit_age
        || ["나이", "연령", "만나이"]
            .iter()
            .any(|c| compact.contains(c))
    {
        out.push(Asked::Age);
    }
    if program.program_type == "campus_benefit"
        && ["학교", "대학교", "캠퍼스"]
            .iter()
            .any(|c| compact.contains(c))
    {
        out.push(Asked::School);
    }
    if ["소득", "분위", "중위", "수급", "차상위", "저소득"]
        .iter()
        .any(|c| compact.contains(c))
    {
        out.push(Asked::Income);
    }
    out
}

/// Fail outranks unknown, unknown outranks pass.
fn rank(status: RuleVerdict) -> u8 {
    match status {
        RuleVerdict::Pass => 0,
        RuleVerdict::Unknown => 1,
        RuleVerdict::Fail => 2,
    }
}

fn label(kind: CheckKind) -> &'static str {
    match kind {
        CheckKind::Age => "연령",
        CheckKind::Region => "지역",
        CheckKind::District => "시·군·구",
        CheckKind::School => "학교",
        CheckKind::Enrollment => "학적",
        CheckKind::Audience => "지원 대상",
    }
}

fn find(
    checks: &[structural::StructuralCheck],
    kind: CheckKind,
) -> Option<&structural::StructuralCheck> {
    checks.iter().find(|c| c.kind == kind)
}

fn structural_summary(check: Option<&structural::StructuralCheck>, kind: CheckKind) -> String {
    let label = label(kind);
    match check {
        None => format!("공고에 등록된 {label} 제한이 없어요. 제한이 없다는 확인은 아니니 원문을 확인해 주세요."),
        Some(c) if c.unresolved_scope => {
            format!("공고의 {label} 범위가 아직 확인되지 않아 비교하지 않았어요.")
        }
        Some(c) => match c.status {
            RuleVerdict::Pass => format!("저장한 {label} 정보가 공고에 등록된 {label} 조건과 맞아요. 최종 자격은 기관 확인이 필요해요."),
            RuleVerdict::Fail => format!("저장한 {label} 정보가 공고에 등록된 {label} 조건과 맞지 않아요."),
            RuleVerdict::Unknown if c.missing_field.is_some() => format!(
                "프로필에 {label} 정보가 없거나 경계에 있어 비교를 확정하지 못했어요."
            ),
            RuleVerdict::Unknown => format!("{label} 조건을 확정하지 못했어요."),
        },
    }
}

/// The verdict for `question`, or `None` when it asks nothing about
/// eligibility.
pub fn verdict(
    question: &str,
    topics: &[Topic],
    program: &Program,
    program_university_id: Option<&str>,
    facts: &StructuralProfile,
    overall: &Overall,
) -> Option<Verdict> {
    // `재학증명서 필요해?` names a status but asks for a document.
    if !topics.contains(&Topic::Eligibility) {
        return None;
    }
    let checks = structural::structural_checks(program, program_university_id, facts);
    let overridden = |kind: CheckKind| {
        overall
            .overridden
            .iter()
            .find(|(k, _, _)| *k == kind)
            .map(|(_, status, exception)| (*status, *exception))
    };
    let structural_verdict = |kind: CheckKind, check: VerdictCheck| {
        // A code rule the notice widens is reported as the card reports it.
        if let Some((status, exception)) = overridden(kind) {
            let label = label(kind);
            return Verdict {
                check,
                status,
                registered: true,
                summary: match (status, exception) {
                    (RuleVerdict::Fail, _) => format!("저장한 {label} 정보가 공고 원문의 넓힌 {label} 조건과도 맞지 않아요."),
                    // Listed only through the notice's exception: the list's wording.
                    (_, Some(exception)) => format!("{exception}. 저장한 {label} 정보는 등록된 {label} 조건 밖이고, 공고 원문의 예외 대상에 들 수 있어요."),
                    _ => format!("공고 원문이 등록된 {label} 조건보다 넓은 대상을 포함해요. 해당 여부는 공고에서 확인해 주세요."),
                },
                asked_value: None,
                asked_status: None,
                passage_id: None,
            };
        }
        let found = find(&checks, kind);
        Verdict {
            check,
            status: found.map_or(RuleVerdict::Unknown, |c| c.status),
            registered: found.is_some(),
            summary: structural_summary(found, kind),
            asked_value: None,
            asked_status: None,
            passage_id: None,
        }
    };
    // The value named in the question, checked alone against the registered
    // condition. `None` when the programme registers no such condition.
    let asked_check = |kind: CheckKind, probe: StructuralProfile| {
        if overridden(kind).is_some() {
            // The registered code no longer decides this condition.
            return None;
        }
        structural::structural_checks(program, program_university_id, &probe)
            .into_iter()
            .find(|c| c.kind == kind)
            .map(|c| c.status)
    };
    let one = |asked: Asked| {
        match asked {
        Asked::Enrollment(code) => {
            let mut v = structural_verdict(CheckKind::Enrollment, VerdictCheck::Enrollment);
            if let Some(code) = code {
                let label = mazimi_core::enrollment_status_label(code);
                v.asked_status = asked_check(
                    CheckKind::Enrollment,
                    StructuralProfile {
                        enrollment_status: Some(code),
                        ..Default::default()
                    },
                );
                if let Some(status) = v.asked_status {
                    v.summary.push(' ');
                    v.summary.push_str(&match status {
                        RuleVerdict::Pass => {
                            format!("질문한 ‘{label}’은 공고에 등록된 학적 조건에 포함돼요.")
                        }
                        RuleVerdict::Fail => {
                            format!("질문한 ‘{label}’은 공고에 등록된 학적 조건에 포함되지 않아요.")
                        }
                        RuleVerdict::Unknown => format!(
                            "질문한 ‘{label}’이 등록된 학적 조건에 해당하는지 확정하지 못했어요."
                        ),
                    });
                }
                v.asked_value = Some(label.to_owned());
            }
            v
        }
        Asked::Region(code) => {
            let mut v = structural_verdict(CheckKind::Region, VerdictCheck::Region);
            let label = mazimi_core::region_label(code);
            v.asked_status = asked_check(
                CheckKind::Region,
                StructuralProfile {
                    region_code: Some(code),
                    ..Default::default()
                },
            );
            if let Some(status) = v.asked_status {
                v.summary.push(' ');
                v.summary.push_str(&match status {
                    RuleVerdict::Pass => {
                        format!("질문한 ‘{label}’은 공고에 등록된 대상 지역이에요.")
                    }
                    RuleVerdict::Fail => {
                        format!("질문한 ‘{label}’은 공고에 등록된 대상 지역이 아니에요.")
                    }
                    RuleVerdict::Unknown => "공고의 대상 지역이 아직 확인되지 않았어요.".to_owned(),
                });
            }
            v.asked_value = Some(label.to_owned());
            v
        }
        Asked::Age => structural_verdict(CheckKind::Age, VerdictCheck::Age),
        Asked::School => structural_verdict(CheckKind::School, VerdictCheck::School),
        Asked::Income => Verdict {
            check: VerdictCheck::Income,
            status: overall.income.unwrap_or(RuleVerdict::Unknown),
            registered: overall.income.is_some(),
            summary: match overall.income {
                Some(RuleVerdict::Pass) => "저장한 소득 정보가 공고에 등록된 소득 조건과 맞아요. 소득 산정 방식은 공고에서 확인해 주세요.",
                Some(RuleVerdict::Fail) => "저장한 소득 정보가 공고에 등록된 소득 조건과 맞지 않아요.",
                Some(RuleVerdict::Unknown) => {
                    "공고에 소득 조건이 등록돼 있지만 저장한 정보로는 확정하지 못했어요."
                }
                None => "공고에 등록된 소득 조건이 없어요. 조건이 없다는 확인은 아니니 원문을 확인해 주세요.",
            }
            .to_owned(),
            asked_value: None,
            asked_status: None,
            passage_id: None,
        },
    }
    };
    let mut verdicts: Vec<Verdict> = asked(question, program).into_iter().map(one).collect();
    let Some(first) = verdicts.first().map(|v| v.check) else {
        return Some(Verdict {
            check: VerdictCheck::Overall,
            status: overall.status,
            registered: true,
            summary: overall.summary.to_owned(),
            asked_value: None,
            asked_status: None,
            passage_id: None,
        });
    };
    if verdicts.len() == 1 {
        return verdicts.pop();
    }
    // Several conditions: each one's sentence, and the worst part as a
    // pair, its check with its status, where a part's status is the worse
    // of the saved profile's and the named value's. No single asked value
    // is the verdict's own (a client badges it before the status).
    let part = |v: &Verdict| {
        v.asked_status
            .map_or(v.status, |asked| worse(v.status, asked))
    };
    let worst = verdicts
        .iter()
        .max_by_key(|v| rank(part(v)))
        .map(|v| (v.check, part(v)))
        .unwrap_or((first, RuleVerdict::Unknown));
    Some(Verdict {
        check: worst.0,
        status: worst.1,
        registered: verdicts.iter().any(|v| v.registered),
        summary: verdicts
            .iter()
            .map(|v| v.summary.as_str())
            .collect::<Vec<_>>()
            .join(" "),
        asked_value: None,
        asked_status: None,
        passage_id: None,
    })
}

fn worse(a: RuleVerdict, b: RuleVerdict) -> RuleVerdict {
    if rank(a) >= rank(b) {
        a
    } else {
        b
    }
}

#[cfg(test)]
mod tests {
    use super::super::passages::tests::program;
    use super::*;
    use serde_json::json;

    /// Final review F-1: a question about the programme asks the value
    /// whatever its polarity (main's readings): "휴학생은 안 돼?" on a
    /// 재학-only programme says 휴학 is not included.
    #[test]
    fn a_negative_question_still_asks_the_value() {
        let p = program(json!({"normalized_payload": {"enrollment_statuses": ["enrolled"]}}));
        for (question, code) in [
            ("휴학생은 안 돼?", "on_leave"),
            ("휴학생은 못 받아?", "on_leave"),
            ("휴학생도 안 되나요?", "on_leave"),
            ("졸업생은 안 돼?", "graduated"),
            ("휴학생 아니어도 돼?", "on_leave"),
            ("휴학생은 아닌가요?", "on_leave"),
            ("재학생이 아니면 안 돼?", "enrolled"),
        ] {
            assert!(
                asked(question, &p)
                    .iter()
                    .any(|a| matches!(a, Asked::Enrollment(Some(c)) if *c == code)),
                "{question}"
            );
        }
        assert!(asked("부산은 안 돼?", &p)
            .iter()
            .any(|a| matches!(a, Asked::Region("busan"))));
        let saved = StructuralProfile {
            enrollment_status: Some("enrolled"),
            ..Default::default()
        };
        let v = verdict(
            "휴학생은 안 돼?",
            &[Topic::Eligibility],
            &p,
            None,
            &saved,
            &overall(),
        )
        .unwrap();
        assert_eq!(v.check, VerdictCheck::Enrollment, "{v:?}");
        assert_eq!(v.asked_value.as_deref(), Some("휴학"), "{v:?}");
        assert_eq!(v.asked_status, Some(RuleVerdict::Fail), "{v:?}");
        assert!(v.summary.contains("포함되지 않아요"), "{}", v.summary);
    }

    fn overall() -> Overall<'static> {
        Overall {
            status: RuleVerdict::Unknown,
            summary: "아직 확인할 조건이 있어요.",
            income: None,
            overridden: &[],
        }
    }

    #[test]
    fn enrollment_question_reports_saved_and_asked_status() {
        let p = program(json!({"normalized_payload": {"enrollment_statuses": ["enrolled"]}}));
        let saved = StructuralProfile {
            enrollment_status: Some("enrolled"),
            ..Default::default()
        };
        let v = verdict(
            "휴학생도 돼?",
            &[Topic::Eligibility],
            &p,
            None,
            &saved,
            &overall(),
        )
        .unwrap();
        assert_eq!(v.check, VerdictCheck::Enrollment);
        assert_eq!(v.status, RuleVerdict::Pass);
        assert_eq!(v.asked_status, Some(RuleVerdict::Fail));
        assert_eq!(v.asked_value.as_deref(), Some("휴학"));
        assert!(v.summary.contains("포함되지 않아요"), "{}", v.summary);
    }

    /// Round 3 (code M-7): "졸업 예정자" is enrolled, so a graduates-only
    /// programme does not pass the asked value.
    #[test]
    fn a_prospective_graduate_is_enrolled() {
        let p = program(json!({"normalized_payload": {"enrollment_statuses": ["graduated"]}}));
        let v = verdict(
            "졸업 예정자도 돼?",
            &[Topic::Eligibility],
            &p,
            None,
            &StructuralProfile::default(),
            &overall(),
        )
        .unwrap();
        assert_eq!(v.asked_value.as_deref(), Some("재학"), "{v:?}");
        assert_eq!(v.asked_status, Some(RuleVerdict::Fail), "{v:?}");
    }

    #[test]
    fn unregistered_conditions_are_unknown_not_pass() {
        let p = program(json!({}));
        let v = verdict(
            "부산 살아도 돼?",
            &[Topic::Eligibility],
            &p,
            None,
            &StructuralProfile::default(),
            &overall(),
        )
        .unwrap();
        assert_eq!(
            (v.check, v.status, v.registered),
            (VerdictCheck::Region, RuleVerdict::Unknown, false)
        );
        assert_eq!(v.asked_status, None);
        assert!(verdict(
            "서류 뭐야?",
            &[Topic::Documents],
            &p,
            None,
            &StructuralProfile::default(),
            &overall()
        )
        .is_none());
        let v = verdict(
            "나도 받을 수 있어?",
            &[Topic::Eligibility],
            &p,
            None,
            &StructuralProfile::default(),
            &overall(),
        )
        .unwrap();
        assert_eq!(v.check, VerdictCheck::Overall);
        let v = verdict(
            "만 25세면 돼?",
            &[Topic::Eligibility],
            &p,
            None,
            &StructuralProfile::default(),
            &overall(),
        )
        .unwrap();
        assert_eq!(v.check, VerdictCheck::Age);
    }
}
