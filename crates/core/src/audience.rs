//! Who a programme is for, read from its stored target text: whether it is
//! youth-relevant, whether an individual can apply at all, and whether it is
//! reserved for a group (seniors, disabled people, veterans, …).
//!
//! A conservative keyword classifier for the personal recommendation list
//! until structured target conditions are ingested. It only excludes the
//! obvious: a youth signal ("청년", "대학생", "구직자", a 만19–39-type age
//! span) always wins over a group term, group terms count only in the title,
//! the first line of the target text or a source's own target tags, and
//! nothing here ever reads as a `Fail` in an eligibility explanation.
//!
//! Inputs are what ingestion already stores:
//! - gov_benefits (보조금24): `지원대상` + `선정기준` as target text, `사용자구분`
//!   as applicant kinds ("개인||법인/시설/단체").
//! - youth_center (온통청년): `addAplyQlfcCndCn` + `plcyAplyMthdCn` as target text.
//! - local/national_welfare (복지로): `trgterIndvdl(Nm)Array` as target tags,
//!   `life(Nm)Array` as life stages.

use crate::rule_engine::RuleVerdict;
use crate::structural::{CheckKind, StructuralCheck};

/// The stored text of one programme the classifier reads.
#[derive(Debug, Clone, Copy, Default)]
pub struct AudienceText<'a> {
    pub source_type: &'a str,
    pub title: &'a str,
    pub summary: Option<&'a str>,
    /// Free text naming who may apply or be selected.
    pub target: Option<&'a str>,
    /// gov_benefits `사용자구분`, `||`-separated.
    pub applicant_kinds: Option<&'a str>,
    /// 복지로 `trgterIndvdlArray`, comma-separated tags.
    pub target_tags: Option<&'a str>,
    /// 복지로 `lifeArray`, comma-separated life stages.
    pub life_stages: Option<&'a str>,
    pub min_age: Option<i32>,
    pub max_age: Option<i32>,
    /// A curator marked the row youth-relevant (`normalized_payload.
    /// youth_relevant`): an adult programme with no youth band or wording
    /// that young people are its main users, such as a city transit pass.
    pub curated_youth: bool,
}

/// A group a programme is reserved for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Group {
    Senior,
    /// 중장년/신중년 (40+).
    MiddleAged,
    Disability,
    Veteran,
    Multicultural,
    Pregnancy,
    FarmFishery,
    Child,
    /// Only a 소상공인/사업자 applies (a youth_fit tier, never read from the
    /// text): inside when the user is self-employed, else unknown (the
    /// profile cannot say "not self-employed").
    SelfEmployed,
    /// Someone no profile answer models: a youth_fit tier (an organisation,
    /// an occupational-disease group, an unstated target) or a text group
    /// ([`UNMODELLED_TERMS`]): always unknown.
    Unmodelled,
}

/// Who may apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applicant {
    /// An individual or household can apply (or nothing says otherwise).
    Individual,
    /// Only a 소상공인/사업자 can apply.
    Business,
    /// Only an institution, municipality, school, corporation or facility.
    Organisation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Audience {
    pub youth: bool,
    pub applicant: Applicant,
    /// Groups the programme is reserved for. For a youth-relevant row only
    /// the groups of a lead line that names nobody else.
    pub groups: Vec<Group>,
    /// `groups` is the whole list of who may apply. False when the lead
    /// line also lists someone no [`Group`] models ("기초생활수급자,
    /// 장애인"): the row is open to them, so it never hides.
    pub groups_closed: bool,
    /// Groups a youth-relevant row names outside a closed lead line (in the
    /// title or the tags: "장애인 청년 취업지원"): an open reservation. A
    /// known member passes it; anyone else is Unknown with the group's
    /// question, never hidden by it and never passed unchecked (review M1).
    pub open_groups: Vec<Group>,
    /// What the user must hold (an issuer-paid offer): never read from the
    /// text. [`classify`] leaves it `None`; the loader sets it from the
    /// row's `current_programs.holder_requirement` ([`crate::holder`]).
    pub holder: Option<crate::holder::Holder>,
    /// The row states a holder requirement no code here reads
    /// ([`crate::holder::Holder::unreadable`]): an Unknown check that no
    /// answer resolves.
    pub holder_unreadable: bool,
}

/// The profile facts the hard exclusion reads. `None` = not answered.
#[derive(Debug, Clone, Copy, Default)]
pub struct AudienceProfile {
    /// Upper reading of the user's 만 나이 from the birth year.
    pub age: Option<i32>,
    pub has_disability: Option<bool>,
    pub veteran_family: Option<bool>,
    pub multicultural_family: Option<bool>,
    pub self_employed: bool,
    /// Carrier, cards and pay apps (`None` inside = not answered).
    pub holdings: crate::holder::Holdings,
}

/// Why the personal list hides a programme regardless of mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudienceExclusion {
    NotIndividual,
    TargetGroup(Group),
}

/// Sources whose every row is meant for young people or students.
const YOUTH_SOURCES: &[&str] = &[
    "youth_center",
    "university_official",
    "corporate_official",
    "scholarship",
];

/// Youth signals that win over any group term. Whitespace is removed before
/// matching.
pub(crate) const YOUTH_TERMS: &[&str] = &[
    "청년",
    "대학생",
    "대학원생",
    "대학(원)생",
    "대학재학",
    "대학교재학",
    "대학신입생",
    "자립준비",
    "보호종료",
];

/// Status words young people share with other ages ("40대 이상 중장년
/// 구직자"): youth only when no group term names someone else.
const WEAK_YOUTH_TERMS: &[&str] = &["구직자", "취업준비", "취준생", "미취업", "사회초년생"];

/// Applicant phrases that say no individual can apply, anywhere in the
/// target text. Whitespace removed.
const ORGANISATION_PHRASES: &[&str] = &[
    "개인신청불가",
    "개인은신청불가",
    "개인신청은불가",
    "개인지원불가",
    "산학협력단",
];

/// Universities as the applicant ("지원대학 4곳"), unless a person noun
/// follows ("지원 대학생", "선정 대학원생", "선정 대학교 재학생").
const UNIVERSITY_APPLICANTS: &[&str] = &["선정대학", "지원대학"];

/// "X 대상" applicant phrases. They name the applicant only as the whole
/// subject of a line (at its start, or right after a 신청자격/지원대상
/// label): "중소기업 대상 채용 연계 프로그램 참여자" is for the 참여자.
const ORGANISATION_SUBJECTS: &[&str] = &[
    "지자체대상",
    "지방자치단체대상",
    "시·군·구대상",
    "시군구대상",
    "기관대상",
    "법인대상",
    "단체대상",
    "기업대상",
    "사업주대상",
    "시설대상",
];

const BUSINESS_SUBJECTS: &[&str] = &["사업자대상", "소상공인대상", "자영업자대상"];

/// Labels that may precede an applicant subject on its line.
const APPLICANT_LABELS: &[&str] = &["신청자격", "지원대상"];

/// Disabled people, the one lexicon this module and [`crate::youth_fit`]
/// read. Not a bare 장애: "연작장애" is a soil problem.
pub(crate) const DISABILITY_TERMS: &[&str] = &["장애인"];

/// Veterans and national-merit families, the one lexicon this module and
/// [`crate::youth_fit`] read: the closed categories of the 보훈 statutes,
/// not a bare 유공자 ("인구증가시책 유공자" are merit awardees).
/// - 국가유공자 등 예우 및 지원에 관한 법률 §4: 순국선열, 애국지사, 전몰군경,
///   전상군경, 순직군경, 공상군경, 무공수훈자, 보국수훈자, 참전유공자, 4·19혁명
///   사망자·부상자·공로자, 순직공무원, 공상공무원 (and the old 상이군경).
/// - 독립유공자예우에 관한 법률: 독립유공자 (순국선열, 애국지사).
/// - 5·18민주유공자예우 및 단체설립에 관한 법률: 5·18민주유공자.
/// - 특수임무유공자 예우 및 단체설립에 관한 법률: 특수임무유공자.
/// - 고엽제후유의증 등 환자지원 및 단체설립에 관한 법률: 고엽제후유(의)증 환자.
/// - 참전유공자 예우 및 단체설립에 관한 법률: 참전유공자 (6·25, 월남).
/// - 보훈보상대상자 지원에 관한 법률: 재해사망·재해부상 군경·공무원.
///
/// Plus 보훈 and 국가유공자, the umbrella words of these laws. Not 제대군인
/// (제대군인지원에 관한 법률): a discharged soldier is not what the
/// `veteran_family` flag asks, so it is a group no profile answer models
/// ([`UNMODELLED_TERMS`], review2 M1).
pub(crate) const VETERAN_TERMS: &[&str] = &[
    "보훈",
    "국가유공자",
    "독립유공자",
    "순국선열",
    "애국지사",
    "전몰군경",
    "전상군경",
    "순직군경",
    "공상군경",
    "상이군경",
    "무공수훈자",
    "보국수훈자",
    "참전",
    "4·19혁명",
    "순직공무원",
    "공상공무원",
    "민주유공자",
    "5·18유공자",
    "특수임무유공자",
    "고엽제",
    "재해사망군경",
    "재해부상군경",
    "재해사망공무원",
    "재해부상공무원",
];

