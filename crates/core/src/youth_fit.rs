//! Whether a catalogue row is for a young individual (`youth_fit`).
//!
//! About a quarter of the visible 보조금24 catalogue is for farms, firms,
//! institutions or people over 40, and about a tenth is for children only
//! (relevance review 2026-10-21). Such rows are demoted, never hidden: they
//! stay searchable and reachable by link, but leave the default catalogue
//! list, the default sort and the recommendation candidates.
//!
//! The rules run in order and the first match wins:
//! - K0 (keep): a source whose every row is for students or young people.
//! - S1 (demote): `사용자구분` has neither 개인 nor 가구 (organisation;
//!   business when 소상공인 is listed), unless the title names a personal
//!   benefit, the text names pre-founders, or it names people served while
//!   the lead line names no organisation. Checked before the youth keeps: an
//!   individual cannot apply, whatever the text says about youth.
//! - Keep (never demoted):
//!   - K1: the recommender's own youth classifier ([`audience::classify`])
//!     says youth, or the title or the lead target line names young people.
//!     So a row the youth recommendations rely on is never demoted.
//!   - K2: the target text names the general public ("누구나", "전국민").
//!   - K3: the target text names pre-founders ("예비창업", "창업팀"), who are
//!     individuals.
//!   - K4: the 보조금24 personal flags name university students.
//!   - K5: the age band reaches young adults: a 보조금24 ceiling of 20-39,
//!     or a target range ending at 20-39 ("만 15~24세 청소년").
//!   - K6: 자활 programmes (low-income adults of any age).
//!   - K7: individual 귀농·귀촌 and young-farmer benefits (귀농, 귀촌,
//!     청년농, 후계농, 창업농) with no age floor above 39.
//! - D1 (disability tier): the title names 장애인, or the only personal flag
//!   is the disabled flag, and the target lists no other group young adults
//!   belong to (한부모, families with children, pregnancy).
//! - V1 (veteran tier): the title names 보훈, 국가유공자, 참전 and the like,
//!   with the same other-group exception. Shown to veteran-family users.
//! - S5: 진폐 (an occupational disease of retired miners).
//! - Structured 보조금24 fields:
//!   - S2: the personal `supportConditions` flags are only farm, fishery,
//!     livestock or forestry operators.
//!   - S3: the age floor (`JA0110`, else `min_age`) is 60 or more, or 40 or
//!     more and the subject of the lead line, with no OR'd young band ("6~23세 및 65세 이상") or
//!     pregnancy group. When disabled people share the floor's band ("노인·
//!     장애인", "장애인 또는 65세 이상") the row gets the disability tier (D2)
//!     instead.
//!   - S4: school-pupil-only flags (S4a), or an age ceiling (`JA0111`, else
//!     `max_age`) of 18 or less with a child noun in the title (S4b) or with
//!     no parent, pregnancy or household wording (S4c); never when the row
//!     reaches adults (대학, 학생증, 전공과, 다둥이 카드).
//!
//! Tiers: `off_target` (organisation, farm, 40+), `business_only` (shown to
//! self-employed users), `child_only` (shown to likely parents: age 25+ or a
//! household of 3+), `disability_only` and `veteran_only` (shown to users
//! with that profile flag). The catalogue list and the recommendations apply
//! the same exemptions: see [`recommendation_exclusion`] and [`demoted_sql`].
//!
//! There is no title deny-list. The relevance review proposed one (farm,
//! business, elder and facility words), but across holdouts 4-9 it was wrong
//! for 10 of the 21 rows it demoted, against 0.5-6.3% for the structured
//! rules (S2 lowest, S3 highest), and 복지로 rows already pass a youth
//! life-stage filter at ingestion.
//!
//! Every [`Decision`] carries the rule that fired and the evidence it read,
//! so a stored verdict can be audited. The stored version
//! ([`classifier_version`]) digests every word list and threshold, here and
//! in [`audience`], so editing one recomputes every row with no manual step;
//! a change to the rule logic itself bumps [`CLASSIFIER_VERSION`].

use crate::audience::{
    self, contains_any, first_line, list_items, squash, youth_age_span, AudienceText, YOUTH_TERMS,
};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// Bump on any change to the rule *logic*. Word lists and thresholds are
/// covered by [`classifier_version`] automatically. v2: `audience::squash`
/// folds full-width digits and signs (audit L8). v3: the veteran lexicon
/// is the statutory category list (`audience::VETERAN_TERMS`, review R-M3).
pub const CLASSIFIER_VERSION: &str = "youth_fit_v3";

/// The version stored with each verdict: [`CLASSIFIER_VERSION`] plus a
/// digest of every word list and threshold this module and
/// [`audience::classify`] read. Editing any of them changes the stored
/// version, so the worker recomputes every row with no manual step.
pub fn classifier_version() -> &'static str {
    static VERSION: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    VERSION.get_or_init(|| {
        let mut hasher = Sha256::new();
        for word in rule_words().iter().chain(audience::rule_words().iter()) {
            hasher.update(word.as_bytes());
            hasher.update([0x1f]);
        }
        let digest = format!("{:x}", hasher.finalize());
        format!("{CLASSIFIER_VERSION}_{}", &digest[..12])
    })
}

/// Every word list and threshold of this module, in a fixed order.
fn rule_words() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut list = |name: &str, words: &[&str]| {
        out.push(format!("#{name}"));
        out.extend(words.iter().map(|w| (*w).to_owned()));
    };
    list("raw_keys", RAW_KEYS);
    list("keep_sources", KEEP_SOURCES);
    list("extra_youth_terms", EXTRA_YOUTH_TERMS);
    list("pre_founder_words", PRE_FOUNDER_WORDS);
    list("general_public", GENERAL_PUBLIC);
    list("farm_flags", FARM_FLAGS);
    list("school_flags", SCHOOL_FLAGS);
    list("parent_flags", PARENT_FLAGS);
    list("group_flags", GROUP_FLAGS);
    list("other_groups", OTHER_GROUPS);
    list("personal_flags", PERSONAL_FLAGS);
    list("personal_benefit", PERSONAL_BENEFIT);
    list("person_cues", PERSON_CUES);
    list("org_subjects", ORG_SUBJECTS);
    list("facility_titles", FACILITY_TITLES);
    list("older_words", OLDER_WORDS);
    list("child_nouns", CHILD_NOUNS);
    list("not_child_title", NOT_CHILD_TITLE);
    list("adult_pupil_terms", ADULT_PUPIL_TERMS);
    list("adult_words", ADULT_WORDS);
    list("pregnancy_words", PREGNANCY_WORDS);
    list("disabled_or", DISABLED_OR);
    list("disabled_people", DISABLED_PEOPLE);
    list("not_eligible_after", NOT_ELIGIBLE_AFTER);
    list("veteran_words", VETERAN_WORDS);
    list("returning_farmer_words", RETURNING_FARMER_WORDS);
    list("occupational_disease", OCCUPATIONAL_DISEASE);
    list("title_only_farmer_words", TITLE_ONLY_FARMER_WORDS);
    list("general_people", GENERAL_PEOPLE);
    list("recipient_words", RECIPIENT_WORDS);
    list("adult_group_words", ADULT_GROUP_WORDS);
    list("not_residents", NOT_RESIDENTS);
    list("recipient_qualifiers", RECIPIENT_QUALIFIERS);
    list("not_residents_before", NOT_RESIDENTS_BEFORE);
    list(
        "self_sufficiency_participants",
        SELF_SUFFICIENCY_PARTICIPANTS,
    );
    out.push(format!(
        "{UNIVERSITY_FLAG};{DISABLED_FLAG};{NO_RESTRICTION_FLAG};{FAMILY_CARD};\
         {SELF_SUFFICIENCY};{SELF_SUFFICIENCY_FIRM};parent{PARENT_AGE};household{PARENT_HOUSEHOLD};\
         adult{ADULT_AGE};band{YOUTH_BAND_LOW}-{YOUTH_MAX_AGE};middle{MIDDLE_AGE};senior{SENIOR_AGE};\
         child{CHILD_MAX_AGE};adult_ceiling{ADULT_CEILING_MIN};range_cap{AGE_RANGE_CAP};age_cap{AGE_CAP};\
         field{MAX_FIELD_CHARS};target{TARGET_CHARS};summary{SUMMARY_CHARS};\
         window{BEFORE_WINDOW}-{AFTER_WINDOW}"
    ));
    out
}

/// Raw-payload keys the classifier reads. The worker selects only these,
/// so a recompute never pulls whole notices.
pub const RAW_KEYS: &[&str] = &[
    "지원대상",
    "선정기준",
    "사용자구분",
    "서비스분야",
    "supportConditions",
    "addAplyQlfcCndCn",
    "plcyAplyMthdCn",
    "trgterIndvdlNmArray",
    "trgterIndvdlArray",
    "lifeNmArray",
    "lifeArray",
];

