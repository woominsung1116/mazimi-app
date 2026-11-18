//! Catalogue category axes (v1 feedback F5.1, F5.2).
//!
//! The rules live in SQL (migration 20261023000002: `program_provider_category`,
//! `program_provider_categories`, `program_field_category`), stored per row
//! as generated columns of `programs` (indexed), so the list, its filters,
//! the detail page and the recommendation list read one answer without
//! computing it per request. This module holds the codes, their Korean
//! labels and the SQL fragments that read those columns.
use serde::{Deserialize, Serialize};

/// Who offers the programme.
pub const PROVIDERS: [(&str, &str); 3] = [
    ("policy", "정책"),
    ("scholarship", "장학금"),
    ("corporate", "기업혜택"),
];

/// What the programme is about.
pub const FIELDS: [(&str, &str); 6] = [
    ("housing", "주거"),
    ("living", "생활"),
    ("education", "교육"),
    ("welfare", "복지"),
    ("finance", "금융"),
    ("employment", "취업"),
];

/// A filter value: a code or its Korean label, trimmed. `None` = unknown.
fn parse(table: &[(&'static str, &'static str)], value: &str) -> Option<&'static str> {
    let value = value.trim();
    table
        .iter()
        .find(|(code, label)| code.eq_ignore_ascii_case(value) || *label == value)
        .map(|(code, _)| *code)
}

pub fn parse_provider(value: &str) -> Option<&'static str> {
    parse(&PROVIDERS, value)
}

pub fn parse_field(value: &str) -> Option<&'static str> {
    parse(&FIELDS, value)
}

fn label(table: &[(&'static str, &'static str)], code: &str) -> Option<&'static str> {
    table.iter().find(|(c, _)| *c == code).map(|(_, l)| *l)
}

/// The category fields served on list items, the detail page and
/// recommendation items.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Categories {
    #[serde(default)]
    pub provider_category: Option<String>,
    #[serde(default)]
    pub provider_label: Option<String>,
    /// Every provider tab the row is listed under (a corporate scholarship
    /// is under both 장학금 and 기업혜택).
    #[serde(default)]
    pub provider_categories: Vec<String>,
    #[serde(default)]
    pub field_category: Option<String>,
    #[serde(default)]
    pub field_label: Option<String>,
    /// The stored 기업혜택 chip ([`crate::corporate_categories`],
    /// `programs.browse_category`) of a row on the 기업혜택 tab; null on every
    /// other row. The app picks its category glyph and colours from it.
    #[serde(default)]
    pub corporate_category: Option<String>,
}

impl Categories {
    /// From the three SQL columns of [`columns_sql`].
    pub fn new(provider: Option<String>, providers: Vec<String>, field: Option<String>) -> Self {
        Self {
            provider_label: provider
                .as_deref()
                .and_then(|c| label(&PROVIDERS, c))
                .map(str::to_owned),
            field_label: field
                .as_deref()
                .and_then(|c| label(&FIELDS, c))
                .map(str::to_owned),
            provider_category: provider,
            provider_categories: providers,
            field_category: field,
            corporate_category: None,
        }
    }
}

fn prefix(alias: &str) -> String {
    if alias.is_empty() {
        String::new()
    } else {
        format!("{alias}.")
    }
}

/// The stored primary provider column of the row `alias` (empty =
/// unqualified).
pub fn provider_sql(alias: &str) -> String {
    format!("{}provider_category", prefix(alias))
}

/// The stored provider tabs (`text[]`, GIN-indexed) of the row `alias`.
pub fn providers_sql(alias: &str) -> String {
    format!("{}provider_categories", prefix(alias))
}

/// Rows of the 기업혜택 tab (the provider tab `corporate`) of the row
/// `alias`: company offers only. A school's own offer ([`campus_offer_sql`])
/// and a company's financial product (금융) have no provider tab (migration
/// 20261030000001). The list's `provider=corporate`, the chip counts, the
/// browse refresh and the admin metric all mean this.
pub fn corporate_tab_sql(alias: &str) -> String {
    format!("{} @> ARRAY['corporate']::text[]", providers_sql(alias))
}

