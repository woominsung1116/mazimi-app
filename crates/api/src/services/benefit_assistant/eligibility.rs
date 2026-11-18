use mazimi_core::{
    applicant_review::ReviewEffect,
    condition_review::{self, overrides::CodeOverride},
    models::{
        CheckEvidence, EligibilityRule, Evidence, EvidenceCheck, ExclusionReason, Program,
        UserProfile,
    },
    rule_engine::{self, EligibilityProfile, RuleNode, RuleOp, RuleVerdict},
    structural::{self, CheckKind, StructuralProfile},
};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeSet;

#[derive(Serialize)]
pub struct Assessment {
    pub status: RuleVerdict,
    pub summary: &'static str,
    pub checks: Vec<Check>,
    pub missing_fields: Vec<String>,
    pub profile_version: Option<i32>,
    /// Structural checks a current review's code override replaced, with
    /// the replacement's status and, when the profile is listed only through
    /// the override, the list's exception label (grounded Q&A reports these,
    /// not the code).
    #[serde(skip)]
    pub overridden: Vec<(CheckKind, RuleVerdict, Option<&'static str>)>,
    /// The income leaves of the hard rule read alone ([`income_leaves`]);
    /// `None` when it has none.
    #[serde(skip)]
    income_in_rule: Option<RuleVerdict>,
    /// How the recommendation list decides this programme for the saved
    /// profile ([`recommendations::decide`]): `Ok(pass)` listed, `Err`
    /// hidden; `None` until applied or when the list cannot decide (no
    /// birth year or region). Only a listed `pass` lets the card pass.
    ///
    /// [`recommendations::decide`]: crate::services::recommendations::decide
    #[serde(skip)]
    listed: ListDecision,
}

/// A list decision for a single programme ([`crate::services::recommendations::Listing`]),
/// `None` when the list cannot decide; see [`Assessment::apply_list_decision`].
pub type ListDecision = Option<crate::services::recommendations::Listing>;

/// What a check is about. The grounded verdict selects checks by it, never
/// by label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Age,
    Region,
    District,
    School,
    Enrollment,
    /// The registered hard-rule bundle ("필수 조건").
    HardRule,
    /// The coded 기준 중위소득 condition ([`mazimi_core::income`]).
    Income,
    ApplicantClass,
    CorporateTerms,
    /// A notice condition a code override or a reviewed rule decides.
    Notice,
    /// The target group the list reads from the notice or its youth_fit
    /// verdict.
    Audience,
    /// A reviewed application window.
    Window,
}

#[derive(Serialize)]
pub struct Check {
    #[serde(skip)]
    pub kind: Kind,
    pub label: &'static str,
    pub status: RuleVerdict,
    pub detail: String,
    /// Citations for this condition; omitted when none exist.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<Evidence>,
    #[serde(skip)]
    cites: Option<EvidenceCheck>,
    /// The registered hard-rule bundle ("필수 조건"): unknown, it names
    /// nothing to check by itself; the assessment's missing profile fields
    /// do (the 비서's answer says those, never the bundle's label).
    #[serde(skip)]
    pub bundle: bool,
}

impl Assessment {
    /// Status of the income conditions: the coded 기준 중위소득 check and
    /// the income leaves of the hard rule, the worse of the two; `None` when
    /// the programme registers neither.
    pub fn income(&self) -> Option<RuleVerdict> {
        self.checks
            .iter()
            .filter(|c| c.kind == Kind::Income)
            .map(|c| c.status)
            .chain(self.income_in_rule)
            .reduce(worse)
    }

    /// What the grounded verdict reads of this assessment.
    pub fn overall(&self) -> crate::services::grounded_qa::verdict::Overall<'_> {
        crate::services::grounded_qa::verdict::Overall {
            status: self.status,
            summary: self.summary,
            income: self.income(),
            overridden: &self.overridden,
        }
    }

    /// Add the reviewed applicant class as a check. A hidden (`organisation`
    /// / `business`) programme fails it, so the assistant never answers
    /// "일치해요" for a programme the list leaves out as not applicable by an
    /// individual. A demoted (`indirect`) one gets an unconfirmed note but
    /// keeps its status, as the list keeps its `eligibility_status`.
    pub fn apply_applicant_review(&mut self, effect: ReviewEffect) {
        let (status, detail) = match effect {
            ReviewEffect::None => return,
            ReviewEffect::Hide => (
                RuleVerdict::Fail,
                "기관·기업이 신청하는 사업으로 검토됐어요. 개인은 직접 신청할 수 없어요.",
            ),
            ReviewEffect::Demote => (
                RuleVerdict::Unknown,
                "기업·대학 등 참여기관을 통해 참여하는 사업이에요. 참여기관 선정 여부를 확인해 주세요.",
            ),
        };
        self.checks.push(Check {
            kind: Kind::ApplicantClass,
            label: "신청 주체",
            status,
            detail: detail.into(),
            evidence: Vec::new(),
            cites: None,
            bundle: false,
        });
        self.settle();
    }

    /// Take the recommendation list's decision for this programme
    /// ([`recommendations::decide_on`]). The card is `pass` only when the
    /// list lists it as `pass` and every check passes. A hidden row is never
    /// `pass`: a gate the card has no check for adds one naming the list's
    /// reason ([`hidden_check`]) when no check already fails; a reason the
    /// card's own checks state (age, region, school, enrollment, income: the
    /// same functions) is left to them, so a hide on a value the profile
    /// lacks reads `unknown`, not `fail`. A target-group hide is one of
    /// those: a stated contradiction fails through the card's own
    /// `audience::membership` check, and the stored youth_fit verdict hides
    /// without reading an answer, so the hide reads like an open audience.
    ///
    /// [`recommendations::decide_on`]: crate::services::recommendations::decide_on
    pub fn apply_list_decision(&mut self, listed: ListDecision) {
        use crate::services::recommendations::Listing;
        match listed {
            Some(Listing::Hidden(reason)) if reason != ExclusionReason::TargetGroup => {
                let named = self.checks.iter().any(|c| c.status == RuleVerdict::Fail);
                if let (false, Some((kind, label, status))) = (named, hidden_check(reason)) {
                    self.checks.push(Check {
                        kind,
                        label,
                        status,
                        detail: reason.label().into(),
                        evidence: Vec::new(),
                        cites: None,
                        bundle: false,
                    });
                }
            }
            // Listed on an open audience check, or hidden for the target
            // group: the card's own membership check (the same
            // `audience::membership`) names it when it is not a pass; never two.
            Some(
                Listing::Open {
                    audience_open: true,
                }
                | Listing::Hidden(ExclusionReason::TargetGroup),
            ) if !self
                .checks
                .iter()
                .any(|c| c.kind == Kind::Audience && c.status != RuleVerdict::Pass) =>
            {
                self.checks.push(Check {
                    kind: Kind::Audience,
                    label: "지원 대상",
                    status: RuleVerdict::Unknown,
                    detail: condition_review::labels::TARGET.to_owned(),
                    evidence: Vec::new(),
                    cites: None,
                    bundle: false,
                });
            }
            _ => {}
        }
        self.listed = listed;
        self.settle();
    }

    /// Status and summary from the checks and the list's decision.
    fn settle(&mut self) {
        let status = if self.checks.iter().any(|c| c.status == RuleVerdict::Fail) {
            RuleVerdict::Fail
        } else if self.listed == Some(crate::services::recommendations::Listing::Pass)
            // A demoted applicant class keeps the list's status (review M2).
            && self
                .checks
                .iter()
                .all(|c| c.status == RuleVerdict::Pass || c.kind == Kind::ApplicantClass)
        {
            RuleVerdict::Pass
        } else {
            RuleVerdict::Unknown
        };
        self.status = status;
        self.summary = summary(status);
    }

    /// Attach each citation to the check it supports.
    pub fn cite(&mut self, evidence: &[CheckEvidence]) {
        for check in &mut self.checks {
            if let Some(kind) = check.cites {
                check.evidence = crate::services::evidence::for_check(evidence, kind);
            }
        }
    }
}

struct OptionalProfile<'a>(Option<&'a UserProfile>);
impl EligibilityProfile for OptionalProfile<'_> {
    fn eligibility_value(&self, field: &str) -> Option<Value> {
        self.0.and_then(|p| p.eligibility_value(field))
    }

    fn eligibility_candidates(&self, field: &str) -> Option<Vec<Value>> {
        self.0.and_then(|p| p.eligibility_candidates(field))
    }

    fn birth(&self) -> Option<(i32, Option<chrono::NaiveDate>)> {
        self.0.and_then(|p| p.birth())
    }
}

/// Missing fields users are asked for first: the ones most checks depend on.
const MISSING_PRIORITY: &[&str] = &[
    "birth_year",
    "region_code",
    "enrollment_status",
    "income_bracket",
];

fn missing_rank(field: &str) -> (usize, &str) {
    (
        MISSING_PRIORITY
            .iter()
            .position(|f| *f == field)
            .unwrap_or(MISSING_PRIORITY.len()),
        field,
    )
}

/// Explain the saved profile against a programme. Structural conditions come
/// from [`structural::structural_checks`], the same function the
/// recommendation list filters with; `school_id` is the catalogue id the saved
/// school resolved to.
/// In production the list's decision comes from
/// [`crate::services::recommendations::decide_on`]; here from
/// [`crate::services::recommendations::decide`] with no audience text,
/// youth_fit verdict or review.
#[cfg(test)]
pub fn assess(
    program: &Program,
    program_university_id: Option<&str>,
    profile: Option<&UserProfile>,
    school_id: Option<&str>,
    rule: Option<&EligibilityRule>,
) -> Assessment {
    use crate::services::recommendations::{decide, GateColumns, ReviewContext};
    let mut assessment = assess_with_overrides(
        program,
        program_university_id,
        profile,
        school_id,
        rule,
        &[],
        None,
    );
    let listed = profile
        .cloned()
        .and_then(super::bundles::profile_input)
        .map(|input| {
            let gate = GateColumns::default();
            let facts = gate.facts(program, None);
            decide(
                program,
                program_university_id,
                &input,
                school_id,
                rule,
                &facts,
                ReviewContext::none(chrono::Utc::now()),
            )
        });
    assessment
        .apply_list_decision(listed.map(|d| crate::services::recommendations::Listing::of(&d)));
    assessment
}

