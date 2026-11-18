//! Source-specific residence scopes. Empty means confirmed nationwide;
//! `unknown` deliberately matches no user region, but stays in the full catalogue.
//! Do not infer eligibility from a policy title or a provider's postal address.
use serde_json::Value;
use std::collections::BTreeSet;

pub const UNKNOWN: &str = "unknown";

fn unknown() -> Vec<String> {
    vec![UNKNOWN.into()]
}

/// 온통청년 uses legal district codes, despite calling the field `zipCd`.
/// The 2026 `12` district mapping was verified against the provider's own
/// /go/ythip/getSggCd/12 dictionary; keep old saved profile regions compatible.
fn district_region(code: &str) -> Option<&'static str> {
    if !matches!(code.len(), 2 | 5 | 10) || !code.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    match &code[..2] {
        "11" => Some("seoul"),
        "26" => Some("busan"),
        "27" => Some("daegu"),
        "28" => Some("incheon"),
        "29" => Some("gwangju"),
        "30" => Some("daejeon"),
        "31" => Some("ulsan"),
        "36" => Some("sejong"),
        "41" => Some("gyeonggi"),
        "42" | "51" => Some("gangwon"),
        "43" => Some("chungbuk"),
        "44" => Some("chungnam"),
        "45" | "52" => Some("jeonbuk"),
        "46" => Some("jeonnam"),
        "47" => Some("gyeongbuk"),
        "48" => Some("gyeongnam"),
        "50" => Some("jeju"),
        "12" if code.len() >= 5 => match &code[..5] {
            "12210" | "12240" | "12270" | "12300" | "12330" => Some("gwangju"),
            "12110" | "12130" | "12150" | "12170" | "12190" | "12710" | "12720" | "12730"
            | "12740" | "12750" | "12760" | "12770" | "12780" | "12790" | "12800" | "12810"
            | "12820" | "12830" | "12840" | "12850" | "12860" | "12870" => Some("jeonnam"),
            _ => None,
        },
        _ => None,
    }
}

/// A residence scope with its optional 시·군·구 detail.
///
/// - `regions`: canonical sido codes; empty = nationwide, `[unknown]` =
///   unresolved (see the module doc).
/// - `districts`: current 법정동 시군구 codes ([`crate::district_codes`]) the
///   scope is limited to, or `None` when the source names no district or the
///   list is equivalent to the whole country. A 시 with 구 is stored together
///   with all its 구, so a profile code of either level matches exactly.
/// - `note`: why a scope was resolved the way it was, when that is not the
///   plain reading of one field (kept in `normalized_payload.scope_note`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Scope {
    pub regions: Vec<String>,
    pub districts: Option<Vec<String>>,
    pub note: Option<&'static str>,
}

impl Scope {
    fn regions(regions: Vec<String>) -> Self {
        Self {
            regions,
            ..Self::default()
        }
    }
}

/// The registrar is one 시·군·구 but the record lists (nearly) every district
/// in the country: the two fields contradict each other, so neither is used.
pub const NOTE_REGISTRAR_SCOPE_CONFLICT: &str = "registrar_scope_conflict";
/// "참여 지자체에 주민등록": open nationwide where a municipality participates.
pub const NOTE_PARTICIPATING_MUNICIPALITIES: &str = "participating_municipalities";
/// A local foundation or public corporation resolved by the province or
/// district name its own name starts with.
pub const NOTE_PROVIDER_NAME_PREFIX: &str = "provider_name_prefix";
/// A local body resolved by its parent government's 보조금24 agency code
/// ([`crate::agency_codes`]).
pub const NOTE_PROVIDER_AGENCY_CODE: &str = "provider_agency_code";
/// A 공공기관 (national public institution) whose eligibility text names no
/// place and no vicinity: nationwide.
pub const NOTE_NATIONAL_PUBLIC_INSTITUTION: &str = "national_public_institution";

/// A record lists a 시·군·구 code the current table does not know and no
/// listed successor resolves: the code is kept as an unresolved entry of its
/// sido, never read as the whole sido (audit M8) and never as unknown scope
/// (review B6). `rule_health` counts and raises these rows.
pub const NOTE_UNKNOWN_DISTRICT_CODE: &str = "unknown_district_code";

/// A youth record whose `zipCd` covers this many districts and every sido is
/// equivalent to a nationwide list.
const NATIONWIDE_DISTRICT_LIST: usize = 200;

pub fn youth(payload: &Value) -> Vec<String> {
    youth_scope(payload).regions
}

/// Sido regions plus districts for an 온통청년 record. Districts come from the
/// same `zipCd` codes as the regions; a lone all-regions code or a list that
/// covers the whole country has no district restriction.
pub fn youth_scope(payload: &Value) -> Scope {
    let regions = youth_regions(payload);
    if regions.is_empty() || regions == [UNKNOWN] {
        return Scope::regions(regions);
    }
    let codes: Vec<&str> = payload["zipCd"]
        .as_str()
        .unwrap_or("")
        .split([',', ';', '|'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    let read = districts_for_codes(&codes);
    let note = read.as_ref().and_then(|read| {
        (!read.unresolved.is_empty()).then(|| {
            tracing::warn!(codes = ?read.unresolved, "youth_center zipCd lists unknown district codes; district left open");
            NOTE_UNKNOWN_DISTRICT_CODE
        })
    });
    let districts = read.map(|read| read.codes);
    let nationwide = regions.len() >= 17
        && districts
            .as_ref()
            .is_some_and(|d| d.len() >= NATIONWIDE_DISTRICT_LIST);
    if nationwide {
        // A single 시·군·구 cannot run a programme for the whole country.
        let registrar = payload["rgtrInstCdNm"].as_str().unwrap_or("");
        if provider_districts(registrar).is_some() {
            return Scope {
                regions: unknown(),
                districts: None,
                note: Some(NOTE_REGISTRAR_SCOPE_CONFLICT),
            };
        }
        return Scope::regions(regions);
    }
    Scope {
        regions,
        districts,
        note,
    }
}

/// A code at 시·군·구 level: 5 digits (or 10 with a zero 읍면동 part) that
/// is not a sido code (`xx000`).
fn is_district_level(code: &str) -> bool {
    let digits = code.bytes().all(|c| c.is_ascii_digit());
    let level = match code.len() {
        5 => true,
        10 => &code[5..] == "00000",
        _ => false,
    };
    digits && level && &code[2..5] != "000"
}

/// Provider district codes read against the current table.
struct ReadDistricts {
    /// Current codes (with 구 and retired aliases), plus each unresolved
    /// code as given, which the district check reads as unknown for its
    /// sido's users (`structural::programme_districts`).
    codes: Vec<String>,
    /// District-level codes neither current nor retired with a listed
    /// successor.
    unresolved: Vec<String>,
}

/// The districts provider codes (5 or 10 digits, current or retired) name.
/// A retired code with listed successors (`into`/`part`, a split or merge)
/// is the union of them: never narrower than the old district. `None` when
/// any code is not at district level (a sido code), so a changed code never
/// narrows or widens a scope silently.
fn districts_for_codes(codes: &[&str]) -> Option<ReadDistricts> {
    let mut out = BTreeSet::new();
    let mut unresolved = Vec::new();
    for code in codes {
        if !is_district_level(code) {
            return None;
        }
        let code = &code[..5];
        if let Some(current) = canonical_district(code) {
            out.extend(with_aliases(with_children(current)));
            continue;
        }
        let successors: Vec<&'static str> = mazimi_core::districts::SUCCESSORS
            .iter()
            .filter(|(retired, ..)| *retired == code)
            .filter_map(|(_, successor, _)| canonical_district(successor))
            .collect();
        if successors.is_empty() {
            out.insert(code.to_owned());
            unresolved.push(code.to_owned());
        }
        for current in successors {
            out.extend(with_aliases(with_children(current)));
        }
    }
    (!out.is_empty()).then(|| ReadDistricts {
        codes: out.into_iter().collect(),
        unresolved,
    })
}

/// The current 5-digit 시군구 code for a current or retired code
/// (광주광역시 29170 → 12300, 강원도 42110 → 51110). `None` for anything else,
/// including sido-only codes.
pub fn canonical_district(code: &str) -> Option<&'static str> {
    use crate::district_codes::{DISTRICTS, LEGACY};
    let code = whole_successor(code).unwrap_or(code);
    if let Some((current, ..)) = DISTRICTS.iter().find(|(c, ..)| *c == code) {
        return Some(current);
    }
    LEGACY
        .iter()
        .find(|(legacy, _)| *legacy == code)
        .map(|(_, current)| *current)
}

/// The current code a retired 5-digit code reads as when its district is,
/// whole, one current district (`same` rows of the reviewed successor table,
/// [`mazimi_core::districts::SUCCESSORS`]): 군위군 47720 → 27720.
fn whole_successor(code: &str) -> Option<&'static str> {
    let code = code.get(..5)?;
    mazimi_core::districts::SUCCESSORS
        .iter()
        .find(|(retired, _, relation)| *retired == code && *relation == "same")
        .map(|(_, successor, _)| *successor)
}