/// Each raw text field is read up to this many characters.
const MAX_FIELD_CHARS: usize = 4000;
/// Target and summary lengths the recommender's audience check reads.
const TARGET_CHARS: usize = 2000;
const SUMMARY_CHARS: usize = 600;
/// Age thresholds. Each is part of [`classifier_version`].
/// The first adult age ("19세 이상" is an adult floor).
const ADULT_AGE: i64 = 19;
/// The youth band a ceiling or range keeps (K5): 20 to 39.
const YOUTH_BAND_LOW: i64 = 20;
const YOUTH_MAX_AGE: i64 = 39;
/// The first age past youth: floors from here on are 40+ rows (S3).
const MIDDLE_AGE: i64 = 40;
/// A floor this high is a senior row by itself (S3).
const SENIOR_AGE: i64 = 60;
/// A ceiling up to this age is a child row (S4b, S4c).
const CHILD_MAX_AGE: i64 = 18;
/// A pupil-flagged row whose ceiling is this or more reaches adults (S4a).
const ADULT_CEILING_MIN: i64 = 20;
/// Ages at or above these are not real bounds ("120세", "200세").
const AGE_RANGE_CAP: i64 = 100;
const AGE_CAP: i64 = 150;
/// Characters around "장애인" read for an exclusion before it and a
/// preference or organisation after it.
const BEFORE_WINDOW: usize = 12;
const AFTER_WINDOW: usize = 10;

/// What a stored verdict means for the catalogue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Verdict {
    /// For a young individual, or nothing says otherwise.
    Fit,
    /// Only an institution, firm, farm operator or older person applies.
    OffTarget,
    /// Only a 소상공인/사업자 applies: relevant to self-employed users.
    BusinessOnly,
    /// The beneficiary is a child (under 19).
    ChildOnly,
    /// For disabled people (alone or with seniors): shown to users whose
    /// profile has the disability flag.
    DisabilityOnly,
    /// For veterans and national-merit families: shown to users whose
    /// profile has the veteran-family flag.
    VeteranOnly,
}

impl Verdict {
    pub const ALL: [Self; 6] = [
        Self::Fit,
        Self::OffTarget,
        Self::BusinessOnly,
        Self::ChildOnly,
        Self::DisabilityOnly,
        Self::VeteranOnly,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fit => "fit",
            Self::OffTarget => "off_target",
            Self::BusinessOnly => "business_only",
            Self::ChildOnly => "child_only",
            Self::DisabilityOnly => "disability_only",
            Self::VeteranOnly => "veteran_only",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.as_str() == value)
    }

    pub fn demoted(self) -> bool {
        self != Self::Fit
    }

    /// The neutral card label of a demoted row. Hedged on purpose: the
    /// classifier is wrong for a few percent of the rows it demotes.
    pub fn label(self) -> Option<&'static str> {
        match self {
            Self::Fit => None,
            Self::OffTarget => Some("청년 대상이 아닐 수 있음"),
            Self::BusinessOnly => Some("사업자 대상일 수 있음"),
            Self::ChildOnly => Some("아동·청소년 대상일 수 있음"),
            Self::DisabilityOnly => Some("장애인 대상일 수 있음"),
            Self::VeteranOnly => Some("국가유공자·보훈 대상일 수 있음"),
        }
    }
}

/// Who a demoted row is for instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Target {
    Organisation,
    Business,
    FarmFishery,
    /// 40+ or senior only.
    Elder,
    Child,
    Disability,
    Veteran,
    /// A specific situation: occupational-disease patients and their
    /// families (S5).
    Other,
}

impl Target {
    pub const ALL: [Self; 8] = [
        Self::Organisation,
        Self::Business,
        Self::FarmFishery,
        Self::Elder,
        Self::Child,
        Self::Disability,
        Self::Veteran,
        Self::Other,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Organisation => "organisation",
            Self::Business => "business",
            Self::FarmFishery => "farm_fishery",
            Self::Elder => "elder",
            Self::Child => "child",
            Self::Disability => "disability",
            Self::Veteran => "veteran",
            Self::Other => "other",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.as_str() == value)
    }
}

/// A stored verdict with its stored target (a reviewed pair wins): what the
/// list and the card read of a row's youth_fit decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tier {
    pub verdict: Verdict,
    pub target: Option<Target>,
}

impl Tier {
    /// The stored columns; `None` when the verdict is not one.
    pub fn parse(verdict: &str, target: Option<&str>) -> Option<Self> {
        Some(Self {
            verdict: Verdict::parse(verdict)?,
            target: target.and_then(Target::parse),
        })
    }
}

/// One verdict with the rule that produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub verdict: Verdict,
    /// `None` exactly when the verdict is [`Verdict::Fit`].
    pub target: Option<Target>,
    /// Rule code: `K0`–`K6` or `none` for a kept row, `S1`, `S1b`,
    /// `S2`, `S3`, `S4a`, `S4b`, `D1`, `D2`.
    pub rule: &'static str,
    /// The value the rule read ("JA0110=65", "word=가축"), if any.
    pub evidence: Option<String>,
}

impl Decision {
    fn keep(rule: &'static str) -> Self {
        Self {
            verdict: Verdict::Fit,
            target: None,
            rule,
            evidence: None,
        }
    }

    fn demote(verdict: Verdict, target: Target, rule: &'static str, evidence: String) -> Self {
        Self {
            verdict,
            target: Some(target),
            rule,
            evidence: Some(evidence),
        }
    }
}

/// The stored columns and raw payload of one programme.
#[derive(Debug, Clone, Copy)]
pub struct ProgramFacts<'a> {
    pub source_type: &'a str,
    pub title: &'a str,
    pub summary: Option<&'a str>,
    pub min_age: Option<i32>,
    pub max_age: Option<i32>,
    /// `normalized_payload.youth_relevant` (a curator marked the row youth).
    pub curated_youth: bool,
    /// The raw payload, or any object holding at least its [`RAW_KEYS`].
    pub raw: &'a Value,
}

/// Sources whose every row is for students or young people (K0).
const KEEP_SOURCES: &[&str] = &[
    "scholarship",
    "university_official",
    "corporate_official",
    "fss_financial",
    "government_official",
];

/// Youth words kept in addition to [`YOUTH_TERMS`] (K1). 학교밖청소년
/// programmes reach 24-year-olds and are often typed org-only; 청춘 names
/// youth programmes ("28청춘창업소").
const EXTRA_YOUTH_TERMS: &[&str] = &["학교밖청소년", "청춘"];
/// Pre-founders are individuals (K3).
const PRE_FOUNDER_WORDS: &[&str] = &["예비창업", "창업팀"];

/// The target text names the general public (K2).
const GENERAL_PUBLIC: &[&str] = &[
    "누구나",
    "일반국민",
    "일반인",
    "주민모두",
    "전국민",
    "국민누구나",
    "모든주민",
    "모든국민",
    "구입자",
    "제한없음",
];