/// The structural check an `S-` target replaces, its card label and the
/// evidence it cites; `C-` targets are named after the notice condition.
fn override_card(
    target: &CodeOverride,
) -> (Option<CheckKind>, &'static str, Option<EvidenceCheck>) {
    match target.structural_kind() {
        Some(CheckKind::Age) => (Some(CheckKind::Age), "연령", Some(EvidenceCheck::Age)),
        Some(CheckKind::Region) => (Some(CheckKind::Region), "지역", Some(EvidenceCheck::Region)),
        Some(CheckKind::District) => (
            Some(CheckKind::District),
            "시·군·구",
            Some(EvidenceCheck::Region),
        ),
        Some(CheckKind::Enrollment) => (
            Some(CheckKind::Enrollment),
            "학적",
            Some(EvidenceCheck::Enrollment),
        ),
        _ => (None, "공고 조건", None),
    }
}

/// The card's checks with the applied code overrides of a current condition review
/// ([`mazimi_core::condition_review::overrides::applicable`]), so the card
/// reads a row exactly as the recommendation list does: an overridden
/// structural check or compiled source condition is replaced by its
/// replacement (`fail` only when the profile contradicts it, else `unknown`
/// with the override's check), never reported as the narrower code rule.
/// The status is never `pass` here: only the list's decision
/// ([`Assessment::apply_list_decision`]) can make it so.
pub fn assess_with_overrides(
    program: &Program,
    program_university_id: Option<&str>,
    profile: Option<&UserProfile>,
    school_id: Option<&str>,
    rule: Option<&EligibilityRule>,
    overrides: &[&CodeOverride],
    tier: Option<mazimi_core::youth_fit::Tier>,
) -> Assessment {
    let mut checks = Vec::new();
    let mut overridden = Vec::new();
    let mut missing = BTreeSet::new();
    // Rule leaves count `age` on the programme's reference days, as the
    // structural age check does.
    let optional = OptionalProfile(profile);
    let basis = rule_engine::AgeBasis::of(program);
    let facts = StructuralProfile {
        birth_year: profile.and_then(|p| p.birth_year),
        birth_date: profile.and_then(|p| p.birth_date),
        region_code: profile.and_then(|p| p.region_code.as_deref()),
        enrollment_status: profile.and_then(|p| p.enrollment_status.as_deref()),
        university_id: school_id,
        city_code: profile.and_then(|p| p.city_code.as_deref()),
    };
    for check in structural::structural_checks(program, program_university_id, &facts)
        .into_iter()
        .filter(|c| {
            !overrides
                .iter()
                .any(|o| o.structural_kind() == Some(c.kind))
        })
    {
        if let Some(field) = check.missing_field {
            missing.insert(field.to_owned());
        }
        let cites = match check.kind {
            CheckKind::Age => Some(EvidenceCheck::Age),
            CheckKind::Region | CheckKind::District => Some(EvidenceCheck::Region),
            CheckKind::Enrollment => Some(EvidenceCheck::Enrollment),
            CheckKind::School | CheckKind::Audience => None,
        };
        let (label, detail) = match check.kind {
            CheckKind::Age => {
                let bounds = [
                    program.min_age.map(|n| format!("{n}세 이상")),
                    program.max_age.map(|n| format!("{n}세 이하")),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" · ");
                if facts.birth_date.is_some() {
                    ("연령", format!(
                        "등록된 연령 범위: {bounds}. 공고의 만 나이 기준일이 다를 수 있으니 공고에서 확인해 주세요."
                    ))
                } else {
                    ("연령", format!(
                        "등록된 연령 범위: {bounds}. 저장한 출생연도로 비교하므로 생일 전후로 한 해 차이가 날 수 있어요. 경계에 있으면 생년월일 확인 필요로 표시하고, 정확한 만 나이 기준일은 공고에서 확인해 주세요."
                    ))
                }
            }
            CheckKind::Region if check.unresolved_scope => ("지역", "공고의 대상 지역이 아직 확인되지 않았어요. 전국 대상이나 거주지 불일치로 단정하지 않아요.".into()),
            CheckKind::Region => ("지역", format!(
                "등록된 대상 지역: {}",
                mazimi_core::regions_display(program.regions.iter().flatten().map(String::as_str))
                    .unwrap_or_else(|| "지역 정보 확인 필요".into())
            )),
            CheckKind::District => ("시·군·구", "공고에 등록된 대상 시·군·구와 저장한 시·군·구를 비교했어요. 시·군·구를 저장하지 않았으면 확인 필요로 표시해요.".into()),
            CheckKind::School => ("학교", "학교 전용 혜택은 선택한 학교와 공고의 소속 학교가 일치해야 해요. 학교 인증은 별도로 필요할 수 있어요.".into()),
            CheckKind::Enrollment => ("학적", "공고에 등록된 재학·휴학 등의 대상 조건과 비교했어요.".into()),
            CheckKind::Audience => unreachable!("structural_checks has no audience check"),
        };
        checks.push(Check {
            kind: match check.kind {
                CheckKind::Age => Kind::Age,
                CheckKind::Region => Kind::Region,
                CheckKind::District => Kind::District,
                CheckKind::School => Kind::School,
                CheckKind::Enrollment => Kind::Enrollment,
                CheckKind::Audience => Kind::Audience,
            },
            label,
            status: check.status,
            detail,
            evidence: Vec::new(),
            cites,
            bundle: false,
        });
    }
    // Who the row is reserved for: the list's membership check, read from
    // the same text and stored tier (`audience::membership`), so the card
    // never passes a row the list caps at unknown.
    let audience_profile = crate::services::recommendations::saved_audience_profile(
        profile,
        mazimi_core::seoul_year(chrono::Utc::now()),
    );
    let membership =
        crate::services::recommendations::program_membership(program, tier, &audience_profile);
    for check in membership.checks() {
        if let Some(field) = check.missing_field {
            missing.insert(field.to_owned());
        }
        let detail = match (check.status, check.missing_field) {
            (RuleVerdict::Pass, _) => String::new(),
            (_, Some(field)) => condition_review::profile_label(field),
            (_, None) => condition_review::labels::TARGET.to_owned(),
        };
        checks.push(Check {
            kind: Kind::Audience,
            label: "지원 대상",
            status: check.status,
            detail,
            evidence: Vec::new(),
            cites: None,
            bundle: false,
        });
    }
    // Overrides the profile is listed through (the code rule hides it): the
    // card names the exception with the list's wording.
    let admitted = condition_review::overrides::admitting(
        program,
        program_university_id,
        &facts,
        &optional,
        overrides,
        &condition_review::overrides::CodeColumns::of(
            program,
            rule.and_then(|r| r.hard_filter_json.as_ref()),
        ),
        mazimi_core::seoul_date(chrono::Utc::now()),
    );
    for o in overrides {
        let status = match &o.replacement {
            Some(replacement) => {
                let evaluation = condition_review::overrides::evaluate_replacement(
                    &optional,
                    replacement,
                    &basis,
                );
                if evaluation.verdict == RuleVerdict::Unknown {
                    missing.extend(
                        rule_engine::fields_to_ask(&evaluation.unknown_rules).map(str::to_owned),
                    );
                }
                match evaluation.verdict {
                    RuleVerdict::Fail => RuleVerdict::Fail,
                    _ => RuleVerdict::Unknown,
                }
            }
            None => RuleVerdict::Unknown,
        };
        let (kind, label, cites) = override_card(o);
        let card_kind = match kind {
            Some(CheckKind::Age) => Kind::Age,
            Some(CheckKind::Region) => Kind::Region,
            Some(CheckKind::District) => Kind::District,
            Some(CheckKind::Enrollment) => Kind::Enrollment,
            Some(CheckKind::Audience) => Kind::Audience,
            Some(CheckKind::School) | None => Kind::Notice,
        };
        let exception = (status != RuleVerdict::Fail
            && admitted.iter().any(|a| std::ptr::eq(*a, *o)))
        .then(|| condition_review::overrides::exception_label(&o.target));
        if let Some(kind) = kind {
            overridden.push((kind, status, exception));
        }
        let note = condition_review::CHECK_KINDS
            .iter()
            .find(|(k, _)| *k == o.check_kind)
            .map_or("추가 자격 조건 확인이 필요해요", |(_, label)| label);
        let intro =
            "공고 원문이 등록된 코드 조건보다 넓은 대상을 포함해, 넓힌 조건으로 비교했어요.";
        checks.push(Check {
            kind: card_kind,
            label,
            status,
            detail: match exception {
                Some(exception) => format!("{exception}. {intro} {note}"),
                None => format!("{intro} {note}"),
            },
            evidence: Vec::new(),
            cites,
            bundle: false,
        });
    }
    // Compiled source conditions an override replaced are not compared.
    let code_targets: Vec<&CodeOverride> = overrides
        .iter()
        .copied()
        .filter(|o| o.structural_kind().is_none())
        .collect();
    let stripped_hard = rule.and_then(|r| {
        if code_targets.is_empty() {
            r.hard_filter_json.clone()
        } else {
            condition_review::overrides::strip_targets(r.hard_filter_json.as_ref(), &code_targets)
        }
    });
    let hard = stripped_hard.as_ref();
    let parsed = hard.and_then(|v| rule_engine::parse_rule_json(v).ok());
    let mut income_in_rule = None;
    if let Some(node) = parsed {
        let result = rule_engine::evaluate(&optional, &node, &basis);
        // A failed leaf in an OR group is not a failed mandatory condition.
        // Only the full tree verdict is used, and missing optional alternatives
        // do not prompt unnecessary questions once the tree already passes.
        if result.verdict == RuleVerdict::Unknown {
            missing.extend(rule_engine::fields_to_ask(&result.unknown_rules).map(str::to_owned));
        }
        let explanation = match result.verdict {
            RuleVerdict::Pass => "등록된 필수 조건 중 확인한 항목은 저장한 정보와 맞아요. 최종 자격은 기관 확인이 필요해요.",
            RuleVerdict::Fail => "등록된 필수 조건 묶음에 맞지 않는 항목이 있어요. 프로필과 공식 대상 조건을 확인해 주세요.",
            RuleVerdict::Unknown => "일부 정보가 없어 등록된 필수 조건을 모두 비교하지 못했어요.",
        };
        income_in_rule = income_leaves(&optional, &node, &basis, true);
        checks.push(Check {
            kind: Kind::HardRule,
            label: "필수 조건",
            status: result.verdict,
            detail: format!("{explanation}\n등록된 조건식: {}", describe_rule(&node, 0)),
            evidence: Vec::new(),
            cites: Some(EvidenceCheck::HardRule),
            bundle: true,
        });
    } else {
        checks.push(Check {
            kind: Kind::HardRule,
            label: "필수 조건",
            status: RuleVerdict::Unknown,
            detail: if hard.is_some() {
                "조건 데이터를 점검 중이에요. 자동으로 자격을 확정할 수 없어요."
            } else {
                "구조화된 필수 조건이 부족해요. 공식 공고에서 대상과 제외 조건을 확인해 주세요."
            }
            .into(),
            evidence: Vec::new(),
            cites: None,
            bundle: true,
        });
    }
    // A coded income condition is named and never decided, as on the
    // recommendation card ([`mazimi_core::income`]).
    let band = profile.and_then(|p| p.income_median_band.as_deref());
    if let Some(income) = mazimi_core::income::check(program, band) {
        let status = income.verdict;
        let detail = match status {
            RuleVerdict::Unknown if income.status_unconfirmed => {
                "저장한 소득 구간은 공고의 기준 중위소득 상한 안에 있지만, 공고가 수급자·차상위 등 자격을 정하고 있어 그 자격은 공고에서 확인해 주세요."
            }
            RuleVerdict::Pass => "공고에 등록된 기준 중위소득 상한 안에 저장한 소득 구간이 있어요. 소득 산정 방식은 공고에서 확인해 주세요.",
            RuleVerdict::Fail => "저장한 기준 중위소득 구간이 공고에 등록된 소득 상한보다 높아요.",
            RuleVerdict::Unknown if band.is_none_or(|b| b == mazimi_core::rule_engine::INCOME_MEDIAN_UNKNOWN) => {
                "공고에 소득 기준이 등록돼 있어요. 기준 중위소득 구간을 입력하면 확인할 수 있어요."
            }
            RuleVerdict::Unknown => "공고에 소득 기준이 등록돼 있지만 저장한 구간만으로는 확정할 수 없어요. 공고의 소득 기준을 확인해 주세요.",
        };
        checks.push(Check {
            kind: Kind::Income,
            label: "소득",
            status,
            detail: detail.into(),
            evidence: Vec::new(),
            cites: None,
            bundle: false,
        });
    }
    if program.program_type == "corporate_benefit" {
        checks.push(Check { kind: Kind::CorporateTerms, label:"기업 이용 조건", status:RuleVerdict::Unknown, detail:"회원 인증·결제수단 등 별도 이용 조건이 있을 수 있어요. 등록된 연령이나 학생 조건의 일치만으로 사용 가능을 확정하지 않아요.".into(), evidence: Vec::new(), cites: None, bundle: false });
    }
    let mut assessment = Assessment {
        status: RuleVerdict::Unknown,
        summary: summary(RuleVerdict::Unknown),
        checks,
        missing_fields: {
            let mut fields: Vec<_> = missing.into_iter().collect();
            fields.sort_by(|a, b| missing_rank(a).cmp(&missing_rank(b)));
            fields.truncate(2);
            fields
        },
        profile_version: profile.map(|p| p.profile_version),
        overridden,
        income_in_rule,
        listed: None,
    };
    assessment.settle();
    assessment
}