/// Current codes plus each one's retired code for the same district, sorted.
/// Profiles saved before the 2023-2026 province changes carry the retired
/// codes (광주 북구 29170, 순천 46150, 춘천 42110); listing both keeps one
/// exact comparison correct for either.
fn with_aliases(codes: Vec<&'static str>) -> Vec<String> {
    use crate::district_codes::LEGACY;
    let mut out = BTreeSet::new();
    for code in codes {
        out.insert(code.to_string());
        out.extend(
            LEGACY
                .iter()
                .filter(|(_, current)| *current == code)
                .map(|(legacy, _)| (*legacy).to_string()),
        );
    }
    out.into_iter().collect()
}

/// A district code with the codes of its 구 when it is a 시 that has them.
fn with_children(code: &'static str) -> Vec<&'static str> {
    use crate::district_codes::DISTRICTS;
    let Some((_, sido, name)) = DISTRICTS.iter().find(|(c, ..)| *c == code) else {
        return vec![code];
    };
    let mut out = vec![code];
    if name.ends_with('시') {
        let prefix = format!("{name} ");
        out.extend(
            DISTRICTS
                .iter()
                .filter(|(_, s, n)| s == sido && n.starts_with(&prefix))
                .map(|(c, ..)| *c),
        );
    }
    out
}

/// Provinces that share one current code block: 광주광역시 and 전라남도 are
/// 전남광주통합특별시 (12) in the 2026 법정동코드.
fn sido_block(label: &str) -> Option<&'static str> {
    match label {
        "전남광주통합특별시" | "전남광주특별시" | "광주광역시" | "전라남도" => {
            Some("12")
        }
        "강원특별자치도" | "강원도" => Some("51"),
        "전북특별자치도" | "전라북도" => Some("52"),
        _ => {
            let region = mazimi_core::canonical_region_code(label)?;
            crate::district_codes::DISTRICTS
                .iter()
                .find(|(code, sido, _)| {
                    !code.starts_with("12")
                        && mazimi_core::canonical_region_code(sido) == Some(region)
                })
                .map(|(code, ..)| &code[..2])
        }
    }
}

/// District codes named by a provider label such as "부산광역시 수영구",
/// "경기도 수원시" (with its 구) or "경기도 고양시 덕양구 …". `None` unless
/// the label starts with an explicit sido and a 시·군·구 of that sido.
pub fn provider_districts(label: &str) -> Option<Vec<String>> {
    use crate::district_codes::DISTRICTS;
    let tokens: Vec<&str> = label.split_whitespace().collect();
    let (sido, district) = (tokens.first()?, tokens.get(1)?);
    let block = sido_block(sido)?;
    if !['시', '군', '구'].iter().any(|s| district.ends_with(*s)) {
        return None;
    }
    let in_block = |code: &str| code.starts_with(block);
    // "수원시 영통구" is one district name in the code list.
    if let Some(gu) = tokens.get(2).filter(|t| t.ends_with('구')) {
        let name = format!("{district} {gu}");
        if let Some((code, ..)) = DISTRICTS.iter().find(|(c, _, n)| in_block(c) && *n == name) {
            return Some(with_aliases(vec![*code]));
        }
    }
    let (code, ..) = DISTRICTS
        .iter()
        .find(|(c, _, n)| in_block(c) && n == district)?;
    Some(with_aliases(with_children(code)))
}

/// The sido regions a current district code belongs to.
fn district_regions(code: &str) -> Option<&'static str> {
    district_region(code)
}

/// Legal-entity prefixes a local foundation's registered name may carry.
const ENTITY_PREFIXES: &[&str] = &[
    "재단법인",
    "(재단법인)",
    "(재)",
    "사단법인",
    "(사단법인)",
    "(사)",
    "주식회사",
    "(주)",
];

/// Province short names that open a local body's own name. Bare "광주" is
/// listed so it collides with 경기도 광주시 and resolves to nothing.
const SIDO_PREFIXES: &[(&str, &str)] = &[
    ("서울", "seoul"),
    ("부산", "busan"),
    ("대구", "daegu"),
    ("인천", "incheon"),
    ("광주", "gwangju"),
    ("대전", "daejeon"),
    ("울산", "ulsan"),
    ("세종", "sejong"),
    ("경기", "gyeonggi"),
    ("강원", "gangwon"),
    ("충북", "chungbuk"),
    ("충남", "chungnam"),
    ("전북", "jeonbuk"),
    ("전남", "jeonnam"),
    ("경북", "gyeongbuk"),
    ("경남", "gyeongnam"),
    ("제주", "jeju"),
];

/// The region of a local foundation or public corporation from the province
/// or district its registered name starts with: "재단법인전남인재평생교육진흥원"
/// → jeonnam, "오산시시설관리공단" → gyeonggi. Every matching name must agree on
/// one province; "광주…" (광주광역시 or 경기도 광주시) and "고성…" (강원 or 경남)
/// stay unresolved. Only for 지방출자·출연기관 and 지방공기업, which are local
/// bodies by definition; a national agency named after its seat is not.
pub fn local_body_region(name: &str) -> Option<Vec<String>> {
    use crate::district_codes::DISTRICTS;
    let mut rest: &str = name.trim();
    loop {
        let before = rest;
        for prefix in ENTITY_PREFIXES {
            rest = rest.strip_prefix(prefix).unwrap_or(rest).trim_start();
        }
        if rest == before {
            break;
        }
    }
    if rest.starts_with("전남광주") {
        return Some(vec!["gwangju".into(), "jeonnam".into()]);
    }
    let full = province_label(rest, None);
    if full != [UNKNOWN] {
        return Some(full);
    }
    let mut found = BTreeSet::new();
    for (short, region) in SIDO_PREFIXES {
        if rest.starts_with(short) {
            found.insert((*region).to_string());
        }
    }
    for (code, _, district) in DISTRICTS {
        // The 시 part only: "수원시 영통구" is reached through 수원시.
        let district = district.split(' ').next().unwrap_or(district);
        let stem: String = {
            let mut chars: Vec<char> = district.chars().collect();
            chars.pop();
            chars.into_iter().collect()
        };
        let matches = !district.is_empty()
            && (rest.starts_with(district)
                || (stem.chars().count() >= 2 && rest.starts_with(stem.as_str())));
        if matches {
            if let Some(region) = district_regions(code) {
                found.insert(region.to_string());
            }
        }
    }
    (found.len() == 1).then(|| found.into_iter().collect())
}

