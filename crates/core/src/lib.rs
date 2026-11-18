pub mod agent;
pub mod applicant_review;
pub mod application_procedure;
pub mod application_resources;
pub mod application_workflow;
pub mod audience;
pub mod calendar;
pub mod categories;
pub mod condition_review;
pub mod config;
pub mod corporate_categories;
pub mod database;
pub mod display_text;
pub mod districts;
pub mod documents;
pub mod education;
pub mod form_templates;
pub mod holder;
pub mod income;
pub mod models;
pub mod notice_dates;
pub mod official_page;
pub mod rule_engine;
pub mod sensitive_digits;
pub mod source_policy;
pub mod structural;
pub mod text_rules;
pub mod title_display;
pub mod youth_fit;

pub use config::AppConfig;
pub use database::{connect_database, DatabaseConfig, PoolRole};

/// Canonical comparison key for Korean/Latin brand names.
///
/// We intentionally keep every Unicode letter and number while removing
/// whitespace and punctuation. This makes values such as `"A-B 카페"` and
/// `"ab카페"` comparable without destroying Hangul.
pub fn normalize_brand_name(name: &str) -> String {
    name.chars()
        .flat_map(char::to_lowercase)
        .filter(|c| c.is_alphanumeric())
        .collect()
}

/// The residence reason's stem, the one place for both writers and the
/// reader: the rule engine writes "대구 거주 조건 맞음", the recommendation's
/// inline score "대구 거주 조건에 맞아요.", and the agent's search drops either
/// for a region the user has not saved (re-review 6 H2).
pub const RESIDENCE_REASON_STEM: &str = " 거주 조건";

/// A residence reason in either wording ("{region} 거주 조건 맞음", "{region}
/// 거주 조건에 맞아요.").
pub fn is_residence_reason(reason: &str) -> bool {
    reason
        .split_once(RESIDENCE_REASON_STEM)
        .is_some_and(|(region, rest)| {
            !region.is_empty() && (rest == " 맞음" || rest == "에 맞아요.")
        })
}

/// The enrollment reason's stem: the rule engine writes "재학 상태 조건 맞음
/// (휴학)" for any status.
pub const ENROLLMENT_REASON_STEM: &str = "재학 상태 조건 맞음";

/// Whether `reason` states that the profile's `field` met a condition
/// (`region_code` or `enrollment_status`); `false` for other fields.
pub fn is_reason_for(field: &str, reason: &str) -> bool {
    match field {
        "region_code" => is_residence_reason(reason),
        "enrollment_status" => reason.starts_with(ENROLLMENT_REASON_STEM),
        _ => false,
    }
}

/// Human-readable Korean label for a region code.
/// Handles both raw codes and JSON-quoted codes (e.g. `"\"busan\""` from `Value::to_string()`).
pub fn region_label(code: &str) -> &'static str {
    // Strip surrounding JSON quotes if present
    let stripped = code.trim_matches('"');
    match stripped {
        "busan" => "부산",
        "daegu" => "대구",
        "seoul" => "서울",
        "incheon" => "인천",
        "gwangju" => "광주",
        "daejeon" => "대전",
        "ulsan" => "울산",
        "sejong" => "세종",
        "gyeonggi" => "경기",
        "gangwon" => "강원",
        "chungbuk" => "충북",
        "chungnam" => "충남",
        "jeonbuk" => "전북",
        "jeonnam" => "전남",
        "gyeongbuk" => "경북",
        "gyeongnam" => "경남",
        "jeju" => "제주",
        _ => "기타",
    }
}