/// The check a hidden row's reason adds to the card, `None` when the card
/// already states it or the row's data could not be read. A target-group
/// hide never comes here ([`Assessment::apply_list_decision`]).
fn hidden_check(reason: ExclusionReason) -> Option<(Kind, &'static str, RuleVerdict)> {
    Some(match reason {
        ExclusionReason::HardRule => (Kind::Notice, "공고 조건", RuleVerdict::Fail),
        ExclusionReason::NotIndividual => (Kind::ApplicantClass, "신청 주체", RuleVerdict::Fail),
        ExclusionReason::NotOpen => (Kind::Window, "신청 기간", RuleVerdict::Fail),
        ExclusionReason::TargetGroup
        | ExclusionReason::Age
        | ExclusionReason::Region
        | ExclusionReason::School
        | ExclusionReason::Enrollment
        | ExclusionReason::Income
        | ExclusionReason::RuleUnavailable
        | ExclusionReason::ScopeUnconfirmed
        | ExclusionReason::OutsideYouthScope => return None,
    })
}

/// Rule fields that state an income condition.
const INCOME_FIELDS: &[&str] = &[
    "income_bracket",
    "kosaf_support_bracket",
    "income_median_pct",
    "is_low_income_household",
];

/// Fail outranks unknown, unknown outranks pass.
fn worse(a: RuleVerdict, b: RuleVerdict) -> RuleVerdict {
    match (a, b) {
        (RuleVerdict::Fail, _) | (_, RuleVerdict::Fail) => RuleVerdict::Fail,
        (RuleVerdict::Unknown, _) | (_, RuleVerdict::Unknown) => RuleVerdict::Unknown,
        _ => RuleVerdict::Pass,
    }
}

/// The income leaves of `node`, each read alone: `fail` when a leaf every
/// path requires (no `any` above it) fails, `pass` when every income leaf
/// passes, else `unknown`; `None` without income leaves. An `any`
/// alternative that fails says nothing about the whole condition.
fn income_leaves(
    profile: &impl EligibilityProfile,
    node: &RuleNode,
    basis: &rule_engine::AgeBasis,
    required: bool,
) -> Option<RuleVerdict> {
    match node {
        RuleNode::Condition { field, .. } => INCOME_FIELDS.contains(&field.as_str()).then(|| {
            match rule_engine::evaluate(profile, node, basis).verdict {
                RuleVerdict::Fail if !required => RuleVerdict::Unknown,
                verdict => verdict,
            }
        }),
        RuleNode::All { all } => all
            .iter()
            .filter_map(|n| income_leaves(profile, n, basis, required))
            .reduce(worse),
        RuleNode::Any { any } => any
            .iter()
            .filter_map(|n| income_leaves(profile, n, basis, false))
            .reduce(worse),
    }
}

fn summary(status: RuleVerdict) -> &'static str {
    match status {
        RuleVerdict::Pass => {
            "등록된 조건 중 확인한 항목은 저장한 정보와 맞아요. 최종 자격은 기관 확인이 필요해요."
        }
        RuleVerdict::Fail => "현재 저장된 정보와 맞지 않는 조건이 있어요. 신청 전에 확인해 주세요.",
        RuleVerdict::Unknown => "아직 확인할 조건이 있어요.",
    }
}