const FARM_FLAGS: &[&str] = &["JA0313", "JA0314", "JA0315", "JA0316"];
const SCHOOL_FLAGS: &[&str] = &["JA0317", "JA0318", "JA0319"];
const PARENT_FLAGS: &[&str] = &["JA0301", "JA0302", "JA0303"];
/// Disabled, veteran and patient flags: an age floor next to one of them is
/// one OR'd group of the band.
const GROUP_FLAGS: &[&str] = &["JA0328", "JA0329", "JA0330"];
/// University student (K4).
const UNIVERSITY_FLAG: &str = "JA0320";
/// Disabled person (D1, D2).
const DISABLED_FLAG: &str = "JA0328";
/// Title words naming veterans and national-merit families (V1): the
/// audience lexicon.
const VETERAN_WORDS: &[&str] = audience::VETERAN_TERMS;
/// Individual 귀농·귀촌 settlement and young-farmer benefits are youth
/// relevant (K7), unless an age floor above 39 applies.
const RETURNING_FARMER_WORDS: &[&str] = &["귀농", "귀촌", "청년농"];
/// Young-farmer words that keep a row only from its title (K7).
const TITLE_ONLY_FARMER_WORDS: &[&str] = &["후계농", "창업농"];
/// People who are not disabled, named next to a disability flag (D1) or a
/// child age ceiling (S4c): the row is not for that group alone. 주민 is
/// read by [`names_residents`] ("주민등록" is not a resident).
const GENERAL_PEOPLE: &[&str] = &["일반시민", "일반주민", "관심있는", "관심이있는"];
/// Benefit recipients: on a list item with no disability word (D1, flag
/// only), they are a group of their own ("65세 이상, 장애인, 수급자").
const RECIPIENT_WORDS: &[&str] = &["수급자", "수급권자"];
/// Words that tie recipients to a disability on the same list item.
/// ("수급자의 수선유지급여" names a benefit, "제외" an exclusion).
const RECIPIENT_QUALIFIERS: &[&str] = &["장애", "중", "으로", "수급자의", "제외"];
/// What "주민" heads when it does not name residents (주민등록, 주민센터).
const NOT_RESIDENTS: &[&str] = &["등록", "센터", "번호", "자치", "에게", "우선"];
/// What precedes "주민" when it names a group, not residents (북한이탈주민).
const NOT_RESIDENTS_BEFORE: &[&str] = &["이탈"];
/// Adult groups next to a child age ceiling (S4c): "노인(만60세이상)".
const ADULT_GROUP_WORDS: &[&str] = &["노인", "어르신"];
/// Occupational diseases of retired workers (S5): 진폐 (pneumoconiosis).
const OCCUPATIONAL_DISEASE: &[&str] = &["진폐"];
/// Groups young adults belong to that a disability row may also list
/// ("독거노인, 한부모, 장애인 세대"): such a row is not for disabled people
/// only, so it gets no disability tier.
const OTHER_GROUPS: &[&str] = &[
    "한부모",
    "부자가정",
    "모자가정",
    "조손",
    "소년소녀",
    "다자녀",
    "자녀",
    "임산부",
    "임신부",
    "영유아",
];
/// "해당사항없음": the flags do not restrict anyone.
const NO_RESTRICTION_FLAG: &str = "JA0322";
/// The personal `supportConditions` flags.
const PERSONAL_FLAGS: &[&str] = &[
    "JA0301", "JA0302", "JA0303", "JA0313", "JA0314", "JA0315", "JA0316", "JA0317", "JA0318",
    "JA0319", "JA0320", "JA0326", "JA0327", "JA0328", "JA0329", "JA0330",
];

/// A title naming a personal benefit keeps an org-only row (S1 exception).
const PERSONAL_BENEFIT: &[&str] = &[
    "장학",
    "학자금",
    "자녀",
    "교육비",
    "수당",
    "육영",
    "창조기업",
];
/// People an org-typed row still serves ("관내 주민", "저소득층"): with no
/// organisation named in the lead line, S1 keeps the row. 보조금24 types
/// some personal services 법인/시설/단체 (food banks, facility discounts).
const PERSON_CUES: &[&str] = &[
    "개인",
    "주민",
    "시민",
    "도민",
    "군민",
    "구민",
    "일반인",
    "청소년",
    "저소득",
    "취약계층",
    "수급자",
    "차상위",
    "이용자",
    "소비자",
    "누구나",
];
/// Titles about running a facility ("청소년 수련시설 운영 지원"): the
/// facility applies, so 청소년 there names its users, not the applicant.
const FACILITY_TITLES: &[&str] = &["운영"];
/// An organisation as the subject of the lead target line.
const ORG_SUBJECTS: &[&str] = &[
    "수련관",
    "수련시설",
    "문화의집",
    "개발원",
    "사업주",
    "기업",
    "법인",
    "단체",
    "시설",
    "기관",
    "업체",
    "사업자",
    "조합",
    "협회",
    "지자체",
    "지방자치단체",
    "시군구",
    "시·군·구",
    "학교",
    "어린이집",
    "센터",
    "경영체",
    "작목반",
    "사업장",
    "농가",
    "소상공인",
    "운영자",
    "공동체",
    "마을",
    "아파트",
    "상인",
    "시장",
];
/// Subject words of an age floor (S3).
const OLDER_WORDS: &[&str] = &[
    "어르신",
    "노인",
    "고령",
    "경로",
    "중장년",
    "신중년",
    "효도",
    "효행",
    "장수",
    "100세",
];
const CHILD_NOUNS: &[&str] = &[
    "아동",
    "어린이",
    "영유아",
    "유아",
    "초등",
    "중학",
    "고등",
    "학생",
    "청소년",
    "입학",
    "교복",
    "학교",
];
/// A child-band title that still faces a parent or a care leaver (S4b).
const NOT_CHILD_TITLE: &[&str] = &["자립", "퇴소", "보호종료", "양육", "부모", "대학"];
/// Target words that reach adults on a pupil-flagged row: a 학생증 holder
/// may be a university student, 전공과 is post-secondary special education.
const ADULT_PUPIL_TERMS: &[&str] = &["학생증", "전공과"];
/// 다둥이 행복카드 is held by the parents of a large family.
const FAMILY_CARD: &str = "다둥이";

/// `raw->>'key'`: a string as is, any other non-null value as JSON text,
/// cut to [`MAX_FIELD_CHARS`].
fn raw_text(raw: &Value, key: &str) -> Option<String> {
    let text = match raw.get(key)? {
        Value::Null => return None,
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    Some(text.chars().take(MAX_FIELD_CHARS).collect())
}

/// Python-`int()`-like reading of a `supportConditions` value.
fn int_value(value: Option<&Value>) -> Option<i64> {
    match value? {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f.trunc() as i64)),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn joined(parts: [Option<&str>; 2], limit: usize) -> Option<String> {
    let parts: Vec<&str> = parts.into_iter().flatten().collect();
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("\n").chars().take(limit).collect())
}

/// The text fields read from the raw payload.
struct Fields {
    target: Option<String>,
    selection: Option<String>,
    kinds: Option<String>,
    service_field: Option<String>,
    youth_extra: Option<String>,
    youth_method: Option<String>,
    tags: Option<String>,
    life: Option<String>,
}

impl Fields {
    fn read(source_type: &str, raw: &Value) -> Self {
        let gov = source_type == "gov_benefits";
        let welfare = matches!(source_type, "local_welfare" | "national_welfare");
        let youth = source_type == "youth_center";
        let pick = |on: bool, key: &str| if on { raw_text(raw, key) } else { None };
        Self {
            target: pick(gov, "지원대상"),
            selection: pick(gov, "선정기준"),
            kinds: pick(gov, "사용자구분"),
            service_field: pick(gov, "서비스분야"),
            youth_extra: pick(youth, "addAplyQlfcCndCn"),
            youth_method: pick(youth, "plcyAplyMthdCn"),
            tags: pick(welfare, "trgterIndvdlNmArray")
                .or_else(|| pick(welfare, "trgterIndvdlArray")),
            life: pick(welfare, "lifeNmArray").or_else(|| pick(welfare, "lifeArray")),
        }
    }
}

/// The recommender's youth verdict for the same row.
fn recommender_youth(p: &ProgramFacts, f: &Fields) -> bool {
    recommender_audience(p, f).youth
}

/// The recommender's audience reading of a row: the same text, cut the same
/// way as the API's `AUDIENCE_COLUMNS` (target 2,000, summary 600).
fn recommender_audience(p: &ProgramFacts, f: &Fields) -> audience::Audience {
    let target = match p.source_type {
        "gov_benefits" => joined([f.target.as_deref(), f.selection.as_deref()], TARGET_CHARS),
        "youth_center" => joined(
            [f.youth_extra.as_deref(), f.youth_method.as_deref()],
            TARGET_CHARS,
        ),
        _ => None,
    };
    let summary: Option<String> = p.summary.map(|s| s.chars().take(SUMMARY_CHARS).collect());
    audience::classify(&AudienceText {
        source_type: p.source_type,
        title: p.title,
        summary: summary.as_deref(),
        target: target.as_deref(),
        applicant_kinds: f.kinds.as_deref(),
        target_tags: f.tags.as_deref(),
        life_stages: f.life.as_deref(),
        min_age: p.min_age,
        max_age: p.max_age,
        curated_youth: p.curated_youth,
    })
}