/// Convert Korean province/metropolitan-city labels and existing Latin codes
/// to the canonical region keys used by profiles and recommendation filters.
pub fn canonical_region_code(value: &str) -> Option<&'static str> {
    match value.trim().trim_matches('"').to_ascii_lowercase().as_str() {
        "seoul" | "서울" | "서울시" | "서울특별시" => Some("seoul"),
        "busan" | "부산" | "부산시" | "부산광역시" => Some("busan"),
        "daegu" | "대구" | "대구시" | "대구광역시" => Some("daegu"),
        "incheon" | "인천" | "인천시" | "인천광역시" => Some("incheon"),
        // 광주시 is a municipality in Gyeonggi, not an unambiguous alias for
        // the metropolitan province. City-level text needs separate evidence.
        "gwangju" | "광주" | "광주광역시" => Some("gwangju"),
        "daejeon" | "대전" | "대전시" | "대전광역시" => Some("daejeon"),
        "ulsan" | "울산" | "울산시" | "울산광역시" => Some("ulsan"),
        "sejong" | "세종" | "세종시" | "세종특별자치시" => Some("sejong"),
        "gyeonggi" | "경기" | "경기도" => Some("gyeonggi"),
        "gangwon" | "강원" | "강원도" | "강원특별자치도" => Some("gangwon"),
        "chungbuk" | "충북" | "충청북도" => Some("chungbuk"),
        "chungnam" | "충남" | "충청남도" => Some("chungnam"),
        "jeonbuk" | "전북" | "전라북도" | "전북특별자치도" => Some("jeonbuk"),
        "jeonnam" | "전남" | "전라남도" => Some("jeonnam"),
        "gyeongbuk" | "경북" | "경상북도" => Some("gyeongbuk"),
        "gyeongnam" | "경남" | "경상남도" => Some("gyeongnam"),
        "jeju" | "제주" | "제주도" | "제주특별자치도" => Some("jeju"),
        _ => None,
    }
}

/// Canonical enrollment code for the vocabulary clients send (web "대학생",
/// mobile "재학", what-if screens "leave_of_absence"). Rules and campus
/// payloads compare these codes exactly, so an unmapped answer would read as
/// a contradiction and hard-exclude the user. `None` means unrecognised.
pub fn canonical_enrollment_status(value: &str) -> Option<&'static str> {
    match value.trim().trim_matches('"').to_lowercase().as_str() {
        "enrolled" | "재학" | "재학생" | "대학생" | "재학중" | "재학 중" | "졸업예정" => {
            Some("enrolled")
        }
        "on_leave" | "leave_of_absence" | "휴학" | "휴학생" | "휴학중" | "휴학 중" => {
            Some("on_leave")
        }
        "graduated" | "졸업" | "졸업생" => Some("graduated"),
        "research_student" | "수료후연구생" | "수료 후 연구생" => {
            Some("research_student")
        }
        _ => None,
    }
}

/// Canonical employment code; see [`canonical_enrollment_status`]. Freelancers
/// are business-income earners for Korean policy purposes, so they share the
/// self-employed code instead of falling into "other".
pub fn canonical_employment_status(value: &str) -> Option<&'static str> {
    match value.trim().trim_matches('"').to_lowercase().as_str() {
        "employed" | "재직" | "재직자" | "직장인" | "재직중" | "재직 중" => {
            Some("employed")
        }
        "new_hire" | "신입" | "신입 재직" | "신규입사" | "신규 입사" => {
            Some("new_hire")
        }
        "unemployed" | "미취업" | "무직" => Some("unemployed"),
        "preparing" | "취업준비생" | "취준생" | "구직중" | "구직 중" | "취업준비" => {
            Some("preparing")
        }
        "self_employed" | "freelancer" | "자영업" | "자영업자" | "프리랜서" => {
            Some("self_employed")
        }
        "student" | "학생" => Some("student"),
        "other" | "기타" => Some("other"),
        _ => None,
    }
}

/// Korean label for an enrollment code (or any alias of it) for user-facing
/// explanations. Codes must never be shown raw.
pub fn enrollment_status_label(value: &str) -> &'static str {
    match canonical_enrollment_status(value) {
        Some("enrolled") => "재학",
        Some("on_leave") => "휴학",
        Some("graduated") => "졸업",
        Some("research_student") => "수료 후 연구생",
        _ => "기타",
    }
}