fn youth_regions(payload: &Value) -> Vec<String> {
    let Some(codes) = payload["zipCd"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return unknown();
    };
    // A lone explicit all-regions code is different from a missing code.
    if matches!(codes, "00" | "000" | "00000" | "전국") {
        return vec![];
    }
    let mut regions = BTreeSet::new();
    for code in codes
        .split([',', ';', '|'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if matches!(code, "12" | "12000" | "1200000000") {
            regions.extend(["gwangju".to_owned(), "jeonnam".to_owned()]);
        } else if let Some(region) = district_region(whole_successor(code).unwrap_or(code)) {
            regions.insert(region.to_string());
        } else {
            // A changed code must not silently broaden residence eligibility.
            return unknown();
        }
    }
    if regions.is_empty() {
        unknown()
    } else {
        regions.into_iter().collect()
    }
}

/// Only a leading, explicit province label is used. In particular, 경기도
/// 광주시 must never be confused with 광주광역시 and an organisation's name
/// containing a city word must never establish residency requirements.
pub fn province_label(label: &str, district: Option<&str>) -> Vec<String> {
    let label = label.trim();
    for merged in ["전남광주통합특별시", "전남광주특별시"] {
        if let Some(rest) = label.strip_prefix(merged) {
            let district = district
                .filter(|s| !s.trim().is_empty())
                .unwrap_or(rest)
                .trim();
            if district.is_empty() {
                return vec!["gwangju".into(), "jeonnam".into()];
            }
            if ["동구", "서구", "남구", "북구", "광산구"]
                .iter()
                .any(|d| district.starts_with(d))
            {
                return vec!["gwangju".into()];
            }
            if [
                "목포시",
                "여수시",
                "순천시",
                "나주시",
                "광양시",
                "담양군",
                "곡성군",
                "구례군",
                "고흥군",
                "보성군",
                "화순군",
                "장흥군",
                "강진군",
                "해남군",
                "영암군",
                "무안군",
                "함평군",
                "영광군",
                "장성군",
                "완도군",
                "진도군",
                "신안군",
            ]
            .iter()
            .any(|d| district.starts_with(d))
            {
                return vec!["jeonnam".into()];
            }
            return unknown();
        }
    }
    if let Some(code) = mazimi_core::canonical_region_code(label) {
        return vec![code.into()];
    }
    for name in [
        "서울특별시",
        "부산광역시",
        "대구광역시",
        "인천광역시",
        "광주광역시",
        "대전광역시",
        "울산광역시",
        "세종특별자치시",
        "경기도",
        "강원특별자치도",
        "강원도",
        "충청북도",
        "충청남도",
        "전북특별자치도",
        "전라북도",
        "전라남도",
        "경상북도",
        "경상남도",
        "제주특별자치도",
        "제주도",
    ] {
        if label.starts_with(name) {
            return vec![mazimi_core::canonical_region_code(name).unwrap().into()];
        }
    }
    unknown()
}

/// Residence scope of a 복지로 지자체 record from `ctpvNm` and `sggNm`
/// ("경상남도" + "밀양시"). Districts are kept only when `sggNm` names a
/// 시·군·구 of that sido; a sido-only record or a name that does not resolve
/// has no district list.
pub fn local_welfare_scope(payload: &Value) -> Scope {
    let Some(sido) = payload["ctpvNm"].as_str() else {
        return Scope::regions(unknown());
    };
    let district = payload["sggNm"].as_str().map(str::trim).unwrap_or("");
    let regions = province_label(sido, Some(district));
    // One district name only ("창원시 의창구" is one): a list ("해운대구
    // 수영구") or a trailing department would otherwise narrow the scope to
    // its first district, so two tokens must be one official name of the sido.
    let tokens: Vec<&str> = district.split_whitespace().collect();
    let one_district = !district.contains([',', '·', 'ㆍ', '/'])
        && match tokens[..] {
            [_] => true,
            [si, gu] => sido_block(sido.trim()).is_some_and(|block| {
                let name = format!("{si} {gu}");
                crate::district_codes::DISTRICTS
                    .iter()
                    .any(|(c, _, n)| c.starts_with(block) && *n == name)
            }),
            _ => false,
        };
    if regions == [UNKNOWN] || !one_district {
        return Scope::regions(regions);
    }
    // Every listed district must lie in the resolved sido regions.
    let districts = provider_districts(&format!("{} {district}", sido.trim())).filter(|codes| {
        codes
            .iter()
            .all(|c| district_region(c).is_some_and(|r| regions.iter().any(|x| x == r)))
    });
    Scope {
        regions,
        districts,
        note: None,
    }
}

pub fn government(payload: &Value) -> Vec<String> {
    government_scope(payload).regions
}

/// Residence scope of a 보조금24 record from its provider type and name.
/// Districts are kept only when the provider is a 시·군·구 (or one of its
/// offices) and names it explicitly: "부산광역시 수영구".
pub fn government_scope(payload: &Value) -> Scope {
    let kind = payload["소관기관유형"].as_str().unwrap_or("").trim();
    let provider = payload["소관기관명"].as_str().unwrap_or("");
    let code = payload["소관기관코드"].as_str().unwrap_or("").trim();
    match kind {
        "시군구" | "지방공기업" => {
            let regions = province_label(provider, None);
            if regions == [UNKNOWN] && kind == "지방공기업" {
                return local_body_scope(provider, code);
            }
            let districts = (regions != [UNKNOWN])
                .then(|| provider_districts(provider))
                .flatten();
            Scope {
                regions,
                districts,
                note: None,
            }
        }
        "광역시도" | "교육청" => {
            // "전남광주통합특별시교육청": the suffix is not a district.
            let sido = provider.trim();
            let sido = sido.strip_suffix("교육청").unwrap_or(sido);
            let regions = province_label(sido, None);
            // Collected and sido-wide: an empty district list.
            let districts = (regions != [UNKNOWN]).then(Vec::new);
            Scope {
                regions,
                districts,
                note: None,
            }
        }
        "지방출자_출연기관" => {
            let regions = province_label(provider, None);
            if regions == [UNKNOWN] {
                local_body_scope(provider, code)
            } else {
                Scope::regions(regions)
            }
        }
        // 공공기관 are the national public institutions (공공기관운영법:
        // 근로복지공단, 한국장학재단, 발전 자회사 …); local bodies have their
        // own types above. Many run site-bound programmes (발전소 주변지역,
        // 댐 주변, "충남 보령시 오천면 전력고객") worded without a residence
        // verb, so nationwide needs text naming no place and no vicinity.
        "공공기관" => {
            let texts =
                ["지원대상", "선정기준", "지원내용"].map(|key| payload[key].as_str().unwrap_or(""));
            // No eligibility text is no evidence of a nationwide scope. The
            // service name is checked for vicinity wording ("발전소 주변지역
            // 장학금"), not for place names: "발전소 시설 견학(당진)" is a
            // venue open to everyone.
            let silent = texts.iter().all(|t| t.trim().is_empty());
            let title = payload["서비스명"].as_str().unwrap_or("");
            if silent
                || texts.iter().any(|t| has_local_scope_hint(t))
                || has_vicinity_wording(title)
            {
                Scope::regions(unknown())
            } else {
                Scope {
                    regions: vec![],
                    districts: None,
                    note: Some(NOTE_NATIONAL_PUBLIC_INSTITUTION),
                }
            }
        }
        "중앙행정기관" => {
            // A ministry can still run a geographically restricted programme.
            // Free-form residence requirements are not a nationwide contract.
            let texts: Vec<&str> = ["지원대상", "선정기준"]
                .iter()
                .map(|key| payload[*key].as_str().unwrap_or(""))
                .collect();
            let participating = texts.iter().any(|t| participating_municipalities(t));
            // An area named by its kind (인구감소지역, 농어촌, 발전소 주변) is a
            // local restriction too, the same test as for 공공기관 (audit M9).
            if texts
                .iter()
                .any(|t| has_place_residence_condition(t) || has_vicinity_wording(t))
            {
                Scope::regions(unknown())
            } else {
                Scope {
                    regions: vec![],
                    districts: None,
                    note: participating.then_some(NOTE_PARTICIPATING_MUNICIPALITIES),
                }
            }
        }
        // Some public agencies are regional. Explicit province jurisdiction
        // remains local; absent evidence is not enough to claim nationwide.
        _ => Scope::regions(province_label(provider, None)),
    }
}

/// The regions of the 광역시도 or 시군구 whose 보조금24 `소관기관코드` this is.
pub fn agency_code_region(code: &str) -> Option<Vec<String>> {
    let (_, label) = crate::agency_codes::LOCAL_GOVERNMENTS
        .iter()
        .find(|(c, _)| *c == code)?;
    let regions = province_label(label, None);
    (regions != [UNKNOWN]).then_some(regions)
}

/// A local body's region from its parent government's agency code and from
/// its own name. The name is kept when it narrows the code ("전남신용보증재단"
/// under 전남광주통합특별시 is jeonnam); the code wins when the name points
/// elsewhere ("세종문화회관" is Seoul's) or names nothing ("광주도시관리공사"
/// is 경기도 광주시).
fn local_body_scope(provider: &str, code: &str) -> Scope {
    let by_name = local_body_region(provider);
    let by_code = agency_code_region(code);
    let (regions, note) = match (by_name, by_code) {
        (Some(name), Some(code)) if name.iter().all(|r| code.contains(r)) => {
            (name, NOTE_PROVIDER_NAME_PREFIX)
        }
        (_, Some(code)) => (code, NOTE_PROVIDER_AGENCY_CODE),
        (Some(name), None) => (name, NOTE_PROVIDER_NAME_PREFIX),
        (None, None) => return Scope::regions(unknown()),
    };
    Scope {
        regions,
        districts: None,
        note: Some(note),
    }
}

struct ResidencePatterns {
    /// Living arrangements, not places: "부모님과 별도 거주", "동일 주소지에
    /// 거주하는 가족", "함께 거주", plus documents ("주민등록등본") and the
    /// 전입신고 act itself.
    household: regex::Regex,
    /// "K-패스 사업에 참여하고 있는 지자체에 주민등록": wherever the
    /// programme runs, not one region.
    participating: regex::Regex,
    /// Words that only ever describe a local restriction.
    local: regex::Regex,
    /// Words that restrict residence when a place is also named.
    residence: regex::Regex,
    /// Site-bound wording: "발전소 주변지역", "반경 5km", "공급지역",
    /// "사업장 주변", "읍, 면, 동 지역", "지사 소재지".
    vicinity: regex::Regex,
    /// Terms that contain a vicinity word but name no place, removed before
    /// the vicinity test (보조금24, 2026-10-16): 건강보험 "지역가입자",
    /// "농어촌특별세", "농어촌학자금융자", "농어촌도로", "농어촌 일손 돕기".
    neutral: regex::Regex,
}

fn residence_patterns() -> &'static ResidencePatterns {
    static PATTERNS: std::sync::OnceLock<ResidencePatterns> = std::sync::OnceLock::new();
    PATTERNS.get_or_init(|| {
        let compile = |p: &str| regex::Regex::new(p).expect("static pattern compiles");
        ResidencePatterns {
            household: compile(concat!(
                r"(?:별도|함께|동일한?|같은|따로|같이)\s*(?:의\s*)?(?:주소지?|주택|세대|가구)?\s*(?:에서|에|로|으로)?\s*(?:거주|주소)",
                r"|주민등록\s*(?:등본|초본|표|번호|상)",
                r"|전입\s*신고",
            )),
            participating: compile(
                r"참여\s*(?:하고\s*있는|하는|한|중인)?\s*(?:지자체|지방자치단체|시[·ㆍ]?군[·ㆍ]?구|시[·ㆍ]?도|지역)\s*(?:에|의)?\s*(?:주민등록|거주|주소)",
            ),
            local: compile(concat!(
                r"관내|해당\s*지역|지역\s*내|출신|주소지|비?수도권",
                r"|(?:해당|관할|소재지?)\s*(?:시|군|구|시[ㆍ·]?군[ㆍ·]?구|시[ㆍ·]?도|읍[ㆍ·]?면[ㆍ·]?동?|지자체|지방자치단체)(?:의|에|내)",
                // "해당 시도 주민", "관할 시·군·구 거주": no particle needed for a
                // multi-syllable unit, which "해당 시기" or "해당 구간" never is.
                r"|(?:해당|관할)\s*(?:시[ㆍ·]?군[ㆍ·]?구|시[ㆍ·]?도|읍[ㆍ·]?면[ㆍ·]?동?|지자체|지방자치단체)(?:[^가-힣]|$)",
                r"|지역에\s*(?:주민등록|거주|주소)",
            )),
            residence: compile(r"거주|소재|주민등록|전입"),
            neutral: compile(r"지역\s*가입자|농어촌\s*(?:특별세|학자금|도로|일손|일자리)"),
            vicinity: compile(concat!(
                // "반경 5km", not "일반경영"; "학교 인근", not "외국인근로자":
                // 주변/인근/부근 stand as their own word or open "…지역".
                r"(?:^|[^가-힣])(?:주변|인근|부근)|(?:주변|인근|부근)\s*지역|반경\s*[0-9]|일원|소재지|연고지|(?:지자체|지방자치단체|지역)\s*별",
                // Statutory and service area types (접경지역, 폐광지역,
                // 인구감소지역 under 인구감소지역 지원 특별법 …).
                r"|(?:사업|공급|대상|시범|피해|접경|낙후|도서|산간|농어촌|농촌|어촌|산촌|폐광|인구감소)\s*지역",
                r"|[읍면동리]\s*(?:지역|주민)|지역\s*(?:주민|학교|소재|출신)",
                // Regional quotas, relocated agencies and site impacts.
                r"|지역\s*인재|혁신도시|이전\s*(?:공공)?기관|위치한|있는\s*지역|벽지|농어촌",
                r"|[영호]남권|충청권|지방\s*(?:소재|대)|일대|경과지|하류|소음\s*(?:피해|대책|영향|지역)|원전",
            )),
        }
    })
}