/// [`audience::classify`] of a loaded programme, read as the list reads its
/// `AUDIENCE_COLUMNS`: what [`audience::membership`] takes on the assistant
/// card, where the row is loaded whole.
pub fn program_audience(program: &crate::models::Program) -> audience::Audience {
    let raw = program.raw_payload.clone().unwrap_or(Value::Null);
    let facts = ProgramFacts {
        source_type: &program.source_type,
        title: &program.title,
        summary: program.summary.as_deref(),
        min_age: program.min_age,
        max_age: program.max_age,
        curated_youth: program
            .normalized_payload
            .as_ref()
            .and_then(|p| p.get("youth_relevant"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
        raw: &raw,
    };
    let fields = Fields::read(facts.source_type, facts.raw);
    let mut audience = recommender_audience(&facts, &fields);
    audience.holder = program
        .holder_requirement
        .as_deref()
        .and_then(crate::holder::Holder::parse);
    audience.holder_unreadable =
        crate::holder::Holder::unreadable(program.holder_requirement.as_deref());
    audience
}

pub fn classify(p: &ProgramFacts) -> Decision {
    if KEEP_SOURCES.contains(&p.source_type) {
        return Decision::keep("K0");
    }
    let gov = p.source_type == "gov_benefits";
    let f = Fields::read(p.source_type, p.raw);
    let title = squash(p.title);
    let lead = squash(first_line(
        f.target
            .as_deref()
            .or(f.youth_extra.as_deref())
            .unwrap_or(""),
    ));
    let target = squash(f.target.as_deref().unwrap_or(""));
    let target_all = format!("{target}{}", squash(f.selection.as_deref().unwrap_or("")));
    // No individual can apply: demoted even when the text names youth.
    if gov {
        let summary: String = squash(p.summary.unwrap_or(""))
            .chars()
            .take(SUMMARY_CHARS)
            .collect();
        if let Some(decision) = applicant_rule(&f, &title, &lead, &target_all, &summary) {
            return decision;
        }
    }
    let youth_named = p.curated_youth
        || contains_any(&title, YOUTH_TERMS)
        || contains_any(&title, EXTRA_YOUTH_TERMS)
        || contains_any(&lead, YOUTH_TERMS)
        || contains_any(&lead, EXTRA_YOUTH_TERMS)
        || youth_age_span(&title)
        || youth_age_span(&lead);
    if youth_named || recommender_youth(p, &f) {
        return Decision::keep("K1");
    }
    if contains_any(&target_all, GENERAL_PUBLIC) {
        return Decision::keep("K2");
    }
    if contains_any(&target_all, PRE_FOUNDER_WORDS) {
        return Decision::keep("K3");
    }
    let conditions = Conditions::of(p);
    if gov && conditions.restricted() && conditions.flag(UNIVERSITY_FLAG) {
        return Decision::keep("K4");
    }
    if gov {
        let floor = int_value(conditions.get("JA0110"));
        let ceiling = int_value(conditions.get("JA0111"));
        if ceiling.is_some_and(|hi| {
            (YOUTH_BAND_LOW..=YOUTH_MAX_AGE).contains(&hi) && floor.is_none_or(|lo| lo <= hi)
        }) {
            return Decision::keep("K5");
        }
    }
    if youth_range(&target_all) {
        return Decision::keep("K5");
    }
    if (title.contains(SELF_SUFFICIENCY) && !title.contains(SELF_SUFFICIENCY_FIRM))
        || contains_any(&target_all, SELF_SUFFICIENCY_PARTICIPANTS)
    {
        return Decision::keep("K6");
    }
    let floor = gov
        .then(|| int_value(conditions.get("JA0110")))
        .flatten()
        .or(p.min_age.map(i64::from));
    // 귀농·귀촌 may be named anywhere; 후계농·창업농 only in the title
    // ("기본형 공익직불금" lists 후계농업경영인 among every farmer).
    if (contains_any(&format!("{title}{target_all}"), RETURNING_FARMER_WORDS)
        || contains_any(&title, TITLE_ONLY_FARMER_WORDS))
        && floor.is_none_or(|lo| lo <= YOUTH_MAX_AGE)
    {
        return Decision::keep("K7");
    }
    if gov {
        let on = conditions.on();
        let disabled_flag_only = conditions.restricted() && on.iter().all(|k| *k == DISABLED_FLAG);
        let titled = names_disability(&title);
        let raw_target = [f.target.as_deref(), f.selection.as_deref()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("\n");
        if (titled || disabled_flag_only)
            && !contains_any(&target_all, OTHER_GROUPS)
            && !names_general_people(&raw_target)
            && (titled || !recipients_listed_alone(&raw_target))
        {
            return Decision::demote(
                Verdict::DisabilityOnly,
                Target::Disability,
                "D1",
                if disabled_flag_only {
                    format!("flags={DISABLED_FLAG}")
                } else {
                    "title=장애인".to_owned()
                },
            );
        }
        // Title words only: the veteran flag alone also marks discount
        // lists that name 국가유공자 among many other groups.
        if let Some(word) = VETERAN_WORDS.iter().find(|w| title.contains(*w)) {
            if !contains_any(&target_all, OTHER_GROUPS) {
                return Decision::demote(
                    Verdict::VeteranOnly,
                    Target::Veteran,
                    "V1",
                    format!("word={word}"),
                );
            }
        }
        if contains_any(&format!("{title}{target_all}"), OCCUPATIONAL_DISEASE) {
            return Decision::demote(
                Verdict::OffTarget,
                Target::Other,
                "S5",
                "word=진폐".to_owned(),
            );
        }
        if let Some(decision) = structured(p, &f, &title, &lead, &target, &target_all) {
            return decision;
        }
    }
    Decision::keep("none")
}

/// S1: `사용자구분` names neither 개인 nor 가구, unless the title names a
/// personal benefit, the text names pre-founders, or it names people served
/// while the lead line names no organisation.
fn applicant_rule(
    f: &Fields,
    title: &str,
    lead: &str,
    target_all: &str,
    summary: &str,
) -> Option<Decision> {
    let kinds: Vec<&str> = f
        .kinds
        .as_deref()
        .unwrap_or("")
        .split("||")
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .collect();
    let facility = contains_any(title, FACILITY_TITLES) || contains_any(title, ORG_SUBJECTS);
    let cues: Vec<&str> = PERSON_CUES
        .iter()
        .copied()
        .filter(|cue| !(facility && *cue == "청소년"))
        .collect();
    let serves_people =
        contains_any(&format!("{target_all}{summary}"), &cues) && !contains_any(lead, ORG_SUBJECTS);
    if kinds.is_empty()
        || kinds.iter().any(|k| *k == "개인" || *k == "가구")
        || contains_any(title, PERSONAL_BENEFIT)
        || contains_any(target_all, PRE_FOUNDER_WORDS)
        || serves_people
    {
        return None;
    }
    let evidence = format!("사용자구분={}", kinds.join("||"));
    Some(if kinds.contains(&"소상공인") {
        Decision::demote(Verdict::BusinessOnly, Target::Business, "S1b", evidence)
    } else {
        Decision::demote(Verdict::OffTarget, Target::Organisation, "S1", evidence)
    })
}

/// Pregnant women share flu shots and other services with seniors (S3).
const PREGNANCY_WORDS: &[&str] = &["임신부", "임산부"];
/// 장애 joined to another group by a conjunction ("어르신 및 장애인",
/// "노인·장애인"): disabled people of any age qualify (D2).
const DISABLED_OR: &[&str] = &[
    "및장애",
    "또는장애",
    "·장애",
    "장애인및",
    "장애인또는",
    "장애인·",
];
/// Target words that put adults on a pupil-flagged row (S4): teachers, the
/// public, care leavers who finished school, missing disabled adults.
const ADULT_WORDS: &[&str] = &[
    "교원",
    "일반인",
    "졸업자",
    "실종장애인",
    "특수교사",
    "자립지원",
];

/// 자활 (self-sufficiency work) participants are low-income adults of any
/// age, young ones included (K6): a 자활 title that is not about 자활기업
/// (firms), or a target naming the participants.
const SELF_SUFFICIENCY: &str = "자활";
const SELF_SUFFICIENCY_FIRM: &str = "자활기업";
const SELF_SUFFICIENCY_PARTICIPANTS: &[&str] = &["자활근로", "자활참여", "자활사업참여"];

/// An age range whose upper end is a young adult age ("만 15~24세 청소년",
/// "9~24세"): the band reaches people of 20-39 (K5).
fn youth_range(text: &str) -> bool {
    ages(text).windows(2).any(|pair| {
        let ((_, _, between), (upper, _, tail)) = (pair[0], pair[1]);
        let joiner = between.strip_prefix('세').unwrap_or(between);
        let joiner = joiner
            .strip_prefix(['~', '∼', '-', '–'])
            .map(|rest| rest.strip_prefix('만').unwrap_or(rest));
        joiner == Some("")
            && (YOUTH_BAND_LOW..=YOUTH_MAX_AGE).contains(&upper)
            && tail.starts_with('세')
    })
}

/// The noun for disabled adults, the audience lexicon ("연작장애" names a
/// soil problem; 장애아, 장애학생 are children and do not lift a child-only
/// row).
const DISABLED_PEOPLE: &[&str] = audience::DISABILITY_TERMS;
/// Words that, right after the noun, make it a preference, an exclusion or
/// an organisation ("장애인 우선", "청각장애인 제외", "장애인체육회").
const NOT_ELIGIBLE_AFTER: &[&str] = &[
    "제외",
    "우선",
    "우대",
    "가점",
    "체육회",
    "단체",
    "협회",
    "시설",
    "기관",
    "복지관",
];

/// The text names disabled people as eligible (D1, D2).
fn names_disability(text: &str) -> bool {
    DISABLED_PEOPLE.iter().any(|noun| {
        text.match_indices(noun).any(|(at, _)| {
            let before: String = text[..at].chars().rev().take(BEFORE_WINDOW).collect();
            let after: String = text[at + noun.len()..].chars().take(AFTER_WINDOW).collect();
            !before.contains("외제") && !contains_any(&after, NOT_ELIGIBLE_AFTER)
        })
    })
}

/// Residents of an area ("관내 주민", "지역주민"), not "주민등록",
/// "주민센터", "주민번호" or "주민자치".
fn names_residents(text: &str) -> bool {
    text.match_indices("주민").any(|(at, word)| {
        let after = &text[at + word.len()..];
        !NOT_RESIDENTS.iter().any(|w| after.starts_with(w))
            && !NOT_RESIDENTS_BEFORE.iter().any(|w| text[..at].ends_with(w))
    })
}

/// Non-disabled people named as eligible (D1): the general public or anyone
/// interested anywhere ("일반시민 무료대여", "수어에 관심 있는 관내 주민"), or
/// residents on a list item that names no disability ("장애인 중 성북구 주민"
/// are disabled residents).
fn names_general_people(raw: &str) -> bool {
    contains_any(&squash(raw), GENERAL_PEOPLE)
        || list_items(raw)
            .iter()
            .any(|item| names_residents(item) && !item.contains("장애"))
}

/// A list item names recipients as a group of their own ("만 65세 이상인 자,
/// 장애인증서 소지자, 국민기초생활보장수급자"), not "등록장애인 중 수급자" or
/// "장애인(…)으로 수급자": no disability, "중" or "으로" on the item.
fn recipients_listed_alone(raw: &str) -> bool {
    list_items(raw).iter().any(|item| {
        contains_any(item, RECIPIENT_WORDS) && !contains_any(item, RECIPIENT_QUALIFIERS)
    })
}

/// Adults named next to a child ceiling (S4c): an adult age floor
/// ("만60세이상"), seniors or residents.
fn names_adults(text: &str) -> bool {
    contains_any(text, ADULT_GROUP_WORDS)
        || names_residents(text)
        || ages(text)
            .iter()
            .any(|(age, _, tail)| *age >= ADULT_AGE && *age < AGE_CAP && tail.starts_with("세이상"))
}

/// Each ASCII digit run of `text` (no whitespace) with its byte offset and
/// the text after it up to the next digit.
fn ages(text: &str) -> Vec<(i64, usize, &str)> {
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(start) = text[at..]
        .find(|c: char| c.is_ascii_digit())
        .map(|i| i + at)
    {
        let len = text[start..]
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(text.len() - start);
        let value = text[start..start + len].parse::<i64>().unwrap_or(i64::MAX);
        let tail_start = start + len;
        let tail_len = text[tail_start..]
            .find(|c: char| c.is_ascii_digit())
            .unwrap_or(text.len() - tail_start);
        out.push((value, start, &text[tail_start..tail_start + tail_len]));
        at = tail_start;
    }
    out
}

/// "N세" naming an age, not "N세대" (a household of N generations).
fn is_age(tail: &str) -> bool {
    tail.starts_with('세') && !tail.starts_with("세대")
}

/// An OR'd band that admits people under 40 (`text` has no whitespace):
/// an age under 40 ("6~23세 및 65세 이상", "만13세 이하 어린이"), or an upper
/// bound of 40 or more ("여자 만49세 이하") when no floor names an adult age.
fn young_band(text: &str) -> bool {
    let ages = ages(text);
    if ages
        .iter()
        .any(|(age, _, tail)| *age < MIDDLE_AGE && is_age(tail))
    {
        return true;
    }
    if ages
        .iter()
        .any(|(age, _, tail)| *age >= ADULT_AGE && tail.starts_with("세이상"))
    {
        return false;
    }
    ages.iter().any(|(age, start, tail)| {
        *age >= MIDDLE_AGE
            && (tail.starts_with("세이하") || tail.starts_with("세미만"))
            && !text[..*start].ends_with(['~', '∼', '-', '–'])
    })
}

/// A senior row that also serves people under 40 (S3).
fn serves_young(target_all: &str) -> bool {
    young_band(target_all) || contains_any(target_all, PREGNANCY_WORDS)
}

/// A pupil-flagged row whose target text reaches adults (S4): teachers, the
/// public, or an age range or bound at 19 or above ("13~19세", "9~24세").
fn reaches_adults(target: &str) -> bool {
    if contains_any(target, ADULT_WORDS) {
        return true;
    }
    let ages = ages(target);
    let range = ages.windows(2).any(|pair| {
        let ((_, _, between), (upper, _, tail)) = (pair[0], pair[1]);
        let joiner = between.strip_prefix('세').unwrap_or(between);
        let joiner = joiner
            .strip_prefix(['~', '∼', '-', '–'])
            .map(|rest| rest.strip_prefix('만').unwrap_or(rest));
        joiner == Some("") && (ADULT_AGE..AGE_RANGE_CAP).contains(&upper) && tail.starts_with('세')
    });
    range
        || ages.iter().any(|(age, _, tail)| {
            (ADULT_AGE..MIDDLE_AGE).contains(age)
                && (tail.starts_with("세이하") || tail.starts_with("세까지"))
        })
}

/// The `supportConditions` object of a 보조금24 row.
struct Conditions<'a>(Option<&'a Value>);

impl<'a> Conditions<'a> {
    fn of(p: &ProgramFacts<'a>) -> Self {
        Self(p.raw.get("supportConditions").filter(|c| c.is_object()))
    }

    fn get(&self, key: &str) -> Option<&'a Value> {
        self.0.and_then(|c| c.get(key))
    }

    fn flag(&self, key: &str) -> bool {
        self.get(key).is_some_and(|v| v.as_str() == Some("Y"))
    }

    /// The personal flags that are set.
    fn on(&self) -> Vec<&'static str> {
        PERSONAL_FLAGS
            .iter()
            .copied()
            .filter(|k| self.flag(k))
            .collect()
    }

    /// Some personal flag restricts the row (and 해당사항없음 is unset).
    fn restricted(&self) -> bool {
        !self.flag(NO_RESTRICTION_FLAG) && !self.on().is_empty()
    }
}