/// A school's own offer (`program_type = campus_benefit`) of the row
/// `alias`. It is for one school's students, so the shared catalogue
/// ([`catalogue_sql`]) never lists it; only the school-scoped 대학 tab does
/// ([`school_offer_sql`]).
pub fn campus_offer_sql(alias: &str) -> String {
    format!("{}program_type = 'campus_benefit'", prefix(alias))
}

/// Rows the shared catalogue lists to any viewer (전체, the provider and
/// field tabs, the 기업혜택 chips and their counts): every row but a school's
/// own offer.
pub fn catalogue_sql(alias: &str) -> String {
    format!("NOT ({})", campus_offer_sql(alias))
}

/// A school's own offer for the school bound at `school_param` (a
/// `universities.id`, e.g. `$1`) of the row `alias`.
pub fn school_offer_sql(alias: &str, school_param: &str) -> String {
    format!(
        "({} AND {}university_id = {school_param})",
        campus_offer_sql(alias),
        prefix(alias)
    )
}

/// The stored field column (indexed) of the row `alias`.
pub fn field_sql(alias: &str) -> String {
    format!("{}field_category", prefix(alias))
}

/// The category columns, named as [`CategoryRow`] reads them. The stored
/// 기업혜택 chip is read only for a row on the tab: a row that left it keeps
/// its column until the next sweep clears it.
pub fn columns_sql(alias: &str) -> String {
    format!(
        "{} AS provider_category, {} AS provider_categories, {} AS field_category, \
         CASE WHEN {} THEN {}browse_category END AS corporate_category",
        provider_sql(alias),
        providers_sql(alias),
        field_sql(alias),
        corporate_tab_sql(alias),
        prefix(alias)
    )
}

/// The columns of [`columns_sql`], for `#[sqlx(flatten)]`.
#[derive(Debug, Clone, Default, sqlx::FromRow)]
pub struct CategoryRow {
    pub provider_category: Option<String>,
    pub provider_categories: Option<Vec<String>>,
    pub field_category: Option<String>,
    pub corporate_category: Option<String>,
}

impl From<CategoryRow> for Categories {
    fn from(row: CategoryRow) -> Self {
        Categories {
            // Only a known chip code, as the chip counts serve it.
            corporate_category: row
                .corporate_category
                .as_deref()
                .and_then(crate::corporate_categories::parse)
                .map(str::to_owned),
            ..Categories::new(
                row.provider_category,
                row.provider_categories.unwrap_or_default(),
                row.field_category,
            )
        }
    }
}

/// Words that name a field in a user's request or in a programme title.
/// Matched inside words (`월세지원`, `주거비`), except entries starting with
/// `=`, which match only as a whole word or with a particle (`집`, `집이`,
/// but not `모집`, `집중`). An entry `a!b` matches `a` unless the word
/// contains `b` (`생활!생활관`: a dormitory is housing, not living).
const FIELD_CUES: &[(&str, &[&str])] = &[
    (
        "housing",
        &[
            "월세",
            "전세",
            "전월세",
            "주거",
            "보증금",
            "임대",
            "임차",
            "기숙사",
            "생활관",
            "자취",
            "원룸",
            "고시원",
            "셰어하우스",
            "주택",
            "집세",
            "방값",
            "이사비",
            "이삿짐",
            "=이사",
            "=집",
            "=방",
        ],
    ),
    (
        "employment",
        &[
            "일자리",
            "취업",
            "취준",
            "구직",
            "면접",
            "인턴",
            "채용",
            "직업",
            "일경험",
            "창업",
            "고용",
            "근로",
            "알바",
            "아르바이트",
            "자격증",
            "응시료",
            "내일배움",
            "직무",
        ],
    ),
    (
        "education",
        &[
            "장학",
            "학비",
            "학자금",
            "등록금",
            "교육비",
            "수업료",
            "학원",
            "강의",
            "어학",
            "토익",
            "교재",
            "학습",
            "=교육",
            "=공부",
        ],
    ),
    (
        "finance",
        &[
            "대출",
            "적금",
            "저축",
            "예금",
            "통장!이통장",
            "이자",
            "금리",
            "자산",
            "목돈",
            "금융",
            "융자",
            "신용",
            "채무",
            "도약계좌",
            "청약",
            "=빚",
        ],
    ),
    (
        "living",
        &[
            "생활비",
            "교통비",
            "교통",
            "통신비",
            "식비",
            "문화생활",
            "문화비",
            "문화패스",
            "=문화",
            "여가",
            "공연",
            "여행",
            "체육",
            "=운동",
            "헬스",
            "전시",
            "도서관",
            "패스",
            "버스",
            "지하철",
            "생활!생활관",
            "=밥",
        ],
    ),
    (
        "welfare",
        &[
            "복지",
            "의료",
            "병원",
            "건강",
            "치료",
            "심리",
            "심리상담",
            "마음건강",
            "=상담",
            "우울",
            "출산",
            "임신",
            "육아",
            "돌봄",
            "장애",
            "검진",
            "치과",
            "=아이",
        ],
    ),
];