/// Korean label for an employment code; see [`enrollment_status_label`].
pub fn employment_status_label(value: &str) -> &'static str {
    match canonical_employment_status(value) {
        Some("employed") => "재직",
        Some("new_hire") => "신입 재직",
        Some("unemployed") => "미취업",
        Some("preparing") => "취업 준비",
        Some("self_employed") => "자영업",
        Some("student") => "학생",
        _ => "기타",
    }
}

/// Korean label for a `housing_type` code (profile vocabulary plus the
/// `rental`/`boarding` codes older rules carry).
pub fn housing_type_label(value: &str) -> &'static str {
    match value.trim().trim_matches('"') {
        "parents" => "부모님 집",
        "monthly_rent" => "월세",
        "jeonse" => "전세",
        "own" => "자가",
        "dormitory" => "기숙사",
        "rental" => "임차",
        "boarding" => "하숙",
        _ => "기타",
    }
}

/// Korean label of a stored programme region for user-facing text: a sido
/// code in any case ("ulsan", "Ulsan" → "울산"), `sido:시군구`
/// ("ulsan:남구" → "울산 남구") or 전국. `None` for `unknown` and any value
/// without a label: show nothing or a neutral line, never the raw value.
pub fn region_display(value: &str) -> Option<String> {
    let value = value.trim().trim_matches('"');
    if matches!(
        value.to_ascii_lowercase().as_str(),
        "nationwide" | "all" | "전국"
    ) {
        return Some("전국".into());
    }
    let (sido, district) = match value.split_once(':') {
        Some((sido, district)) => (sido, district.trim()),
        None => (value, ""),
    };
    let label = region_label(canonical_region_code(sido)?);
    let hangul_name = !district.is_empty()
        && district
            .chars()
            .all(|c| c == ' ' || ('가'..='힣').contains(&c));
    Some(if hangul_name {
        format!("{label} {district}")
    } else {
        label.to_owned()
    })
}

/// One line for a programme's regions: labels in order without repeats,
/// "전국" for an empty list or one naming every sido, `None` when no value
/// has a label. Mirrors `regionsLabel` in the clients' displayLabels.ts.
pub fn regions_display<'a>(values: impl IntoIterator<Item = &'a str>) -> Option<String> {
    let mut labels: Vec<String> = Vec::new();
    let mut sidos = std::collections::BTreeSet::new();
    let mut empty = true;
    for value in values {
        empty = false;
        let Some(label) = region_display(value) else {
            continue;
        };
        if !value.contains(':') {
            sidos.extend(canonical_region_code(value));
        }
        if !labels.contains(&label) {
            labels.push(label);
        }
    }
    // An empty list is confirmed nationwide (crates/worker/src/regions.rs).
    if empty || labels.iter().any(|l| l == "전국") || sidos.len() == agent::REGION_CODES.len() {
        return Some("전국".into());
    }
    (!labels.is_empty()).then(|| labels.join(" · "))
}

/// Calendar year in Asia/Seoul (UTC+9, no daylight saving). Age and birth-year
/// checks must not change answer between 00:00 and 09:00 KST on 1 January.
pub fn seoul_year(now: chrono::DateTime<chrono::Utc>) -> i32 {
    use chrono::Datelike;
    seoul_date(now).year()
}

/// Calendar date in Korea for an instant. Deadlines are entered and read as
/// Korean dates; formatting the UTC instant would show the previous day for
/// anything stored at KST midnight.
pub fn seoul_date(now: chrono::DateTime<chrono::Utc>) -> chrono::NaiveDate {
    let seoul = chrono::FixedOffset::east_opt(9 * 3600).expect("KST is a fixed offset");
    now.with_timezone(&seoul).date_naive()
}

