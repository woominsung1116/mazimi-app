//! Structural eligibility: the column-level conditions every programme carries
//! outside its rule tree (age band, residence regions, campus school,
//! enrollment statuses). One function serves both the recommendation filter
//! and the assistant's explanation, so a programme listed as `pass` can never
//! be explained as `fail`.
//!
//! Semantics, shared by every caller:
//! - Age: `min_age`/`max_age` via [`rule_engine::age_gate_on`] over the
//!   programme's [`rule_engine::AgeBasis`]: the exact 만 나이 on each
//!   reference day when the date of birth is known, else the bare birth year,
//!   whose boundary cohort is `Unknown`, not `Fail`. A birthday inside the
//!   window is `Unknown` too.
//! - Region: no regions = nationwide (no check). `["unknown"]` = the source
//!   scope is unresolved → `Unknown` with `unresolved_scope`. Otherwise the
//!   saved region (canonicalised) must be listed.
//! - School: `campus_benefit` only. The selected school must resolve in the
//!   university catalogue and equal the programme's `university_id`; an
//!   unresolved school on either side is `Unknown` with `unresolved_scope`.
//! - Enrollment: any programme type whose normalized payload lists
//!   `enrollment_statuses` (non-empty array) requires one of them.
//! - District ([`district_check`]): only when the programme carries
//!   `region_districts`. `None` (not collected) adds nothing. A sido in
//!   `regions` with no listed district of that sido is sido-wide. Otherwise
//!   the user's `city_code` must be covered by a listed district of their
//!   sido (`Fail` if not); a missing, or coarser (시 for a 구 programme),
//!   `city_code` is `Unknown` with "시·군·구 확인 필요".
//!
//! Listing policy ([`listable`]): a recommendation hides `Fail` and any
//! unresolved scope (a university-only or region-unresolved offer is not a
//! nationwide offer). The explanation shows those as `Unknown`, never `Fail`.
use crate::{
    models::{ExclusionReason, Program},
    rule_engine::{self, RuleVerdict},
};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckKind {
    Age,
    Region,
    School,
    Enrollment,
    District,
    /// Who among individuals the row is reserved for
    /// ([`crate::audience::membership`]).
    Audience,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructuralCheck {
    pub kind: CheckKind,
    pub status: RuleVerdict,
    /// Profile field that would resolve an `Unknown`, when the gap is ours.
    pub missing_field: Option<&'static str>,
    /// `Unknown` because the programme or school scope is unresolved, not
    /// because the user left a field blank.
    pub unresolved_scope: bool,
}

/// The profile facts structural checks read. `university_id` is the catalogue
/// id the saved/entered school resolved to (`None` = no resolvable school).
#[derive(Debug, Clone, Copy, Default)]
pub struct StructuralProfile<'a> {
    pub birth_year: Option<i32>,
    /// Date of birth when known: the age check then uses the exact 만 나이.
    pub birth_date: Option<chrono::NaiveDate>,
    pub region_code: Option<&'a str>,
    pub enrollment_status: Option<&'a str>,
    pub university_id: Option<&'a str>,
    /// 시군구 code of the user's residence (`city_code`). A value that is not
    /// a 시군구 code, or lies outside `region_code`'s sido, reads as absent.
    pub city_code: Option<&'a str>,
}

/// Canonical 5-digit 시군구 code: an all-digit value of 5 or 10 characters
/// (법정동 codes carry the 시군구 code as their prefix). Renumbered provinces
/// (강원 42→51, 전북 45→52) take the current prefix so both spellings compare
/// equal.
pub fn district_code(value: &str) -> Option<String> {
    let v = value.trim();
    if !(v.len() == 5 || v.len() == 10) || !v.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let code = &v[..5];
    Some(match &code[..2] {
        "42" => format!("51{}", &code[2..]),
        "45" => format!("52{}", &code[2..]),
        _ => code.to_owned(),
    })
}

/// Canonical region key of a district code's sido.
pub fn district_region(code: &str) -> Option<&'static str> {
    Some(match code.get(..2)? {
        "11" => "seoul",
        "26" => "busan",
        "27" => "daegu",
        "28" => "incheon",
        "29" => "gwangju",
        "30" => "daejeon",
        "31" => "ulsan",
        "36" => "sejong",
        "41" => "gyeonggi",
        "42" | "51" => "gangwon",
        "43" => "chungbuk",
        "44" => "chungnam",
        "45" | "52" => "jeonbuk",
        "46" => "jeonnam",
        "47" => "gyeongbuk",
        "48" => "gyeongnam",
        "50" => "jeju",
        _ => return None,
    })
}