/// S2–S4 on 보조금24 fields.
fn structured(
    p: &ProgramFacts,
    f: &Fields,
    title: &str,
    lead: &str,
    target: &str,
    target_all: &str,
) -> Option<Decision> {
    let conditions = Conditions::of(p);
    let flag = |key: &str| conditions.flag(key);
    let on = conditions.on();
    let restricted = conditions.restricted();
    let only = |set: &[&str]| on.iter().all(|k| set.contains(k));

    // A 귀촌 household need not farm.
    if restricted && only(FARM_FLAGS) && !title.contains("귀촌") && !target_all.contains("귀촌인")
    {
        return Some(Decision::demote(
            Verdict::OffTarget,
            Target::FarmFishery,
            "S2",
            format!("flags={}", on.join(",")),
        ));
    }

    let floor = int_value(conditions.get("JA0110")).or(p.min_age.map(i64::from));
    let ceiling = int_value(conditions.get("JA0111")).or(p.max_age.map(i64::from));
    if let Some(floor) = floor.filter(|lo| (MIDDLE_AGE..=AGE_CAP).contains(lo)) {
        let or_group = on.len() <= 3 && on.iter().any(|k| GROUP_FLAGS.contains(k));
        // A floor of 60 or more is the subject by itself (치매 검진 60+).
        let subject = floor >= SENIOR_AGE
            || format!("{lead}{title}").contains(&format!("{floor}세"))
            || contains_any(&format!("{title}{lead}"), OLDER_WORDS);
        // A flag counts only on a short restricted list (not 해당사항없음).
        let disabled = (restricted && on.len() <= 3 && on.contains(&DISABLED_FLAG))
            || lead.contains("장애")
            || contains_any(target_all, DISABLED_OR)
            || names_disability(target_all);
        if subject
            && !serves_young(target_all)
            && !(disabled && contains_any(target_all, OTHER_GROUPS))
        {
            // "노인·장애인": seniors and disabled people share the
            // disability tier; another OR'd group keeps the row.
            if disabled {
                return Some(Decision::demote(
                    Verdict::DisabilityOnly,
                    Target::Disability,
                    "D2",
                    format!("JA0110={floor}"),
                ));
            }
            if !or_group {
                return Some(Decision::demote(
                    Verdict::OffTarget,
                    Target::Elder,
                    "S3",
                    format!("JA0110={floor}"),
                ));
            }
        }
    }

    let title_target = format!("{title}{target}");
    let family_card = format!("{title}{lead}").contains(FAMILY_CARD);
    // An age ceiling of 20 or more on pupil flags reaches adults (a 34-year
    // disabled student); 19 is the usual ceiling of a high-school band.
    let adult_ceiling = ceiling.is_some_and(|hi| (ADULT_CEILING_MIN..AGE_RANGE_CAP).contains(&hi));
    if restricted
        && only(SCHOOL_FLAGS)
        && !adult_ceiling
        && !title_target.contains("대학")
        && !contains_any(&title_target, ADULT_PUPIL_TERMS)
        && !reaches_adults(target)
        && !family_card
        && (contains_any(&title_target, CHILD_NOUNS) || title_target.contains("장학"))
    {
        return Some(Decision::demote(
            Verdict::ChildOnly,
            Target::Child,
            "S4a",
            format!("flags={}", on.join(",")),
        ));
    }
    if let Some(ceiling) = ceiling.filter(|hi| *hi <= CHILD_MAX_AGE) {
        let parent_facing = (!flag(NO_RESTRICTION_FLAG)
            && on.iter().any(|k| PARENT_FLAGS.contains(k)))
            || f.service_field.as_deref() == Some("임신·출산");
        if !parent_facing
            && !family_card
            && !contains_any(target, ADULT_PUPIL_TERMS)
            && !reaches_adults(target)
            && contains_any(title, CHILD_NOUNS)
            && !contains_any(title, NOT_CHILD_TITLE)
        {
            return Some(Decision::demote(
                Verdict::ChildOnly,
                Target::Child,
                "S4b",
                format!("JA0111={ceiling}"),
            ));
        }
        // A structured ceiling under 19 with no child noun ("아토피 치료비"):
        // still a child row unless it reaches parents or adults.
        if !parent_facing
            && !family_card
            && !contains_any(target, ADULT_PUPIL_TERMS)
            && !reaches_adults(target)
            && !contains_any(title, NOT_CHILD_TITLE)
            && !contains_any(&format!("{title}{target_all}"), PARENT_WORDS)
            && !names_adults(target_all)
        {
            return Some(Decision::demote(
                Verdict::ChildOnly,
                Target::Child,
                "S4c",
                format!("ceiling={ceiling}"),
            ));
        }
    }
    None
}