/// Particles that may follow a whole-word cue (`집이`, `방을`, `교육도`).
const CUE_PARTICLES: &[&str] = &[
    "", "이", "가", "을", "를", "은", "는", "도", "만", "에", "의", "값", "세",
];

fn cue_in_word(cue: &str, word: &str) -> bool {
    if let Some(whole) = cue.strip_prefix('=') {
        return word
            .strip_prefix(whole)
            .is_some_and(|rest| CUE_PARTICLES.contains(&rest));
    }
    match cue.split_once('!') {
        Some((cue, unless)) => word.contains(cue) && !word.contains(unless),
        None => word.contains(cue),
    }
}

fn words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
}

/// The cue words of `field` that `text` names, in table order.
pub fn field_cues_in(field: &str, text: &str) -> Vec<&'static str> {
    let Some((_, cues)) = FIELD_CUES.iter().find(|(f, _)| *f == field) else {
        return Vec::new();
    };
    let words: Vec<String> = words(text).collect();
    cues.iter()
        .filter(|cue| words.iter().any(|w| cue_in_word(cue, w)))
        .map(|cue| {
            cue.trim_start_matches('=')
                .split_once('!')
                .map_or(cue.trim_start_matches('='), |(c, _)| c)
        })
        .collect()
}

/// The fields a request names, in [`FIELDS`] order. A loan word makes a
/// request finance as well ("학자금 대출", "전세 대출"), as the catalogue
/// files those rows.
pub fn fields_named(text: &str) -> Vec<&'static str> {
    FIELDS
        .iter()
        .map(|(code, _)| *code)
        .filter(|code| !field_cues_in(code, text).is_empty())
        .collect()
}

/// Korean label of a field or provider code.
pub fn field_label(code: &str) -> Option<&'static str> {
    label(&FIELDS, code)
}

pub fn provider_label(code: &str) -> Option<&'static str> {
    label(&PROVIDERS, code)
}