/// Groups a programme can be reserved for that no profile field answers:
/// listed for everyone, never passed ([`Group::Unmodelled`]).
const UNMODELLED_TERMS: &[&str] = &["제대군인"];

/// Group terms, checked in the title and the first line of the target text.
const GROUP_TERMS: &[(Group, &[&str])] = &[
    (
        Group::Senior,
        &[
            "어르신",
            "노인",
            "노년",
            "경로당",
            "경로우대",
            "고령자",
            "장수수당",
            "장수축하",
            "기초연금",
        ],
    ),
    (
        Group::MiddleAged,
        &["중장년", "신중년", "40대이상", "50대이상"],
    ),
    (Group::Disability, DISABILITY_TERMS),
    (Group::Veteran, VETERAN_TERMS),
    (
        Group::Multicultural,
        &["다문화", "결혼이민", "북한이탈", "탈북", "새터민"],
    ),
    (
        Group::Pregnancy,
        &["임산부", "임신", "산모", "출산", "난임", "신생아", "산후"],
    ),
    (
        Group::FarmFishery,
        &[
            "농업인",
            "어업인",
            "농어업인",
            "농어민",
            "농민",
            "어민",
            "농가",
            "축산",
            "임업인",
            "해녀",
            "나잠",
            "영농",
            "수산업",
            "농업경영체",
        ],
    ),
    (
        Group::Child,
        &[
            "영유아",
            "아동",
            "어린이",
            "유아",
            "초등학생",
            "초등생",
            "중학생",
        ],
    ),
    (Group::Unmodelled, UNMODELLED_TERMS),
];

/// Right after a group term, these make it a preference or an exclusion,
/// not the eligible group ("장애인 우선", "기초연금 수급자 제외").
const NOT_EXCLUSIVE: &[&str] = &["우선", "우대", "가점", "제외"];

/// Words that contain a group term without naming the group: "저출산 대응
/// 신혼부부" is not for 산모, "귀농가구" is not a 농가.
const NOT_GROUP_WORDS: &[&str] = &["저출산", "출산율", "귀농가", "귀농민", "귀어가", "축산물"];

/// Every word list and threshold [`classify`] reads, in a fixed order, so a
/// derived classifier (`youth_fit`) can fingerprint them and recompute its
/// stored verdicts when any of them changes.
pub(crate) fn rule_words() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut list = |name: &str, words: &[&str]| {
        out.push(format!("#{name}"));
        out.extend(words.iter().map(|w| (*w).to_owned()));
    };
    list("youth_sources", YOUTH_SOURCES);
    list("youth_terms", YOUTH_TERMS);
    list("weak_youth_terms", WEAK_YOUTH_TERMS);
    list("organisation_phrases", ORGANISATION_PHRASES);
    list("university_applicants", UNIVERSITY_APPLICANTS);
    list("organisation_subjects", ORGANISATION_SUBJECTS);
    list("business_subjects", BUSINESS_SUBJECTS);
    list("applicant_labels", APPLICANT_LABELS);
    list("not_exclusive", NOT_EXCLUSIVE);
    list("not_group_words", NOT_GROUP_WORDS);
    list("list_conjunctions", LIST_CONJUNCTIONS);
    for (group, terms) in GROUP_TERMS {
        list(&format!("group_{group:?}"), terms);
    }
    for (tag, group) in TAG_GROUPS {
        out.push(format!("tag={tag}:{group:?}"));
    }
    out.push(format!("list_separators={LIST_SEPARATORS:?}"));
    // Numeric thresholds in classify, youth_age_span and senior_age_floor.
    out.push("band:hi24-45,lo14-29;span:15-29..29-45,upper29-39;senior60-120".to_owned());
    out
}

/// Whether `term` occurs in `text` as the eligible group: some occurrence is
/// not part of a [`NOT_GROUP_WORDS`] word and is not followed, within a few
/// characters, by a preference/exclusion word.
fn names_group(text: &str, term: &str) -> bool {
    text.match_indices(term).any(|(at, _)| {
        let after: String = text[at + term.len()..].chars().take(8).collect();
        !inside_other_word(text, at, term) && !contains_any(&after, NOT_EXCLUSIVE)
    })
}

/// Whether the occurrence of `term` at byte `at` sits inside one of
/// [`NOT_GROUP_WORDS`].
fn inside_other_word(text: &str, at: usize, term: &str) -> bool {
    NOT_GROUP_WORDS.iter().any(|word| {
        word.match_indices(term).any(|(offset, _)| {
            at.checked_sub(offset)
                .and_then(|start| text.get(start..))
                .is_some_and(|rest| rest.starts_with(word))
        })
    })
}

/// 복지로 target tags that reserve a programme for a flagged group.
const TAG_GROUPS: &[(&str, Group)] = &[
    ("장애인", Group::Disability),
    ("보훈대상자", Group::Veteran),
    ("다문화·탈북민", Group::Multicultural),
];

/// `text` without whitespace, with full-width ASCII forms (`１９`, `～`,
/// `％`, U+FF01–U+FF5E) folded to ASCII as NFKC does, so every number and
/// sign reader sees one spelling.
pub(crate) fn squash(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_whitespace())
        .map(|c| match c {
            '\u{ff01}'..='\u{ff5e}' => char::from_u32(c as u32 - 0xfee0).unwrap_or(c),
            _ => c,
        })
        .collect()
}

pub(crate) fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| haystack.contains(n))
}

/// Numbers in the text with the (whitespace-free) text that follows each one,
/// up to the next number.
fn numbers(text: &str) -> Vec<(i32, &str)> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find(|c: char| c.is_ascii_digit()) {
        let after = &rest[start..];
        let len = after
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(after.len());
        let value = after[..len].parse::<i32>().unwrap_or(i32::MAX);
        let tail = &after[len..];
        let next = tail
            .find(|c: char| c.is_ascii_digit())
            .unwrap_or(tail.len());
        out.push((value, &tail[..next]));
        rest = tail;
    }
    out
}

/// A youth age span ("만19~39세", "19세이상34세이하", "18-34세") or a youth
/// upper bound ("만39세이하"). `text` has no whitespace.
pub(crate) fn youth_age_span(text: &str) -> bool {
    let nums = numbers(text);
    for (i, (value, tail)) in nums.iter().enumerate() {
        let year = |s: &str| s.starts_with("세") || s.starts_with("살");
        if (29..=39).contains(value) && year(tail) && tail[3..].starts_with("이하") {
            return true;
        }
        let Some((upper, upper_tail)) = nums.get(i + 1) else {
            continue;
        };
        let joiner = tail.trim_start_matches(['세', '살', '만']);
        let joined = ["~", "∼", "-", "–", "부터", "이상", "에서"]
            .iter()
            .any(|j| joiner.starts_with(j))
            && joiner.chars().count() <= 6;
        if joined && (15..=29).contains(value) && (29..=45).contains(upper) && year(upper_tail) {
            return true;
        }
    }
    false
}

/// "65세이상", "만70세이상": a senior lower bound. `text` has no whitespace.
fn senior_age_floor(text: &str) -> bool {
    numbers(text)
        .iter()
        .any(|(value, tail)| (60..=120).contains(value) && tail.starts_with("세이상"))
}

/// What separates the items of a list of eligible people.
const LIST_SEPARATORS: &[char] = &[',', '，', '·', '/', '\n', '、'];
/// Conjunctions that join list items ("한부모가족 및 다문화가족").
const LIST_CONJUNCTIONS: &[&str] = &["및", "또는", "혹은"];

/// The non-empty items of a list of eligible people split at
/// [`LIST_SEPARATORS`], squashed. The one splitter this module and
/// [`crate::youth_fit`] read lists with.
pub(crate) fn list_items(raw: &str) -> Vec<String> {
    raw.split(LIST_SEPARATORS)
        .map(squash)
        .filter(|item| !item.is_empty())
        .collect()
}

/// [`list_items`] after reading [`LIST_CONJUNCTIONS`] as separators too.
/// Only the audience lead line uses it: youth_fit's demotion rules were
/// measured on holdouts with the separator split, and reading conjunctions
/// there changes a measured verdict (holdout11 #43, "장애인 및 지역주민").
fn list_items_and_conjunctions(raw: &str) -> Vec<String> {
    let mut text = raw.to_owned();
    for word in LIST_CONJUNCTIONS {
        text = text.replace(word, ",");
    }
    list_items(&text)
}

/// The groups a lead target line names, and whether the line also lists
/// people no group models.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LeadGroups {
    pub groups: Vec<Group>,
    /// Some list item names no [`GROUP_TERMS`] group.
    pub open: bool,
}

pub(crate) fn lead_groups(line: &str) -> LeadGroups {
    let groups = groups_in(&squash(line));
    let open = !groups.is_empty()
        && list_items_and_conjunctions(line)
            .iter()
            .any(|item| groups_in(item).is_empty());
    LeadGroups { groups, open }
}