/// Whether a canonical 시군구 code ([`district_code`]) is a current
/// 시·군·구 in the numbering this module reads ([`crate::districts::CITY_CODES`]).
pub fn is_known_district(code: &str) -> bool {
    crate::districts::CITY_CODES.binary_search(&code).is_ok()
}

/// How the programme's district scope matched the user, for ranking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DistrictMatch {
    /// A listed district covers the user's district.
    Matched,
    /// Collected as covering the whole of the user's sido.
    SidoWide,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cover {
    Yes,
    No,
    /// The user gave a 시 (수원시 41110) and the programme names one of its 구
    /// (영통구 41117): cannot tell.
    Ambiguous,
}

/// The 시 a 구 belongs to (영통구 41117 → 수원시 41110), from the generated
/// [`crate::districts::CITY_PARENTS`]. Never read from the digits: 영동군
/// 43740 and 증평군 43745 share four.
fn parent_city(code: &str) -> Option<&'static str> {
    let parents = crate::districts::CITY_PARENTS;
    parents
        .binary_search_by_key(&code, |(child, _)| child)
        .ok()
        .map(|at| parents[at].1)
}

/// A district covers itself and, for a 시, each of its 구.
fn covers(program: &str, user: &str) -> Cover {
    if program == user || parent_city(user) == Some(program) {
        Cover::Yes
    } else if parent_city(program) == Some(user) {
        Cover::Ambiguous
    } else {
        Cover::No
    }
}

/// Whether a provider name is the government of `region`'s whole sido
/// ("부산광역시", "부산광역시 청년산학국 청년정책과", "부산광역시교육청"),
/// not one of its 시·군·구 ("부산광역시 수영구", "경기도 수원시"). Used only
/// for ranking while `region_districts` is not collected.
pub fn sido_wide_provider(provider: &str, region: &str) -> bool {
    let mut tokens = provider.split_whitespace();
    let Some(head) = tokens.next() else {
        return false;
    };
    let head = head.strip_suffix("교육청").unwrap_or(head);
    if crate::canonical_region_code(head) != Some(region) {
        return false;
    }
    match tokens.next() {
        None => true,
        Some(next) => {
            let district =
                ['시', '군', '구'].iter().any(|s| next.ends_with(*s)) && next.chars().count() <= 6;
            !district
        }
    }
}

/// Whether the canonical district `program` covers the user's canonical
/// district `user` ([`district_code`] form): `Some(true)` covers,
/// `Some(false)` does not, `None` cannot tell (the user gave a 시 and the
/// programme names one of its 구). The rule [`district_check`] applies.
pub fn district_covers(program: &str, user: &str) -> Option<bool> {
    match covers(program, user) {
        Cover::Yes => Some(true),
        Cover::No => Some(false),
        Cover::Ambiguous => None,
    }
}

/// What a district value is against the current table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Current {
    /// A current 시·군·구 ([`district_code`], then the official 12xxx
    /// numbering's [`crate::districts::CODE_ALIASES`]).
    Current(String),
    /// A district code that is not current (retired or a sido code). The
    /// official file the generator reads lists current rows only, so this is
    /// "not current", not a listed retirement.
    Retired,
    /// No district code at all ("수영구").
    NotACode,
}

/// The one reader of a district value, for the programme side and the user
/// side alike.
pub fn current_district(value: &str) -> Current {
    let Some(code) = district_code(value) else {
        return Current::NotACode;
    };
    if is_known_district(&code) {
        return Current::Current(code);
    }
    let aliases = crate::districts::CODE_ALIASES;
    match aliases.binary_search_by_key(&code.as_str(), |(official, _)| official) {
        Ok(at) => Current::Current(aliases[at].1.to_owned()),
        Err(_) => Current::Retired,
    }
}

/// A programme's `region_districts` read against the current table: each
/// entry is a current code (after [`district_code`] and the official
/// numbering's [`crate::districts::CODE_ALIASES`]) or unresolved.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProgrammeDistricts {
    /// Current codes, deduplicated, in listed order.
    pub codes: Vec<String>,
    /// The sido of each unresolved entry: `Some` for a code that is not a
    /// current district (retired with no listed successor, or a sido code),
    /// `None` for an entry that is no code at all ("수영구").
    pub unresolved: Vec<Option<&'static str>>,
}

impl ProgrammeDistricts {
    /// Whether an unresolved entry may lie in `region`.
    pub fn unresolved_in(&self, region: &str) -> bool {
        self.unresolved
            .iter()
            .any(|sido| sido.is_none_or(|sido| sido == region))
    }
}

/// The one reading of a programme's listed districts; [`district_check`]
/// and the review overrides' `S-district` rule both use it.
pub fn programme_districts(listed: &[String]) -> ProgrammeDistricts {
    let mut out = ProgrammeDistricts::default();
    for entry in listed {
        match current_district(entry) {
            Current::Current(code) if !out.codes.contains(&code) => out.codes.push(code),
            Current::Current(_) => {}
            Current::Retired => out
                .unresolved
                .push(district_code(entry).and_then(|code| district_region(&code))),
            Current::NotACode => out.unresolved.push(None),
        }
    }
    out
}