fn participating_municipalities(text: &str) -> bool {
    residence_patterns().participating.is_match(text)
}

/// Whether eligibility text restricts residence to a place: a local-only word
/// (관내, 해당 지역, 주소지, 수도권), or a residence word together with a named
/// sido or 시·군·구. Living arrangements ("부모님과 별도 거주"), documents
/// ("주민등록등본"), 전입신고 and "참여 지자체 주민등록" are not places.
pub fn has_place_residence_condition(text: &str) -> bool {
    let patterns = residence_patterns();
    let text = patterns.participating.replace_all(text, " ");
    let text = patterns.household.replace_all(&text, " ");
    if patterns.local.is_match(&text) {
        return true;
    }
    patterns.residence.is_match(&text) && names_a_place(&text)
}

/// The stricter test for a 공공기관's text: a residence condition, any named
/// sido or 시·군·구 (with or without a residence verb), or vicinity wording.
/// Living arrangements and documents are still not places.
pub fn has_local_scope_hint(text: &str) -> bool {
    if has_place_residence_condition(text) {
        return true;
    }
    let patterns = residence_patterns();
    let text = patterns.participating.replace_all(text, " ");
    let text = patterns.household.replace_all(&text, " ");
    has_vicinity_wording(&text) || names_a_place(&text)
}

/// Vicinity or service-area wording only, without the place-name test.
pub fn has_vicinity_wording(text: &str) -> bool {
    let patterns = residence_patterns();
    patterns
        .vicinity
        .is_match(&patterns.neutral.replace_all(text, " "))
}

/// A sido name (full or short) or a 시·군·구 name standing as its own word:
/// the character before it is not Hangul, so "원가구" is not "가구" and
/// "도중구간" is not 중구.
fn names_a_place(text: &str) -> bool {
    use crate::district_codes::DISTRICTS;
    let standalone = |name: &str| {
        text.match_indices(name).any(|(at, _)| {
            text[..at]
                .chars()
                .next_back()
                .is_none_or(|c| !('가'..='힣').contains(&c))
        })
    };
    SIDO_PREFIXES.iter().any(|(short, _)| standalone(short))
        || DISTRICTS.iter().any(|(_, sido, name)| {
            standalone(sido)
                || name
                    .split(' ')
                    .any(|part| part.chars().count() >= 2 && standalone(part))
        })
}

pub fn scholarship(payload: &Value) -> Vec<String> {
    scholarship_scope(payload).regions
}