pub(crate) fn first_line(text: &str) -> &str {
    text.trim_start_matches(|c: char| c.is_whitespace() || "○ㅇ-·*◦•".contains(c))
        .lines()
        .next()
        .unwrap_or("")
}

/// Groups named as the subject of `text` (no whitespace), ignoring a line
/// that only favours or excludes the group.
fn groups_in(text: &str) -> Vec<Group> {
    let mut groups: Vec<Group> = GROUP_TERMS
        .iter()
        .filter(|(_, terms)| terms.iter().any(|term| names_group(text, term)))
        .map(|(group, _)| *group)
        .collect();
    if senior_age_floor(text) && !groups.contains(&Group::Senior) {
        groups.push(Group::Senior);
    }
    groups
}

/// The groups a title, or a target whose lead line names only groups,
/// reserves a programme for, read as [`classify`] reads them but before its
/// youth reset (which drops them whenever a youth signal is present). For a
/// caller that must tell "youth" from "youth, and reserved for another group
/// too". A lead line that also lists people no group models ("기초생활수급자,
/// 장애인, 청년") is open to them, so it reserves nothing.
pub fn reserved_groups(title: &str, target: Option<&str>) -> Vec<Group> {
    let mut groups = groups_in(&squash(title));
    let lead = lead_groups(first_line(target.unwrap_or("")));
    if !lead.open {
        for group in lead.groups {
            if !groups.contains(&group) {
                groups.push(group);
            }
        }
    }
    groups
}

pub fn classify(t: &AudienceText) -> Audience {
    let title = squash(t.title);
    let summary = squash(t.summary.unwrap_or(""));
    let target = squash(t.target.unwrap_or(""));
    let all = format!("{title}\n{summary}\n{target}");

    let life_youth_only = t.life_stages.is_some_and(|stages| {
        let stages: Vec<&str> = stages
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        !stages.is_empty() && stages.iter().all(|s| *s == "청년" || *s == "청소년")
    });
    let band_youth = matches!((t.min_age, t.max_age), (lo, Some(hi))
        if (24..=45).contains(&hi) && lo.is_none_or(|lo| (14..=29).contains(&lo)));
    let strong_youth = t.curated_youth
        || YOUTH_SOURCES.contains(&t.source_type)
        || contains_any(&all, YOUTH_TERMS)
        || youth_age_span(&all)
        || life_youth_only
        || band_youth;

    // 보조금24 tags most 청년 창업 programmes 소상공인 or 법인 even when
    // 예비창업자 apply ("창업희망청년", "예비창업자"), so for a youth programme
    // only explicit text ("개인 신청 불가") rules individuals out.
    let applicant = match applicant(t.applicant_kinds, t.target.unwrap_or("")) {
        (_, Basis::Kinds) if strong_youth => Applicant::Individual,
        (applicant, _) => applicant,
    };

    let mut groups = Vec::new();
    let lead = lead_groups(first_line(t.target.unwrap_or("")));
    for group in groups_in(&title)
        .into_iter()
        .chain(lead.groups.iter().copied())
    {
        if !groups.contains(&group) {
            groups.push(group);
        }
    }
    if let Some(tags) = t.target_tags {
        let tags: Vec<&str> = tags
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        // Reserved only when every tag is a flagged group: "저소득, 장애인"
        // is open to any low-income applicant.
        let tagged: Vec<Group> = tags
            .iter()
            .filter_map(|tag| TAG_GROUPS.iter().find(|(t, _)| t == tag).map(|(_, g)| *g))
            .collect();
        if !tags.is_empty() && tagged.len() == tags.len() {
            for group in tagged {
                if !groups.contains(&group) {
                    groups.push(group);
                }
            }
        }
    }
    let youth = strong_youth || (groups.is_empty() && contains_any(&all, WEAK_YOUTH_TERMS));
    let mut open_groups = Vec::new();
    if youth {
        // A lead line every item of which names a group says who among the
        // young may apply ("만 19~34세 등록 장애인", "등록 장애인 청년"):
        // youth and reserved, capped by [`membership`] (review2 M2). A group
        // named only in the title or the tags ("장애인 청년 취업지원",
        // "청년·장애인 일자리") may be the audience or one route of several:
        // an open reservation, never a closed one (review M1).
        let (closed, open): (Vec<Group>, Vec<Group>) = groups
            .into_iter()
            .partition(|group| !lead.open && lead.groups.contains(group));
        groups = closed;
        open_groups = open;
    }
    Audience {
        youth,
        applicant,
        groups,
        groups_closed: !lead.open,
        open_groups,
        holder: None,
        holder_unreadable: false,
    }
}

/// What the applicant verdict was read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Basis {
    /// The source's structured applicant kinds (`사용자구분`).
    Kinds,
    /// An explicit phrase in the target text, or nothing at all.
    Text,
}

/// `target` is the stored target text, line breaks intact.
fn applicant(kinds: Option<&str>, target: &str) -> (Applicant, Basis) {
    if let Some(kinds) = kinds.filter(|k| !k.trim().is_empty()) {
        let kinds: Vec<&str> = kinds.split("||").map(str::trim).collect();
        let applicant = if kinds.iter().any(|k| *k == "개인" || *k == "가구") {
            Applicant::Individual
        } else if kinds.contains(&"소상공인") {
            Applicant::Business
        } else if kinds.contains(&"법인/시설/단체") {
            Applicant::Organisation
        } else {
            Applicant::Individual
        };
        return (applicant, Basis::Kinds);
    }
    let squashed = squash(target);
    let university = UNIVERSITY_APPLICANTS.iter().any(|name| {
        squashed.match_indices(name).any(|(at, _)| {
            let after: String = squashed[at + name.len()..].chars().take(4).collect();
            !after.contains('생')
        })
    });
    let subjects: Vec<String> = target.lines().map(line_subject).collect();
    let subject_is = |phrases: &[&str]| {
        subjects
            .iter()
            .any(|subject| phrases.iter().any(|p| subject.starts_with(p)))
    };
    let applicant = if contains_any(&squashed, ORGANISATION_PHRASES)
        || university
        || subject_is(ORGANISATION_SUBJECTS)
    {
        Applicant::Organisation
    } else if subject_is(BUSINESS_SUBJECTS) {
        Applicant::Business
    } else {
        Applicant::Individual
    };
    (applicant, Basis::Text)
}

/// One target line without bullets, whitespace or a leading
/// [`APPLICANT_LABELS`] label: "○ 지원대상 : 기관 대상" → "기관대상".
fn line_subject(line: &str) -> String {
    let line =
        squash(line.trim_start_matches(|c: char| c.is_whitespace() || "○ㅇ-·*◦•".contains(c)));
    let rest = APPLICANT_LABELS
        .iter()
        .find_map(|label| line.strip_prefix(label))
        .unwrap_or(&line);
    rest.trim_start_matches([':', '：', ')', ']', '-'])
        .to_owned()
}

/// Whether no individual can apply (in any mode): an organisation-only row,
/// or a business-only row for a user who is not self-employed. Who among
/// individuals the row is reserved for is [`membership`].
pub fn hard_exclusion(audience: &Audience, profile: &AudienceProfile) -> Option<AudienceExclusion> {
    match audience.applicant {
        Applicant::Organisation => Some(AudienceExclusion::NotIndividual),
        Applicant::Business if !profile.self_employed => Some(AudienceExclusion::NotIndividual),
        _ => None,
    }
}

/// The user's membership of one group: `Pass` known inside, `Fail` known
/// outside, `Unknown` otherwise. The one reading of a group against a
/// profile. Groups with no profile answer (pregnancy, farming, children)
/// are always unknown.
fn group_membership(group: Group, profile: &AudienceProfile) -> RuleVerdict {
    let by_age = |floor: i32| match profile.age {
        Some(age) if age >= floor => RuleVerdict::Pass,
        Some(_) => RuleVerdict::Fail,
        None => RuleVerdict::Unknown,
    };
    let by_flag = |flag: Option<bool>| match flag {
        Some(true) => RuleVerdict::Pass,
        Some(false) => RuleVerdict::Fail,
        None => RuleVerdict::Unknown,
    };
    match group {
        Group::Senior => by_age(60),
        Group::MiddleAged => by_age(40),
        Group::Disability => by_flag(profile.has_disability),
        Group::Veteran => by_flag(profile.veteran_family),
        Group::Multicultural => by_flag(profile.multicultural_family),
        Group::SelfEmployed if profile.self_employed => RuleVerdict::Pass,
        Group::Pregnancy
        | Group::FarmFishery
        | Group::Child
        | Group::SelfEmployed
        | Group::Unmodelled => RuleVerdict::Unknown,
    }
}