/// The district condition of one programme for one user, and how it matched.
/// No check when `region_districts` is not collected or no listed district
/// limits the user's sido. An unresolved entry that may lie in the user's
/// sido turns a mismatch into `Unknown` (shown, never confirmed, no
/// ranking bonus): the programme may cover the user.
pub fn district_check(
    program: &Program,
    profile: &StructuralProfile,
) -> (Option<StructuralCheck>, Option<DistrictMatch>) {
    let Some(listed) = program.region_districts.as_ref() else {
        return (None, None);
    };
    let user_region = profile.region_code.and_then(crate::canonical_region_code);
    let sido_listed = user_region.is_some_and(|region| {
        program
            .regions
            .as_ref()
            .is_some_and(|regions| regions.iter().any(|r| r == region))
    });
    let districts = programme_districts(listed);
    let limited = !districts.codes.is_empty() || !districts.unresolved.is_empty();
    let mut c = check(CheckKind::District, RuleVerdict::Unknown);
    let Some(region) = user_region else {
        if !limited {
            return (None, None);
        }
        c.missing_field = Some("city_code");
        return (Some(c), None);
    };
    let mine: Vec<&String> = districts
        .codes
        .iter()
        .filter(|code| district_region(code) == Some(region))
        .collect();
    // A code retired across sidos (군위 47720, now 대구) may cover any listed
    // sido: an unresolved entry reaches every sido in `regions`.
    let unresolved_mine =
        districts.unresolved_in(region) || (sido_listed && !districts.unresolved.is_empty());
    if mine.is_empty() && !unresolved_mine {
        if sido_listed {
            return (None, Some(DistrictMatch::SidoWide));
        }
        if !limited {
            return (None, None);
        }
        // Limited to districts of other sidos only.
        c.status = RuleVerdict::Fail;
        return (Some(c), None);
    }
    // A saved code that is not current (a profile from before a
    // reorganisation) is a missing city_code, never a mismatch.
    let user = profile
        .city_code
        .and_then(|value| match current_district(value) {
            Current::Current(code) => Some(code),
            Current::Retired | Current::NotACode => None,
        })
        .filter(|code| district_region(code) == Some(region));
    let Some(user) = user else {
        c.missing_field = Some("city_code");
        return (Some(c), None);
    };
    let found: Vec<Cover> = mine.iter().map(|code| covers(code, &user)).collect();
    if found.contains(&Cover::Yes) {
        c.status = RuleVerdict::Pass;
        (Some(c), Some(DistrictMatch::Matched))
    } else if found.contains(&Cover::Ambiguous) {
        c.missing_field = Some("city_code");
        (Some(c), None)
    } else if unresolved_mine {
        // The user's answer is complete; the programme's scope is not.
        (Some(c), None)
    } else {
        c.status = RuleVerdict::Fail;
        (Some(c), None)
    }
}

fn verdict(pass: bool) -> RuleVerdict {
    if pass {
        RuleVerdict::Pass
    } else {
        RuleVerdict::Fail
    }
}

fn check(kind: CheckKind, status: RuleVerdict) -> StructuralCheck {
    StructuralCheck {
        kind,
        status,
        missing_field: None,
        unresolved_scope: false,
    }
}

pub fn enrollment_statuses(program: &Program) -> Option<&Vec<Value>> {
    let payload = program.normalized_payload.as_ref()?;
    payload
        .get("enrollment_statuses")
        .and_then(Value::as_array)
        .filter(|a| !a.is_empty())
        // A row for university students with no stated list (an
        // auto-published "대학생" offer, audit M3) admits the statuses of a
        // university student; a graduate is outside it.
        .or_else(|| {
            (payload.get("audience_scope").and_then(Value::as_str) == Some(HIGHER_EDUCATION))
                .then(|| &*UNIVERSITY_STUDENT_STATUSES)
        })
}

/// `normalized_payload.audience_scope` of a row for university students.
pub const HIGHER_EDUCATION: &str = "higher_education";

/// The enrollment statuses of a university student ([`enrollment_statuses`]
/// for a [`HIGHER_EDUCATION`] row without its own list): 재학, 휴학, 연구생.
static UNIVERSITY_STUDENT_STATUSES: std::sync::LazyLock<Vec<Value>> =
    std::sync::LazyLock::new(|| {
        ["enrolled", "on_leave", "research_student"]
            .into_iter()
            .map(Value::from)
            .collect()
    });