/// Words that put a parent, a pregnancy or a household on a row with a
/// child age ceiling (S4c): such a row is not for the child alone.
const PARENT_WORDS: &[&str] = &[
    "부모",
    "보호자",
    "임신",
    "임산부",
    "출산",
    "산모",
    "가정",
    "가구",
    "세대",
    "양육",
    "자녀를둔",
];

/// SHA-256 over everything [`classify`] reads. With the stored version it
/// decides whether a row needs recomputing.
pub fn input_hash(p: &ProgramFacts) -> String {
    let mut hasher = Sha256::new();
    let mut field = |value: &str| {
        hasher.update(value.as_bytes());
        hasher.update([0x1f]);
    };
    field(p.source_type);
    field(p.title);
    field(
        &p.summary
            .map(|s| s.chars().take(SUMMARY_CHARS).collect::<String>())
            .unwrap_or_default(),
    );
    field(&format!(
        "{:?}|{:?}|{}",
        p.min_age, p.max_age, p.curated_youth
    ));
    for key in RAW_KEYS {
        field(&raw_text(p.raw, key).unwrap_or_default());
    }
    format!("{:x}", hasher.finalize())
}

/// Why a recommendation list leaves a demoted row out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exclusion {
    /// No individual applies (institution, firm).
    NotIndividual,
    /// Reserved for farm operators, older people or children.
    TargetGroup,
}

/// The profile facts the recommendation gate reads.
#[derive(Debug, Clone, Copy, Default)]
pub struct GateProfile {
    pub self_employed: bool,
    /// The profile's disability flag; `None` = not answered.
    pub has_disability: Option<bool>,
    /// The profile's veteran-family flag; `None` = not answered.
    pub veteran_family: Option<bool>,
    /// Upper reading of the user's 만 나이; `None` = unknown.
    pub age: Option<i32>,
    /// The profile's household size; `None` = not answered.
    pub household_size: Option<i32>,
}

impl GateProfile {
    /// Profiles record no children: age [`PARENT_AGE`]+ or a household of
    /// [`PARENT_HOUSEHOLD`]+ stands in for "may be a parent".
    pub fn likely_parent(&self) -> bool {
        self.age.is_some_and(|age| age >= PARENT_AGE)
            || self
                .household_size
                .is_some_and(|size| size >= PARENT_HOUSEHOLD)
    }
}

/// From this age a user may be a parent (see [`GateProfile::likely_parent`]).
pub const PARENT_AGE: i32 = 25;
/// From this household size a user may be a parent.
pub const PARENT_HOUSEHOLD: i32 = 3;

/// Whether a recommendation list leaves out a row with this stored verdict.
/// A 소상공인-only row stays for a self-employed user, a disability or
/// veteran row unless the user answered that flag "no", a child-only row for a
/// user aged [`PARENT_AGE`] or more (young parents apply for their
/// children), and a row whose target the user asked for (`requested`, see
/// [`requested_targets`]) stays.
pub fn recommendation_exclusion(
    verdict: Verdict,
    target: Option<Target>,
    profile: GateProfile,
    requested: &[Target],
) -> Option<Exclusion> {
    if target.is_some_and(|t| requested.contains(&t)) {
        return None;
    }
    match verdict {
        Verdict::Fit => None,
        Verdict::BusinessOnly if profile.self_employed => None,
        Verdict::BusinessOnly => Some(Exclusion::NotIndividual),
        Verdict::OffTarget => match target {
            Some(Target::Organisation | Target::Business) => Some(Exclusion::NotIndividual),
            _ => Some(Exclusion::TargetGroup),
        },
        Verdict::ChildOnly if profile.likely_parent() => None,
        Verdict::ChildOnly => Some(Exclusion::TargetGroup),
        // An unanswered flag is unknown and never hides its tier.
        Verdict::DisabilityOnly if profile.has_disability != Some(false) => None,
        Verdict::DisabilityOnly => Some(Exclusion::TargetGroup),
        Verdict::VeteranOnly if profile.veteran_family != Some(false) => None,
        Verdict::VeteranOnly => Some(Exclusion::TargetGroup),
    }
}

/// Question words that clearly ask for one demoted target.
const REQUEST_WORDS: &[(Target, &[&str])] = &[
    (
        Target::FarmFishery,
        &[
            "농업", "농가", "농민", "농어", "축산", "가축", "어업", "어민", "어선", "수산", "양식",
            "양봉", "임업", "영농", "귀농", "귀어", "한우",
        ],
    ),
    (
        Target::Business,
        &[
            "소상공인",
            "자영업",
            "사업자",
            "중소기업",
            "가게",
            "점포",
            "사장님",
        ],
    ),
    (
        Target::Elder,
        &[
            "어르신",
            "노인",
            "부모님",
            "할머니",
            "할아버지",
            "고령",
            "경로",
            "중장년",
            "노후",
        ],
    ),
    (
        Target::Child,
        &[
            // Not a bare 아이 (아이디어) or 딸 (딸기).
            "우리아이",
            "아이교육",
            "아이돌봄",
            "아이양육",
            "아동",
            "자녀",
            "어린이",
            "영유아",
            "유아",
            "초등",
            "중학생",
            "고등학생",
            "육아",
            "학부모",
        ],
    ),
    (Target::Organisation, &["법인", "지자체"]),
    (Target::Disability, &["장애"]),
    (Target::Veteran, &["보훈", "유공자", "참전"]),
];

/// Demoted targets a free-text question clearly asks for ("축산 농가
/// 지원금", "아이 교육비"). Assistant search lists rows of these targets
/// after the others instead of leaving them out.
pub fn requested_targets(question: &str) -> Vec<Target> {
    let text = squash(question);
    REQUEST_WORDS
        .iter()
        .filter(|(_, words)| contains_any(&text, words))
        .map(|(target, _)| *target)
        .collect()
}