/// The group a stored youth_fit tier reserves a row for, read with its
/// stored target: one closed part of [`membership`]. `None` for a fit row.
fn tier_group(tier: crate::youth_fit::Tier) -> Option<Group> {
    use crate::youth_fit::{Target, Verdict};
    let by_target = |target: Option<Target>| match target {
        Some(Target::FarmFishery) => Group::FarmFishery,
        Some(Target::Elder) => Group::MiddleAged,
        Some(Target::Child) => Group::Child,
        Some(Target::Disability) => Group::Disability,
        Some(Target::Veteran) => Group::Veteran,
        Some(Target::Business) => Group::SelfEmployed,
        Some(Target::Organisation | Target::Other) | None => Group::Unmodelled,
    };
    match tier.verdict {
        Verdict::Fit => None,
        Verdict::OffTarget => Some(by_target(tier.target)),
        Verdict::BusinessOnly => Some(Group::SelfEmployed),
        Verdict::ChildOnly => Some(Group::Child),
        Verdict::DisabilityOnly => Some(Group::Disability),
        Verdict::VeteranOnly => Some(Group::Veteran),
    }
}

/// The profile field that answers a group, in [`crate::rule_engine::FIELDS`].
fn group_field(group: Group) -> Option<&'static str> {
    match group {
        Group::Disability => Some("has_disability"),
        Group::Veteran => Some("veteran_family"),
        Group::Multicultural => Some("is_multicultural_family"),
        _ => None,
    }
}

/// Whether a user may belong to who a programme is reserved for: the
/// audience groups read from its text and its stored youth_fit tier with its
/// target ([`tier_group`]). One check beside the structural checks
/// ([`crate::structural::CheckKind::Audience`]); the list and the assistant
/// card both take it from here, so a reserved row's status never exceeds
/// it, whatever keeps the row listed (an exempt profile, a question that
/// asks for the target). Construct with [`membership`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Membership {
    checks: Vec<StructuralCheck>,
}

impl Membership {
    /// The audience checks (none for a row reserved for nobody): `Pass`
    /// (known inside a group), `Fail` (a closed list the user is known to be
    /// outside of), or `Unknown` once per unanswered flag that would settle
    /// it, else once with no field ("지원 대상" to confirm).
    pub fn checks(&self) -> &[StructuralCheck] {
        &self.checks
    }

    /// The list hides the row: the user is known outside every group.
    pub fn hides(&self) -> bool {
        self.checks.iter().any(|c| c.status == RuleVerdict::Fail)
    }
}

/// The membership check of a row. `audience` is its [`classify`] result;
/// `tier` its stored youth_fit verdict and target. Pass = known inside a
/// modelled group; Fail = a closed list known outside every group, or a
/// reserved tier the profile answers against; Unknown = everything else,
/// including an open list ([`Audience::groups_closed`]) and an unanswered
/// flag.
pub fn membership(
    audience: &Audience,
    tier: Option<crate::youth_fit::Tier>,
    profile: &AudienceProfile,
) -> Membership {
    // (group, closed): text groups close with the list. A tier is the
    // classifier's hedged reading ("…일 수 있음"), so it closes only on a
    // flag the user answered against it; on age or anything else it admits
    // (Pass) or stays Unknown, never a stated mismatch (review-final H-1).
    let mut parts: Vec<Vec<(Group, bool)>> = Vec::new();
    if !audience.groups.is_empty() {
        parts.push(
            audience
                .groups
                .iter()
                .map(|group| (*group, audience.groups_closed))
                .collect(),
        );
    }
    if !audience.open_groups.is_empty() {
        parts.push(audience.open_groups.iter().map(|g| (*g, false)).collect());
    }
    if let Some(group) = tier.and_then(tier_group) {
        let answered_flag = group_field(group).is_some();
        parts.push(vec![(group, answered_flag)]);
    }
    let mut checks = group_checks(&parts, profile);
    // The holder requirement is one more condition that must admit
    // (review C2): Pass when held, Fail (hidden) when answered "no",
    // otherwise Unknown with the field as the missing check.
    if let Some(holder) = &audience.holder {
        checks.push(StructuralCheck {
            kind: CheckKind::Audience,
            status: profile.holdings.verdict(holder),
            missing_field: (profile.holdings.verdict(holder) == RuleVerdict::Unknown)
                .then_some(holder.field.name()),
            unresolved_scope: false,
        });
    }
    if audience.holder_unreadable {
        checks.push(StructuralCheck {
            kind: CheckKind::Audience,
            status: RuleVerdict::Unknown,
            missing_field: None,
            unresolved_scope: true,
        });
    }
    Membership { checks }
}

/// The checks of the reserved-group parts: none when the row is reserved
/// for nobody.
fn group_checks(parts: &[Vec<(Group, bool)>], profile: &AudienceProfile) -> Vec<StructuralCheck> {
    if parts.is_empty() {
        return Vec::new();
    }
    // Each part is one "reserved for any of these" list; all must admit.
    let verdicts: Vec<RuleVerdict> = parts
        .iter()
        .map(|groups| {
            let each: Vec<RuleVerdict> = groups
                .iter()
                .map(|(group, _)| group_membership(*group, profile))
                .collect();
            let closed = groups.iter().all(|(_, closed)| *closed);
            if each.contains(&RuleVerdict::Pass) {
                RuleVerdict::Pass
            } else if closed && each.iter().all(|v| *v == RuleVerdict::Fail) {
                RuleVerdict::Fail
            } else {
                RuleVerdict::Unknown
            }
        })
        .collect();
    let check = |status, missing_field| StructuralCheck {
        kind: CheckKind::Audience,
        status,
        missing_field,
        unresolved_scope: false,
    };
    let checks = if verdicts.contains(&RuleVerdict::Fail) {
        vec![check(RuleVerdict::Fail, None)]
    } else if verdicts.iter().all(|v| *v == RuleVerdict::Pass) {
        vec![check(RuleVerdict::Pass, None)]
    } else {
        // The unanswered flags of the undecided parts, in FIELDS order.
        let mut asks: Vec<&'static str> = parts
            .iter()
            .zip(&verdicts)
            .filter(|(_, v)| **v == RuleVerdict::Unknown)
            .flat_map(|(groups, _)| groups.iter())
            .filter(|(group, _)| group_membership(*group, profile) == RuleVerdict::Unknown)
            .filter_map(|(group, _)| group_field(*group))
            .collect();
        asks.sort_by_key(|field| {
            crate::rule_engine::FIELDS
                .iter()
                .position(|spec| spec.name == *field)
        });
        asks.dedup();
        if asks.is_empty() {
            vec![check(RuleVerdict::Unknown, None)]
        } else {
            asks.into_iter()
                .map(|field| check(RuleVerdict::Unknown, Some(field)))
                .collect()
        }
    };
    checks
}

/// Whether the title itself names young people ("청년 월세", "대학생 학자금",
/// "만19~39세"): a stronger ranking signal than a youth source or target text.
pub fn youth_title(title: &str) -> bool {
    let title = squash(title);
    contains_any(&title, YOUTH_TERMS) || youth_age_span(&title)
}

/// Persona interests used for ranking only.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Persona {
    pub student: bool,
    pub job_seeker: bool,
    pub renter: bool,
}

const STUDENT_TERMS: &[&str] = &["장학", "학자금", "등록금", "교육", "교재", "수강", "대학생"];
const JOB_TERMS: &[&str] = &[
    "취업",
    "구직",
    "일자리",
    "채용",
    "면접",
    "자격증",
    "직업훈련",
    "인턴",
];
const HOUSING_TERMS: &[&str] = &["월세", "전세", "주거", "임대", "보증금", "주택"];

/// Whether the programme serves one of the persona's interests: student →
/// scholarships/education, job-seeker → employment, renter → housing.
pub fn persona_match(persona: &Persona, program_type: &str, title: &str) -> bool {
    let title = squash(title);
    (persona.student
        && (matches!(program_type, "scholarship" | "campus_benefit")
            || contains_any(&title, STUDENT_TERMS)))
        || (persona.job_seeker && (program_type == "employment" || contains_any(&title, JOB_TERMS)))
        || (persona.renter && (program_type == "housing" || contains_any(&title, HOUSING_TERMS)))
}

#[cfg(test)]
mod tests {
    #[test]
    fn reserved_groups_are_read_before_the_youth_reset() {
        use super::{reserved_groups, Group};
        assert_eq!(
            reserved_groups("행복적금", Some("만 19세~34세 장애인")),
            [Group::Disability]
        );
        assert_eq!(
            reserved_groups("장애인 청년 적금", None),
            [Group::Disability]
        );
        assert!(reserved_groups("청년 적금", Some("기초생활수급자, 장애인, 청년")).is_empty());
        assert!(reserved_groups("청년 적금", Some("만 19세~34세 개인")).is_empty());
    }

    use super::*;

    /// What the list does with a row: no individual applies, or the user is
    /// known outside every group it is reserved for.
    fn hidden(audience: &Audience, profile: &AudienceProfile) -> Option<AudienceExclusion> {
        hard_exclusion(audience, profile).or_else(|| {
            membership(audience, None, profile)
                .hides()
                .then(|| AudienceExclusion::TargetGroup(audience.groups[0]))
        })
    }