pub fn structural_checks(
    program: &Program,
    program_university_id: Option<&str>,
    profile: &StructuralProfile,
) -> Vec<StructuralCheck> {
    let today = crate::seoul_date(chrono::Utc::now());
    structural_checks_at(program, program_university_id, profile, today)
}

/// [`structural_checks`] seen on the Korean date `today`: the age check
/// counts 만 나이 on the programme's [`rule_engine::AgeBasis`].
pub fn structural_checks_at(
    program: &Program,
    program_university_id: Option<&str>,
    profile: &StructuralProfile,
    today: chrono::NaiveDate,
) -> Vec<StructuralCheck> {
    let mut checks = Vec::new();
    if program.min_age.is_some() || program.max_age.is_some() {
        let mut c = check(CheckKind::Age, RuleVerdict::Unknown);
        match profile.birth_year {
            None => c.missing_field = Some("birth_year"),
            Some(year) => {
                c.status = rule_engine::age_gate_on(
                    year,
                    profile.birth_date,
                    program.min_age,
                    program.max_age,
                    &rule_engine::AgeBasis::for_program(program, today),
                );
                // Only a bare birth year is the user's gap: an inverted band
                // (L7), or a birthday inside the notice window with the date
                // of birth given, is Unknown whatever the user enters.
                let inverted = matches!((program.min_age, program.max_age),
                    (Some(min), Some(max)) if min > max);
                if c.status == RuleVerdict::Unknown && !inverted && profile.birth_date.is_none() {
                    c.missing_field = Some("birth_year");
                }
            }
        }
        checks.push(c);
    }
    if let Some(regions) = program.regions.as_ref().filter(|r| !r.is_empty()) {
        let mut c = check(CheckKind::Region, RuleVerdict::Unknown);
        if regions.iter().any(|r| r == "unknown") {
            c.unresolved_scope = true;
        } else {
            match profile.region_code.and_then(crate::canonical_region_code) {
                None => c.missing_field = Some("region_code"),
                Some(region) => c.status = verdict(regions.iter().any(|r| r == region)),
            }
        }
        checks.push(c);
    }
    if program.program_type == "campus_benefit" {
        let mut c = check(CheckKind::School, RuleVerdict::Unknown);
        match (profile.university_id, program_university_id) {
            (Some(mine), Some(theirs)) => c.status = verdict(mine == theirs),
            (None, _) => {
                c.missing_field = Some("school_name");
                c.unresolved_scope = true;
            }
            (Some(_), None) => c.unresolved_scope = true,
        }
        checks.push(c);
    }
    if let Some(statuses) = enrollment_statuses(program) {
        let mut c = check(CheckKind::Enrollment, RuleVerdict::Unknown);
        match profile.enrollment_status.filter(|s| !s.is_empty()) {
            None => c.missing_field = Some("enrollment_status"),
            Some(s) => c.status = verdict(statuses.iter().any(|v| v.as_str() == Some(s))),
        }
        checks.push(c);
    }
    if let (Some(c), _) = district_check(program, profile) {
        checks.push(c);
    }
    checks
}

/// Whether a recommendation list may show the programme at all.
pub fn listable(checks: &[StructuralCheck]) -> bool {
    exclusion(checks).is_none()
}

/// Why a recommendation list hides the programme: the first check that is a
/// `Fail` or an unresolved scope. An unresolved residence scope is reported
/// as [`ExclusionReason::ScopeUnconfirmed`], not as a region mismatch.
pub fn exclusion(checks: &[StructuralCheck]) -> Option<ExclusionReason> {
    checks
        .iter()
        .find(|c| c.status == RuleVerdict::Fail || c.unresolved_scope)
        .map(|c| match c.kind {
            CheckKind::Age => ExclusionReason::Age,
            CheckKind::Region if c.unresolved_scope => ExclusionReason::ScopeUnconfirmed,
            CheckKind::Region | CheckKind::District => ExclusionReason::Region,
            CheckKind::School => ExclusionReason::School,
            CheckKind::Enrollment => ExclusionReason::Enrollment,
            CheckKind::Audience => ExclusionReason::TargetGroup,
        })
}

/// Combined structural verdict: any `Fail` fails, all `Pass` (or no checks)
/// passes, otherwise `Unknown`.
pub fn combined(checks: &[StructuralCheck]) -> RuleVerdict {
    if checks.iter().any(|c| c.status == RuleVerdict::Fail) {
        RuleVerdict::Fail
    } else if checks.iter().all(|c| c.status == RuleVerdict::Pass) {
        RuleVerdict::Pass
    } else {
        RuleVerdict::Unknown
    }
}