/// The Korean calendar day a deadline falls on: the day of its last open
/// second, so an end at 24:00 KST (00:00 the next day) is that day's
/// deadline. D-day counts read this.
pub fn deadline_date(deadline: chrono::DateTime<chrono::Utc>) -> chrono::NaiveDate {
    seoul_date(deadline - chrono::Duration::seconds(1))
}

/// Korean calendar days from `now` to the day `deadline` falls on
/// ([`deadline_date`]): 0 = closes today. The one day count of scores and
/// "마감이 N일 남았어요" (audit L4).
pub fn days_until(
    deadline: chrono::DateTime<chrono::Utc>,
    now: chrono::DateTime<chrono::Utc>,
) -> i64 {
    (deadline_date(deadline) - seoul_date(now)).num_days()
}

/// SQL twin of [`deadline_date`] for the effective deadline of the row
/// `alias` (empty = unqualified); NULL without a deadline.
pub fn deadline_date_sql(alias: &str) -> String {
    let a = if alias.is_empty() {
        String::new()
    } else {
        format!("{alias}.")
    };
    format!(
        "((LEAST({a}benefit_valid_until, {a}deadline_at, {a}application_end_at) - interval '1 second') AT TIME ZONE 'Asia/Seoul')::date"
    )
}

/// SQL: today's Korean calendar date.
pub const SEOUL_TODAY_SQL: &str = "(now() AT TIME ZONE 'Asia/Seoul')::date";

/// Canonical `programs.program_type` keys with their Korean labels: the one
/// list the worker normalizers, seeds, admin validation and both clients'
/// category tabs must agree on. The public list filter compares these keys
/// exactly, so a value outside this list is invisible in every category tab.
pub const PROGRAM_TYPES: &[(&str, &str)] = &[
    ("scholarship", "장학금"),
    ("policy", "정책"),
    ("support", "청년정책"),
    ("housing", "주거"),
    ("benefit", "생활"),
    ("welfare", "복지"),
    ("financial_product", "금융"),
    ("employment", "취업"),
    ("corporate_benefit", "기업혜택"),
    ("campus_benefit", "대학"),
];

/// Korean label for a canonical program type; `None` for an unknown key.
pub fn program_type_label(value: &str) -> Option<&'static str> {
    PROGRAM_TYPES
        .iter()
        .find(|(key, _)| *key == value)
        .map(|(_, label)| *label)
}

pub fn is_program_type(value: &str) -> bool {
    program_type_label(value).is_some()
}

/// Whether a programme's `benefit_amount_*` columns are support money. A
/// corporate offer's are not: its discount is stated in the shown sentence
/// and stays out of sums, rankings, sort order and "지원 금액" wording.
pub fn pays_support(program_type: &str) -> bool {
    program_type != "corporate_benefit"
}