    /// Review R-H2: every reserved lead list the review probed is either
    /// hidden (known outside) or Unknown (listed, capped); none is ever Pass
    /// for a 26-year-old with no group, answered or not.
    #[test]
    fn a_reserved_row_never_passes_for_a_non_member() {
        let answered_no = AudienceProfile {
            age: Some(26),
            ..NOBODY
        };
        let unanswered = AudienceProfile {
            age: Some(26),
            has_disability: None,
            veteran_family: None,
            multicultural_family: None,
            self_employed: false,
            holdings: crate::holder::Holdings::UNANSWERED,
        };
        for (title, target) in [
            ("보장구 지원", "○ 장애인(지체, 뇌병변)"),
            ("재활 지원", "○ 지체·뇌병변 장애인"),
            ("생활 지원", "○ 관내 거주 장애인, 관내 등록"),
            ("돌봄 지원", "○ 장애인/보호자"),
            ("돌봄 지원", "○ 등록 장애인 및 보호자"),
            ("가족 지원", "○ 장애인 및 그 가족"),
            ("의료 지원", "○ 국가유공자 및 유족"),
            ("기념 지원", "○ 5·18민주유공자 및 유족"),
            ("정착 지원", "○ 북한이탈주민 및 그 자녀"),
            ("정착 지원", "○ 결혼이민자 및 귀화자"),
            ("일자리 지원", "○ 40대 이상 중장년 구직자 및 재직자"),
            (
                "중장년내일센터",
                "○ 40대 이상 중장년 구직자, 재직자(퇴직예정자) 및 중장년 고용예정 사업주",
            ),
            ("장애인 보조기기 지원", "○ 등록 장애인"),
        ] {
            let audience = classify(&gov(title, target, "개인"));
            assert!(!audience.groups.is_empty(), "{target}: {audience:?}");
            for profile in [&answered_no, &unanswered] {
                let m = membership(&audience, None, profile);
                assert!(!m.checks().is_empty(), "{target}");
                assert!(
                    m.checks().iter().all(|c| c.status != RuleVerdict::Pass),
                    "{target}: {m:?}"
                );
            }
            // Unanswered flags never hide.
            let m = membership(&audience, None, &unanswered);
            let age_only = audience
                .groups
                .iter()
                .all(|g| matches!(g, Group::Senior | Group::MiddleAged));
            if !age_only {
                assert!(!m.hides(), "{target}: {m:?}");
            }
        }
        // The unanswered flag is the question; an open list with none asks
        // for 지원 대상 (no field).
        let disabled = classify(&gov("장애인 보조기기 지원", "○ 등록 장애인", "개인"));
        let m = membership(&disabled, None, &unanswered);
        assert_eq!(
            m.checks()
                .iter()
                .map(|c| c.missing_field)
                .collect::<Vec<_>>(),
            [Some("has_disability")]
        );
        let middle = classify(&gov(
            "일자리 지원",
            "○ 40대 이상 중장년 구직자 및 재직자",
            "개인",
        ));
        let m = membership(&middle, None, &answered_no);
        assert_eq!(
            m.checks()
                .iter()
                .map(|c| (c.status, c.missing_field))
                .collect::<Vec<_>>(),
            [(RuleVerdict::Unknown, None)]
        );
        // A known member passes; the youth_fit tier reads the same flag.
        let yes = AudienceProfile {
            has_disability: Some(true),
            ..unanswered
        };
        assert_eq!(
            membership(&disabled, None, &yes).checks()[0].status,
            RuleVerdict::Pass
        );
        let none = classify(&gov("생활 지원", "○ 관내 거주자", "개인"));
        let tier = Some(crate::youth_fit::Tier {
            verdict: crate::youth_fit::Verdict::DisabilityOnly,
            target: Some(crate::youth_fit::Target::Disability),
        });
        assert_eq!(
            membership(&none, tier, &unanswered).checks()[0].missing_field,
            Some("has_disability")
        );
        assert!(membership(&none, tier, &answered_no).hides());
        assert!(membership(&none, None, &answered_no).checks().is_empty());
    }

    /// Review M-2 (final code review): every demoted tier is a closed part of
    /// who the row is for, read with its stored target, so a row the list
    /// keeps because the question asked for its target (or the profile is
    /// exempt) is capped by the same check and never passes for a user not
    /// known to be in that group.
    #[test]
    fn every_demoted_tier_caps_the_row_by_its_target() {
        use crate::youth_fit::{Target, Verdict};
        let plain = classify(&gov("생활 지원", "○ 관내 거주자", "개인"));
        let young = AudienceProfile {
            age: Some(26),
            ..AudienceProfile::default()
        };
        let older = AudienceProfile {
            age: Some(45),
            ..AudienceProfile::default()
        };
        let tier = |verdict, target| Some(crate::youth_fit::Tier { verdict, target });
        // Unknown for everyone: no profile answers farming, children, or an
        // unmodelled target; an organisation-only row admits no individual.
        for (verdict, target) in [
            (Verdict::OffTarget, Some(Target::FarmFishery)),
            (Verdict::OffTarget, Some(Target::Other)),
            (Verdict::OffTarget, Some(Target::Organisation)),
            (Verdict::OffTarget, None),
            (Verdict::ChildOnly, Some(Target::Child)),
            (Verdict::BusinessOnly, Some(Target::Business)),
        ] {
            for profile in [&young, &older] {
                let m = membership(&plain, tier(verdict, target), profile);
                assert_eq!(
                    m.checks()
                        .iter()
                        .map(|c| (c.status, c.missing_field))
                        .collect::<Vec<_>>(),
                    [(RuleVerdict::Unknown, None)],
                    "{verdict:?} {target:?}"
                );
            }
        }
        // 40+ reads the age: a 45-year-old is inside; a 26-year-old stays
        // unknown, never a stated mismatch (the tier is a hedged reading).
        let elder = tier(Verdict::OffTarget, Some(Target::Elder));
        let m = membership(&plain, elder, &young);
        assert!(!m.hides());
        assert_eq!(m.checks()[0].status, RuleVerdict::Unknown);
        assert_eq!(
            membership(&plain, elder, &older).checks()[0].status,
            RuleVerdict::Pass
        );
        // A self-employed user is inside a business-only row.
        let own = AudienceProfile {
            self_employed: true,
            ..young
        };
        assert_eq!(
            membership(
                &plain,
                tier(Verdict::BusinessOnly, Some(Target::Business)),
                &own
            )
            .checks()[0]
                .status,
            RuleVerdict::Pass
        );
        // A fit row is reserved for nobody.
        assert!(membership(&plain, tier(Verdict::Fit, None), &young)
            .checks()
            .is_empty());
    }

    /// Review C2: an issuer offer (sponsor brand with a holder key) is
    /// three-state on the holder field, alone or beside a reserved group.
    #[test]
    fn a_holder_offer_is_three_state_on_its_holder_field() {
        use crate::holder::{Holder, Holdings};
        let skt = Holder::parse("telecom_carrier:skt").unwrap();
        let mut offer = classify(&AudienceText {
            source_type: "corporate_official",
            title: "SKT 청년 혜택 CGV 영화 50% 할인",
            ..Default::default()
        });
        offer.holder = Some(skt);
        let unanswered = AudienceProfile {
            age: Some(24),
            ..AudienceProfile::default()
        };
        let m = membership(&offer, None, &unanswered);
        assert!(!m.hides());
        assert_eq!(
            m.checks()
                .iter()
                .map(|c| (c.status, c.missing_field))
                .collect::<Vec<_>>(),
            [(RuleVerdict::Unknown, Some("telecom_carrier"))]
        );
        let other = AudienceProfile {
            holdings: Holdings::from_answers(Some("kt"), None, None),
            ..unanswered
        };
        assert!(
            membership(&offer, None, &other).hides(),
            "answered no: hidden"
        );
        let holds = AudienceProfile {
            holdings: Holdings::from_answers(Some("skt"), None, None),
            ..unanswered
        };
        let m = membership(&offer, None, &holds);
        assert!(!m.hides());
        assert_eq!(m.checks()[0].status, RuleVerdict::Pass);
        // Beside a reserved group both must admit: the group's unanswered
        // flag and the holder are each asked.
        let mut reserved = classify(&gov("보장구 지원", "○ 등록 장애인", "개인"));
        reserved.holder = Some(skt);
        let fields: Vec<_> = membership(&reserved, None, &unanswered)
            .checks()
            .iter()
            .map(|c| c.missing_field)
            .collect();
        assert_eq!(fields, [Some("has_disability"), Some("telecom_carrier")]);
        assert!(membership(&reserved, None, &other).hides());
        // A row with no holder requirement is unchanged.
        let plain = classify(&gov("생활 지원", "○ 관내 거주자", "개인"));
        assert!(membership(&plain, None, &other).checks().is_empty());
    }