#[cfg(test)]
pub(crate) fn test_program(program_type: &str) -> Program {
    let now = chrono::Utc::now();
    Program {
        id: uuid::Uuid::nil(),
        program_type: program_type.into(),
        source_type: "manual".into(),
        source_id: None,
        title: "t".into(),
        summary: None,
        provider_name: None,
        official_url: None,
        program_status: "active".into(),
        application_start_at: None,
        application_end_at: None,
        benefit_amount_monthly: None,
        benefit_amount_semester: None,
        benefit_amount_once: None,
        region_scope: None,
        school_scope: None,
        tags: None,
        raw_payload: None,
        normalized_payload: None,
        min_age: None,
        max_age: None,
        regions: None,
        deadline_at: None,
        benefit_valid_until: None,
        is_active: true,
        last_synced_at: None,
        created_at: now,
        updated_at: now,
        company_name: None,
        company_logo_url: None,
        benefit_category: None,
        provider_kind: None,
        application_steps: None,
        application_method: None,
        submission_documents: None,
        screening_method: None,
        region_districts: None,
        eligibility_condition: None,
        holder_requirement: None,
        age_basis: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn statuses(checks: &[StructuralCheck]) -> Vec<(CheckKind, RuleVerdict)> {
        checks.iter().map(|c| (c.kind, c.status)).collect()
    }

    #[test]
    fn enrollment_applies_to_every_programme_type() {
        for program_type in [
            "scholarship",
            "policy",
            "campus_benefit",
            "corporate_benefit",
        ] {
            let mut p = test_program(program_type);
            p.normalized_payload = Some(json!({"enrollment_statuses":["enrolled"]}));
            let profile = StructuralProfile {
                enrollment_status: Some("on_leave"),
                university_id: Some("pusan"),
                ..Default::default()
            };
            let checks = structural_checks(&p, Some("pusan"), &profile);
            assert!(
                checks
                    .iter()
                    .any(|c| c.kind == CheckKind::Enrollment && c.status == RuleVerdict::Fail),
                "{program_type}"
            );
            assert!(!listable(&checks));
        }
    }

    #[test]
    fn unresolved_region_and_school_are_unknown_and_not_listable() {
        let mut p = test_program("campus_benefit");
        p.regions = Some(vec!["unknown".into()]);
        let checks = structural_checks(
            &p,
            Some("pusan"),
            &StructuralProfile {
                region_code: Some("busan"),
                ..Default::default()
            },
        );
        assert_eq!(
            statuses(&checks),
            [
                (CheckKind::Region, RuleVerdict::Unknown),
                (CheckKind::School, RuleVerdict::Unknown)
            ]
        );
        assert!(checks.iter().all(|c| c.unresolved_scope));
        assert!(!listable(&checks));
        assert_eq!(exclusion(&checks), Some(ExclusionReason::ScopeUnconfirmed));
        assert_eq!(combined(&checks), RuleVerdict::Unknown);
    }

    #[test]
    fn region_is_canonicalised_and_missing_region_asks_for_it() {
        let mut p = test_program("policy");
        p.regions = Some(vec!["busan".into()]);
        let pass = structural_checks(
            &p,
            None,
            &StructuralProfile {
                region_code: Some("부산광역시"),
                ..Default::default()
            },
        );
        assert_eq!(combined(&pass), RuleVerdict::Pass);
        let missing = structural_checks(&p, None, &StructuralProfile::default());
        assert_eq!(missing[0].missing_field, Some("region_code"));
        assert!(
            listable(&missing),
            "a blank field is not an unresolved scope"
        );
        let seoul = structural_checks(
            &p,
            None,
            &StructuralProfile {
                region_code: Some("seoul"),
                ..Default::default()
            },
        );
        assert_eq!(combined(&seoul), RuleVerdict::Fail);
        assert_eq!(exclusion(&seoul), Some(ExclusionReason::Region));
        assert_eq!(exclusion(&missing), None);
    }

    #[test]
    fn age_uses_the_three_state_gate() {
        let mut p = test_program("policy");
        p.max_age = Some(34);
        let year = rule_engine::age_candidates(2000)[1] + 2000;
        let at = |birth| {
            combined(&structural_checks(
                &p,
                None,
                &StructuralProfile {
                    birth_year: Some(birth),
                    ..Default::default()
                },
            ))
        };
        assert_eq!(at(year - 20), RuleVerdict::Pass);
        assert_eq!(at(year - 35), RuleVerdict::Unknown);
        assert_eq!(at(year - 40), RuleVerdict::Fail);
        // An inverted band is Unknown and asks for nothing (verifier gap 13).
        p.min_age = Some(40);
        let inverted = structural_checks(
            &p,
            None,
            &StructuralProfile {
                birth_year: Some(year - 20),
                ..Default::default()
            },
        );
        assert_eq!(
            (inverted[0].status, inverted[0].missing_field),
            (RuleVerdict::Unknown, None)
        );
    }

    #[test]
    fn a_date_of_birth_settles_the_age_check() {
        use chrono::Datelike;
        let mut p = test_program("policy");
        p.max_age = Some(34);
        let today = crate::seoul_date(chrono::Utc::now());
        let year = today.year() - 35;
        let check = |birth_date| {
            combined(&structural_checks(
                &p,
                None,
                &StructuralProfile {
                    birth_year: Some(year),
                    birth_date,
                    ..Default::default()
                },
            ))
        };
        assert_eq!(check(None), RuleVerdict::Unknown);
        assert_eq!(
            check(chrono::NaiveDate::from_ymd_opt(year, 1, 1)),
            RuleVerdict::Fail
        );
        if (today.month(), today.day()) != (12, 31) {
            assert_eq!(
                check(chrono::NaiveDate::from_ymd_opt(year, 12, 31)),
                RuleVerdict::Pass
            );
        }
    }

    /// Audit H1, structural side: the age column counts 만 나이 on the
    /// notice's window, not on today alone. The clock is injected.
    #[test]
    fn a_birthday_inside_the_window_is_unknown() {
        use chrono::TimeZone;
        let today = chrono::NaiveDate::from_ymd_opt(2026, 11, 10).unwrap();
        let mut p = test_program("policy");
        p.max_age = Some(34);
        p.application_start_at = Some(
            chrono::Utc
                .with_ymd_and_hms(2026, 10, 24, 15, 0, 0)
                .unwrap(),
        );
        p.application_end_at = Some(
            chrono::Utc
                .with_ymd_and_hms(2026, 11, 30, 14, 59, 59)
                .unwrap(),
        );
        let age = |p: &Program, born: (i32, u32, u32)| {
            let checks = structural_checks_at(
                p,
                None,
                &StructuralProfile {
                    birth_year: Some(born.0),
                    birth_date: chrono::NaiveDate::from_ymd_opt(born.0, born.1, born.2),
                    ..Default::default()
                },
                today,
            );
            (checks[0].status, checks[0].missing_field)
        };
        // The date of birth is given: nothing to ask (review R-L1).
        assert_eq!(age(&p, (1991, 10, 28)), (RuleVerdict::Unknown, None));
        assert_eq!(age(&p, (1991, 11, 20)).0, RuleVerdict::Unknown);
        assert_eq!(age(&p, (1991, 12, 15)).0, RuleVerdict::Pass);
        assert_eq!(age(&p, (1991, 10, 1)).0, RuleVerdict::Fail);
        // No closing date: counted on today, as before.
        p.application_end_at = None;
        assert_eq!(age(&p, (1991, 10, 28)).0, RuleVerdict::Fail);
        assert_eq!(age(&p, (1991, 11, 20)).0, RuleVerdict::Pass);
    }

    fn district(p: &Program, region: &str, city: Option<&str>) -> Option<StructuralCheck> {
        let profile = StructuralProfile {
            region_code: Some(region),
            city_code: city,
            ..Default::default()
        };
        let checks = structural_checks(p, None, &profile);
        checks.into_iter().find(|c| c.kind == CheckKind::District)
    }

    #[test]
    fn district_codes_normalise() {
        assert_eq!(district_code("26410").as_deref(), Some("26410"));
        assert_eq!(district_code("2641000000").as_deref(), Some("26410"));
        assert_eq!(district_code("42110").as_deref(), Some("51110"));
        assert_eq!(district_code("45111").as_deref(), Some("52111"));
        for bad in ["gangnam", "2641", "264100", "", "2641O"] {
            assert_eq!(district_code(bad), None, "{bad}");
        }
        assert_eq!(district_region("51110"), Some("gangwon"));
        assert_eq!(district_region("99999"), None);
    }

    #[test]
    fn uncollected_or_sido_wide_districts_add_no_check() {
        let mut p = test_program("policy");
        p.regions = Some(vec!["busan".into()]);
        assert_eq!(district(&p, "busan", Some("26410")), None);
        p.region_districts = Some(vec![]);
        assert_eq!(district(&p, "busan", None), None);
        assert_eq!(
            district_check(
                &p,
                &StructuralProfile {
                    region_code: Some("busan"),
                    ..Default::default()
                }
            )
            .1,
            Some(DistrictMatch::SidoWide)
        );
    }

    #[test]
    fn district_mismatch_excludes_and_missing_city_asks_for_it() {
        // 부산 수영구 only (audit 00638ac6); P1 lives in 금정구.
        let mut p = test_program("policy");
        p.regions = Some(vec!["busan".into()]);
        p.region_districts = Some(vec!["26500".into()]);
        let fail = district(&p, "busan", Some("26410")).unwrap();
        assert_eq!(fail.status, RuleVerdict::Fail);
        assert_eq!(exclusion(&[fail]), Some(ExclusionReason::Region));

        let pass = district(&p, "busan", Some("26500")).unwrap();
        assert_eq!(pass.status, RuleVerdict::Pass);

        let missing = district(&p, "busan", None).unwrap();
        assert_eq!(missing.status, RuleVerdict::Unknown);
        assert_eq!(missing.missing_field, Some("city_code"));
        assert!(
            listable(&[missing]),
            "a blank district is not a hidden scope"
        );

        // A city_code outside the saved sido is not trusted as the district.
        let elsewhere = district(&p, "busan", Some("11620")).unwrap();
        assert_eq!(elsewhere.missing_field, Some("city_code"));
    }

    #[test]
    fn city_covers_its_gu_and_a_coarser_user_city_is_unknown() {
        let mut p = test_program("policy");
        p.regions = Some(vec!["gyeonggi".into()]);
        p.region_districts = Some(vec!["41110".into()]); // 수원시
        assert_eq!(
            district(&p, "gyeonggi", Some("41117")).unwrap().status, // 영통구
            RuleVerdict::Pass
        );
        assert_eq!(
            district(&p, "gyeonggi", Some("41460")).unwrap().status, // 용인시
            RuleVerdict::Fail
        );
        p.region_districts = Some(vec!["41117".into()]);
        let coarse = district(&p, "gyeonggi", Some("41110")).unwrap();
        assert_eq!(coarse.status, RuleVerdict::Unknown);
        assert_eq!(coarse.missing_field, Some("city_code"));
        // Metropolitan 구 never join by prefix: 금정구 26410 vs 해운대구 26350.
        p.regions = Some(vec!["busan".into()]);
        p.region_districts = Some(vec!["26350".into()]);
        assert_eq!(
            district(&p, "busan", Some("26410")).unwrap().status,
            RuleVerdict::Fail
        );
    }

    /// Audit H2: 영동군 43740 ends in 0 and shares four digits with 증평군
    /// 43745, but it is not a 시 with 구. Parentage comes from the official
    /// table, not from the digits.
    #[test]
    fn a_gun_is_not_the_parent_of_its_numeric_neighbour() {
        let mut p = test_program("policy");
        p.regions = Some(vec!["chungbuk".into()]);
        p.region_districts = Some(vec!["43740".into()]); // 영동군
        let check = district(&p, "chungbuk", Some("43745")).unwrap(); // 증평군
        assert_eq!(check.status, RuleVerdict::Fail);
        assert_eq!(
            district_check(
                &p,
                &StructuralProfile {
                    region_code: Some("chungbuk"),
                    city_code: Some("43745"),
                    ..Default::default()
                }
            )
            .1,
            None
        );
        p.region_districts = Some(vec!["43745".into()]);
        let reverse = district(&p, "chungbuk", Some("43740")).unwrap();
        assert_eq!(reverse.status, RuleVerdict::Fail);
        assert_eq!(reverse.missing_field, None);
        // 청주시 43110 is the parent of 상당구 43111.
        p.region_districts = Some(vec!["43110".into()]);
        assert_eq!(
            district(&p, "chungbuk", Some("43111")).unwrap().status,
            RuleVerdict::Pass
        );
    }

    /// Audit M2: a programme entry that is not a current code is not a
    /// mismatch. A retired code with no successor in the official table
    /// (인천 중구 28110, 군위군 47720) or a name ("수영구") leaves the user's
    /// district unknown: shown as 확인 필요, never hidden, never confirmed,
    /// and no district ranking bonus. The official 광주·전남 12xxx numbering
    /// reads as the client code (12210 → 29110).
    #[test]
    fn a_retired_or_unparseable_district_is_unresolved_not_a_mismatch() {
        let listed_unknown = |p: &Program, region: &str, city: &str| {
            let profile = StructuralProfile {
                region_code: Some(region),
                city_code: Some(city),
                ..Default::default()
            };
            let (check, matched) = district_check(p, &profile);
            let check = check.expect("a district check");
            assert_eq!(check.status, RuleVerdict::Unknown, "{city}");
            assert_eq!(check.missing_field, None, "{city}");
            assert!(listable(&[check]), "{city}");
            assert_eq!(matched, None, "{city}");
        };
        let mut p = test_program("policy");
        p.regions = Some(vec!["incheon".into()]);
        p.region_districts = Some(vec!["28110".into()]);
        listed_unknown(&p, "incheon", "28125");
        // Another sido's retired code still limits the row to that sido.
        p.regions = Some(vec!["incheon".into()]);
        assert_eq!(
            district(&p, "busan", Some("26410")).unwrap().status,
            RuleVerdict::Fail
        );

        p.regions = Some(vec!["gyeongbuk".into()]);
        p.region_districts = Some(vec!["47720".into()]);
        listed_unknown(&p, "gyeongbuk", "47730");

        p.regions = Some(vec!["busan".into()]);
        p.region_districts = Some(vec!["수영구".into()]);
        listed_unknown(&p, "busan", "26410");
        // A listed current code still decides for its own users.
        p.region_districts = Some(vec!["수영구".into(), "26500".into()]);
        assert_eq!(
            district(&p, "busan", Some("26500")).unwrap().status,
            RuleVerdict::Pass
        );
        listed_unknown(&p, "busan", "26410");

        p.regions = Some(vec!["gwangju".into()]);
        p.region_districts = Some(vec!["12210".into()]); // 광주 동구, official numbering
        assert_eq!(
            district(&p, "gwangju", Some("29110")).unwrap().status,
            RuleVerdict::Pass
        );
        assert_eq!(
            district(&p, "gwangju", Some("29140")).unwrap().status,
            RuleVerdict::Fail
        );
    }

    /// Review R-M4: an unresolved programme entry leaves every listed sido
    /// unknown (a retired code may have moved sido: 군위 47720 is now 대구),
    /// and a saved user code that is not current is a missing city_code,
    /// never a mismatch.
    #[test]
    fn a_retired_code_on_either_side_is_unknown_not_a_mismatch() {
        let mut p = test_program("policy");
        p.regions = Some(vec!["daegu".into()]);
        p.region_districts = Some(vec!["47720".into()]);
        let (check, matched) = district_check(
            &p,
            &StructuralProfile {
                region_code: Some("daegu"),
                city_code: Some("27110"),
                ..Default::default()
            },
        );
        assert_eq!(check.map(|c| c.status), Some(RuleVerdict::Unknown));
        assert_eq!(matched, None);
        p.regions = Some(vec!["incheon".into()]);
        p.region_districts = Some(vec!["28125".into()]);
        let stale = district(&p, "incheon", Some("28110")).unwrap();
        assert_eq!(
            (stale.status, stale.missing_field),
            (RuleVerdict::Unknown, Some("city_code"))
        );
        assert_eq!(current_district("28125"), Current::Current("28125".into()));
        assert_eq!(
            current_district("2812500000"),
            Current::Current("28125".into())
        );
        assert_eq!(current_district("12210"), Current::Current("29110".into()));
        assert_eq!(current_district("28110"), Current::Retired);
        assert_eq!(current_district("수영구"), Current::NotACode);
    }

    /// Audit H2 table: among every pair of current codes that share four
    /// digits, only a listed (구, 시) pair joins.
    #[test]
    fn only_listed_parents_cover_a_numeric_neighbour() {
        let codes = crate::districts::CITY_CODES;
        let listed =
            |child: &str, parent: &str| crate::districts::CITY_PARENTS.contains(&(child, parent));
        for a in codes {
            for b in codes {
                if a == b || a[..4] != b[..4] {
                    continue;
                }
                assert_eq!(
                    district_covers(a, b) == Some(true),
                    listed(b, a),
                    "programme {a} user {b}"
                );
                assert_eq!(
                    district_covers(a, b).is_none(),
                    listed(a, b),
                    "programme {a} user {b}"
                );
            }
        }
        assert!(crate::districts::CITY_PARENTS
            .windows(2)
            .all(|w| w[0].0 < w[1].0));
        for (child, parent) in crate::districts::CITY_PARENTS {
            assert!(is_known_district(child) && is_known_district(parent));
        }
    }

    #[test]
    fn sido_government_providers_are_sido_wide_and_districts_are_not() {
        for wide in [
            "부산광역시",
            "부산광역시 청년산학국 청년정책과",
            "부산광역시교육청",
        ] {
            assert!(sido_wide_provider(wide, "busan"), "{wide}");
        }
        for not in [
            "부산광역시 수영구",
            "부산광역시 기장군",
            "경기도 수원시",
            "부산진구",
            "경제교통국",
            "",
        ] {
            assert!(!sido_wide_provider(not, "busan"), "{not}");
        }
        assert!(!sido_wide_provider("서울특별시", "busan"));
    }

    #[test]
    fn a_sido_listed_without_its_districts_is_sido_wide() {
        // 부산 전체 + 경남 김해시 only.
        let mut p = test_program("policy");
        p.regions = Some(vec!["busan".into(), "gyeongnam".into()]);
        p.region_districts = Some(vec!["48250".into()]);
        assert_eq!(district(&p, "busan", Some("26410")), None);
        assert_eq!(
            district(&p, "gyeongnam", Some("48121")).unwrap().status,
            RuleVerdict::Fail
        );
        assert_eq!(
            district(&p, "gyeongnam", Some("48250")).unwrap().status,
            RuleVerdict::Pass
        );
    }
}