/// SQL predicate: the programme `id_expr` has a stored verdict demoted for
/// this viewer: the same exemptions as the recommendation gate
/// ([`recommendation_exclusion`] with no requested target), so a
/// self-employed viewer keeps business rows, a likely parent child rows, a
/// viewer who has not answered "no" to the disability or veteran flag those
/// rows. A reviewed verdict
/// wins over the computed one; a row with no stored verdict yet is not
/// demoted. Binds nothing.
pub fn demoted_sql(id_expr: &str, viewer: GateProfile) -> String {
    let shown: Vec<String> = Verdict::ALL
        .into_iter()
        .filter(|v| {
            // Off-target rows are never exempt, whatever their target.
            *v != Verdict::OffTarget && recommendation_exclusion(*v, None, viewer, &[]).is_none()
        })
        .map(|v| format!("'{}'", v.as_str()))
        .collect();
    format!(
        "EXISTS (SELECT 1 FROM program_youth_fit yf_d WHERE yf_d.program_id = {id_expr} \
         AND COALESCE(yf_d.reviewed_verdict, yf_d.verdict) NOT IN ({}))",
        shown.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Verify finding F10 (mutation a3): the assistant card's audience
    /// carries the row's holder requirement, so an SKT-paid offer is
    /// 확인 필요 asking 통신사 when unanswered and hidden for a KT answer.
    #[test]
    fn the_assistant_card_reads_the_holder_requirement() {
        use crate::audience::{membership, AudienceProfile};
        use crate::holder::{Holder, Holdings};
        let mut program = crate::structural::test_program("corporate_benefit");
        program.source_type = "corporate_official".into();
        program.title = "SK텔레콤 · 공차 10% 할인".into();
        program.holder_requirement = Some("telecom_carrier:skt".into());
        let audience = program_audience(&program);
        assert_eq!(audience.holder, Holder::parse("telecom_carrier:skt"));
        let unanswered = AudienceProfile {
            age: Some(24),
            ..AudienceProfile::default()
        };
        let m = membership(&audience, None, &unanswered);
        assert!(!m.hides());
        assert!(m
            .checks()
            .iter()
            .any(|c| c.missing_field == Some("telecom_carrier")));
        let kt = AudienceProfile {
            holdings: Holdings::from_answers(Some("kt"), None, None),
            ..unanswered
        };
        assert!(membership(&audience, None, &kt).hides());
    }

    fn gov(title: &str, raw: Value) -> Decision {
        classify(&ProgramFacts {
            source_type: "gov_benefits",
            title,
            summary: None,
            min_age: None,
            max_age: None,
            curated_youth: false,
            raw: &raw,
        })
    }

    #[test]
    fn verdict_and_target_codes_round_trip() {
        for v in Verdict::ALL {
            assert_eq!(Verdict::parse(v.as_str()), Some(v));
            assert_eq!(v.label().is_some(), v.demoted());
        }
        for t in Target::ALL {
            assert_eq!(Target::parse(t.as_str()), Some(t));
        }
        assert_eq!(Verdict::parse("off"), None);
    }

    #[test]
    fn structured_rules_fire_with_auditable_evidence() {
        let d = gov(
            "가축분뇨 처리 지원",
            json!({"사용자구분": "법인/시설/단체", "지원대상": "축산농가"}),
        );
        assert_eq!(
            (d.verdict, d.target, d.rule),
            (Verdict::OffTarget, Some(Target::Organisation), "S1")
        );
        assert_eq!(d.evidence.as_deref(), Some("사용자구분=법인/시설/단체"));

        let d = gov("경영 컨설팅", json!({"사용자구분": "소상공인"}));
        assert_eq!((d.verdict, d.rule), (Verdict::BusinessOnly, "S1b"));

        let d = gov(
            "벼 재배 지원",
            json!({"사용자구분": "개인", "supportConditions": {"JA0313": "Y"}}),
        );
        assert_eq!(
            (d.verdict, d.target, d.rule),
            (Verdict::OffTarget, Some(Target::FarmFishery), "S2")
        );

        let d = gov(
            "교통비 지원",
            json!({"사용자구분": "개인", "지원대상": "만 65세 이상 주민",
                   "supportConditions": {"JA0110": 65, "JA0111": 150}}),
        );
        assert_eq!((d.rule, d.evidence.as_deref()), ("S3", Some("JA0110=65")));

        let d = gov(
            "중학교 신입생 교복 지원",
            json!({"사용자구분": "개인", "supportConditions": {"JA0318": "Y", "JA0111": "15"}}),
        );
        assert_eq!((d.verdict, d.rule), (Verdict::ChildOnly, "S4a"));
    }

    #[test]
    fn keep_overrides_win_over_every_demotion_rule() {
        // A youth word in the title keeps a row an individual applies for.
        assert_eq!(
            gov(
                "청년 창업 지원",
                json!({"사용자구분": "개인||법인/시설/단체"})
            )
            .rule,
            "K1"
        );
        assert_eq!(
            gov(
                "지역화폐 할인",
                json!({"사용자구분": "소상공인", "지원대상": "구입자 누구나"})
            )
            .rule,
            "K2"
        );
        assert_eq!(
            gov(
                "창업 교육",
                json!({"사용자구분": "소상공인", "지원대상": "소상공인 및 예비창업자"})
            )
            .rule,
            "K3"
        );
        // A farming household that need not farm.
        assert_eq!(
            gov(
                "귀촌 가구 정착 지원",
                json!({"사용자구분": "개인", "supportConditions": {"JA0313": "Y"}})
            )
            .verdict,
            Verdict::Fit
        );
        // Disabled people OR'd with seniors share the disability tier.
        assert_eq!(
            gov(
                "장애인 또는 65세 이상 이동 지원",
                json!({"사용자구분": "개인",
                "지원대상": "장애인 또는 65세 이상",
                "supportConditions": {"JA0110": 65}})
            )
            .verdict,
            Verdict::DisabilityOnly
        );
        let scholarship = classify(&ProgramFacts {
            source_type: "scholarship",
            title: "축산 장학금",
            summary: None,
            min_age: None,
            max_age: None,
            curated_youth: false,
            raw: &json!({}),
        });
        assert_eq!(scholarship.rule, "K0");
    }

    #[test]
    fn titles_alone_never_demote() {
        let welfare = classify(&ProgramFacts {
            source_type: "local_welfare",
            title: "어르신 교통비",
            summary: None,
            min_age: None,
            max_age: None,
            curated_youth: false,
            raw: &json!({}),
        });
        assert_eq!((welfare.verdict, welfare.rule), (Verdict::Fit, "none"));
        assert_eq!(
            gov("임산부 친환경농산물 꾸러미", json!({"사용자구분": "개인"})).verdict,
            Verdict::Fit
        );
        assert_eq!(
            gov("중소기업 R&D 보증", json!({"사용자구분": "개인"})).verdict,
            Verdict::Fit
        );
    }

    #[test]
    fn senior_floor_with_another_or_band_is_kept_or_shared() {
        let floor = |target: &str| {
            gov(
                "어르신 지원",
                json!({"사용자구분": "개인", "지원대상": target, "supportConditions": {"JA0110": 65}}),
            )
            .verdict
        };
        assert_eq!(floor("○ 만 65세 이상 노인"), Verdict::OffTarget);
        assert_eq!(floor("○ 6~23세 및 65세 이상 시민"), Verdict::Fit);
        assert_eq!(floor("○ 65세 이상 어르신\n○ 임신부"), Verdict::Fit);
        assert_eq!(
            floor("○ 65세 이상 어르신 및 장애인"),
            Verdict::DisabilityOnly
        );
        // "3세대" is a household, not an age.
        assert_eq!(
            floor("○ 65세 이상 노인을 모시는 3세대 가정"),
            Verdict::OffTarget
        );
    }

    #[test]
    fn pupil_rows_that_reach_adults_are_kept() {
        let pupils = |target: &str, ceiling: i64| {
            gov(
                "학생 지원",
                json!({"사용자구분": "개인", "지원대상": target,
                       "supportConditions": {"JA0319": "Y", "JA0111": ceiling}}),
            )
            .verdict
        };
        assert_eq!(pupils("○ 고등학생", 19), Verdict::ChildOnly);
        assert_eq!(pupils("○ 13~19세 청소년", 120), Verdict::Fit);
        assert_eq!(pupils("○ 장애인 학생", 34), Verdict::Fit);
        assert_eq!(pupils("○ 학생 및 교원", 120), Verdict::Fit);
    }

    #[test]
    fn recommendation_gate_keeps_self_employed_parents_disabled_and_requested() {
        use Exclusion::*;
        let young = GateProfile {
            age: Some(22),
            ..GateProfile::default()
        };
        let parent = GateProfile {
            age: Some(31),
            ..GateProfile::default()
        };
        let owner = GateProfile {
            self_employed: true,
            age: Some(22),
            ..GateProfile::default()
        };
        let disabled = GateProfile {
            has_disability: Some(true),
            age: Some(22),
            ..GateProfile::default()
        };
        let answered_no = GateProfile {
            has_disability: Some(false),
            veteran_family: Some(false),
            age: Some(22),
            ..GateProfile::default()
        };
        let gate = |v, t, p, r: &[Target]| recommendation_exclusion(v, t, p, r);
        let org = Some(Target::Organisation);
        assert_eq!(gate(Verdict::Fit, None, young, &[]), None);
        assert_eq!(
            gate(Verdict::OffTarget, org, owner, &[]),
            Some(NotIndividual)
        );
        let farm = Some(Target::FarmFishery);
        assert_eq!(
            gate(Verdict::OffTarget, farm, young, &[]),
            Some(TargetGroup)
        );
        let biz = Some(Target::Business);
        assert_eq!(
            gate(Verdict::BusinessOnly, biz, young, &[]),
            Some(NotIndividual)
        );
        assert_eq!(gate(Verdict::BusinessOnly, biz, owner, &[]), None);
        let child = Some(Target::Child);
        assert_eq!(
            gate(Verdict::ChildOnly, child, young, &[]),
            Some(TargetGroup)
        );
        assert_eq!(
            gate(Verdict::ChildOnly, child, GateProfile::default(), &[]),
            Some(TargetGroup)
        );
        assert_eq!(gate(Verdict::ChildOnly, child, parent, &[]), None);
        assert_eq!(
            gate(Verdict::ChildOnly, child, young, &[Target::Child]),
            None
        );
        let dis = Some(Target::Disability);
        assert_eq!(
            gate(Verdict::DisabilityOnly, dis, answered_no, &[]),
            Some(TargetGroup)
        );
        assert_eq!(gate(Verdict::DisabilityOnly, dis, disabled, &[]), None);
        // Audit M5: an unanswered flag never hides its tier.
        assert_eq!(gate(Verdict::DisabilityOnly, dis, young, &[]), None);
        let vet = Some(Target::Veteran);
        assert_eq!(gate(Verdict::VeteranOnly, vet, young, &[]), None);
        assert_eq!(
            gate(Verdict::VeteranOnly, vet, answered_no, &[]),
            Some(TargetGroup)
        );
    }

    #[test]
    fn org_only_rows_stay_demoted_even_when_the_text_names_youth() {
        let d = gov(
            "학교밖청소년 직업역량강화 프로그램",
            json!({"사용자구분": "법인/시설/단체", "지원대상": "○ 만 15~24세 학교밖청소년"}),
        );
        assert_eq!((d.verdict, d.rule), (Verdict::OffTarget, "S1"));
        // The same youth terms keep a row an individual applies for.
        let d = gov(
            "학교밖청소년 직업역량강화 프로그램",
            json!({"사용자구분": "개인", "지원대상": "○ 학교밖청소년"}),
        );
        assert_eq!(d.rule, "K1");
        // Pre-founders and people served are individuals.
        assert_eq!(
            gov(
                "창업 교육",
                json!({"사용자구분": "소상공인", "지원대상": "소상공인 및 예비창업자"})
            )
            .verdict,
            Verdict::Fit
        );
    }

    #[test]
    fn youth_bands_and_self_sufficiency_keep_individual_rows() {
        let rule = |title: &str, raw: Value| gov(title, raw).rule;
        assert_eq!(
            rule(
                "상담 지원",
                json!({"사용자구분": "개인", "지원대상": "○ 만 9~24세 청소년"})
            ),
            "K5"
        );
        assert_eq!(
            rule(
                "직업 훈련",
                json!({"사용자구분": "개인",
                "supportConditions": {"JA0110": 15, "JA0111": 24}})
            ),
            "K5"
        );
        assert_eq!(rule("자활지원", json!({"사용자구분": "개인"})), "K6");
        assert_eq!(
            rule(
                "근로 지원",
                json!({"사용자구분": "개인", "지원대상": "○ 자활근로 참여자"})
            ),
            "K6"
        );
    }

    #[test]
    fn disabled_targets_get_the_disability_tier() {
        let tier = |title: &str, raw: Value| {
            let d = gov(title, raw);
            (d.verdict, d.rule)
        };
        assert_eq!(
            tier("장애인 보조기기 지원", json!({"사용자구분": "개인"})),
            (Verdict::DisabilityOnly, "D1")
        );
        assert_eq!(
            tier(
                "보조기기 지원",
                json!({"사용자구분": "개인", "supportConditions": {"JA0328": "Y"}})
            ),
            (Verdict::DisabilityOnly, "D1")
        );
        // Seniors and disabled people together: disability tier, not off.
        assert_eq!(
            tier(
                "의치 보철",
                json!({"사용자구분": "개인", "지원대상": "○ 65세 이상 노인 및 장애인",
                "supportConditions": {"JA0110": 65}})
            ),
            (Verdict::DisabilityOnly, "D2")
        );
        // An excluded group is not an eligible one.
        assert_eq!(
            tier(
                "보청기 지원",
                json!({"사용자구분": "개인", "지원대상": "○ 70세 이상 노인\n- 청각장애인 제외",
                "supportConditions": {"JA0110": 70}})
            ),
            (Verdict::OffTarget, "S3")
        );
        // An organisation-only disability row is still off-target.
        assert_eq!(
            tier(
                "장애인기업 판로 지원",
                json!({"사용자구분": "법인/시설/단체"})
            ),
            (Verdict::OffTarget, "S1")
        );
    }

    /// Every numeric threshold is part of the stored version: changing one
    /// recomputes every row.
    #[test]
    fn rule_words_carry_every_threshold() {
        let words = rule_words().join(";");
        for needle in [
            format!("adult{ADULT_AGE}"),
            format!("band{YOUTH_BAND_LOW}-{YOUTH_MAX_AGE}"),
            format!("middle{MIDDLE_AGE}"),
            format!("senior{SENIOR_AGE}"),
            format!("child{CHILD_MAX_AGE}"),
            format!("adult_ceiling{ADULT_CEILING_MIN}"),
            format!("range_cap{AGE_RANGE_CAP}"),
            format!("age_cap{AGE_CAP}"),
            format!("field{MAX_FIELD_CHARS}"),
            format!("target{TARGET_CHARS}"),
            format!("summary{SUMMARY_CHARS}"),
            format!("window{BEFORE_WINDOW}-{AFTER_WINDOW}"),
            format!("parent{PARENT_AGE}"),
            format!("household{PARENT_HOUSEHOLD}"),
        ] {
            assert!(words.contains(&needle), "{needle} missing");
        }
    }

    #[test]
    fn requested_targets_read_clear_question_words_only() {
        assert_eq!(
            requested_targets("축산 농가 지원금 알려줘"),
            vec![Target::FarmFishery]
        );
        assert_eq!(
            requested_targets("우리 아이 교육비 지원"),
            vec![Target::Child]
        );
        assert_eq!(requested_targets("소상공인 대출"), vec![Target::Business]);
        assert!(requested_targets("부산 사는 휴학생 장학금").is_empty());
        assert!(requested_targets("청년 월세 지원").is_empty());
        assert!(requested_targets("딸기 아이디어 공모").is_empty());
    }

    #[test]
    fn input_hash_tracks_inputs_not_unread_fields() {
        let facts = |raw: &Value| {
            input_hash(&ProgramFacts {
                source_type: "gov_benefits",
                title: "t",
                summary: None,
                min_age: None,
                max_age: None,
                curated_youth: false,
                raw,
            })
        };
        let base = facts(&json!({"지원대상": "a"}));
        assert_eq!(base, facts(&json!({"지원대상": "a", "지원내용": "unread"})));
        assert_ne!(base, facts(&json!({"지원대상": "b"})));
        assert_eq!(base.len(), 64);
    }

    #[test]
    fn demoted_predicate_prefers_the_reviewed_verdict() {
        let answered_no = GateProfile {
            has_disability: Some(false),
            veteran_family: Some(false),
            ..GateProfile::default()
        };
        let sql = demoted_sql("programs.id", answered_no);
        assert!(sql.contains("yf_d.program_id = programs.id"));
        assert!(sql.contains("COALESCE(yf_d.reviewed_verdict, yf_d.verdict) NOT IN ('fit')"));
        assert!(!sql.contains('$'));
        let disabled = GateProfile {
            has_disability: Some(true),
            ..answered_no
        };
        assert!(demoted_sql("p.id", disabled).contains("NOT IN ('fit', 'disability_only')"));
        // Audit M5: unanswered flags keep both tiers in the list.
        assert!(demoted_sql("p.id", GateProfile::default())
            .contains("NOT IN ('fit', 'disability_only', 'veteran_only')"));
    }
}