    /// Code review M-11: a holder requirement this build cannot read is an
    /// unanswerable Unknown check, never "no requirement".
    #[test]
    fn an_unreadable_holder_requirement_is_never_no_requirement() {
        use crate::holder::Holder;
        assert!(Holder::unreadable(Some("telecom_carrier:xyz")));
        assert!(Holder::unreadable(Some("loyalty_card:abc")));
        assert!(!Holder::unreadable(Some("telecom_carrier:skt")));
        assert!(!Holder::unreadable(None));
        assert!(!Holder::unreadable(Some(" ")));
        let mut offer = classify(&AudienceText {
            source_type: "corporate_official",
            title: "CGV 영화 50% 할인",
            ..Default::default()
        });
        offer.holder_unreadable = true;
        let answered = AudienceProfile {
            age: Some(24),
            ..AudienceProfile::default()
        };
        let m = membership(&offer, None, &answered);
        assert!(!m.hides());
        let statuses: Vec<RuleVerdict> = m.checks().iter().map(|c| c.status).collect();
        assert_eq!(statuses, [RuleVerdict::Unknown]);
        assert!(m.checks()[0].unresolved_scope);
    }

    fn gov<'a>(title: &'a str, target: &'a str, kinds: &'a str) -> AudienceText<'a> {
        AudienceText {
            source_type: "gov_benefits",
            title,
            target: Some(target),
            applicant_kinds: Some(kinds),
            ..Default::default()
        }
    }

    fn youth<'a>(title: &'a str, target: &'a str) -> AudienceText<'a> {
        AudienceText {
            source_type: "youth_center",
            title,
            target: Some(target),
            ..Default::default()
        }
    }

    fn welfare<'a>(title: &'a str, tags: &'a str, life: &'a str) -> AudienceText<'a> {
        AudienceText {
            source_type: "local_welfare",
            title,
            target_tags: Some(tags),
            life_stages: Some(life),
            ..Default::default()
        }
    }

    /// A 24-year-old who answered "no" to every group flag.
    const NOBODY: AudienceProfile = AudienceProfile {
        age: Some(24),
        has_disability: Some(false),
        veteran_family: Some(false),
        multicultural_family: Some(false),
        self_employed: false,
        holdings: crate::holder::Holdings::UNANSWERED,
    };

    /// Real titles and target text from the 2026-10-12 accuracy audit
    /// (`reco-accuracy-audit.md`), with the outcome the list must reach for a
    /// 20-30 year old with no group flags.
    #[test]
    fn audit_rows_table() {
        use AudienceExclusion::*;
        let cases: Vec<(&str, AudienceText, bool, Option<AudienceExclusion>)> = vec![
            // ── wrong group (audit F1) ──
            // Pregnancy, farming and child rows have no profile flag, so they
            // are not hidden here; being non-youth, the youth scope leaves
            // them out (review H4).
            (
                "02f62504",
                gov("100세 어르신 장수축하물품 지원", "○ 관내 거주 100세 도래 어르신", "개인"),
                false,
                Some(TargetGroup(Group::Senior)),
            ),
            (
                "02563373",
                gov("나잠어업인 진료비 지원", "○ 나잠어업인(해녀)", "개인"),
                false,
                None,
            ),
            (
                "0292c298",
                gov("산모·신생아 건강관리 지원", "○ 출산가정", "개인"),
                false,
                None,
            ),
            (
                "e9dd1ccd",
                gov("고엽제후유의증환자 수당", "○ 고엽제후유의증 환자", "개인"),
                false,
                Some(TargetGroup(Group::Veteran)),
            ),
            (
                "01fca3d4",
                gov("어린이 눈건강 검진", "○ 만 12세 이하 어린이", "개인"),
                false,
                None,
            ),
            (
                "97eb7411",
                gov("어르신 무임교통", "○ 만 72세 이상 시민", "개인"),
                false,
                Some(TargetGroup(Group::Senior)),
            ),
            (
                "3cfb0ca7",
                gov("예방약품 등 지원", "○ 가축 백신, 방제약품, 질병 컨설팅, 등이 필요한 축산농가", "개인"),
                false,
                None,
            ),
            // ── not individual (audit F1) ──
            (
                "a5868825",
                gov("장애인고용장려금지원", "○ 장애인을 고용한 사업주", "법인/시설/단체"),
                false,
                Some(NotIndividual),
            ),
            (
                "333bd46f",
                gov("전통시장 안전관리패키지", "○ 시장단위 전통시장·상점가", "소상공인"),
                false,
                Some(NotIndividual),
            ),
            (
                "b03ddc0d",
                gov("청소년 수련시설 운영 지원", "○ 공공 청소년수련관, 청소년문화의집 등", "법인/시설/단체"),
                false,
                Some(NotIndividual),
            ),
            (
                "010bf65e",
                youth("AI·스마트도시 혁신인재육성", "대학 4곳 대상으로 지원(개인 신청 불가) / 지원대학 4곳 대상(개인 신청 불가)"),
                true,
                Some(NotIndividual),
            ),
            (
                "52966617",
                youth("예비예술인 현장역량 강화지원(예비예술인지원)", "ㅇ 신청자격 : 「산업교육진흥 및 산학연협력촉진에 관한 법률」에 근거한 산학협력단이 대표단체로서 구성된 컨소시엄"),
                true,
                Some(NotIndividual),
            ),
            // ── youth programmes that must stay (audit recall) ──
            (
                "c7323b31",
                gov("청년월세 지원", "○ 부모님과 별도 거주하는 무주택자로서 만 19~34세 청년", "개인"),
                true,
                None,
            ),
            (
                "030863bc",
                gov("국가장학금 Ⅰ유형", "○ 대한민국 국적의 국내 대학 재학생 중 소득 8구간 이하 대학생", "개인"),
                true,
                None,
            ),
            (
                "7fe1309d",
                gov("국민취업지원제도", "○ 15~69세 구직자 중 가구 중위소득 60% 이하", "개인"),
                true,
                None,
            ),
            (
                "3d84e4bd",
                gov("부산 대학생 학자금 대출이자 지원", "○ 부산 거주 대학생", "개인"),
                true,
                None,
            ),
            // A shared status word does not make a 중장년 programme youth
            // (found in the first local re-run, 092cdb6b). Not hidden
            // either since audit H3: whether "40대 이상 중장년" also governs
            // "재직자" cannot be read from the list, and an open reading
            // never hides. The youth scope still leaves it out.
            (
                "092cdb6b",
                gov("중장년내일센터", "○ 40대 이상 중장년 구직자, 재직자(퇴직예정자) 및 중장년 고용예정 사업주", "개인"),
                false,
                None,
            ),
            (
                "구직",
                gov("구직자 취업역량 강화", "○ 관내 구직자", "개인"),
                true,
                None,
            ),
            // 청년 창업: tagged 소상공인/법인 but open to 예비창업자 (kept).
            (
                "6511ed9f",
                gov("청년 골목창업 지원", "서울특별시 거주 외식업 창업희망청년(만19세~39세)", "소상공인"),
                true,
                None,
            ),
            (
                "770cc52d",
                gov("청년 소모임 활동 지원", "해남군 내 거주하고 있는 청년 3인 이상으로 이루어진 소모임", "법인/시설/단체"),
                true,
                None,
            ),
            // A youth signal wins over a group term.
            (
                "청년농",
                gov("청년창업농 영농정착지원", "○ 만 18세 이상 만 40세 미만 청년 농업인", "개인"),
                true,
                None,
            ),
            (
                "자립준비",
                gov("보호종료아동 자립수당", "○ 아동양육시설 보호종료 후 5년 이내 자립준비청년", "개인"),
                true,
                None,
            ),
            // A group term that only favours or excludes is not the target.
            (
                "우대",
                gov("주민 생활안정 지원", "○ 관내 거주 주민 (장애인 우선)", "개인||가구"),
                false,
                None,
            ),
            (
                "제외",
                gov("주민 생활안정 지원", "○ 관내 거주 주민(기초연금 제외)", "개인||가구"),
                false,
                None,
            ),
            (
                "포함",
                gov("어르신 돌봄 지원", "○ 만 65세 이상 노인(독거노인 포함)", "개인"),
                false,
                Some(TargetGroup(Group::Senior)),
            ),
            // A general individual programme is neither youth nor excluded.
            (
                "6704f3ba",
                gov("전국민 마음투자 지원사업", "○ 우울·불안 등 정서적 어려움이 있는 국민", "개인"),
                false,
                None,
            ),
            // 복지로 tags: reserved only when every tag is a flagged group.
            (
                "bokjiro-장애",
                welfare("장애인 보조기기 지원", "장애인", "청년, 중장년, 노년"),
                false,
                Some(TargetGroup(Group::Disability)),
            ),
            (
                "bokjiro-저소득",
                welfare("저소득층 에너지바우처", "저소득, 장애인", "청년, 중장년, 노년"),
                false,
                None,
            ),
            (
                "bokjiro-청년",
                welfare("청년 마음건강 바우처", "", "청년"),
                true,
                None,
            ),
            (
                "bokjiro-생애",
                welfare("구직급여", "", "청년"),
                true,
                None,
            ),
            // Review H2: organisation phrases inside ordinary youth text.
            (
                "H2-지원대학생",
                youth("학자금 대출이자 지원", "학자금 대출 지원 대학생 중 성적 우수자"),
                true,
                None,
            ),
            (
                "H2-선정대학생",
                youth("우수 대학생 장학금", "최종 선정 대학생에게 지급"),
                true,
                None,
            ),
            (
                "H2-기업대상",
                youth("일자리 연계 지원", "중소기업 대상 채용 연계 프로그램 참여자"),
                true,
                None,
            ),
            (
                "H2-기업대상-gov",
                AudienceText {
                    source_type: "gov_benefits",
                    title: "채용 연계 지원",
                    target: Some("중소기업 대상 채용 연계 프로그램 참여자"),
                    ..Default::default()
                },
                false,
                None,
            ),
            // A labelled organisation line still rules individuals out.
            (
                "H2-label",
                AudienceText {
                    source_type: "gov_benefits",
                    title: "시설 환경개선 지원",
                    target: Some("○ 지원대상: 기관 대상\n○ 선정기준: 노후도"),
                    ..Default::default()
                },
                false,
                Some(NotIndividual),
            ),
            // Review H3: group terms inside other words.
            (
                "H3-귀농가구",
                gov("귀농가구 주택구입 지원", "○ 귀농가구", "개인"),
                false,
                None,
            ),
            (
                "H3-저출산",
                gov("저출산 대응 신혼부부 주거비 지원", "○ 저출산 대응 신혼부부", "개인"),
                false,
                None,
            ),
        ];
        let failures: Vec<String> = cases
            .into_iter()
            .filter_map(|(id, text, want_youth, want_exclusion)| {
                let audience = classify(&text);
                let exclusion = hidden(&audience, &NOBODY);
                (audience.youth != want_youth || exclusion != want_exclusion)
                    .then(|| format!("{id}: {exclusion:?} {audience:?}"))
            })
            .collect();
        assert!(failures.is_empty(), "{failures:#?}");
    }

    /// Review H1: a list of groups hides a programme only when the user is in
    /// none of them.
    #[test]
    fn group_lists_exclude_only_when_no_group_matches() {
        let disabled = AudienceProfile {
            age: Some(25),
            has_disability: Some(true),
            ..NOBODY
        };
        let either = classify(&gov(
            "장애인·어르신 이동지원",
            "○ 장애인, 65세 이상 어르신",
            "개인",
        ));
        assert!(either.groups.contains(&Group::Senior), "{either:?}");
        assert!(either.groups.contains(&Group::Disability), "{either:?}");
        assert_eq!(hidden(&either, &disabled), None);
        assert_eq!(
            hidden(&either, &NOBODY),
            Some(AudienceExclusion::TargetGroup(Group::Senior))
        );

        let tagged = classify(&welfare("이동 지원", "장애인, 보훈대상자", "청년, 노년"));
        assert_eq!(tagged.groups, vec![Group::Disability, Group::Veteran]);
        assert_eq!(hidden(&tagged, &disabled), None);
        assert!(hidden(&tagged, &NOBODY).is_some());
    }

    /// Audit M5: an unanswered flag is unknown, as in the rule engine: it
    /// never hides its group. Only an answered "no" does.
    #[test]
    fn an_unanswered_flag_never_hides_its_group() {
        let unanswered = AudienceProfile {
            has_disability: None,
            veteran_family: None,
            multicultural_family: None,
            ..NOBODY
        };
        for text in [
            welfare("장애인 보조기기 지원", "장애인", "청년, 노년"),
            gov("국가유공자 의료지원", "○ 국가유공자", "개인"),
            gov("다문화가족 방문교육", "○ 다문화가족", "개인"),
        ] {
            let audience = classify(&text);
            assert!(!audience.groups.is_empty(), "{audience:?}");
            assert_eq!(hidden(&audience, &unanswered), None, "{audience:?}");
            assert!(hidden(&audience, &NOBODY).is_some(), "{audience:?}");
        }
        // Age groups read the age, which every list has.
        let senior = classify(&gov("어르신 일자리", "○ 만 65세 이상", "개인"));
        assert!(hidden(&senior, &unanswered).is_some());
    }

    /// Audit H3: a lead line that lists a flagged group beside a group the
    /// profile cannot answer (수급자, 차상위, 한부모, 저소득) is open to that
    /// other group too, so it never hides the row.
    #[test]
    fn a_flagged_group_listed_beside_unmodelled_groups_never_hides() {
        for (title, target) in [
            ("에너지바우처", "○ 기초생활수급자, 차상위계층, 장애인"),
            ("가족 지원", "○ 저소득 한부모가족 및 다문화가족"),
            ("생활 지원", "○ 장애인, 국가유공자, 기초생활수급자"),
            ("생활 지원", "○ 장애인 또는 기초생활수급자"),
        ] {
            let audience = classify(&gov(title, target, "개인"));
            assert!(!audience.groups.is_empty(), "{target}: {audience:?}");
            assert_eq!(hidden(&audience, &NOBODY), None, "{target}: {audience:?}");
        }
        // Every item a flagged group: still reserved.
        let closed = classify(&gov("이동 지원", "○ 장애인, 국가유공자", "개인"));
        assert!(hidden(&closed, &NOBODY).is_some(), "{closed:?}");
        // A group named only in the title stays closed.
        let titled = classify(&gov("장애인 보조기기 지원", "○ 관내 거주자", "개인"));
        assert_eq!(
            hidden(&titled, &NOBODY),
            Some(AudienceExclusion::TargetGroup(Group::Disability))
        );
    }

    /// Audit M6: the group words are one lexicon shared with youth_fit. A
    /// bare 유공자 names merit awardees, not veterans; a bare 장애 names a
    /// soil problem ("연작장애") as readily as people.
    #[test]
    fn merit_awardees_are_not_veterans() {
        let merit = classify(&gov(
            "인구증가시책 유공자 지원",
            "○ 관내 다자녀 가정",
            "개인",
        ));
        assert!(!merit.groups.contains(&Group::Veteran), "{merit:?}");
        assert_eq!(hidden(&merit, &NOBODY), None);
        let soil = classify(&gov("연작장애 예방 지원", "○ 관내 농가", "개인"));
        assert!(!soil.groups.contains(&Group::Disability), "{soil:?}");
        // The real groups still read.
        let veteran = classify(&gov("독립유공자 후손 지원", "○ 독립유공자 후손", "개인"));
        assert_eq!(veteran.groups, vec![Group::Veteran]);
        let disabled = classify(&gov("장애인 보장구 지원", "○ 등록 장애인", "개인"));
        assert_eq!(disabled.groups, vec![Group::Disability]);
    }

    /// Review R-M3: every statutory veteran category reads as the veteran
    /// group (title and lead line), and none of them as anything else. The
    /// list is pinned as a set (review2-verify gap 3: a removed term must
    /// fail here, not only change what the loop below iterates).
    #[test]
    fn veteran_terms_are_the_statutory_categories() {
        let statutory: std::collections::BTreeSet<&str> = [
            "보훈",
            "국가유공자",
            "독립유공자",
            "순국선열",
            "애국지사",
            "전몰군경",
            "전상군경",
            "순직군경",
            "공상군경",
            "상이군경",
            "무공수훈자",
            "보국수훈자",
            "참전",
            "4·19혁명",
            "순직공무원",
            "공상공무원",
            "민주유공자",
            "5·18유공자",
            "특수임무유공자",
            "고엽제",
            "재해사망군경",
            "재해부상군경",
            "재해사망공무원",
            "재해부상공무원",
        ]
        .into_iter()
        .collect();
        assert_eq!(
            VETERAN_TERMS
                .iter()
                .copied()
                .collect::<std::collections::BTreeSet<_>>(),
            statutory
        );
        for term in VETERAN_TERMS {
            let audience = classify(&gov(&format!("{term} 지원"), &format!("○ {term}"), "개인"));
            assert_eq!(
                audience.groups,
                vec![Group::Veteran],
                "{term}: {audience:?}"
            );
        }
    }

    /// Review2 M2: a youth word does not clear a group the whole lead line
    /// names; the row is youth and reserved, never passed for a non-member.
    /// A group named only beside young people, or as a preference, still
    /// clears.
    #[test]
    fn a_youth_row_keeps_a_group_its_lead_line_names_alone() {
        let unanswered = AudienceProfile {
            age: Some(26),
            ..AudienceProfile::default()
        };
        for (title, target) in [
            ("장애인 청년 인턴 지원", "○ 만 19~34세 등록 장애인"),
            ("청년 장애인 고용장려금", "○ 등록 장애인 청년"),
        ] {
            let audience = classify(&gov(title, target, "개인"));
            assert!(audience.youth, "{title}");
            assert_eq!(audience.groups, [Group::Disability], "{title}");
            let m = membership(&audience, None, &unanswered);
            assert_eq!(
                m.checks()
                    .iter()
                    .map(|c| (c.status, c.missing_field))
                    .collect::<Vec<_>>(),
                [(RuleVerdict::Unknown, Some("has_disability"))],
                "{title}"
            );
            assert!(membership(&audience, None, &NOBODY).hides(), "{title}");
        }
        for (title, target) in [
            ("청년 일자리", "○ 청년 및 장애인"),
            ("청년 일자리", "○ 만 19~34세 청년(장애인 우대)"),
            ("청년·장애인 일자리 지원", "○ 만 19~34세 미취업자"),
            ("대학생 학자금", "○ 대학생·청년"),
        ] {
            let audience = classify(&gov(title, target, "개인"));
            assert!(
                audience.youth && audience.groups.is_empty(),
                "{title} {target}"
            );
        }
    }

    /// Review M1: a group a youth row names only in its title is an open
    /// reservation: a non-member is never passed unchecked (Unknown with
    /// the group's question), an answered "no" is never hidden by it, and
    /// a known member passes.
    #[test]
    fn a_youth_rows_title_group_is_an_open_reservation() {
        let unanswered = AudienceProfile {
            age: Some(26),
            ..AudienceProfile::default()
        };
        let member = AudienceProfile {
            age: Some(26),
            has_disability: Some(true),
            ..AudienceProfile::default()
        };
        for title in [
            "장애인 청년 취업지원",
            "보훈가족 청년 장학금",
            "다문화가족 청년 취업 지원",
            "청년 농업인 영농정착 지원",
        ] {
            let audience = classify(&gov(title, "○ 만 18~39세 미취업자", "개인"));
            assert!(audience.youth, "{title}");
            let m = membership(&audience, None, &unanswered);
            assert!(
                m.checks().iter().all(|c| c.status != RuleVerdict::Pass) && !m.checks().is_empty(),
                "{title}: {:?}",
                m.checks()
            );
            assert!(!membership(&audience, None, &NOBODY).hides(), "{title}");
        }
        let disability = classify(&gov(
            "장애인 청년 취업지원",
            "○ 만 18~39세 미취업자",
            "개인",
        ));
        assert!(membership(&disability, None, &member)
            .checks()
            .iter()
            .all(|c| c.status == RuleVerdict::Pass));
    }

    /// Review2 M1: a discharged soldier is not a veteran family; 제대군인 is
    /// reserved (never passes) but no profile flag answers it.
    #[test]
    fn a_discharged_soldier_row_asks_no_veteran_flag() {
        for (title, target) in [
            ("의무복무 제대군인 지원", "○ 의무복무 제대군인"),
            ("제대군인 취업지원", "○ 10년 이상 장기복무 제대군인 본인"),
        ] {
            let audience = classify(&gov(title, target, "개인"));
            assert_eq!(audience.groups, [Group::Unmodelled], "{title}");
            for profile in [&NOBODY, &AudienceProfile::default()] {
                let m = membership(&audience, None, profile);
                assert_eq!(
                    m.checks()
                        .iter()
                        .map(|c| (c.status, c.missing_field))
                        .collect::<Vec<_>>(),
                    [(RuleVerdict::Unknown, None)],
                    "{title}"
                );
            }
        }
    }

    /// Review H3: a group term inside another word names no group.
    #[test]
    fn group_terms_inside_other_words_are_not_groups() {
        let farm = classify(&gov("귀농가구 주택구입 지원", "○ 귀농가구", "개인"));
        assert!(!farm.groups.contains(&Group::FarmFishery), "{farm:?}");
        let low_birth = classify(&gov(
            "저출산 대응 신혼부부 주거비 지원",
            "○ 저출산 대응 신혼부부",
            "개인",
        ));
        assert!(
            !low_birth.groups.contains(&Group::Pregnancy),
            "{low_birth:?}"
        );
        // The real groups still read.
        let farm = classify(&gov("예방약품", "○ 축산농가", "개인"));
        assert_eq!(farm.groups, vec![Group::FarmFishery]);
        let birth = classify(&gov("출산 축하금", "○ 출산가정", "개인"));
        assert_eq!(birth.groups, vec![Group::Pregnancy]);
    }

    /// Review H4: pregnancy, farming and child programmes have no profile
    /// flag to match, so they are never hidden as a target group. They stay
    /// non-youth, so only the default youth scope leaves them out.
    #[test]
    fn groups_without_a_profile_flag_never_exclude() {
        for (text, group) in [
            (
                gov("산모·신생아 건강관리 지원", "○ 출산가정", "개인"),
                Group::Pregnancy,
            ),
            (
                gov("나잠어업인 진료비 지원", "○ 나잠어업인(해녀)", "개인"),
                Group::FarmFishery,
            ),
            (
                gov("어린이 눈건강 검진", "○ 만 12세 이하 어린이", "개인"),
                Group::Child,
            ),
        ] {
            let audience = classify(&text);
            assert_eq!(audience.groups, vec![group], "{audience:?}");
            assert!(!audience.youth, "{audience:?}");
            assert_eq!(hidden(&audience, &NOBODY), None, "{audience:?}");
        }
        // Mixed with a flagged group the user is not in: still not hidden.
        let mixed = classify(&gov("장애아동 재활 지원", "○ 장애아동", "개인"));
        assert_eq!(hidden(&mixed, &NOBODY), None, "{mixed:?}");
    }

    #[test]
    fn flags_unlock_their_group_and_self_employment_unlocks_business() {
        let disability = classify(&welfare("장애인 보조기기 지원", "장애인", "청년, 노년"));
        let flagged = AudienceProfile {
            has_disability: Some(true),
            ..NOBODY
        };
        assert_eq!(hidden(&disability, &flagged), None);
        assert!(hidden(&disability, &NOBODY).is_some());

        let veteran = classify(&gov("국가유공자 의료지원", "○ 국가유공자 및 유족", "개인"));
        let family = AudienceProfile {
            veteran_family: Some(true),
            ..NOBODY
        };
        assert_eq!(hidden(&veteran, &family), None);

        let senior = classify(&gov("어르신 일자리", "○ 만 65세 이상", "개인"));
        let old = AudienceProfile {
            age: Some(66),
            ..NOBODY
        };
        assert_eq!(hidden(&senior, &old), None);

        let market = classify(&gov("소상공인 경영안정자금", "○ 소상공인", "소상공인"));
        let owner = AudienceProfile {
            self_employed: true,
            ..NOBODY
        };
        assert_eq!(hidden(&market, &owner), None);
        assert_eq!(
            hidden(&market, &NOBODY),
            Some(AudienceExclusion::NotIndividual)
        );
        // Mixed applicant kinds keep individuals in.
        let mixed = classify(&gov("창업 지원", "○ 예비창업자", "개인||법인/시설/단체"));
        assert_eq!(mixed.applicant, Applicant::Individual);
    }

    /// Review M3: 동백패스 is open to every adult from 만 19세 with no upper
    /// age and no youth wording; only the curator's flag makes it youth.
    #[test]
    fn curated_youth_flag_marks_an_adult_programme_youth() {
        let pass = AudienceText {
            source_type: "government_official",
            title: "동백패스 (부산 대중교통 요금 환급)",
            summary: Some("만 19세 이상이 동백전 교통카드로 부산 대중교통을 이용하면 환급"),
            min_age: Some(19),
            ..Default::default()
        };
        assert!(!classify(&pass).youth);
        let curated = AudienceText {
            curated_youth: true,
            ..pass
        };
        assert!(classify(&curated).youth);
    }

    #[test]
    fn age_spans_read_as_youth_only_in_the_youth_band() {
        for youth in [
            "만19~39세",
            "만 19세 ~ 만 39세",
            "19세이상34세이하",
            "18-34세",
            "만 39세 이하",
            "만18세부터 만34세까지",
            // Audit L8: full-width digits and signs read as ASCII.
            "만１９세~３９세",
            "만 １９세 ～ ３９세",
        ] {
            assert!(youth_age_span(&squash(youth)), "{youth}");
        }
        assert!(senior_age_floor(&squash("만 ６５세 이상")));
        for not in [
            "만 12세 이하",
            "50~65세",
            "65세 이상",
            "2025~2026년",
            "1~3세",
        ] {
            assert!(!youth_age_span(&squash(not)), "{not}");
        }
        assert!(senior_age_floor(&squash("만 65세 이상")));
        assert!(!senior_age_floor(&squash("만 19세 이상")));
    }

    #[test]
    fn youth_titles() {
        assert!(youth_title("청년월세 지원사업"));
        assert!(youth_title("대학생 학자금 대출이자 지원"));
        assert!(!youth_title("도심융합특구 조성"));
        assert!(!youth_title("청소년 수련시설 운영 지원"));
    }

    #[test]
    fn persona_categories() {
        let student = Persona {
            student: true,
            ..Default::default()
        };
        assert!(persona_match(&student, "scholarship", "x"));
        assert!(persona_match(
            &student,
            "benefit",
            "대학생 학자금 대출이자 지원"
        ));
        assert!(!persona_match(&student, "housing", "청년 월세 지원"));
        let seeker = Persona {
            job_seeker: true,
            ..Default::default()
        };
        assert!(persona_match(&seeker, "employment", "x"));
        assert!(persona_match(&seeker, "benefit", "구직청년 자격증 응시료"));
        let renter = Persona {
            renter: true,
            ..Default::default()
        };
        assert!(persona_match(&renter, "benefit", "청년월세 지원"));
        assert!(!persona_match(&Persona::default(), "scholarship", "장학금"));
    }
}