/// Fields to offer when a request's field has nothing: the closest first.
pub fn nearest_fields(field: &str) -> &'static [&'static str] {
    match field {
        "housing" => &["finance", "living", "welfare"],
        "employment" => &["education", "finance", "living"],
        "education" => &["employment", "living", "finance"],
        "finance" => &["housing", "employment", "living"],
        "living" => &["welfare", "housing", "education"],
        "welfare" => &["living", "housing", "employment"],
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_name_fields() {
        for (text, fields) in [
            (
                "부산 사는 대학생인데 월세 지원 받을 수 있는 거 있을까?",
                &["housing"][..],
            ),
            ("전세 보증금 도와주는 거", &["housing"]),
            ("집 구하는데 도움 되는 혜택", &["housing"]),
            ("기숙사비 지원", &["housing"]),
            ("모집 중인 장학금", &["education"]),
            ("학자금 대출 이자 지원", &["education", "finance"]),
            ("취업 준비할 때 받을 수 있는 거", &["employment"]),
            ("면접 정장 대여", &["employment"]),
            ("청년 적금 있어?", &["finance"]),
            ("교통비 지원", &["living"]),
            ("심리 상담 받을 수 있는 곳", &["welfare"]),
            ("심리상담 지원", &["welfare"]),
            ("대학생 연합생활관", &["housing"]),
            ("생활비 지원", &["living"]),
            ("뭐 받을 수 있어?", &[]),
            ("집중 지원", &[]),
            ("아이디 찾기", &[]),
            ("이사장 추천 장학", &["education"]),
            ("이통장 회의", &[]),
            ("도서지역 지원", &[]),
            ("이사 도와주는 혜택", &["housing"]),
            // Code review L5: a word inside another word adds no field.
            ("마음에 드는 장학금", &["education"]),
            ("운동화 할인", &[]),
            ("문화 지원", &["living"]),
            ("상담 받을 수 있는 곳", &["welfare"]),
        ] {
            assert_eq!(fields_named(text), fields, "{text}");
        }
        assert_eq!(
            field_cues_in("housing", "청년 월세 및 보증금 지원"),
            ["월세", "보증금"]
        );
        assert_eq!(field_cues_in("living", "생활관 입사"), Vec::<&str>::new());
    }

    #[test]
    fn filters_take_codes_or_korean_labels() {
        assert_eq!(parse_provider("scholarship"), Some("scholarship"));
        assert_eq!(parse_provider(" 장학금 "), Some("scholarship"));
        assert_eq!(parse_provider("기업혜택"), Some("corporate"));
        assert_eq!(parse_provider("POLICY"), Some("policy"));
        assert_eq!(parse_provider("government"), None);
        assert_eq!(parse_field("교육"), Some("education"));
        assert_eq!(parse_field("employment"), Some("employment"));
        assert_eq!(parse_field("취업"), Some("employment"));
        assert_eq!(parse_field("benefit"), None);
        assert_eq!(parse_field(""), None);
    }

    #[test]
    fn labels_follow_codes() {
        let c = Categories::new(
            Some("scholarship".into()),
            vec!["scholarship".into(), "corporate".into()],
            Some("education".into()),
        );
        assert_eq!(c.provider_label.as_deref(), Some("장학금"));
        assert_eq!(c.field_label.as_deref(), Some("교육"));
        assert_eq!(Categories::new(None, vec![], None), Categories::default());
    }

    #[test]
    fn sql_fragments_qualify_the_alias() {
        assert_eq!(provider_sql("p"), "p.provider_category");
        assert_eq!(
            corporate_tab_sql("programs"),
            "programs.provider_categories @> ARRAY['corporate']::text[]"
        );
        assert_eq!(providers_sql(""), "provider_categories");
        assert_eq!(
            catalogue_sql("programs"),
            "NOT (programs.program_type = 'campus_benefit')"
        );
        assert_eq!(
            school_offer_sql("p", "$1"),
            "(p.program_type = 'campus_benefit' AND p.university_id = $1)"
        );
        assert_eq!(
            columns_sql("programs"),
            "programs.provider_category AS provider_category, \
             programs.provider_categories AS provider_categories, \
             programs.field_category AS field_category, \
             CASE WHEN programs.provider_categories @> ARRAY['corporate']::text[] \
             THEN programs.browse_category END AS corporate_category"
        );
    }

    #[test]
    fn a_stored_chip_is_served_only_as_a_known_code() {
        let row = |chip: &str| CategoryRow {
            provider_category: Some("corporate".into()),
            provider_categories: Some(vec!["corporate".into()]),
            field_category: Some("living".into()),
            corporate_category: Some(chip.into()),
        };
        assert_eq!(
            Categories::from(row("food")).corporate_category.as_deref(),
            Some("food")
        );
        assert_eq!(
            Categories::from(row("other")).corporate_category.as_deref(),
            Some("other")
        );
        assert_eq!(Categories::from(row("nonsense")).corporate_category, None);
        assert_eq!(
            Categories::from(CategoryRow::default()).corporate_category,
            None
        );
    }
}