/// SQL predicate for a program a user can act on right now: published, in an
/// open status ([`models::OPEN_PROGRAM_STATUSES`]), already started and before
/// its effective deadline (`LEAST(benefit_valid_until, deadline_at,
/// application_end_at)`, the SQL twin of [`models::Program::effective_deadline`]).
///
/// `alias` is the column prefix (`""` or `"p."`). `now` is the SQL expression
/// for the clock: `"now()"`, or a bound parameter such as `"$1"` when the
/// caller already fixed one instant for the whole request. The fragment binds
/// nothing itself, so the caller's parameter numbering is unaffected.
///
/// The catalogue list (`GET /programs`) uses this only when no `status` filter
/// is given; an explicit `status=upcoming|closed` filter is a different
/// question and keeps its own predicate.
pub fn currently_available_sql(alias: &str, now: &str) -> String {
    let statuses = models::OPEN_PROGRAM_STATUSES
        .iter()
        .map(|status| format!("'{status}'"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "{a}is_active = true \
         AND {a}program_status IN ({statuses}) \
         AND ({a}application_start_at IS NULL OR {a}application_start_at <= {now}) \
         AND (LEAST({a}benefit_valid_until, {a}deadline_at, {a}application_end_at) IS NULL \
              OR LEAST({a}benefit_valid_until, {a}deadline_at, {a}application_end_at) > {now})",
        a = alias
    )
}

#[cfg(test)]
mod tests {
    use super::{
        canonical_employment_status, canonical_enrollment_status, canonical_region_code,
        currently_available_sql, employment_status_label, enrollment_status_label, is_program_type,
        normalize_brand_name, program_type_label, region_display, region_label, regions_display,
        seoul_date, seoul_year, PROGRAM_TYPES,
    };

    /// `key: "label"` pairs of `export const <name> ... = { ... };` in a TS file.
    fn ts_label_map(source: &str, name: &str) -> Vec<(String, String)> {
        let body = source
            .split(&format!("export const {name}"))
            .nth(1)
            .and_then(|rest| rest.split_once('{'))
            .and_then(|(_, rest)| rest.split("};").next())
            .unwrap_or_else(|| panic!("{name} not found"));
        body.split(',')
            .filter_map(|pair| {
                let (key, label) = pair.split_once(':')?;
                let label = label.trim().strip_prefix('"')?.strip_suffix('"')?;
                Some((key.trim().to_owned(), label.to_owned()))
            })
            .collect()
    }

    /// Both clients label every enrollment and employment code exactly as the
    /// server does; a code added without a client label fails here.
    #[test]
    fn client_profile_labels_match_core() {
        for path in [
            "apps/web/src/lib/profileCodes.ts",
            "apps/mobile/lib/profileCodes.ts",
        ] {
            let source = repo_file(path);
            for (name, codes, label) in [
                (
                    "ENROLLMENT_LABELS",
                    crate::agent::ENROLLMENT_CODES,
                    enrollment_status_label as fn(&str) -> &'static str,
                ),
                (
                    "EMPLOYMENT_LABELS",
                    crate::agent::EMPLOYMENT_CODES,
                    employment_status_label,
                ),
            ] {
                let client = ts_label_map(&source, name);
                let keys: Vec<&str> = client.iter().map(|(k, _)| k.as_str()).collect();
                assert_eq!(keys, codes, "{path} {name} keys");
                for (code, client_label) in &client {
                    assert_eq!(client_label, label(code), "{path} {name} {code}");
                }
            }
        }
    }

    /// Every canonical region code has a Korean label, server-side and in the
    /// generated client table. A new code without one fails here.
    #[test]
    fn every_canonical_region_has_a_label() {
        let client = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../apps/web/src/lib/districts.generated.ts"
        ))
        .unwrap();
        let line = client
            .lines()
            .find(|l| l.starts_with("export const DISTRICT_REGION_LABELS = "))
            .expect("DISTRICT_REGION_LABELS");
        let table: serde_json::Map<String, serde_json::Value> = serde_json::from_str(
            line.trim_start_matches("export const DISTRICT_REGION_LABELS = ")
                .trim_end_matches(" as const;"),
        )
        .unwrap();
        let keys: Vec<&str> = table.keys().map(String::as_str).collect();
        let mut codes = crate::agent::REGION_CODES.to_vec();
        let mut sorted = keys.clone();
        codes.sort_unstable();
        sorted.sort_unstable();
        assert_eq!(sorted, codes, "client label table and REGION_CODES differ");
        for code in crate::agent::REGION_CODES {
            let label = region_label(code);
            assert_ne!(label, "기타", "{code} has no label");
            assert_eq!(canonical_region_code(code), Some(*code));
            assert_eq!(canonical_region_code(label), Some(*code), "{label}");
            assert_eq!(table[*code], label, "client label of {code}");
            assert_eq!(region_display(code).as_deref(), Some(label));
            assert_eq!(region_display(&code.to_uppercase()).as_deref(), Some(label));
        }
    }

    #[test]
    fn region_display_never_returns_a_raw_value() {
        assert_eq!(region_display("Ulsan").as_deref(), Some("울산"));
        assert_eq!(region_display("ulsan:남구").as_deref(), Some("울산 남구"));
        assert_eq!(
            region_display("gangwon:속초시").as_deref(),
            Some("강원 속초시")
        );
        assert_eq!(region_display("gangwon:51210").as_deref(), Some("강원"));
        assert_eq!(region_display("전국").as_deref(), Some("전국"));
        for raw in ["unknown", "", "mars", "jeonnam_gwangju", ":남구"] {
            assert_eq!(region_display(raw), None, "{raw}");
        }
        assert_eq!(
            regions_display(["busan", "unknown", "busan", "ulsan:남구"]).as_deref(),
            Some("부산 · 울산 남구")
        );
        assert_eq!(regions_display(["unknown"]), None);
        assert_eq!(regions_display(std::iter::empty()).as_deref(), Some("전국"));
        assert_eq!(region_display("울산광역시").as_deref(), Some("울산"));
        assert_eq!(
            regions_display(crate::agent::REGION_CODES.iter().copied()).as_deref(),
            Some("전국")
        );
    }

    #[test]
    fn availability_fragment_prefixes_every_column_and_binds_nothing() {
        let plain = currently_available_sql("", "now()");
        assert!(plain.starts_with("is_active = true"));
        assert!(plain.contains("program_status IN ('open', 'active', 'always_open')"));
        assert!(plain.contains("application_start_at <= now()"));
        assert!(
            plain.contains("LEAST(benefit_valid_until, deadline_at, application_end_at) > now()")
        );
        assert!(!plain.contains('$'), "the default clock binds no parameter");

        let aliased = currently_available_sql("p.", "$1");
        for column in [
            "is_active",
            "program_status",
            "application_start_at",
            "benefit_valid_until",
            "deadline_at",
            "application_end_at",
        ] {
            assert!(
                !aliased.contains(&format!(" {column}"))
                    && !aliased.contains(&format!("({column}")),
                "{column} must carry the alias: {aliased}"
            );
            assert!(aliased.contains(&format!("p.{column}")));
        }
        assert_eq!(aliased.matches("$1").count(), 2);
    }

    fn repo_file(relative: &str) -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(relative);
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    }

    /// `(alias, code)` pairs of every `WHEN '<alias>' THEN '<code>'` arm in a
    /// SQL fragment.
    fn sql_case_arms(sql: &str) -> Vec<(String, String)> {
        sql.lines()
            .filter_map(|line| {
                let rest = line.trim().strip_prefix("WHEN '")?;
                let (alias, rest) = rest.split_once("' THEN '")?;
                let code = rest.split_once('\'')?.0;
                Some((alias.to_string(), code.to_string()))
            })
            .collect()
    }

    /// `(value, label)` pairs of every `{ value: "x", label: "y" }` or
    /// `{ label: "y", value: "x" }` object literal in a TypeScript fragment.
    fn ts_value_label_pairs(source: &str) -> Vec<(String, String)> {
        let quoted = |text: &str, key: &str| -> Option<String> {
            let start = text.find(&format!("{key}: \""))? + key.len() + 3;
            let end = text[start..].find('"')? + start;
            Some(text[start..end].to_string())
        };
        source
            .split('{')
            .skip(1)
            .filter_map(|object| {
                let object = object.split('}').next()?;
                Some((quoted(object, "value")?, quoted(object, "label")?))
            })
            .collect()
    }

    #[test]
    fn normalizes_korean_and_latin_brand_names() {
        assert_eq!(normalize_brand_name(" 스타벅스 코리아 "), "스타벅스코리아");
        assert_eq!(normalize_brand_name("A-B Cafe!"), "abcafe");
    }

    #[test]
    fn canonicalizes_korean_region_labels() {
        assert_eq!(canonical_region_code("부산광역시"), Some("busan"));
        assert_eq!(canonical_region_code("강원특별자치도"), Some("gangwon"));
        assert_eq!(canonical_region_code("JEJU"), Some("jeju"));
        assert_eq!(canonical_region_code("해운대구"), None);
        assert_eq!(canonical_region_code("광주시"), None);
        assert_eq!(canonical_region_code("광주광역시"), Some("gwangju"));
    }

    #[test]
    fn canonicalizes_client_enrollment_and_employment_vocabulary() {
        for value in [
            "enrolled",
            " ENROLLED ",
            "재학",
            "재학생",
            "대학생",
            "재학중",
            "졸업예정",
        ] {
            assert_eq!(
                canonical_enrollment_status(value),
                Some("enrolled"),
                "{value}"
            );
        }
        for value in ["on_leave", "leave_of_absence", "휴학", "휴학생", "휴학중"] {
            assert_eq!(
                canonical_enrollment_status(value),
                Some("on_leave"),
                "{value}"
            );
        }
        assert_eq!(canonical_enrollment_status("졸업"), Some("graduated"));
        assert_eq!(canonical_enrollment_status("졸업생"), Some("graduated"));
        assert_eq!(
            canonical_enrollment_status("수료후연구생"),
            Some("research_student")
        );
        for value in ["", "취업준비생", "학생아님", "typo"] {
            assert_eq!(canonical_enrollment_status(value), None, "{value}");
        }

        for value in ["employed", "재직", "재직자", "직장인"] {
            assert_eq!(
                canonical_employment_status(value),
                Some("employed"),
                "{value}"
            );
        }
        assert_eq!(canonical_employment_status("신입"), Some("new_hire"));
        assert_eq!(canonical_employment_status("신규입사"), Some("new_hire"));
        assert_eq!(canonical_employment_status("미취업"), Some("unemployed"));
        assert_eq!(canonical_employment_status("무직"), Some("unemployed"));
        for value in ["취업준비생", "취준생", "구직중", "preparing"] {
            assert_eq!(
                canonical_employment_status(value),
                Some("preparing"),
                "{value}"
            );
        }
        for value in ["자영업", "자영업자", "freelancer", "프리랜서"] {
            assert_eq!(
                canonical_employment_status(value),
                Some("self_employed"),
                "{value}"
            );
        }
        assert_eq!(canonical_employment_status("학생"), Some("student"));
        assert_eq!(canonical_employment_status("기타"), Some("other"));
        for value in ["", "재학", "typo"] {
            assert_eq!(canonical_employment_status(value), None, "{value}");
        }
    }

    #[test]
    fn status_labels_never_echo_codes() {
        assert_eq!(enrollment_status_label("\"enrolled\""), "재학");
        assert_eq!(enrollment_status_label("on_leave"), "휴학");
        assert_eq!(enrollment_status_label("garbage"), "기타");
        assert_eq!(employment_status_label("\"employed\""), "재직");
        assert_eq!(employment_status_label("preparing"), "취업 준비");
        assert_eq!(employment_status_label("garbage"), "기타");
    }

    #[test]
    fn seoul_year_rolls_over_at_kst_midnight_not_utc_midnight() {
        let before: chrono::DateTime<chrono::Utc> = "2026-12-31T14:59:59Z".parse().unwrap();
        let after: chrono::DateTime<chrono::Utc> = "2026-12-31T15:00:00Z".parse().unwrap();
        assert_eq!(seoul_year(before), 2026);
        assert_eq!(seoul_year(after), 2027);
        assert_eq!(seoul_date(before).to_string(), "2026-12-31");
        assert_eq!(seoul_date(after).to_string(), "2027-01-01");
    }

    /// The status rewrite migration carries its own copy of the alias maps.
    /// Every alias it maps must map to the same code here, or a profile the
    /// migration canonicalized would be rejected by the API on its next save.
    #[test]
    fn migration_status_aliases_are_a_subset_of_the_canonical_maps() {
        let sql = repo_file("infra/migrations/20261008000004_canonical_profile_status.sql");
        let (enrollment_sql, employment_sql) = sql
            .split_once("SET employment_status = CASE lower(")
            .expect("migration 23 rewrites employment_status in a second UPDATE");
        let enrollment = sql_case_arms(enrollment_sql);
        let employment = sql_case_arms(employment_sql);
        assert!(
            enrollment.len() >= 19 && employment.len() >= 28,
            "parser lost arms"
        );
        for (alias, code) in &enrollment {
            if alias == "취업준비생" {
                // The enrollment UPDATE moves this answer to employment_status.
                assert_eq!(canonical_employment_status(alias), Some(code.as_str()));
                continue;
            }
            assert_eq!(
                canonical_enrollment_status(alias),
                Some(code.as_str()),
                "{alias}"
            );
        }
        for (alias, code) in &employment {
            assert_eq!(
                canonical_employment_status(alias),
                Some(code.as_str()),
                "{alias}"
            );
        }
    }

    #[test]
    fn program_types_are_labelled_and_unique() {
        let mut keys: Vec<&str> = PROGRAM_TYPES.iter().map(|(key, _)| *key).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), PROGRAM_TYPES.len());
        assert_eq!(program_type_label("scholarship"), Some("장학금"));
        assert!(is_program_type("corporate_benefit"));
        assert!(!is_program_type("Scholarship"));
        assert!(!is_program_type(""));
    }

    /// The web admin dropdown and the mobile category tabs are copies of this
    /// list. Web must match it exactly (the dropdown writes program_type);
    /// mobile tabs may omit a type but never invent one or relabel it.
    #[test]
    fn client_program_type_lists_match_the_canonical_list() {
        let canonical: Vec<(String, String)> = PROGRAM_TYPES
            .iter()
            .map(|(key, label)| (key.to_string(), label.to_string()))
            .collect();
        // One shared label module per client (programTypeLabel reads it).
        for path in [
            "apps/web/src/lib/displayLabels.ts",
            "apps/mobile/constants/displayLabels.ts",
        ] {
            let source = repo_file(path);
            let list = source
                .split("export const PROGRAM_TYPES")
                .nth(1)
                .and_then(|rest| rest.split("];").next())
                .unwrap_or_else(|| panic!("{path} exports PROGRAM_TYPES"));
            assert_eq!(
                ts_value_label_pairs(list),
                canonical,
                "{path} PROGRAM_TYPES drifted"
            );
        }

        let mobile = repo_file("apps/mobile/components/FigmaUI.tsx");
        let mobile = mobile
            .split("export const benefitCategories")
            .nth(1)
            .and_then(|rest| rest.split("];").next())
            .expect("FigmaUI.tsx exports benefitCategories");
        let tabs: Vec<_> = ts_value_label_pairs(mobile)
            .into_iter()
            .filter(|(value, _)| !value.is_empty()) // "전체" is the no-filter tab
            .collect();
        assert!(tabs.len() >= 8, "parser lost mobile tabs: {tabs:?}");
        for (value, label) in &tabs {
            assert_eq!(
                program_type_label(value),
                Some(label.as_str()),
                "mobile tab {value}"
            );
        }
        // The campus tab is added at runtime when a school is selected.
        assert_eq!(program_type_label("campus_benefit"), Some("대학"));
    }
}

#[cfg(test)]
mod residence_reason_tests {
    use super::*;

    /// Both writers' wordings are read; other reasons are not.
    #[test]
    fn both_residence_wordings_are_one_reason() {
        assert!(is_residence_reason(&format!(
            "대구{RESIDENCE_REASON_STEM} 맞음"
        )));
        assert!(is_residence_reason(&format!(
            "대구{RESIDENCE_REASON_STEM}에 맞아요."
        )));
        assert!(!is_residence_reason("무주택 조건 맞음"));
        assert!(!is_residence_reason("현재 연령 조건에 맞아요."));
        assert!(!is_residence_reason(" 거주 조건 맞음"));
    }
}