/// Residence scope of a 학자금지원정보 row from `지역거주여부 상세내용`, the
/// row's own residence condition: 제한없음 is nationwide; a bare sido label
/// ("경기도") is that sido; any other text stays unknown.
///
/// Free text is not read for a place (review B2, verify V-H1 rounds 1-2):
/// resolving a place only hides the row from everyone outside it, so it
/// needs the precision of a hide, and no reading of the free text reached
/// it. A deny-list of residents and a list of negation and route words were
/// each defeated by ordinary phrasings ("…에 주소를 둔 보호자의 자녀",
/// "영천시 외 지역", "… 및 …"), and the strict whole-clause template kept
/// none of the four applicant-only positives. The text itself is retained
/// for the detail page.
pub fn scholarship_scope(payload: &Value) -> Scope {
    let residence = payload["지역거주여부 상세내용"]
        .as_str()
        .unwrap_or("")
        .trim();
    let compact: String = residence.chars().filter(|c| !c.is_whitespace()).collect();
    if matches!(
        compact.as_str(),
        "제한없음" | "지역제한없음" | "해당없음" | "전국" | "거주지역제한없음"
    ) {
        return Scope::regions(vec![]);
    }
    if let Some(code) = mazimi_core::canonical_region_code(residence) {
        return Scope::regions(vec![code.to_string()]);
    }
    Scope::regions(unknown())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Every region a source can resolve to is shown in Korean. A code added
    /// here without a label in mazimi_core fails this test.
    #[test]
    fn every_resolved_region_has_a_label() {
        use crate::district_codes::DISTRICTS;
        let prefixes = (0..100).map(|n| format!("{n:02}"));
        let districts = DISTRICTS.iter().map(|(code, ..)| (*code).to_owned());
        let mut seen = BTreeSet::new();
        for code in prefixes.chain(districts) {
            if let Some(region) = district_region(&code) {
                seen.insert(region);
            }
        }
        assert_eq!(seen.len(), mazimi_core::agent::REGION_CODES.len());
        for region in seen {
            assert!(
                mazimi_core::agent::REGION_CODES.contains(&region),
                "{region} is not canonical"
            );
            let label = mazimi_core::region_display(region);
            assert!(
                label.as_deref().is_some_and(|l| l != region && l != "기타"),
                "{region} has no Korean label"
            );
        }
        assert_eq!(mazimi_core::region_display(UNKNOWN), None);
    }

    #[test]
    fn yongin_is_not_busan_and_multi_region_codes_are_deduplicated() {
        assert_eq!(youth(&json!({"zipCd":"41461,41463,41465"})), ["gyeonggi"]);
        assert_eq!(
            youth(&json!({"zipCd":"26110,26140,27110"})),
            ["busan", "daegu"]
        );
        assert_eq!(
            youth(&json!({"zipCd":"51110,42110,52111,45111"})),
            ["gangwon", "jeonbuk"]
        );
    }

    #[test]
    fn missing_or_changed_codes_never_become_nationwide() {
        for raw in [
            json!({}),
            json!({"zipCd":""}),
            json!({"zipCd":"99110"}),
            json!({"zipCd":"26110,99110"}),
            json!({"zipCd":"261"}),
        ] {
            assert_eq!(youth(&raw), [UNKNOWN]);
        }
        assert!(youth(&json!({"zipCd":"00000"})).is_empty());
        assert_eq!(
            youth(&json!({"zipCd":"11110,26110,27110"})),
            ["busan", "daegu", "seoul"]
        );
    }

    #[test]
    fn merged_districts_preserve_saved_region_compatibility() {
        assert_eq!(youth(&json!({"zipCd":"12210,12330"})), ["gwangju"]);
        assert_eq!(youth(&json!({"zipCd":"12110,12870"})), ["jeonnam"]);
        assert_eq!(
            province_label("전남광주통합특별시", Some("광산구")),
            ["gwangju"]
        );
        assert_eq!(
            province_label("전남광주통합특별시 목포시", None),
            ["jeonnam"]
        );
    }

    #[test]
    fn municipal_provider_scope_and_central_scope_are_distinct() {
        assert_eq!(
            government(&json!({"소관기관유형":"시군구","소관기관명":"경기도 용인시"})),
            ["gyeonggi"]
        );
        assert_eq!(
            government(&json!({"소관기관유형":"시군구","소관기관명":"경기도 광주시"})),
            ["gyeonggi"]
        );
        assert_eq!(
            government(&json!({"소관기관유형":"교육청","소관기관명":"부산광역시교육청"})),
            ["busan"]
        );
        assert!(
            government(&json!({"소관기관유형":"중앙행정기관","소관기관명":"교육부"})).is_empty()
        );
        assert_eq!(
            government(&json!({"소관기관유형":"시군구","소관기관명":"알수없는시"})),
            [UNKNOWN]
        );
        assert_eq!(province_label("서울대학교", None), [UNKNOWN]);
    }

    /// Audit M2: a 지역거주여부 text that names one place where the
    /// applicant (or a parent) must live is that place (real rows,
    /// tests/fixtures/condrules/programmes*.jsonl); several places, an
    /// ambiguous name or no place stay unknown.
    /// Verify V-H1 round 2 (fallback): only a bare sido label resolves. The
    /// four free-text positives of the earlier readers stay unknown too:
    /// before 3 resolved, after 0 (and the fixture corpus has no bare label).
    #[test]
    fn only_a_bare_sido_label_resolves_a_scholarship_residence() {
        let scope = |text: &str| scholarship_scope(&json!({"지역거주여부 상세내용": text}));
        for (text, region) in [
            ("경기도", "gyeonggi"),
            ("부산광역시", "busan"),
            ("서울", "seoul"),
        ] {
            assert_eq!(scope(text).regions, [region], "{text}");
            assert_eq!(scope(text).note, None, "{text}");
        }
        for text in [
            "○ 장학생 선발계획 공고일 현재 1년 이상 영천시에 주소를 두고 있는 자",
            "○ 20251101 이후 다른 지역에서 음성군으로 전입한 관내 대학교 재학생",
            "○ 경기도에 주민등록상 2025.04.01 이전부터 현재까지 경기도 계속 거주)도민 (신청자본인) 중 한국장학재단 학자금 대출로 인해 한국신용정보원에 신용도판단정보가 등록된 사람",
            "신청자 본인이 영천시에 1년 이상 주소를 둔 자",
            "○ 도내 대학 소속 학부생",
            "부산광역시 또는 대구광역시 거주",
            "부산광역시 미거주자",
            "",
        ] {
            assert_eq!(scope(text).regions, [UNKNOWN], "{text}");
        }
        assert!(scope("해당없음").regions.is_empty());
        assert!(scope("지역 제한 없음").regions.is_empty());
    }

    /// Review B2: a residence resolves only when the applicant alone is the
    /// resident and the place is where the residence verb puts them. A
    /// guardian's residence, a school's location or a text saying residence
    /// does not matter states no applicant residence.
    #[test]
    fn another_residents_place_or_a_school_location_stays_unknown() {
        let scope = |text: &str| scholarship_scope(&json!({"지역거주여부 상세내용": text}));
        for text in [
            "○ 장학생 선발계획 공고일 현재 1년 이상 영천시에 주소를 두고 있는 자(본인 또는 보호자)",
            "○ 추천일 기준 본인 또는 부모(보호자)가 주민등록상 3년(36개월)이상 아산시에 계속 거주하는 자※ 자세한 사항은 첨부파일 또는 홈페이지 참고",
            "○ 수상 기준일 본인 또는 보호자가 남원시에 3년 이상 주민등록상 주소지를 두고 있는 대학교 학생",
            "○ 보호자가 신청일 기준으로 6개월 이상 원주시 관내에 주민등록을 두고 계속 거주하는 만 24세 이하의 둘째아 이상 자녀",
            "○ 사업 신청일 기준 주민등록상 충청북도에 1년 이상 계속해서 거주하고 있는 본인 또는 직계존속 중 한국장학재단 학자금대출 장기연체로 인하여 신용 유의정보가 등록된 자 중 만 39세 이하인 자",
            "서울특별시 소재 대학 재학생 (거주지 제한 없음)",
            "부산광역시 소재 고등학교 졸업자로서 주소지와 무관",
        ] {
            assert_eq!(scope(text).regions, [UNKNOWN], "{text}");
        }
    }

    /// Verify V-H1: a residence resolves only when its clause names no
    /// subject but the applicant, and the text states no negation and no
    /// other route. Each of these stays unknown.
    #[test]
    fn a_residence_of_anyone_but_the_applicant_alone_stays_unknown() {
        let scope = |text: &str| scholarship_scope(&json!({"지역거주여부 상세내용": text}));
        let resolved: Vec<&str> = [
            "세대주가 영천시에 1년 이상 주민등록을 둔 가정의 자녀",
            "부 또는 모가 아산시에 계속 거주하는 자",
            "가족 전원이 원주시에 주민등록",
            "친권자가 영천시에 주소를 둔 자",
            "법정대리인이 남원시에 거주하는 자",
            "영천시 소재 고등학교 졸업자 또는 영천시에 주소를 둔 자",
            "영천시 출신으로 타 지역 거주자 포함",
            "영천시에 주소를 두지 않은 자",
            // Verify round 2.
            "영천시에 주소를 둔 보호자의 자녀",
            "영천시에 거주하는 부모를 둔 학생",
            "영천시에 주민등록된 학부모의 자녀",
            "세대주와 함께 영천시에 주민등록이 되어 있는 자녀",
            "영천시 외 지역에 거주하는 자",
            "영천시 이외 지역 주민등록자",
            "영천시 거주자는 신청 불가",
            "영천시에 주소를 둔 자 및 영천시 소재 학교 재학생",
            "영천시에 거주하거나 영천시 소재 고교를 졸업한 자",
            "영천시에 주소를 둔 자나 그 자녀",
        ]
        .into_iter()
        .filter(|text| scope(text).regions != [UNKNOWN])
        .collect();
        assert!(resolved.is_empty(), "resolved: {resolved:?}");
    }

    /// Audit M8 and review B6: one unknown 시·군·구 code must neither widen
    /// the scope to its whole sido nor hide the row from everyone. The sido
    /// is decided by the code; only the district is not: the code is kept
    /// as an unresolved entry, which the district check reads as unknown
    /// for that sido's users. A sido-level code is not a district.
    #[test]
    fn an_unknown_district_code_keeps_its_sido_and_leaves_the_district_open() {
        let scope = youth_scope(&json!({"zipCd": "41461,41999"}));
        assert_eq!(scope.regions, ["gyeonggi"]);
        assert_eq!(scope.note, Some(NOTE_UNKNOWN_DISTRICT_CODE));
        let listed = scope.districts.clone().unwrap();
        assert!(listed.iter().any(|c| c == "41461"), "{listed:?}");
        let read = mazimi_core::structural::programme_districts(&listed);
        assert!(read.unresolved_in("gyeonggi") && !read.unresolved_in("busan"));

        let busan = youth_scope(&json!({"zipCd": "26110,26140,26999"}));
        assert_eq!(busan.regions, ["busan"]);
        // A retired code with listed successors is their union, never
        // narrower than the old district (인천 중구 → 제물포구 + 영종구).
        let old_junggu = youth_scope(&json!({"zipCd": "28110"}));
        assert_eq!(old_junggu.regions, ["incheon"]);
        let listed = old_junggu.districts.unwrap();
        for successor in ["28125", "28155"] {
            assert!(listed.iter().any(|c| c == successor), "{listed:?}");
        }
        assert_eq!(old_junggu.note, None);
        let sido = youth_scope(&json!({"zipCd": "26000"}));
        assert_eq!(sido.regions, ["busan"]);
        let known = youth_scope(&json!({"zipCd": "41461"}));
        assert_eq!(known.regions, ["gyeonggi"]);
        assert!(known.districts.is_some());
    }

    /// Audit M9: a ministry programme limited to an area named by its kind
    /// (인구감소지역, 농어촌) is not nationwide; a word that merely contains a
    /// vicinity term (외국인근로자) is not an area.
    #[test]
    fn a_ministry_area_condition_without_a_place_name_is_unknown() {
        let ministry = |target: &str| {
            government(
                &json!({"소관기관유형": "중앙행정기관", "소관기관명": "행정안전부", "지원대상": target}),
            )
        };
        assert_eq!(ministry("인구감소지역 거주 청년"), [UNKNOWN]);
        assert_eq!(ministry("농어촌 거주 청년"), [UNKNOWN]);
        assert!(ministry("○ 외국인근로자").is_empty());
        assert!(ministry("만 19세 이상 34세 이하 청년").is_empty());
        let public = |target: &str| {
            government(
                &json!({"소관기관유형": "공공기관", "소관기관명": "근로복지공단", "지원대상": target}),
            )
        };
        assert!(public("○ 외국인근로자").is_empty());
        assert_eq!(public("댐주변지역 주민"), [UNKNOWN]);
        assert_eq!(public("발전소 인근 주민"), [UNKNOWN]);
    }

    /// Follow-up (review L5): ingestion reads a retired code with one whole
    /// successor as the successor (군위군 47720 → 대구 27720, from the
    /// reviewed table scripts/district-successors.tsv); a split one (인천
    /// 중구 28110) stays unresolved.
    #[test]
    fn a_retired_code_with_a_whole_successor_reads_as_the_successor() {
        let scope = youth_scope(&json!({"zipCd": "47720"}));
        assert_eq!(scope.regions, ["daegu"]);
        assert!(scope
            .districts
            .as_ref()
            .is_some_and(|d| d.iter().any(|c| c == "27720")));
        assert_eq!(canonical_district("47720"), Some("27720"));
        assert_eq!(canonical_district("28110"), None);
        use mazimi_core::structural::{current_district, Current};
        // The serving reader keeps a retired code unresolved: a stored row
        // may still pair 47720 with 경북 until it is renormalized.
        assert_eq!(current_district("47720"), Current::Retired);
        assert_eq!(current_district("28110"), Current::Retired);
        // Every successor row names a retired code and a current one.
        for (retired, successor, relation) in mazimi_core::districts::SUCCESSORS {
            assert!(
                !crate::district_codes::DISTRICTS
                    .iter()
                    .any(|(c, ..)| c == retired),
                "{retired}"
            );
            assert!(
                crate::district_codes::DISTRICTS
                    .iter()
                    .any(|(c, ..)| c == successor),
                "{successor}"
            );
            assert!(["same", "into", "part"].contains(relation), "{relation}");
        }
    }

    #[test]
    fn scholarships_do_not_infer_nationwide_from_missing_residence() {
        assert_eq!(scholarship(&json!({})), [UNKNOWN]);
        assert_eq!(
            scholarship(&json!({"지역거주여부 상세내용":"경기도"})),
            ["gyeonggi"]
        );
        assert!(scholarship(&json!({"지역거주여부 상세내용":"지역 제한 없음"})).is_empty());
        for description in [
            "부산광역시 또는 대구광역시 거주",
            "부산광역시 미거주자",
            "부산광역시 출신 부모의 자녀",
        ] {
            assert_eq!(
                scholarship(&json!({"지역거주여부 상세내용":description})),
                [UNKNOWN]
            );
        }
        assert_eq!(
            government(
                &json!({"소관기관유형":"중앙행정기관", "소관기관명":"교육부", "지원대상":"용인시에 거주하는 학생"})
            ),
            [UNKNOWN]
        );
    }

    #[test]
    fn provider_districts_use_current_codes_with_retired_aliases() {
        assert_eq!(
            provider_districts("부산광역시 수영구"),
            Some(vec!["26500".into()])
        );
        let suwon = provider_districts("경기도 수원시").unwrap();
        assert!(["41110", "41111", "41113", "41115", "41117"]
            .iter()
            .all(|c| suwon.contains(&c.to_string())));
        assert_eq!(
            provider_districts("경기도 수원시 영통구"),
            Some(vec!["41117".into()])
        );
        // 광주 북구 under either province name; 순천 keeps 46150.
        for label in ["광주광역시 북구", "전남광주통합특별시 북구"] {
            assert_eq!(
                provider_districts(label),
                Some(vec!["12300".into(), "29170".into()]),
                "{label}"
            );
        }
        assert_eq!(
            provider_districts("전남광주통합특별시 순천시"),
            Some(vec!["12150".into(), "46150".into()])
        );
        assert!(provider_districts("강원특별자치도 춘천시")
            .unwrap()
            .contains(&"42110".to_string()));
        // 경기도 광주시 is not 광주광역시.
        assert_eq!(
            provider_districts("경기도 광주시"),
            Some(vec!["41610".into()])
        );
        for label in [
            "부산광역시",
            "국토교통부",
            "부산광역시 청년정책과",
            "알수없는도 수영구",
        ] {
            assert_eq!(provider_districts(label), None, "{label}");
        }
    }

    #[test]
    fn government_scope_keeps_municipal_districts_and_sido_wide_marker() {
        let gu =
            government_scope(&json!({"소관기관유형":"시군구","소관기관명":"부산광역시 수영구"}));
        assert_eq!(gu.regions, ["busan"]);
        assert_eq!(gu.districts, Some(vec!["26500".into()]));
        let sido = government_scope(&json!({"소관기관유형":"광역시도","소관기관명":"부산광역시"}));
        assert_eq!(sido.districts, Some(vec![]));
        let ministry =
            government_scope(&json!({"소관기관유형":"중앙행정기관","소관기관명":"교육부"}));
        assert_eq!(ministry.districts, None);
    }

    /// 지원대상 texts of 청년월세 지원 (c7323b31) and K-패스 (c305f28a), 2026-10-12.
    #[test]
    fn living_arrangements_and_participating_municipalities_are_not_places() {
        let rent = "ㅇ (대상) ｢청년기본법｣상 청년(만 19~34세 이하*)으로 부모님과 별도 거주하는 무주택자\r\n※ 제외대상 : ▲공공주택특별법에 따른 공공임대주택 거주 ▲1실(방)에 다수가 거주하는 방식의 전대차인 경우 등\r\n - 임차료 계좌입금 증빙내역 제출(전입신고 필수)\r\n    * (원가구) 청년 + 부모 + 부모와 동일 주소지에 거주하는 그 외 「민법」상 가족\r\n  ※ 형제·자매 관계의 청년 2인 이상이 동일한 주택에 함께 거주하는 경우 가구당 1명에게만 지원";
        let scope = government_scope(
            &json!({"소관기관유형":"중앙행정기관","소관기관명":"국토교통부","지원대상":rent,"선정기준":rent}),
        );
        assert_eq!(scope, Scope::default());
        let kpass = "○ 지원조건\r\n - K-패스 사업에 참여하고 있는 지자체에 주민등록이 되어있는 자\r\n - 만 19세 이상의 국민";
        let scope = government_scope(
            &json!({"소관기관유형":"중앙행정기관","소관기관명":"국토교통부","지원대상":kpass}),
        );
        assert!(scope.regions.is_empty());
        assert_eq!(scope.note, Some(NOTE_PARTICIPATING_MUNICIPALITIES));
        for restricted in [
            "용인시에 거주하는 학생",
            "관내 거주 청년",
            "비수도권 소재 중소기업 재직자",
            "부산 거주 대학생",
            "해당 지역 주민",
            "상수원관리지역 지정 전부터 해당 시ㆍ군ㆍ구의 주민등록이 되어 있는 주민",
            "농어촌지역에 주민등록 주소를 두고 6개월 이상 거주",
        ] {
            assert_eq!(
                government(&json!({"소관기관유형":"중앙행정기관","지원대상":restricted})),
                [UNKNOWN],
                "{restricted}"
            );
        }
        // A document is not a residence condition.
        assert!(!has_place_residence_condition("주민등록등본 1부 제출"));
    }

    /// Provider names from 보조금24 (2026-10-12): 39e32154, 1db5194e and others.
    #[test]
    fn local_bodies_resolve_by_the_province_or_district_they_are_named_after() {
        for (name, expected) in [
            ("재단법인전남인재평생교육진흥원", vec!["jeonnam"]),
            (
                "재단법인세종특별자치시인재육성평생교육진흥원",
                vec!["sejong"],
            ),
            ("(재)부산여성가족과평생교육진흥원", vec!["busan"]),
            ("재단법인충남경제진흥원", vec!["chungnam"]),
            ("오산시시설관리공단", vec!["gyeonggi"]),
            ("재단법인영등포구미래교육재단", vec!["seoul"]),
            ("인천시설공단", vec!["incheon"]),
            ("재단법인종로복지재단", vec!["seoul"]),
        ] {
            assert_eq!(
                local_body_region(name),
                Some(expected.iter().map(|s| s.to_string()).collect()),
                "{name}"
            );
        }
        // 광주 is 광주광역시 or 경기도 광주시; 고성군 is in 강원 and 경남.
        for name in ["광주도시관리공사", "고성군시설관리공단", "한국주택금융공사"]
        {
            assert_eq!(local_body_region(name), None, "{name}");
        }
        assert_eq!(
            government(
                &json!({"소관기관유형":"지방출자_출연기관","소관기관명":"재단법인전남인재평생교육진흥원"})
            ),
            ["jeonnam"]
        );
        // A national agency is never resolved by the city in its name.
        assert!(government(
            &json!({"소관기관유형":"공공기관","소관기관명":"인천국제공항공사",
                "지원대상":"유아 동반 여객"})
        )
        .is_empty());
    }

    /// 보조금24 공공기관 rows left unknown by v7 (production, 2026-10-16).
    #[test]
    fn public_institutions_are_nationwide_unless_the_text_names_a_place_or_vicinity() {
        let gov = |provider: &str, target: &str| {
            government_scope(
                &json!({"소관기관유형":"공공기관","소관기관명":provider,"지원대상":target}),
            )
        };
        for (provider, target) in [
            ("서민금융진흥원", "연소득 3,500만원 이하(신용점수 무관)이거나, 연소득 4,500만원 이하이면서 신용점수 하위 20% 이하이신 분"),
            ("대한법률구조공단", "○ 북한이탈주민\n - 건강보험료 본인부담금 합산액이 기준중위소득 125% 이하인 자"),
            ("한국동서발전(주)", "방문을 원하는 모든 국민"),
            ("한국승강기안전공단", "○ 전국 초등학교(초등학생 1학년)"),
            ("한국도로공사", "○ 유공자 또는 당해 유공자와 동일한 세대별 주민등록표에 등재된 세대원이 소유하는 차량"),
            ("한국보훈복지의료공단", "○ 국민 모두 가능(다만, 일반국민은 진료비 감면 혜택 없음)"),
        ] {
            let scope = gov(provider, target);
            assert!(scope.regions.is_empty(), "{provider}: {target}");
            assert_eq!(scope.note, Some(NOTE_NATIONAL_PUBLIC_INSTITUTION));
        }
        for (provider, target) in [
            ("한국전력공사", "□ 송전선로 및 변전소 주변지역 주민"),
            ("한국서부발전(주)", "발전소 주변지역(발전기 설치지점 반경 5km 이내의 육지 및 섬지역이 속하는 읍, 면, 동 지역)"),
            ("한국중부발전(주)", "충남 보령시 오천면, 주교면, 천북면, 주포면의 주택용 및 산업용 전력고객"),
            ("한국중부발전(주)", "충남 서천군 서면 주민의 주택용 및 산업용 전력고객"),
            ("(주)강원랜드", "고한/사북/남면지역 거주 아카데미 소속 청소년"),
            ("한국수자원공사", "댐주변지역 및 K-water 지사 소재지의 중·고등학생"),
            ("한국지역난방공사", "한국지역난방공사 공급지역에 거주하는 개별 세대 중"),
            ("한국장학재단", "각 지자체별 지원 기준에 맞는 대학생 혹은 대학원생"),
            ("창업진흥원", "비수도권 소재 업력 10년 미만의 신산업분야 스타트업"),
            ("한국도로공사", "○ 대구·경북지역 대학 창업동아리"),
        ] {
            assert_eq!(gov(provider, target).regions, [UNKNOWN], "{provider}: {target}");
        }
        // A place named only in 지원내용 is enough to stay unknown.
        let scope = government_scope(
            &json!({"소관기관유형":"공공기관","소관기관명":"국립공원공단",
            "지원대상":"○ 장애인 관련 증빙 서류를 제출한 자",
            "지원내용":"○ 이용료 감면(30%)\n - 「주민등록법」에 따라 (구)광주광역시에 주소를 둔 사람"}),
        );
        assert_eq!(scope.regions, [UNKNOWN]);
        // Ministries keep the v7 rule: the vicinity words alone do not apply.
        assert!(government(
            &json!({"소관기관유형":"중앙행정기관","소관기관명":"국토교통부","지원대상":"방문을 원하는 모든 국민"})
        )
        .is_empty());
    }

    /// Review follow-up: wording a later 보조금24 row could use.
    #[test]
    fn public_institution_review_cases_stay_unknown() {
        let gov = |title: &str, target: &str| {
            government_scope(
                &json!({"소관기관유형":"공공기관","소관기관명":"한국전력공사",
                "서비스명":title,"지원대상":target}),
            )
            .regions
        };
        for target in [
            "지역인재 채용 대상자",
            "혁신도시 이전공공기관 소재 지역 청년",
            "본사가 위치한 지역의 대학생",
            "사업장이 있는 지역 주민",
            "도서·벽지 학교 학생",
            "농어촌 학교 재학생",
            "호남권 대학 재학생",
            "충청권 소재 중소기업",
            "지방 소재 대학 재학생",
            "지방대 졸업예정자",
            "발전소 일대 주민",
            "송전선로 경과지 마을",
            "댐 하류 주민",
            "공항 소음 피해 가구",
            "원전 인접 마을 학생",
            "해당 시도 주민",
            "관할 시·군·구 거주 청년",
        ] {
            assert_eq!(gov("장학금 지원", target), [UNKNOWN], "{target}");
        }
        // Vicinity in the service name alone; no text at all.
        assert_eq!(gov("발전소 주변지역 장학금 지원", "중·고등학생"), [UNKNOWN]);
        assert_eq!(
            government(
                &json!({"소관기관유형":"공공기관","소관기관명":"한국마사회","서비스명":"말 사랑 캠프"})
            ),
            [UNKNOWN]
        );
        assert_eq!(gov("장학금 지원", " "), [UNKNOWN]);
        // Terms that only contain a vicinity word (production texts).
        for (title, target) in [
            (
                "일반경영안정자금",
                "｢소상공인기본법｣제2조의 소상공인(업력 무관)",
            ),
            (
                "학자금대출 체크카드 포인트 상환",
                "※ 정부보증학자금대출 및 농어촌학자금융자를 제외한 재단 내 대출 상품",
            ),
            (
                "장애인 건강보험료 경감",
                "장애인복지법상의 등록장애인이 있는 지역가입자 세대",
            ),
            (
                "건설노동자 종합 건강검진 지원",
                "○ 분진, 소음 등 건강에 유해한 근로환경",
            ),
            (
                "주택담보 노후연금",
                "부부 중 1인 만 55세 이상. 농어촌특별세 면제",
            ),
        ] {
            assert!(gov(title, target).is_empty(), "{title}");
        }
        // A venue in the name is not a residence scope.
        assert!(gov("발전소 시설 견학(당진)", "방문을 원하는 모든 국민").is_empty());
        // The particle-less form needs a multi-syllable unit.
        for text in [
            "해당 시기 신청자",
            "해당 시설 이용자",
            "해당 구간 통행 차량",
        ] {
            assert!(!has_place_residence_condition(text), "{text}");
        }
        assert!(has_place_residence_condition("해당 시도 주민"));
    }

    #[test]
    fn merged_education_office_is_its_sido() {
        let scope = government_scope(
            &json!({"소관기관유형":"교육청","소관기관명":"전남광주통합특별시교육청","소관기관코드":"7140000"}),
        );
        assert_eq!(scope.regions, ["gwangju", "jeonnam"]);
        assert_eq!(scope.districts, Some(vec![]));
        assert_eq!(
            government(&json!({"소관기관유형":"교육청","소관기관명":"부산광역시교육청"})),
            ["busan"]
        );
    }

    /// Local bodies by 소관기관코드 (production names and codes, 2026-10-16).
    #[test]
    fn local_bodies_use_the_parent_government_code() {
        let body = |kind: &str, name: &str, code: &str| {
            government_scope(&json!({"소관기관유형":kind,"소관기관명":name,"소관기관코드":code}))
        };
        for (kind, name, code, expected, note) in [
            // 경기도 광주시, not 광주광역시.
            (
                "지방공기업",
                "광주도시관리공사",
                "5540000",
                vec!["gyeonggi"],
                NOTE_PROVIDER_AGENCY_CODE,
            ),
            (
                "지방출자_출연기관",
                "재단법인광주시문화재단",
                "5540000",
                vec!["gyeonggi"],
                NOTE_PROVIDER_AGENCY_CODE,
            ),
            (
                "지방공기업",
                "광주교통공사",
                "6130000",
                vec!["gwangju", "jeonnam"],
                NOTE_PROVIDER_AGENCY_CODE,
            ),
            // 경상남도 고성군, not 강원 고성군.
            (
                "지방출자_출연기관",
                "재단법인고성교육재단",
                "5420000",
                vec!["gyeongnam"],
                NOTE_PROVIDER_AGENCY_CODE,
            ),
            (
                "지방출자_출연기관",
                "(주)벡스코",
                "6260000",
                vec!["busan"],
                NOTE_PROVIDER_AGENCY_CODE,
            ),
            (
                "지방출자_출연기관",
                "(재)한국전통문화전당",
                "4641000",
                vec!["jeonbuk"],
                NOTE_PROVIDER_AGENCY_CODE,
            ),
            // The name names a different sido: Seoul's 세종문화회관.
            (
                "지방출자_출연기관",
                "재단법인세종문화회관",
                "6110000",
                vec!["seoul"],
                NOTE_PROVIDER_AGENCY_CODE,
            ),
            // The name narrows the merged province code.
            (
                "지방출자_출연기관",
                "전남신용보증재단",
                "6130000",
                vec!["jeonnam"],
                NOTE_PROVIDER_NAME_PREFIX,
            ),
            // No code in the table: the name alone, as before.
            (
                "지방공기업",
                "오산시시설관리공단",
                "",
                vec!["gyeonggi"],
                NOTE_PROVIDER_NAME_PREFIX,
            ),
        ] {
            let scope = body(kind, name, code);
            assert_eq!(scope.regions, expected, "{name}");
            assert_eq!(scope.note, Some(note), "{name}");
            assert_eq!(scope.districts, None, "{name}");
        }
        // Ambiguous name and an unknown code: never guessed.
        assert_eq!(
            body("지방공기업", "광주도시관리공사", "9999999").regions,
            [UNKNOWN]
        );
        assert_eq!(agency_code_region("6110000"), Some(vec!["seoul".into()]));
        assert_eq!(agency_code_region("5540000"), Some(vec!["gyeonggi".into()]));
    }

    /// Every table label resolves to one region set, and codes are unique.
    #[test]
    fn agency_code_table_resolves_every_entry() {
        let mut seen = BTreeSet::new();
        for (code, label) in crate::agency_codes::LOCAL_GOVERNMENTS {
            assert!(seen.insert(*code), "{code} twice");
            assert!(
                code.len() == 7 && code.bytes().all(|b| b.is_ascii_digit()),
                "{code}"
            );
            assert!(agency_code_region(code).is_some(), "{code} {label}");
        }
        assert_eq!(seen.len(), 243);
    }

    #[test]
    fn youth_districts_and_the_registrar_scope_contradiction() {
        let scope = youth_scope(&json!({"zipCd":"26410,26500"}));
        assert_eq!(scope.regions, ["busan"]);
        assert_eq!(scope.districts, Some(vec!["26410".into(), "26500".into()]));
        let merged = youth_scope(&json!({"zipCd":"12300"}));
        assert_eq!(merged.districts, Some(vec!["12300".into(), "29170".into()]));

        let every: Vec<&str> = crate::district_codes::DISTRICTS
            .iter()
            .map(|(c, ..)| *c)
            .collect();
        let all = every.join(",");
        let district_registrar =
            youth_scope(&json!({"zipCd": all, "rgtrInstCdNm": "충청남도 서산시"}));
        assert_eq!(district_registrar.regions, [UNKNOWN]);
        assert_eq!(district_registrar.note, Some(NOTE_REGISTRAR_SCOPE_CONFLICT));
        let ministry = youth_scope(&json!({"zipCd": all, "rgtrInstCdNm": "국토교통부 국토도시실"}));
        assert_eq!(ministry.regions.len(), 17);
        assert_eq!(ministry.districts, None);
        // An unknown code never yields a silently partial district list: it
        // stays in the list as an unresolved entry of its sido (review B6).
        assert_eq!(
            youth_scope(&json!({"zipCd":"26410,26999"})).districts,
            Some(vec!["26410".into(), "26999".into()])
        );
    }

    /// The web and mobile 시·군·구 pickers send these codes as `city_code`.
    /// Each must be one the core reads as a district of its region; the 2026
    /// 12xxx (전남광주통합특별시) codes are not, so those rows carry their
    /// 29xxx/46xxx aliases. Regenerate with `node scripts/gen-district-codes.mjs`.
    #[test]
    fn client_district_lists_match_core() {
        use crate::district_codes::DISTRICTS;
        use mazimi_core::structural::{district_code, district_region};
        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
        let read = |path: &str| std::fs::read_to_string(format!("{root}/{path}")).unwrap();
        let web = read("apps/web/src/lib/districts.generated.ts");
        let mobile = read("apps/mobile/constants/districts.generated.ts");
        assert_eq!(web, mobile, "the two generated copies differ");

        let mut region: Option<&str> = None;
        let mut seen = BTreeSet::new();
        let mut emitted = Vec::new();
        for line in web.lines().map(str::trim) {
            if let Some(key) = line.strip_suffix(": [") {
                region = Some(key);
                continue;
            }
            let Some(row) = line.strip_prefix("[\"") else {
                continue;
            };
            let (code, name) = row
                .strip_suffix("\"],")
                .and_then(|r| r.split_once("\", \""))
                .unwrap_or_else(|| panic!("unparsed row {line}"));
            let region = region.expect("row outside a region");
            // Stored as sent, and read as a district of the region.
            assert_eq!(district_code(code).as_deref(), Some(code), "{code} {name}");
            assert_eq!(district_region(code), Some(region), "{code} {name}");
            // The same district, by name, as the official table.
            let current = canonical_district(code).unwrap_or_else(|| panic!("{code}"));
            let (_, sido, official) = DISTRICTS.iter().find(|(c, ..)| *c == current).unwrap();
            let official = if official.is_empty() { sido } else { official };
            assert_eq!(name, *official, "{code}");
            assert!(seen.insert(current), "{code} listed twice");
            emitted.push(code);
        }
        assert_eq!(seen.len(), DISTRICTS.len(), "a district is missing");
        // The server accepts exactly the codes the clients offer.
        emitted.sort_unstable();
        assert_eq!(mazimi_core::districts::CITY_CODES, emitted.as_slice());
        assert!(mazimi_core::structural::is_known_district("41117"));
        assert!(!mazimi_core::structural::is_known_district("26999"));
        assert!(web.contains("[\"41110\", \"수원시\"]"));
        assert!(web.contains("[\"41117\", \"수원시 영통구\"]"));
    }
}