/// Preserve AND/OR grouping in a bounded, deterministic explanation instead of
/// claiming that every failed leaf is a mandatory failure.
/// "부산 금정구" for a 시·군·구 code (current or retired), from the official
/// district table; "기타 시·군·구" for any other value. Never the code.
fn district_label(code: &str) -> String {
    use mazimi_worker::district_codes::DISTRICTS;
    let Some((_, sido, name)) = mazimi_worker::regions::canonical_district(code.trim())
        .and_then(|current| DISTRICTS.iter().find(|(c, ..)| *c == current))
    else {
        return "기타 시·군·구".into();
    };
    let regions = mazimi_worker::regions::province_label(sido, Some(name));
    let sido = match regions.as_slice() {
        [region] => mazimi_core::region_label(region),
        _ => "",
    };
    [sido, name]
        .iter()
        .filter(|s| !s.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(" ")
}

/// "만 19~24세" for an `age_band` such as "19-24", "만 35세 이상" for "35+";
/// "기타 연령대" otherwise.
fn age_band_label(band: &str) -> String {
    let band = band.trim();
    let number = |s: &str| s.parse::<u8>().ok();
    if let Some((low, high)) = band.split_once('-') {
        if let (Some(low), Some(high)) = (number(low), number(high)) {
            return format!("만 {low}~{high}세");
        }
    }
    match band.strip_suffix('+').and_then(number) {
        Some(low) => format!("만 {low}세 이상"),
        None => "기타 연령대".into(),
    }
}

fn describe_rule(node: &RuleNode, depth: usize) -> String {
    if depth >= 5 {
        return "세부 조건은 공식 공고 확인".into();
    }
    match node {
        RuleNode::All { all } | RuleNode::Any { any: all } => {
            let label = if matches!(node, RuleNode::All { .. }) {
                "모두 필요"
            } else {
                "하나 이상 필요"
            };
            let mut parts: Vec<_> = all
                .iter()
                .take(6)
                .map(|n| describe_rule(n, depth + 1))
                .collect();
            if all.len() > 6 {
                parts.push("나머지 조건은 공식 공고 확인".into());
            }
            format!("{label} [{}]", parts.join("; "))
        }
        RuleNode::Condition {
            field, op, value, ..
        } => {
            // Coded fields are shown by label, never by code.
            let code_label: Option<fn(&str) -> String> = match field.as_str() {
                "education_stage" => {
                    Some(|v| mazimi_core::education::education_stage_label(v).into())
                }
                "major_category" => {
                    Some(|v| mazimi_core::education::major_category_label(v).into())
                }
                "major_group" => Some(|v| {
                    mazimi_core::education::canonical_major_category(v)
                        .map_or("기타", mazimi_core::education::major_category_label)
                        .into()
                }),
                "enrollment_status" => Some(|v| mazimi_core::enrollment_status_label(v).into()),
                "employment_status" => Some(|v| mazimi_core::employment_status_label(v).into()),
                "region_code" | "region" => {
                    Some(|v| mazimi_core::region_display(v).unwrap_or_else(|| "기타".into()))
                }
                "city_code" => Some(district_label),
                "age_band" => Some(age_band_label),
                "marital_status" => Some(|v| {
                    match v.trim() {
                        "married" => "기혼",
                        "unmarried" => "미혼",
                        _ => "기타",
                    }
                    .into()
                }),
                "housing_type" => Some(|v| mazimi_core::housing_type_label(v).into()),
                "school_type" => Some(|v| mazimi_core::education::school_type_label(v).into()),
                _ => None,
            };
            let field = match field.as_str() {
                "age" => "연령",
                "birth_year" => "출생연도",
                "region_code" | "region" => "거주 지역",
                "income_bracket" => "소득 구간",
                "kosaf_support_bracket" => "학자금 지원구간",
                "enrollment_status" => "학적 상태",
                "employment_status" => "고용 상태",
                "school_name" => "학교",
                "school_year" => "학년",
                "school_type" => "학교 유형",
                "housing_type" => "주거 형태",
                "household_size" => "가구원 수",
                "major_group" => "전공 계열",
                "has_disability" => "장애 해당",
                "is_multicultural_family" => "다문화가정 해당",
                "is_low_income_household" => "저소득가구 해당",
                "veteran_family" => "보훈가족 해당",
                "education_stage" => "학력",
                "major_category" => "전공 계열",
                other => rule_engine::field_label(other),
            };
            let item = |v: &Value| match (v.as_str(), code_label) {
                (Some(s), Some(label)) => label(s),
                (Some(s), None) => s.to_owned(),
                (None, _) => v.to_string(),
            };
            let value = match value.as_array() {
                Some(items) => {
                    let mut labels: Vec<String> = Vec::new();
                    for label in items.iter().map(item) {
                        if !labels.contains(&label) {
                            labels.push(label);
                        }
                    }
                    labels.join(", ")
                }
                None => item(value),
            };
            let value = if value.chars().count() > 120 {
                "상세 값은 공식 공고 확인".into()
            } else {
                value
            };
            match op {
                RuleOp::IsTrue => format!("{field}: 해당"),
                RuleOp::IsFalse => format!("{field}: 비해당"),
                RuleOp::IsNull => format!("{field}: 정보 없음"),
                RuleOp::IsNotNull => format!("{field}: 정보 있음"),
                _ => format!(
                    "{field} {} {value}",
                    match op {
                        RuleOp::Eq => "=",
                        RuleOp::Ne => "≠",
                        RuleOp::In => "다음 중 포함:",
                        RuleOp::NotIn => "다음에서 제외:",
                        RuleOp::Between => "범위:",
                        RuleOp::Gte => "≥",
                        RuleOp::Lte => "≤",
                        RuleOp::Gt => ">",
                        RuleOp::Lt => "<",
                        RuleOp::Contains => "포함:",
                        _ => unreachable!(),
                    }
                ),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::recommendations::evaluate;
    use mazimi_core::models::ProfileInput;
    use serde_json::json;

    fn program(
        program_type: &str,
        regions: Option<Vec<&str>>,
        enrollment: Option<Vec<&str>>,
        ages: (Option<i32>, Option<i32>),
    ) -> Program {
        let now = chrono::Utc::now();
        serde_json::from_value(json!({
            "id": uuid::Uuid::new_v4(), "program_type": program_type, "source_type": "manual",
            "source_id": null, "title": "property", "summary": null, "provider_name": null,
            "official_url": null, "program_status": "active", "application_start_at": null,
            "application_end_at": null, "benefit_amount_monthly": null,
            "benefit_amount_semester": null, "benefit_amount_once": null, "region_scope": null,
            "school_scope": null, "tags": null, "raw_payload": null,
            "normalized_payload": enrollment.map(|e| json!({"enrollment_statuses": e})),
            "min_age": ages.0, "max_age": ages.1, "regions": regions, "deadline_at": null,
            "benefit_valid_until": null, "is_active": true, "last_synced_at": null,
            "created_at": now, "updated_at": now, "company_name": null,
            "company_logo_url": null, "benefit_category": null, "provider_kind": null,
            "application_steps": null, "application_method": null,
            "submission_documents": null, "screening_method": null
        }))
        .unwrap()
    }

    fn input(
        birth_year: i32,
        region: &str,
        enrollment: Option<&str>,
        income: Option<i32>,
    ) -> ProfileInput {
        serde_json::from_value(json!({
            "birth_year": birth_year, "region_code": region,
            "enrollment_status": enrollment, "income_bracket": income
        }))
        .unwrap()
    }

    fn saved(input: &ProfileInput) -> UserProfile {
        serde_json::from_value(json!({
            "user_id": uuid::Uuid::nil(), "birth_year": input.birth_year,
            "region_code": input.region_code, "city_code": null, "school_name": null,
            "school_year": null, "enrollment_status": input.enrollment_status,
            "employment_status": null, "major_group": null,
            "income_bracket": input.income_bracket, "kosaf_support_bracket": null,
            "housing_type": null, "household_size": null,
            "has_disability": input.has_disability,
            "is_multicultural_family": input.is_multicultural_family,
            "is_low_income_household": input.is_low_income_household,
            "veteran_family": input.veteran_family, "preferred_categories": [], "school_type": null,
            "age_band": null, "profile_version": 1, "updated_at": chrono::Utc::now(),
            "nickname": null, "profile_image_url": null
        }))
        .unwrap()
    }

    fn rule(hard: Option<Value>) -> Option<EligibilityRule> {
        hard.map(|hard| {
            serde_json::from_value(json!({
                "id": uuid::Uuid::new_v4(), "program_id": uuid::Uuid::nil(),
                "rule_json": hard, "hard_filter_json": hard, "version": 1,
                "created_at": chrono::Utc::now(), "updated_at": chrono::Utc::now()
            }))
            .unwrap()
        })
    }

    /// A reviewed `organisation`/`business` row (hidden from the list as
    /// not_individual) is never "일치해요"; a listed `indirect` row is never
    /// `fail` and keeps the list's `pass` (H2, review M2).
    #[test]
    fn applicant_review_matches_the_list() {
        let p = program("policy", Some(vec!["busan"]), None, (None, None));
        let i = input(2000, "busan", None, None);
        let profile = saved(&i);
        let hard = rule(Some(
            json!({"field": "region_code", "op": "eq", "value": "busan"}),
        ));
        let assessed = || assess(&p, None, Some(&profile), None, hard.as_ref());
        assert_eq!(assessed().status, RuleVerdict::Pass);

        let mut hidden = assessed();
        hidden.apply_applicant_review(ReviewEffect::Hide);
        assert_eq!(hidden.status, RuleVerdict::Fail);
        assert_eq!(hidden.summary, summary(RuleVerdict::Fail));
        assert!(hidden.checks.iter().any(|c| c.label == "신청 주체"));

        // Review M2: the list keeps a demoted row's `pass`; so does the card,
        // which still carries the "신청 주체" note.
        let mut demoted = assessed();
        demoted.apply_applicant_review(ReviewEffect::Demote);
        assert_eq!(demoted.status, RuleVerdict::Pass);
        assert_eq!(demoted.summary, summary(RuleVerdict::Pass));
        let note = demoted.checks.iter().find(|c| c.label == "신청 주체");
        assert_eq!(note.map(|c| c.status), Some(RuleVerdict::Unknown));

        let mut unchanged = assessed();
        unchanged.apply_applicant_review(ReviewEffect::None);
        assert_eq!(unchanged.status, RuleVerdict::Pass);
        assert_eq!(unchanged.checks.len(), assessed().checks.len());

        // A demotion never softens an existing failure.
        let mut failing = assess(
            &p,
            None,
            Some(&saved(&input(2000, "seoul", None, None))),
            None,
            hard.as_ref(),
        );
        assert_eq!(failing.status, RuleVerdict::Fail);
        failing.apply_applicant_review(ReviewEffect::Demote);
        assert_eq!(failing.status, RuleVerdict::Fail);
    }

    /// Verifier gap 2 (audit H1 at the entry points): a hard `age lte 34`
    /// leaf counts on the notice window on the list and on the card. A
    /// birthday inside the window is unknown on both; one outside passes.
    #[test]
    fn rule_leaves_count_age_on_the_notice_window() {
        use chrono::{Datelike, Duration};
        let now = chrono::Utc::now();
        let today = mazimi_core::seoul_date(now);
        let mut p = program("policy", Some(vec!["busan"]), None, (None, None));
        p.application_start_at = Some(now - Duration::days(10));
        p.application_end_at = Some(now + Duration::days(20));
        let hard = rule(Some(json!({"all": [
            {"field": "region_code", "op": "eq", "value": "busan"},
            {"field": "age", "op": "lte", "value": 34}
        ]})));
        let born = |days_ahead: i64| {
            let birthday = today + Duration::days(days_ahead);
            birthday
                .with_year(birthday.year() - 35)
                .unwrap_or(birthday - Duration::days(1))
        };
        for (days, list, card) in [
            (5, "unknown", RuleVerdict::Unknown),
            (60, "pass", RuleVerdict::Pass),
        ] {
            let birth = born(days);
            let mut i = input(birth.year(), "busan", None, None);
            i.birth_date = Some(birth);
            let mut profile = saved(&i);
            profile.birth_date = Some(birth);
            let listed = evaluate(&p, None, &i, None, hard.as_ref()).unwrap().1;
            assert_eq!(listed.eligibility_status, list, "{birth}");
            let assessed = assess(&p, None, Some(&profile), None, hard.as_ref());
            assert_eq!(assessed.status, card, "{birth}");
        }
    }

    /// Review R-H3: the card takes the list's membership check. A row
    /// reserved for 장애인 with a passing rule is unknown on the card for a
    /// user who never answered the flag, with the question and a "지원 대상"
    /// check; the list says the same. A known member passes.
    #[test]
    fn the_card_caps_a_reserved_row_as_the_list_does() {
        let mut p = program("policy", Some(vec!["busan"]), None, (None, None));
        p.title = "장애인 보조기기 지원".into();
        p.source_type = "gov_benefits".into();
        p.raw_payload = Some(json!({"지원대상": "○ 등록 장애인", "사용자구분": "개인"}));
        let hard = rule(Some(
            json!({"field": "region_code", "op": "eq", "value": "busan"}),
        ));
        let i = input(2000, "busan", None, None);
        let profile = saved(&i);
        let card = assess(&p, None, Some(&profile), None, hard.as_ref());
        assert_eq!(card.status, RuleVerdict::Unknown);
        let check = card.checks.iter().find(|c| c.label == "지원 대상").unwrap();
        assert_eq!(check.status, RuleVerdict::Unknown);
        assert!(
            card.missing_fields.contains(&"has_disability".to_owned()),
            "{:?}",
            card.missing_fields
        );
        let listed = evaluate(&p, None, &i, None, hard.as_ref()).unwrap().1;
        assert_eq!(listed.eligibility_status, "unknown");
        let mut member = profile.clone();
        member.has_disability = Some(true);
        assert_eq!(
            assess(&p, None, Some(&member), None, hard.as_ref()).status,
            RuleVerdict::Pass
        );
        // The stored tier alone reserves it too.
        let mut plain = p.clone();
        plain.title = "보조기기 지원".into();
        plain.raw_payload = None;
        let tiered = assess_with_overrides(
            &plain,
            None,
            Some(&profile),
            None,
            hard.as_ref(),
            &[],
            Some(mazimi_core::youth_fit::Tier {
                verdict: mazimi_core::youth_fit::Verdict::DisabilityOnly,
                target: Some(mazimi_core::youth_fit::Target::Disability),
            }),
        );
        assert_eq!(tiered.status, RuleVerdict::Unknown);
    }

    /// F6.1: the card names a coded income condition and never passes on
    /// it, as the list does.
    #[test]
    fn a_coded_income_condition_is_an_unknown_card() {
        let mut p = program("policy", Some(vec!["busan"]), None, (None, None));
        let hard = rule(Some(
            json!({"field": "region_code", "op": "eq", "value": "busan"}),
        ));
        let i = input(2000, "busan", None, None);
        let profile = saved(&i);
        assert_eq!(
            assess(&p, None, Some(&profile), None, hard.as_ref()).status,
            RuleVerdict::Pass
        );
        p.normalized_payload = Some(json!({"income_median_bands": ["0-50"]}));
        // Audit M1: the coded bound decides only when the notice states it.
        p.raw_payload = Some(json!({"지원대상": "기준 중위소득 50% 이하 청년"}));
        // No band answer: unknown, on the card and in the list.
        let assessed = assess(&p, None, Some(&profile), None, hard.as_ref());
        assert_eq!(assessed.status, RuleVerdict::Unknown);
        let card = assessed.checks.iter().find(|c| c.label == "소득").unwrap();
        assert_eq!(card.status, RuleVerdict::Unknown);
        let listed = evaluate(&p, None, &i, None, hard.as_ref()).unwrap().1;
        assert_eq!(listed.eligibility_status, "unknown");
        // A band within the bound passes on both; one above it fails on
        // the card and is not listed.
        for (band, status) in [("le50", RuleVerdict::Pass), ("le60", RuleVerdict::Fail)] {
            let mut answered = i.clone();
            answered.income_median_band = Some(band.into());
            let mut saved_answer = saved(&answered);
            saved_answer.income_median_band = Some(band.into());
            let assessed = assess(&p, None, Some(&saved_answer), None, hard.as_ref());
            let card = assessed.checks.iter().find(|c| c.label == "소득").unwrap();
            assert_eq!(card.status, status, "{band}");
            assert_eq!(assessed.status, status, "{band}");
            let listed = evaluate(&p, None, &answered, None, hard.as_ref());
            match status {
                RuleVerdict::Pass => assert_eq!(listed.unwrap().1.eligibility_status, "pass"),
                _ => assert_eq!(
                    listed.err(),
                    Some(mazimi_core::models::ExclusionReason::Income)
                ),
            }
        }
        // A bound the text does not state is a named check, never a hide.
        p.raw_payload = Some(json!({"지원대상": "관내 거주 청년 누구나"}));
        let mut above = i.clone();
        above.income_median_band = Some("le60".into());
        let listed = evaluate(&p, None, &above, None, hard.as_ref()).unwrap().1;
        assert_eq!(listed.eligibility_status, "unknown");
    }

    /// Code overrides of a current review the property feeds: none; widen
    /// age, region and job and drop enrollment; drop age and widen job.
    fn override_sets() -> [Option<Value>; 3] {
        let other = |o: Value| {
            let mut o = o;
            o["check_kind"] = json!("other");
            o["evidence"] = json!("x");
            o
        };
        [
            None,
            Some(json!([
                other(json!({"target": "S-age", "action": "widen",
                    "replacement": {"field": "age", "op": "between", "value": [15, 40]}})),
                other(json!({"target": "S-region", "action": "widen",
                    "replacement": {"field": "region_code", "op": "in", "value": ["busan", "seoul"]}})),
                other(json!({"target": "S-enrollment", "action": "drop"})),
                other(json!({"target": "C-job", "action": "drop"})),
            ])),
            Some(json!([
                other(json!({"target": "S-age", "action": "drop"})),
                other(json!({"target": "C-job", "action": "widen",
                    "replacement": {"field": "employment_status", "op": "in",
                                    "value": ["preparing", "student", "unemployed", "employed"]}})),
            ])),
        ]
    }

    /// H2 property: whatever the recommendation list shows, the assistant
    /// never explains as `fail`; a listed `pass` is explained as `pass`.
    /// Re-review M: the same holds for rows a current review's code
    /// overrides widen (the card reads the row as the list does).
    #[test]
    fn listed_programmes_are_never_explained_as_fail() {
        use crate::services::condition_reviews::ReviewFacts;
        use crate::services::recommendations::{evaluate_ranked, Relevance, ReviewContext};
        use mazimi_core::condition_review::{overrides, ServeReview};
        let year = rule_engine::age_candidates(2000)[1] + 2000;
        let now = chrono::Utc::now();
        let job = json!({"field": "employment_status", "op": "in",
                         "value": ["preparing", "student", "unemployed"], "origin": "source_codes"});
        let mut listed = 0;
        let mut passes = 0;
        let mut overridden_listed = 0;
        for program_type in [
            "scholarship",
            "policy",
            "campus_benefit",
            "corporate_benefit",
        ] {
            for regions in [
                None,
                Some(vec![]),
                Some(vec!["busan"]),
                Some(vec!["seoul"]),
                Some(vec!["unknown"]),
            ] {
                for enrollment in [
                    None,
                    Some(vec!["enrolled"]),
                    Some(vec!["on_leave", "graduated"]),
                ] {
                    for ages in [(None, None), (Some(19), Some(34)), (None, Some(24))] {
                        for univ in [None, Some("pusan"), Some("snu")] {
                            for hard in [
                                None,
                                Some(json!({"field":"income_bracket","op":"lte","value":5})),
                                Some(json!({"all": [job.clone()]})),
                            ] {
                                let p = program(
                                    program_type,
                                    regions.clone(),
                                    enrollment.clone(),
                                    ages,
                                );
                                let r = rule(hard.clone());
                                for set in override_sets() {
                                    let facts = set.as_ref().map(|list| ReviewFacts {
                                        review: Some(
                                            ServeReview::from_columns(
                                                "incomplete",
                                                None,
                                                &json!({"kind": "unknown"}),
                                                vec!["other".into()],
                                                None,
                                                true,
                                            )
                                            .with_code_overrides(Some(list)),
                                        ),
                                        applicant: None,
                                    });
                                    let applied: Vec<&CodeOverride> = facts
                                        .as_ref()
                                        .and_then(|f| f.review.as_ref())
                                        .map(|review| {
                                            overrides::applicable(
                                                &review.code_overrides,
                                                &overrides::CodeColumns::of(
                                                    &p,
                                                    r.as_ref()
                                                        .and_then(|r| r.hard_filter_json.as_ref()),
                                                ),
                                            )
                                        })
                                        .unwrap_or_default();
                                    for birth in [year - 20, year - 25, year - 35] {
                                        for status in [None, Some("enrolled"), Some("on_leave")] {
                                            for income in [None, Some(3), Some(9)] {
                                                for school in [None, Some("pusan"), Some("snu")] {
                                                    // Overrides never touch the school check:
                                                    // one school pair is enough for them.
                                                    if set.is_some()
                                                        && (univ == Some("snu")
                                                            || school == Some("snu"))
                                                    {
                                                        continue;
                                                    }
                                                    let mut i =
                                                        input(birth, "busan", status, income);
                                                    // Income 9 also answers employment, so the
                                                    // job rule and its overrides are decided.
                                                    if income == Some(9) {
                                                        i.employment_status =
                                                            Some("employed".into());
                                                    }
                                                    let mut profile = saved(&i);
                                                    profile.employment_status =
                                                        i.employment_status.clone();
                                                    let Ok((_, item)) = evaluate_ranked(
                                                        &p,
                                                        univ,
                                                        &i,
                                                        school,
                                                        r.as_ref(), &crate::services::recommendations::unreserved(),
                                                        Relevance::default(),
                                                        ReviewContext {
                                                            facts: facts.as_ref(),
                                                            now,
                                                            section_enabled: false,
                                                        }) else {
                                                        continue;
                                                    };
                                                    listed += 1;
                                                    if !applied.is_empty() {
                                                        overridden_listed += 1;
                                                    }
                                                    let assessed = || {
                                                        let mut a = assess_with_overrides(
                                                            &p,
                                                            univ,
                                                            Some(&profile),
                                                            school,
                                                            r.as_ref(),
                                                            &applied,
                                                            None,
                                                        );
                                                        use crate::services::recommendations::Listing;
                                                        a.apply_list_decision(Some(
                                                            if item.eligibility_status == "pass" {
                                                                Listing::Pass
                                                            } else {
                                                                Listing::Open {
                                                                    audience_open: false,
                                                                }
                                                            },
                                                        ));
                                                        a
                                                    };
                                                    let a = assessed();
                                                    // A reviewed `indirect` row stays listed
                                                    // with its status (review M2).
                                                    let mut demoted = assessed();
                                                    demoted.apply_applicant_review(
                                                        ReviewEffect::Demote,
                                                    );
                                                    for a in [&a, &demoted] {
                                                        assert_ne!(a.status, RuleVerdict::Fail, "{program_type} {regions:?} {enrollment:?} {ages:?} {univ:?} {hard:?} {set:?} {birth} {status:?} {income:?} {school:?}");
                                                        if item.eligibility_status == "pass" {
                                                            assert_eq!(a.status, RuleVerdict::Pass, "{program_type} {regions:?} {enrollment:?} {status:?} {school:?} {set:?}");
                                                        } else {
                                                            // The other direction: a row the list does not pass never passes on the card.
                                                            assert_ne!(a.status, RuleVerdict::Pass, "{program_type} {regions:?} {enrollment:?} {status:?} {school:?} {set:?}");
                                                        }
                                                    }
                                                    if item.eligibility_status == "pass" {
                                                        passes += 1;
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        assert!(
            listed > 1000 && passes > 50 && overridden_listed > 1000,
            "{listed} listed, {passes} pass, {overridden_listed} overridden"
        );
    }

    /// Pre-merge (mutation b): a card whose own checks all pass is `pass`
    /// only when the list passes the row. An open listing (a disagreement
    /// between the two evaluators, or an open audience) keeps it `unknown`.
    #[test]
    fn an_open_listing_never_passes_the_card() {
        use crate::services::recommendations::Listing;
        let p = program("policy", Some(vec!["busan"]), None, (None, None));
        let hard = rule(Some(
            json!({"field": "region_code", "op": "eq", "value": "busan"}),
        ));
        let profile = saved(&input(2000, "busan", None, None));
        let card =
            || assess_with_overrides(&p, None, Some(&profile), None, hard.as_ref(), &[], None);
        let own = card();
        assert!(
            own.checks.iter().all(|c| c.status == RuleVerdict::Pass),
            "every check of the card passes on its own"
        );
        let mut passed = card();
        passed.apply_list_decision(Some(Listing::Pass));
        assert_eq!(passed.status, RuleVerdict::Pass);
        for listing in [
            Listing::Open {
                audience_open: false,
            },
            Listing::Open {
                audience_open: true,
            },
        ] {
            let mut a = card();
            a.apply_list_decision(Some(listing));
            assert_eq!(a.status, RuleVerdict::Unknown, "{listing:?}");
            assert_eq!(a.summary, summary(RuleVerdict::Unknown), "{listing:?}");
        }
        let mut undecided = card();
        undecided.apply_list_decision(None);
        assert_eq!(undecided.status, RuleVerdict::Unknown);
    }

    /// Re-review M: the 37-year-old veteran case. The list shows the row
    /// (age widened to 39); the card and the grounded age verdict must not
    /// say the age condition fails.
    #[test]
    fn a_widened_age_is_never_explained_as_a_fail() {
        use crate::assistant_provider::Topic;
        use crate::services::grounded_qa::verdict::{self, VerdictCheck};
        let year = rule_engine::age_candidates(2000)[1] + 2000;
        let p = program("policy", None, None, (Some(18), Some(34)));
        let veteran: CodeOverride = serde_json::from_value(json!({
            "target": "S-age", "action": "widen",
            "replacement": {"field": "age", "op": "between", "value": [18, 39]},
            "check_kind": "military_service", "evidence": "x"}))
        .unwrap();
        let profile = saved(&input(year - 37, "busan", None, None));
        let facts = StructuralProfile {
            birth_year: profile.birth_year,
            region_code: profile.region_code.as_deref(),
            ..Default::default()
        };
        let card = |o: &[&CodeOverride]| {
            assess_with_overrides(&p, None, Some(&profile), None, None, o, None)
        };
        let before = card(&[]);
        assert_eq!(
            before.status,
            RuleVerdict::Fail,
            "the code alone says 18-34"
        );
        let after = card(&[&veteran]);
        assert_ne!(after.status, RuleVerdict::Fail);
        let age = after.checks.iter().find(|c| c.label == "연령").unwrap();
        assert_eq!(age.status, RuleVerdict::Unknown);
        assert!(
            age.detail.contains("병역 조건 확인이 필요해요"),
            "{}",
            age.detail
        );
        let v = verdict::verdict(
            "나이 제한 있어?",
            &[Topic::Eligibility],
            &p,
            None,
            &facts,
            &after.overall(),
        )
        .unwrap();
        assert_eq!(v.check, VerdictCheck::Age);
        assert_eq!(v.status, RuleVerdict::Unknown);
        // Listed only through the exception: the list's wording, on both.
        let exception = "연령 조건 예외에 해당하는지 확인이 필요해요";
        assert!(age.detail.starts_with(exception), "{}", age.detail);
        assert!(v.summary.starts_with(exception), "{}", v.summary);
        // Inside the code band the override admits nothing: no exception.
        let younger = saved(&input(year - 30, "busan", None, None));
        let inside = assess_with_overrides(&p, None, Some(&younger), None, None, &[&veteran], None);
        let age = inside.checks.iter().find(|c| c.label == "연령").unwrap();
        assert!(!age.detail.contains(exception), "{}", age.detail);
        let v = verdict::verdict(
            "나이 제한 있어?",
            &[Topic::Eligibility],
            &p,
            None,
            &StructuralProfile {
                birth_year: younger.birth_year,
                region_code: younger.region_code.as_deref(),
                ..Default::default()
            },
            &inside.overall(),
        )
        .unwrap();
        assert!(!v.summary.contains(exception), "{}", v.summary);
        // Past the widened band it still fails, on both.
        let older = saved(&input(year - 45, "busan", None, None));
        assert_eq!(
            assess_with_overrides(&p, None, Some(&older), None, None, &[&veteran], None).status,
            RuleVerdict::Fail
        );
    }

    /// Grounded Q&A property: the verdict next to quoted passages never
    /// contradicts the eligibility card (same check, same status) and is
    /// never `fail` for a programme the recommendation list shows.
    #[test]
    fn grounded_verdicts_never_contradict_the_assessment() {
        use crate::assistant_provider::Topic;
        use crate::services::grounded_qa::verdict::{self, VerdictCheck};
        let year = rule_engine::age_candidates(2000)[1] + 2000;
        let questions = [
            "휴학생도 돼?",
            "부산 살아도 돼?",
            "서울 살면 돼?",
            "나이 제한 있어?",
            "학교 상관없어?",
            "소득 기준은?",
            "나도 받을 수 있어?",
        ];
        let mut compared = 0;
        for program_type in ["scholarship", "campus_benefit", "corporate_benefit"] {
            for regions in [
                None,
                Some(vec!["busan"]),
                Some(vec!["seoul"]),
                Some(vec!["unknown"]),
            ] {
                for enrollment in [None, Some(vec!["enrolled"]), Some(vec!["on_leave"])] {
                    for ages in [(None, None), (Some(19), Some(34))] {
                        for hard in [
                            None,
                            Some(json!({"field":"income_bracket","op":"lte","value":5})),
                        ] {
                            let p =
                                program(program_type, regions.clone(), enrollment.clone(), ages);
                            let r = rule(hard.clone());
                            for birth in [year - 20, year - 35] {
                                for status in [None, Some("enrolled"), Some("on_leave")] {
                                    for school in [None, Some("pusan")] {
                                        let i = input(birth, "busan", status, Some(3));
                                        let profile = saved(&i);
                                        let a = assess(
                                            &p,
                                            Some("pusan"),
                                            Some(&profile),
                                            school,
                                            r.as_ref(),
                                        );
                                        let listed =
                                            evaluate(&p, Some("pusan"), &i, school, r.as_ref())
                                                .is_ok();
                                        let facts = StructuralProfile {
                                            birth_year: profile.birth_year,
                                            birth_date: profile.birth_date,
                                            region_code: profile.region_code.as_deref(),
                                            enrollment_status: profile.enrollment_status.as_deref(),
                                            university_id: school,
                                            city_code: profile.city_code.as_deref(),
                                        };
                                        let overall = a.overall();
                                        for question in questions {
                                            let v = verdict::verdict(
                                                question,
                                                &[Topic::Eligibility],
                                                &p,
                                                Some("pusan"),
                                                &facts,
                                                &overall,
                                            )
                                            .expect("eligibility questions get a verdict");
                                            let card = |label: &str| {
                                                a.checks
                                                    .iter()
                                                    .find(|c| c.label == label)
                                                    .map(|c| c.status)
                                            };
                                            let expected = match v.check {
                                                VerdictCheck::Overall => Some(a.status),
                                                VerdictCheck::Income => a.income(),
                                                VerdictCheck::Age => card("연령"),
                                                VerdictCheck::Region => card("지역"),
                                                VerdictCheck::School => card("학교"),
                                                VerdictCheck::Enrollment => card("학적"),
                                            };
                                            assert_eq!(
                                                v.registered,
                                                expected.is_some(),
                                                "{question} {program_type}"
                                            );
                                            assert_eq!(v.status, expected.unwrap_or(RuleVerdict::Unknown), "{question} {program_type} {regions:?} {enrollment:?} {status:?}");
                                            if listed {
                                                assert_ne!(
                                                    v.status,
                                                    RuleVerdict::Fail,
                                                    "{question} listed {program_type}"
                                                );
                                            }
                                            compared += 1;
                                        }
                                        // Round 2 (code M-7): a two-condition
                                        // question is its worst part as a
                                        // pair, the named value counted.
                                        for (both, parts) in [
                                            (
                                                "서울 사는데 소득 기준은?",
                                                ["서울 살면 돼?", "소득 기준은?"],
                                            ),
                                            (
                                                "휴학생인데 나이 제한 있어?",
                                                ["휴학생도 돼?", "나이 제한 있어?"],
                                            ),
                                            (
                                                "부산 살고 나이 제한 있어?",
                                                ["부산 살아도 돼?", "나이 제한 있어?"],
                                            ),
                                        ] {
                                            let ask = |q: &str| {
                                                verdict::verdict(
                                                    q,
                                                    &[Topic::Eligibility],
                                                    &p,
                                                    Some("pusan"),
                                                    &facts,
                                                    &overall,
                                                )
                                                .unwrap()
                                            };
                                            let rank = |s: RuleVerdict| match s {
                                                RuleVerdict::Pass => 0,
                                                RuleVerdict::Unknown => 1,
                                                RuleVerdict::Fail => 2,
                                            };
                                            let parts: Vec<_> = parts
                                                .iter()
                                                .map(|q| {
                                                    let v = ask(q);
                                                    let part =
                                                        v.asked_status.map_or(v.status, |a| {
                                                            if rank(a) > rank(v.status) {
                                                                a
                                                            } else {
                                                                v.status
                                                            }
                                                        });
                                                    (v.check, part)
                                                })
                                                .collect();
                                            let worst = parts
                                                .iter()
                                                .copied()
                                                .max_by_key(|(_, s)| rank(*s))
                                                .unwrap();
                                            let v = ask(both);
                                            assert_eq!((v.check, v.status), worst, "{both} {program_type} {regions:?} {enrollment:?} {status:?}");
                                            assert_eq!(v.asked_value, None);
                                            compared += 1;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        assert!(compared > 5000, "{compared}");
    }

    /// Audit H2: an income question is answered by the income conditions
    /// (the coded 기준 중위소득 check and income leaves of the hard rule),
    /// never by the status of the whole hard-rule bundle.
    #[test]
    fn an_income_question_reads_the_income_conditions() {
        use crate::assistant_provider::Topic;
        use crate::services::grounded_qa::verdict;
        let ask = |p: &Program, profile: &UserProfile, hard: Option<&EligibilityRule>| {
            let a = assess(p, None, Some(profile), None, hard);
            let facts = StructuralProfile {
                birth_year: profile.birth_year,
                region_code: profile.region_code.as_deref(),
                ..Default::default()
            };
            verdict::verdict(
                "소득 기준 넘어도 돼?",
                &[Topic::Eligibility],
                p,
                None,
                &facts,
                &a.overall(),
            )
            .unwrap()
        };
        let region = rule(Some(
            json!({"field": "region_code", "op": "eq", "value": "busan"}),
        ));
        let mut p = program("policy", Some(vec!["busan"]), None, (None, None));
        let mut profile = saved(&input(2000, "busan", None, None));
        // No income condition anywhere: nothing registered, unknown.
        let v = ask(&p, &profile, region.as_ref());
        assert_eq!((v.status, v.registered), (RuleVerdict::Unknown, false));
        // A coded bound the notice does not state decides nothing (the
        // sibling's income reader): unknown, registered.
        p.normalized_payload = Some(json!({"income_median_bands": ["0-50"]}));
        profile.income_median_band = Some("le60".into());
        let v = ask(&p, &profile, region.as_ref());
        assert_eq!((v.status, v.registered), (RuleVerdict::Unknown, true));
        // The bound stated in the notice, the saved band above it: fail,
        // whatever the bundle.
        p.raw_payload = Some(json!({"지원대상": "기준 중위소득 50% 이하 청년"}));
        let v = ask(&p, &profile, region.as_ref());
        assert_eq!((v.status, v.registered), (RuleVerdict::Fail, true));
        // No band saved: unknown.
        profile.income_median_band = None;
        assert_eq!(
            ask(&p, &profile, region.as_ref()).status,
            RuleVerdict::Unknown
        );
        // An income leaf of the hard rule is read alone.
        let p = program("policy", Some(vec!["busan"]), None, (None, None));
        let leaf = rule(Some(json!({"all": [
            {"field": "region_code", "op": "eq", "value": "busan"},
            {"field": "income_bracket", "op": "lte", "value": 5}
        ]})));
        for (bracket, status) in [(3, RuleVerdict::Pass), (9, RuleVerdict::Fail)] {
            let profile = saved(&input(2000, "busan", None, Some(bracket)));
            let v = ask(&p, &profile, leaf.as_ref());
            assert_eq!((v.status, v.registered), (status, true), "{bracket}");
        }
    }

    /// Audit H1, the reverse property: a row the recommendation list hides
    /// is never explained as `pass`. The four gates the card skipped: the
    /// audience text, the stored youth_fit verdict, and a complete current
    /// review whose rule fails or whose window is closed.
    #[test]
    fn rows_the_list_hides_are_never_explained_as_pass() {
        use crate::services::condition_reviews::ReviewFacts;
        use crate::services::recommendations::{self, GateColumns, ReviewContext};
        use mazimi_core::condition_review::ServeReview;
        let now = chrono::Utc::now();
        let today = mazimi_core::seoul_date(now);
        let window = |opens: chrono::NaiveDate, closes: chrono::NaiveDate| json!({"kind": "dated", "opens_at": opens, "closes_at": closes});
        let open = window(today - chrono::Days::new(10), today + chrono::Days::new(10));
        let closed = window(today - chrono::Days::new(30), today - chrono::Days::new(1));
        let review = |rule: Option<Value>, window: &Value| ReviewFacts {
            review: Some(ServeReview::from_columns(
                "complete",
                rule.as_ref(),
                window,
                Vec::new(),
                None,
                true,
            )),
            applicant: None,
        };
        let job = json!({"all": [{"id": "R1", "field": "employment_status", "op": "in",
            "value": ["unemployed", "preparing"], "origin": "review", "evidence": "미취업 청년"}]});
        let hard = rule(Some(
            json!({"field": "region_code", "op": "eq", "value": "busan"}),
        ));
        let mut profile = saved(&input(2000, "busan", None, None));
        profile.employment_status = Some("employed".into());
        // The list reads the saved profile, as `decide_on` does.
        let i = list_input(&profile);
        // (title, youth_fit verdict and target, review, card status). A
        // stated contradiction (an age outside a closed list) fails through
        // the card's own membership check. A saved flag is never an
        // answered "no" (`stored_flag`), so a reserved tier is listed open
        // (`a_reserved_row_with_an_unanswered_flag_never_passes`); youth_fit
        // hides without an answer read unknown
        // (`a_youth_fit_target_hide_is_never_a_fail`).
        type Case<'a> = (
            &'a str,
            Option<(&'a str, &'a str)>,
            Option<ReviewFacts>,
            RuleVerdict,
        );
        let cases: [Case; 4] = [
            // A closed list the profile's age is outside of.
            ("어르신 교통비 지원", None, None, RuleVerdict::Fail),
            (
                "청년 교통비 지원",
                Some(("off_target", "organisation")),
                None,
                RuleVerdict::Fail,
            ),
            (
                "청년 교통비 지원",
                None,
                Some(review(Some(job), &open)),
                RuleVerdict::Fail,
            ),
            (
                "청년 교통비 지원",
                None,
                Some(review(None, &closed)),
                RuleVerdict::Fail,
            ),
        ];
        for (title, fit, facts, expected) in cases {
            let mut p = program("policy", Some(vec!["busan"]), None, (None, None));
            p.title = title.into();
            let gate = GateColumns {
                youth_fit: fit.map(|(v, _)| v.to_owned()),
                youth_fit_target: fit.map(|(_, t)| t.to_owned()),
                ..GateColumns::default()
            };
            let row = gate.facts(&p, None);
            let review = ReviewContext {
                facts: facts.as_ref(),
                now,
                section_enabled: false,
            };
            let listed = recommendations::decide(&p, None, &i, None, hard.as_ref(), &row, review);
            assert!(listed.is_err(), "{title} {fit:?}: the list hides it");
            let tier = fit.and_then(|(v, t)| mazimi_core::youth_fit::Tier::parse(v, Some(t)));
            let mut card =
                assess_with_overrides(&p, None, Some(&profile), None, hard.as_ref(), &[], tier);
            card.apply_list_decision(Some(recommendations::Listing::of(&listed)));
            assert_eq!(card.status, expected, "{title} {fit:?}");
            assert_eq!(card.summary, summary(expected));
            assert!(
                card.checks.iter().any(|c| c.status == expected),
                "{title}: a check names why"
            );
        }
    }

    /// The list's decision input from a saved profile, as `decide_on` builds it.
    fn list_input(profile: &UserProfile) -> ProfileInput {
        let mut i = super::super::bundles::profile_input(profile.clone()).expect("decidable");
        crate::services::profile_validation::normalize_profile(&mut i).expect("valid");
        i
    }

    /// Final review H-1: the list hides `off_target` (farm, elder, other,
    /// none) and `child_only` rows by the stored youth_fit verdict without
    /// reading an answer. The hide carries nothing the card lacks: the card
    /// reads `unknown` with exactly one non-pass "지원 대상" check, never
    /// `fail` (the briefing's "현재 조건 불일치 확인" keys on `fail`).
    #[test]
    fn a_youth_fit_target_hide_is_never_a_fail() {
        use crate::services::recommendations::{self, GateColumns, Listing, ReviewContext};
        let hard = rule(Some(
            json!({"field": "region_code", "op": "eq", "value": "busan"}),
        ));
        let now = chrono::Utc::now();
        let year = mazimi_core::seoul_year(now);
        // (title, verdict, target, birth year)
        let rows: [(&str, &str, Option<&str>, i32); 6] = [
            (
                "농업인 경영 지원",
                "off_target",
                Some("farm_fishery"),
                year - 24,
            ),
            ("생활 지원", "off_target", Some("elder"), year - 24),
            // An elder the card's own audience check passes.
            ("어르신 생활 지원", "off_target", Some("elder"), year - 62),
            ("직업병 환자 지원", "off_target", Some("other"), year - 24),
            ("생활 지원", "off_target", None, year - 24),
            // Under 25, household unanswered.
            ("첫만남이용권", "child_only", Some("child"), year - 24),
        ];
        for (title, verdict, target, birth_year) in rows {
            let mut p = program("policy", Some(vec!["busan"]), None, (None, None));
            p.title = title.into();
            let gate = GateColumns {
                youth_fit: Some(verdict.to_owned()),
                youth_fit_target: target.map(str::to_owned),
                ..GateColumns::default()
            };
            let profile = saved(&input(birth_year, "busan", None, None));
            let listed = recommendations::decide(
                &p,
                None,
                &list_input(&profile),
                None,
                hard.as_ref(),
                &gate.facts(&p, None),
                ReviewContext::none(now),
            );
            let case = format!("{title} {verdict} {target:?} {birth_year}");
            assert_eq!(
                Listing::of(&listed),
                Listing::Hidden(ExclusionReason::TargetGroup),
                "{case}: the list hides it"
            );
            let tier = mazimi_core::youth_fit::Tier::parse(verdict, target);
            let mut card =
                assess_with_overrides(&p, None, Some(&profile), None, hard.as_ref(), &[], tier);
            card.apply_list_decision(Some(Listing::of(&listed)));
            assert_eq!(card.status, RuleVerdict::Unknown, "{case}");
            assert_eq!(card.summary, summary(RuleVerdict::Unknown), "{case}");
            let open: Vec<_> = card
                .checks
                .iter()
                .filter(|c| c.label == "지원 대상" && c.status != RuleVerdict::Pass)
                .collect();
            assert_eq!(open.len(), 1, "{case}: one open audience check");
            assert_eq!(open[0].status, RuleVerdict::Unknown, "{case}");
        }
    }

    /// Rebase (arch H1): a row reserved for a group, its flag unanswered,
    /// its rule passing, is listed but never `pass`: the list, `decide`
    /// and the card all read `unknown` with a named "지원 대상" check; a
    /// member passes on all three. Both the audience text path and the
    /// youth_fit tier path; an assumed region keeps it open.
    #[test]
    fn a_reserved_row_with_an_unanswered_flag_never_passes() {
        use crate::services::recommendations::{self, GateColumns, Listing, ReviewContext};
        use mazimi_core::youth_fit::Verdict;
        let hard = rule(Some(
            json!({"field": "region_code", "op": "eq", "value": "busan"}),
        ));
        let now = chrono::Utc::now();
        // (title, audience text, stored tier)
        let rows: [(&str, Option<&str>, Option<Verdict>); 2] = [
            ("장애인 보조기기 지원", Some("○ 등록 장애인"), None),
            ("보조기기 지원", None, Some(Verdict::DisabilityOnly)),
        ];
        for (title, target, tier) in rows {
            let mut p = program("policy", Some(vec!["busan"]), None, (None, None));
            p.title = title.into();
            p.source_type = "gov_benefits".into();
            p.raw_payload = Some(json!({"사용자구분": "개인", "지원대상": target.unwrap_or("")}));
            let gate = GateColumns {
                audience_target: target.map(str::to_owned),
                applicant_kinds: Some("개인".into()),
                youth_fit: tier.map(|t| t.as_str().to_owned()),
                ..GateColumns::default()
            };
            let tier = tier.map(|verdict| mazimi_core::youth_fit::Tier {
                verdict,
                target: None,
            });
            for member in [false, true] {
                let mut i = input(2000, "busan", None, None);
                if member {
                    i.has_disability = Some(true);
                }
                let mut profile = saved(&i);
                profile.has_disability = member.then_some(true);
                let decided = recommendations::decide(
                    &p,
                    None,
                    &i,
                    None,
                    hard.as_ref(),
                    &gate.facts(&p, None),
                    ReviewContext::none(now),
                );
                let listing = Listing::of(&decided);
                let mut card =
                    assess_with_overrides(&p, None, Some(&profile), None, hard.as_ref(), &[], tier);
                card.apply_list_decision(Some(listing));
                if member {
                    assert_eq!(listing, Listing::Pass, "{title}");
                    assert_eq!(card.status, RuleVerdict::Pass, "{title}");
                } else {
                    assert_eq!(
                        listing,
                        Listing::Open {
                            audience_open: true
                        },
                        "{title}"
                    );
                    let item = decided.unwrap().item;
                    assert_eq!(item.eligibility_status, "unknown", "{title}");
                    assert!(!item.verified && !item.conditions_verified, "{title}");
                    assert_eq!(card.status, RuleVerdict::Unknown, "{title}");
                    assert!(
                        card.checks
                            .iter()
                            .any(|c| c.label == "지원 대상" && c.status == RuleVerdict::Unknown),
                        "{title}: the open check is named"
                    );
                    // Under an assumed region the row stays open.
                    let membership = recommendations::program_membership(
                        &p,
                        tier,
                        &recommendations::saved_audience_profile(
                            Some(&profile),
                            mazimi_core::seoul_year(now),
                        ),
                    );
                    let (_, assumed) = recommendations::evaluate_assumed(
                        &p,
                        None,
                        &i,
                        None,
                        hard.as_ref(),
                        &membership,
                        Default::default(),
                        ReviewContext::none(now),
                        &["region_code"],
                    )
                    .unwrap();
                    assert_eq!(assumed.eligibility_status, "unknown", "{title}");
                }
            }
        }
    }

    /// Audit M4: one question can ask several conditions; each is answered
    /// and the verdict is the worst of them. "대학생" is a target word, not
    /// an enrollment status.
    #[test]
    fn every_condition_a_question_asks_is_answered() {
        use crate::assistant_provider::Topic;
        use crate::services::grounded_qa::verdict::{self, VerdictCheck};
        let mut p = program(
            "scholarship",
            Some(vec!["busan"]),
            Some(vec!["enrolled"]),
            (None, None),
        );
        p.normalized_payload = Some(json!({"enrollment_statuses": ["enrolled"],
            "income_median_bands": ["0-50"]}));
        // The notice states the bound, so the coded band decides.
        p.raw_payload = Some(json!({"지원대상": "기준 중위소득 50% 이하 재학생"}));
        let hard = rule(Some(
            json!({"field": "region_code", "op": "eq", "value": "busan"}),
        ));
        let mut profile = saved(&input(2000, "busan", Some("enrolled"), None));
        profile.income_median_band = Some("le60".into());
        let a = assess(&p, None, Some(&profile), None, hard.as_ref());
        let facts = StructuralProfile {
            birth_year: profile.birth_year,
            region_code: profile.region_code.as_deref(),
            enrollment_status: profile.enrollment_status.as_deref(),
            ..Default::default()
        };
        let v = verdict::verdict(
            "부산 대학생인데 소득 기준 돼?",
            &[Topic::Eligibility],
            &p,
            None,
            &facts,
            &a.overall(),
        )
        .unwrap();
        // Round 2 (code M-7): check and status come from the worst part as
        // a pair; the passing region is not reported as failing.
        assert_eq!(v.check, VerdictCheck::Income, "{v:?}");
        assert_eq!(v.asked_value, None, "{v:?}");
        assert_eq!(v.status, RuleVerdict::Fail, "{v:?}");
        assert!(v.summary.contains("소득 조건"), "{}", v.summary);
        assert!(v.summary.contains("지역"), "{}", v.summary);
        // A value the question names counts: 서울 is not the programme's
        // region even though the saved region and the income pass.
        profile.income_median_band = Some("le50".into());
        let a = assess(&p, None, Some(&profile), None, hard.as_ref());
        let v = verdict::verdict(
            "서울 사는데 소득 기준 돼?",
            &[Topic::Eligibility],
            &p,
            None,
            &facts,
            &a.overall(),
        )
        .unwrap();
        assert_eq!(
            (v.check, v.status),
            (VerdictCheck::Region, RuleVerdict::Fail),
            "{v:?}"
        );
    }

    /// Review B5: a coded group or gender leaf names a check, never a
    /// profile field to ask, so it neither reads "추가 프로필 조건" on the
    /// card nor pushes an answerable field out of the first two.
    #[test]
    fn a_field_no_profile_answers_is_never_asked() {
        let p = program("policy", Some(vec!["busan"]), None, (None, None));
        let r = rule(Some(json!({"all": [
            {"field": "region_code", "op": "eq", "value": "busan"},
            {"field": "gender", "op": "eq", "value": "female", "severity": "label"},
            {"field": "target_group", "op": "in", "value": ["farmer"], "severity": "label"},
            {"field": "income_bracket", "op": "lte", "value": 5}
        ]})));
        let profile = saved(&input(2000, "busan", None, None));
        let card = assess(&p, None, Some(&profile), None, r.as_ref());
        for field in mazimi_core::rule_engine::NO_PROFILE_ANSWER {
            assert!(
                !card.missing_fields.iter().any(|f| f == field),
                "{:?}",
                card.missing_fields
            );
        }
        assert!(
            card.missing_fields.contains(&"income_bracket".to_owned()),
            "{:?}",
            card.missing_fields
        );
    }

    /// Verify V-M1: a field the profile answered is never asked again, even
    /// when its answer leaves the leaf undecided ("대학 (전문대 포함)" on a
    /// 전문대-only row): re-answering cannot change the result.
    #[test]
    fn an_answered_field_is_never_asked_again() {
        let p = program("scholarship", Some(vec!["busan"]), None, (None, None));
        let r = rule(Some(json!({"all": [
            {"field": "school_type", "op": "in", "value": ["college"]},
            {"field": "income_bracket", "op": "lte", "value": 5}
        ]})));
        let mut profile = saved(&input(2000, "busan", None, None));
        profile.school_type = Some("university".into());
        let card = assess(&p, None, Some(&profile), None, r.as_ref());
        assert!(
            !card.missing_fields.contains(&"school_type".to_owned()),
            "{:?}",
            card.missing_fields
        );
        assert!(
            card.missing_fields.contains(&"income_bracket".to_owned()),
            "{:?}",
            card.missing_fields
        );
    }

    #[test]
    fn missing_fields_follow_priority_not_alphabet() {
        let p = program(
            "scholarship",
            Some(vec!["busan"]),
            Some(vec!["enrolled"]),
            (None, Some(34)),
        );
        let r = rule(Some(json!({"field":"income_bracket","op":"lte","value":5})));
        assert_eq!(
            assess(&p, None, None, None, r.as_ref()).missing_fields,
            ["birth_year", "region_code"]
        );
        let mut profile = saved(&input(2000, "busan", None, None));
        assert_eq!(
            assess(&p, None, Some(&profile), None, r.as_ref()).missing_fields,
            ["enrollment_status", "income_bracket"]
        );
        profile.enrollment_status = Some("enrolled".into());
        assert_eq!(
            assess(&p, None, Some(&profile), None, r.as_ref()).missing_fields,
            ["income_bracket"]
        );
    }

    /// Rule values seen in production (2026-10-22) read as Korean labels.
    #[test]
    fn rule_and_region_details_never_show_codes() {
        let node = rule_engine::parse_rule_json(&json!({"all": [
            {"field": "employment_status", "op": "in", "value": ["preparing", "student", "unemployed"]},
            {"field": "enrollment_status", "op": "in", "value": ["enrolled", "재학", "on_leave"]},
            {"field": "housing_type", "op": "in", "value": ["rental", "boarding"]},
            {"field": "region_code", "op": "eq", "value": "ulsan"},
            {"field": "school_name", "op": "in", "value": ["부산대학교"]},
            {"field": "city_code", "op": "in", "value": ["26410", "99999"]}
        ]}))
        .unwrap();
        // describe_rule lists six conditions per group; the rest go in a second rule.
        let more = rule_engine::parse_rule_json(&json!({"any": [
            {"field": "major_group", "op": "in", "value": ["engineering", "it"]},
            {"field": "age_band", "op": "eq", "value": "19-24"},
            {"field": "marital_status", "op": "eq", "value": "married"}
        ]}))
        .unwrap();
        let text = format!("{}\n{}", describe_rule(&node, 0), describe_rule(&more, 0));
        for code in [
            "preparing",
            "student",
            "unemployed",
            "enrolled",
            "on_leave",
            "rental",
            "boarding",
            "ulsan",
            "city_code",
            "[\"",
            "26410",
            "99999",
            "engineering",
            "19-24",
            "married",
        ] {
            assert!(!text.contains(code), "{code} in {text}");
        }
        assert!(
            text.contains("고용 상태 다음 중 포함: 취업 준비, 학생, 미취업"),
            "{text}"
        );
        assert!(
            text.contains("학적 상태 다음 중 포함: 재학, 휴학"),
            "{text}"
        );
        assert!(text.contains("거주 지역 = 울산"), "{text}");
        assert!(text.contains("학교 다음 중 포함: 부산대학교"), "{text}");
        assert!(
            text.contains("거주 시·군·구 다음 중 포함: 부산 금정구, 기타 시·군·구"),
            "{text}"
        );
        assert!(
            text.contains("전공 계열 다음 중 포함: 공학계열, 기타"),
            "{text}"
        );
        assert!(text.contains("연령대 = 만 19~24세"), "{text}");
        assert!(text.contains("혼인 여부 = 기혼"), "{text}");

        let p = program(
            "policy",
            Some(vec!["Ulsan", "busan", "ulsan"]),
            None,
            (None, None),
        );
        let profile = saved(&input(2000, "ulsan", None, None));
        let region = assess(&p, None, Some(&profile), None, None)
            .checks
            .into_iter()
            .find(|c| c.label == "지역")
            .expect("region check");
        assert_eq!(region.detail, "등록된 대상 지역: 울산 · 부산");
    }
}
