//! rule_v1: deterministic eligibility facts from a programme's eligibility
//! text, no model at runtime. A Rust port of the annotated prototype
//! (`rules.py`, 300 + 100 labelled programmes), limited to the kinds that
//! were exact on held-out text plus the kinds the optional profile answers
//! made checkable.
//!
//! [`extract`] returns every fact it read, each with its verbatim quote, a
//! role (`req`, `alt`, `excl`, or `unknown` when the line structure is not
//! clear) and whether the whole text was consumed (`complete`). A rule fires
//! only on unambiguous phrasing and abstains otherwise; an abstention leaves
//! the phrase in the residual text, so the programme is not complete.
//!
//! Completeness is conservative: every line's role must be known, and the
//! text left after removing the matched spans must hold only filler words
//! (no token outside [`STOP`]) and no [`CONDITION_CUES`]. When unsure, the
//! programme is not complete.

use regex::Regex;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};
use std::sync::LazyLock;

/// Source text fields the rules read, per source. Welfare list rows hold a
/// purpose summary only: their conditions are not in our data.
pub fn eligibility_fields(source_type: &str) -> &'static [&'static str] {
    match source_type {
        "gov_benefits" => &["지원대상", "선정기준"],
        "youth_center" => &["addAplyQlfcCndCn", "ptcpPrpTrgtCn", "earnEtcCn"],
        "scholarship" => &[
            "특정자격 상세내용",
            "자격제한 상세내용",
            "소득기준 상세내용",
            "성적기준 상세내용",
            "지역거주여부 상세내용",
        ],
        _ => &[],
    }
}

/// Fields that also state who qualifies but that the rules do not
/// structure: 온통청년 descriptions name targets ("○○연합회 회원", "추천받은
/// 스타트업") as often as benefits. A programme with any of them non-empty is
/// never complete.
pub fn context_fields(source_type: &str) -> &'static [&'static str] {
    match source_type {
        "youth_center" => &["plcyExplnCn", "plcySprtCn"],
        _ => &[],
    }
}

/// Fields whose content is an exclusion list by definition.
const EXCLUSION_FIELDS: [&str; 2] = ["ptcpPrpTrgtCn", "자격제한 상세내용"];
/// 온통청년 carries the age band as codes, so "청년" in its text adds nothing
/// the codes do not already check.
const YOUTH_CODE_AGE_FIELDS: [&str; 3] = ["addAplyQlfcCndCn", "ptcpPrpTrgtCn", "earnEtcCn"];
/// Kinds that may fire in an exclusion field (status and category kinds).
const EXCLUSION_FIELD_KINDS: [&str; 2] = ["business", "prior_receipt"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Req,
    Alt,
    Excl,
    Unknown,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Req => "req",
            Self::Alt => "alt",
            Self::Excl => "excl",
            Self::Unknown => "unknown",
        }
    }
}

/// One fact read from the text.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Fact {
    pub kind: &'static str,
    pub value: Value,
    pub role: Role,
    pub field: String,
    /// Verbatim substring of the field's text.
    pub quote: String,
    /// Rule id (`income_median.pct`).
    pub rule: &'static str,
    /// Byte offset of the quote in the field's text.
    #[serde(skip)]
    pub start: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Extraction {
    pub facts: Vec<Fact>,
    /// Every line had a clear role, every fact a known role, and the
    /// residual text holds no condition-like content.
    pub complete: bool,
    pub has_text: bool,
}

// ── regex helpers ───────────────────────────────────────────────────────────

fn rx(pattern: &str) -> Regex {
    Regex::new(pattern).unwrap_or_else(|e| panic!("rule_v1 pattern {pattern}: {e}"))
}

macro_rules! re {
    ($name:ident, $pattern:expr) => {
        static $name: LazyLock<Regex> = LazyLock::new(|| rx($pattern));
    };
}

re!(
    EXPLICIT_NONE,
    r"^\s*(?:해당\s?없음|해당\s?사항\s?없음|제한\s?없음|없음|-+|\.)\s*$"
);
re!(
    BULLET,
    r"^\s*(?:[○o❍ㅇ●•▶□■◦\-*※]|\d+[.)]|[①-⑩]|[가-하][.)])\s*"
);
re!(
    HEADER,
    r"^\s*(?:[○o❍ㅇ□■]\s*)?\(?(?:지원\s?대상|신청\s?자격|참여\s?조건|지원\s?자격|대상)\)?\s*[:：]?\s*$"
);
re!(
    EXCL_HEADER,
    r"^\(?(?:지원\s?)?(?:제외|제한)\s?(?:대상자?|업체|기준|요건)?\s*\)?\s*[:：]?\s*$"
);
re!(ALL_OF, r"모두\s?(?:충족|해당|에 해당|갖춘)|요건을\s?모두");
re!(INCLUSION_NOTE, r"^\s*[-※*]\s*[^,\n]{0,15}포함\)?\s*$");
re!(
    SUBJECT_OTHER,
    r"부\s?또는\s?모|(?:^|[^한])부모|보호자|본인\s?또는|직계존속|자녀가|사망자|사망일|사업장|유족|배우자|부부|남녀\s?모두|세대원|가족이"
);
re!(EXCEPTION, r"단\s?,|다만\s?,|단\)|\(단|예외");
re!(
    LINE_EXCL,
    r"제외|제한|불가|아님|아닌|지급할\s?수\s?없|않는\s?자|없는\s?자"
);
re!(EXCEPT_PHRASE, r"(?:을|를)\s?제외한");
re!(
    AGE_TAIL_OTHER,
    r"^\s?(?:인\s?)?(?:\)?\s?(?:자녀|영아|유아|아기|노인\s?세대|세대|어르신\s?세대|부부|입학자|자는\s?보호자|미만인\s?자는|아동을|아동이|자녀를|가\s?도래|이전|생))"
);
re!(AGE_PAREN_HEAD, r"(?:청년|청소년|농업인)\s?\($");
re!(AGE_CALENDAR, r"연\s?나이");
re!(PRIOR_TAIL, r"^\s?조건\s*$");
re!(KOSAF_TAIL, r"^\s?(?:이하|이내)");
re!(
    ATTR_END,
    r"(?:이하|이상|미만|이내|인|한|된|는|은|을|를|둔|있고|있으며|하며|하고|중인|있으면서)\s*$"
);
re!(SEP, r"[,/ㆍ]|\s및\s");
re!(
    PERSON_HEAD,
    r"(?:자|청년|청소년|사람|분|학생|아동|주민|군민|시민|농업인|여성|가구|세대|부부|대학생)\)?\s*$"
);
re!(REL_CLAUSE_END, r"(?:[는은인한된둔을]\s?자|자로서)\s*$");
re!(MIXED, r"또는|이거나|이나\s|혹은|내지|등(?:$|[\s,.):·])");
re!(
    LABELLED,
    r"^\s*(?:[○ㅇo❍\-]\s*)?(?:\((?:연령|나이|소득|재산|자산|순자산|거주|거주\s?조건|주소|학력|자격|요건|주택가격|[가-힣]{0,4}\s?기준)[가-힣 ,·]{0,6}\)|(?:연령|나이|소득|재산|자산|순자산|거주|거주\s?조건|주소|학력|자격|요건|주택가격|[가-힣]{0,4}\s?기준)\s?[:：])"
);
re!(LABEL_INNER_COLON, r"[가-힣]\s?[:：]\s");
re!(PRIORITY_NOTE, r"\([^()]*(?:우선|포함)\)");
re!(PRIORITY_WORD, r"우선|포함");
// A head count left in the residual ("2명 이상", "중 1인") is a condition.
re!(LEFTOVER_COUNT, r"\d+\s?(?:명|인)");
re!(
    RESIDUAL_PUNCT,
    r#"[\s\d.,:;()\[\]~\-·ㆍ/○o❍ㅇ※"'「」『』]+"#
);
// Child-age or child-kind qualifiers in front of a child count: the count
// is then of some children only.
re!(CHILD_QUALIFIER, r"세\s?(?:이하|미만)|개월|미혼|손|막내");

/// Residual words that state a condition even next to filler: any of them
/// left over means a condition the rules did not structure.
const CONDITION_CUES: [&str; 41] = [
    "거주",
    "주민등록",
    "주소",
    "전입",
    "소득",
    "재산",
    "자산",
    "제외",
    "다만",
    "이외",
    "예외",
    "학년",
    "재학",
    "졸업",
    "휴학",
    "재직",
    "취업",
    "사업",
    "창업",
    "가구",
    "세대",
    "부부",
    "자녀",
    "혼인",
    "결혼",
    "주택",
    "수급",
    "장애",
    "국적",
    "외국인",
    "남성",
    "여성",
    "학생",
    "대학",
    "고등",
    "중학",
    "성적",
    "학점",
    "추천",
    "%",
    "혼",
];

static STOP: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    "자 인 인자 사람 분 중 및 대상 지원대상 신청자격 참여조건 대상자 \
     있는 있고 있으면서 두고 둔 주민등록 주민등록상 주소지 주소 거주 거주자 거주하는 거주중인 중인 계속 계속하여 현재 \
     기준 신청일 공고일 이상 이하 미만 해당 실제 하고 인 한 된 \
     에 을 를 이 가 은 는 의 로 으로 둔 두고 있는 있고 거주하고 거주한 거주중 되어 되어있는 누구나 모든 본인 명 등록 \
     신청자"
        .split_whitespace()
        .collect()
});
const PARTICLES: [&str; 21] = [
    "에서",
    "으로",
    "에게",
    "이며",
    "이고",
    "으로서",
    "은",
    "는",
    "이",
    "가",
    "을",
    "를",
    "에",
    "의",
    "로",
    "과",
    "와",
    "도",
    "인",
    "중",
    "",
];
const AGE_NOUNS: [&str; 5] = ["청년", "청소년", "노인", "어르신", "아동"];
const TOP_MARK: &str = "○o❍ㅇ●•□■";

// ── char-window helpers (the prototype indexes code points) ──────────────

/// Byte offset `n` characters before `at`.
fn back(s: &str, at: usize, n: usize) -> usize {
    s[..at]
        .char_indices()
        .rev()
        .nth(n.saturating_sub(1))
        .map_or(0, |(i, _)| i)
        .min(at)
}

/// `s[at - n chars .. at]`.
fn before(s: &str, at: usize, n: usize) -> &str {
    if n == 0 {
        return "";
    }
    &s[back(s, at, n)..at]
}

/// `s[at .. at + n chars]`.
fn after(s: &str, at: usize, n: usize) -> &str {
    let end = s[at..]
        .char_indices()
        .nth(n)
        .map_or(s.len(), |(i, _)| at + i);
    &s[at..end]
}

fn char_len(s: &str) -> usize {
    s.chars().count()
}

fn in_parens(line: &str, start: usize) -> bool {
    let mut depth: i32 = 0;
    for ch in line[..start].chars() {
        match ch {
            '(' => depth += 1,
            ')' => depth = (depth - 1).max(0),
            _ => {}
        }
    }
    depth > 0
}

/// The comma/semicolon-free clause around `[start, end)`.
fn clause_bounds(line: &str, start: usize, end: usize) -> (usize, usize) {
    let left = [',', ';', ':', '·', '/']
        .iter()
        .filter_map(|c| line[..start].rfind(*c).map(|i| i + c.len_utf8()))
        .max()
        .unwrap_or(0);
    let right = [',', ';', ':', '/']
        .iter()
        .filter_map(|c| line[end..].find(*c).map(|i| end + i))
        .min()
        .unwrap_or(line.len());
    (left, right)
}

// ── matchers ───────────────────────────────────────────────────────────────

struct Ctx<'a> {
    line: &'a str,
    field: &'a str,
}

type Builder = fn(&regex::Captures<'_>, &Ctx<'_>) -> Option<Value>;

struct Matcher {
    kind: &'static str,
    rule: &'static str,
    regex: Regex,
    build: Builder,
}

fn int(caps: &regex::Captures<'_>, name: &str) -> Option<i64> {
    caps.name(name)?.as_str().parse().ok()
}

fn age_range(caps: &regex::Captures<'_>, _: &Ctx<'_>) -> Option<Value> {
    let lo = int(caps, "lo")?;
    let mut hi = int(caps, "hi")?;
    if caps.name("suf").map(|m| m.as_str()) == Some("미만") {
        hi -= 1;
    }
    (0 <= lo && lo <= hi && hi <= 99).then(|| json!({"min": lo, "max": hi}))
}

fn income_median(caps: &regex::Captures<'_>, _: &Ctx<'_>) -> Option<Value> {
    let pct = int(caps, "p")?;
    Some(if caps.name("s").map(|m| m.as_str()) == Some("미만") {
        json!({"max_pct": pct, "strict": true})
    } else {
        json!({"max_pct": pct})
    })
}

fn kosaf(caps: &regex::Captures<'_>, ctx: &Ctx<'_>) -> Option<Value> {
    let whole = caps.get(0)?;
    let tail = after(ctx.line, whole.end(), 4);
    if ctx.field != "소득기준 상세내용" && !KOSAF_TAIL.is_match(tail) {
        return None;
    }
    Some(json!({"max": int(caps, "n")?}))
}

fn major(caps: &regex::Captures<'_>, _: &Ctx<'_>) -> Option<Value> {
    Some(json!({"major": format!("{}학", caps.name("maj")?.as_str())}))
}

fn business_kind(kind: &'static str) -> Value {
    json!({ "type": kind })
}

fn homeless(caps: &regex::Captures<'_>, ctx: &Ctx<'_>) -> Option<Value> {
    let whole = caps.get(0)?;
    let text = whole.as_str();
    let next = after(ctx.line, whole.end(), 1);
    // 무주택 세대주 / 가구주: also the head of the household, which the
    // profile does not ask.
    if text.ends_with("세대주") || (text.ends_with("가구") && next == "주") {
        return Some(json!({"level": "head"}));
    }
    if text.ends_with('자') {
        // 무주택자녀 is not the applicant.
        return (next != "녀").then(|| json!({"level": "person"}));
    }
    // 무주택 세대 기간 / 무주택 가구원 수 are not the household's status.
    if next == "기" || next == "원" {
        return None;
    }
    Some(json!({"level": "household"}))
}

fn newlywed_years(caps: &regex::Captures<'_>, _: &Ctx<'_>) -> Option<Value> {
    let years = int(caps, "n")?;
    (1..=20)
        .contains(&years)
        .then(|| json!({"status": "married", "newlywed_years": years}))
}

fn newlywed_bare(_: &regex::Captures<'_>, _: &Ctx<'_>) -> Option<Value> {
    Some(json!({"status": "married", "newlywed_years": null}))
}

fn unmarried(caps: &regex::Captures<'_>, ctx: &Ctx<'_>) -> Option<Value> {
    let start = caps.get(0)?.start() + "미혼".len();
    let rest = after(ctx.line, start, 4).trim_start();
    // 미혼모·미혼부·미혼 한부모 are family statuses; 미혼 자녀 is a child.
    if ["모", "부", "한부모", "자녀"]
        .iter()
        .any(|p| rest.starts_with(p))
    {
        return None;
    }
    Some(json!({"status": "unmarried"}))
}

fn married(caps: &regex::Captures<'_>, ctx: &Ctx<'_>) -> Option<Value> {
    let start = caps.get(0)?.start() + "기혼".len();
    let rest = after(ctx.line, start, 4).trim_start();
    if ["여성", "남성", "자녀"].iter().any(|p| rest.starts_with(p)) {
        return None;
    }
    Some(json!({"status": "married"}))
}

fn children(caps: &regex::Captures<'_>, ctx: &Ctx<'_>) -> Option<Value> {
    let whole = caps.get(0)?;
    // "18세 자녀" is a child's age, not 세 자녀.
    if CHILD_QUALIFIER.is_match(before(ctx.line, whole.start(), 10))
        || before(ctx.line, whole.start(), 1)
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_digit())
    {
        return None;
    }
    let n = int(caps, "n").or_else(|| int(caps, "m")).or_else(|| {
        match caps.name("w").map(|m| m.as_str()) {
            Some("두") => Some(2),
            Some("세") => Some(3),
            Some("네") => Some(4),
            _ => None,
        }
    })?;
    Some(json!({ "min": n }))
}

static MATCHERS: LazyLock<Vec<Matcher>> = LazyLock::new(|| {
    let m = |kind, rule, pattern: &str, build: Builder| Matcher {
        kind,
        rule,
        regex: rx(pattern),
        build,
    };
    vec![
        m(
            "age",
            "age.range_words",
            r"(?:만\s?)?(?P<lo>\d{1,2})\s?세\s?이상\s?[~∼〜]?\s?(?:만\s?)?(?P<hi>\d{1,2})\s?세(?:\s?(?P<suf>이하|미만))?",
            age_range,
        ),
        m(
            "age",
            "age.range_tilde",
            r"(?:만\s?)?(?P<lo>\d{1,2})\s?세?\s?[~∼〜\-–]\s?(?:만\s?)?(?P<hi>\d{1,2})\s?세(?:\s?(?P<suf>이하|미만|까지))?",
            age_range,
        ),
        m(
            "income_median",
            "income_median.pct",
            r"(?:기준\s?)?중위\s?소득\s?(?:의\s?)?(?P<p>\d{2,3})\s?(?:%|퍼센트)\s?(?P<s>이하|이내|미만|를\s?초과하지\s?않)",
            income_median,
        ),
        m(
            "income_median",
            "income_median.fraction",
            r"기준\s?중위\s?소득의\s?100분의\s?(?P<p>\d{2,3})\s?(?:이하|이내)",
            income_median,
        ),
        m(
            "income_kosaf",
            "income_kosaf.bracket",
            r"학자금\s?지원\s?(?:구간\s?)?(?P<n>\d{1,2})\s?구간",
            kosaf,
        ),
        m(
            "major",
            "major.dept",
            r"(?P<maj>[가-힣]{2,8})학과\s?(?:대학생|학생|재학생)",
            major,
        ),
        m(
            "business",
            "business.prospective",
            r"예비\s?창업자",
            |_, _| Some(business_kind("prospective_founder")),
        ),
        m(
            "business",
            "business.startup_years",
            r"창업\s?(?P<n>\d{1,2})\s?년\s?이내",
            |caps, _| Some(json!({"type": "startup_within_years", "years": int(caps, "n")?})),
        ),
        m("business", "business.small", r"소상공인", |_, _| {
            Some(business_kind("small_business"))
        }),
        m(
            "business",
            "business.sme",
            r"중소기업\s?기본법",
            |_, _| Some(business_kind("sme")),
        ),
        m(
            "business",
            "business.self_employed",
            r"개인\s?사업자|(?:^|[,\s])사업자(?:$|[,\s])",
            |_, ctx| {
                EXCLUSION_FIELDS
                    .contains(&ctx.field)
                    .then(|| business_kind("self_employed"))
            },
        ),
        m(
            "prior_receipt",
            "prior_receipt.term",
            r"중복\s?(?:지급|참여|지원|수혜|신청)\s?(?:불가|제한|되지|안\s?됨|금지)|기\s?수혜(?:자|기업)|유사\s?(?:복지\s?)?(?:서비스|사업)\s?(?:수혜자|참여\s?중인\s?자)|동일\s?(?:학기|사업)[^.○※]{0,20}(?:받았거나|받은|지원자)",
            |caps, _| Some(json!({"desc": caps.get(0)?.as_str()})),
        ),
        // ── kinds the optional profile answers check ──
        m(
            "homeless",
            "homeless.household_all",
            r"세대원\s?전원(?:이|은)?\s?(?:모두\s?)?무주택(?:자|인\s?자)?",
            |_, _| Some(json!({"level": "household"})),
        ),
        m(
            "homeless",
            "homeless.term",
            r"무주택(?:\s?(?:세대\s?구성원|세대원|가구|세대주|세대)|자)",
            homeless,
        ),
        m(
            "marital",
            "marital.newlywed_years",
            r"(?:신혼부부\s?\(\s?)?(?:혼인|결혼)\s?(?:기간\s?)?(?:신고\s?)?(?:일\s?)?(?:기준\s?|로부터\s?|부터\s?|후\s?|이후\s?)?(?P<n>\d{1,2})\s?년\s?(?:차\s?)?(?:이내|이하)(?:\s?\)|\s?(?:의\s?|인\s?)?신혼부부)?",
            newlywed_years,
        ),
        m("marital", "marital.newlywed", r"신혼\s?부부", newlywed_bare),
        m("marital", "marital.unmarried", r"미혼(?:자|인)?", unmarried),
        m("marital", "marital.married", r"기혼(?:자)?", married),
        m(
            "children",
            "children.count",
            r"다자녀\s?(?:가구|가정|가족)?\s?\(\s?(?P<n>[2-9])\s?자녀\s?이상\s?\)|(?P<w>두|세|네)\s?자녀\s?이상|[2-9]\s?자녀\s?이상|자녀\s?(?:가\s?)?(?P<m>[1-9])\s?명\s?이상",
            |caps, ctx| {
                // `[2-9]자녀 이상` has no named group: read its digit.
                if caps.name("n").is_none() && caps.name("m").is_none() && caps.name("w").is_none()
                {
                    let whole = caps.get(0)?;
                    if CHILD_QUALIFIER.is_match(before(ctx.line, whole.start(), 10)) {
                        return None;
                    }
                    let digit = whole.as_str().chars().next()?.to_digit(10)?;
                    return Some(json!({ "min": digit }));
                }
                children(caps, ctx)
            },
        ),
    ]
});

// ── line structure ─────────────────────────────────────────────────────────

/// One candidate fact on a line: byte span within the line.
#[derive(Debug, Clone)]
struct Cand {
    kind: &'static str,
    start: usize,
    end: usize,
    value: Value,
    rule: &'static str,
}

/// Same-length (in bytes) copy with parenthetical text and digit commas
/// neutralised, so separator positions still index the original line.
fn mask(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::with_capacity(line.len());
    let mut depth: i32 = 0;
    for (i, &ch) in chars.iter().enumerate() {
        let fill = |out: &mut String, c: char| {
            for _ in 0..ch.len_utf8() {
                out.push(c);
            }
        };
        if ch == '(' {
            depth += 1;
            out.push('(');
        } else if ch == ')' {
            depth = (depth - 1).max(0);
            out.push(')');
        } else if depth > 0 {
            fill(&mut out, '_');
        } else if ch == ','
            && i > 0
            && i + 1 < chars.len()
            && chars[i - 1].is_ascii_digit()
            && chars[i + 1].is_ascii_digit()
        {
            out.push('0');
        } else {
            out.push(ch);
        }
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    Single,
    Conj,
    Enum,
    Mixed,
}

/// 'single' (no separator), 'conj' (comma parts are attribute clauses: AND),
/// 'enum' (comma/및 parts are category nouns each carrying a fact: OR) or
/// 'mixed' (abstain).
fn line_shape(line: &str, cands: &[Cand]) -> Shape {
    let mut m: Vec<u8> = mask(line).into_bytes();
    // A separator inside one fact is not a split.
    for c in cands {
        let mut i = c.start;
        while i < c.end {
            let ch = line[i..].chars().next().unwrap_or(' ');
            if [',', '/', 'ㆍ', '및'].contains(&ch) {
                for b in m.iter_mut().skip(i).take(ch.len_utf8()) {
                    *b = b'X';
                }
            }
            i += ch.len_utf8().max(1);
        }
    }
    let m = String::from_utf8(m).unwrap_or_default();
    if MIXED.is_match(&m) {
        return Shape::Mixed;
    }
    let cuts: Vec<(usize, usize)> = SEP.find_iter(&m).map(|x| (x.start(), x.end())).collect();
    if cuts.is_empty() {
        return Shape::Single;
    }
    let mut bounds = Vec::new();
    let mut prev = 0;
    for (a, b) in &cuts {
        bounds.push((prev, *a));
        prev = *b;
    }
    bounds.push((prev, line.len()));
    let parts: Vec<(usize, usize)> = bounds
        .into_iter()
        .filter(|(a, b)| {
            !m[*a..*b]
                .trim_matches(|c| c == ' ' || c == '_' || c == '(' || c == ')')
                .is_empty()
        })
        .collect();
    let texts: Vec<&str> = parts.iter().map(|(a, b)| m[*a..*b].trim()).collect();
    let Some((last, init)) = texts.split_last() else {
        return Shape::Mixed;
    };
    if init.iter().all(|t| ATTR_END.is_match(t)) && PERSON_HEAD.is_match(last) {
        return Shape::Conj;
    }
    let per_part: Vec<usize> = parts
        .iter()
        .map(|(a, b)| {
            cands
                .iter()
                .filter(|c| *a <= c.start && c.start < *b)
                .count()
        })
        .collect();
    if per_part.iter().filter(|n| **n > 0).count() >= 2
        && per_part.iter().all(|n| *n <= 1)
        && !init.iter().any(|t| ATTR_END.is_match(t))
        && texts.iter().all(|t| char_len(t) <= 30)
    {
        return Shape::Enum;
    }
    Shape::Mixed
}

fn overlaps(a: (usize, usize), b: (usize, usize)) -> bool {
    a.0 < b.1 && b.0 < a.1
}

/// Exclusion or negation context near a span: 단, 제외, ~를 제외한, 다만,
/// 이외 within the clause or a few characters around it.
fn exclusion_near(line: &str, start: usize, end: usize) -> bool {
    let (ca, cb) = clause_bounds(line, start, end);
    let window = format!(
        "{}{}{}",
        before(line, start, 15),
        &line[start..end],
        after(line, end, 15)
    );
    let clause = &line[ca.min(start)..cb.max(end)];
    let cue = |text: &str| {
        text.contains("제외")
            || text.contains("다만")
            || text.contains("이외")
            || text.contains("예외")
            || standalone_dan(text)
    };
    cue(&window) || cue(clause)
}

/// 단 as its own word (단, / 단) / (단 / 단 :) — not 단체, 판단, 지원단.
fn standalone_dan(text: &str) -> bool {
    let chars: Vec<char> = text.chars().collect();
    chars.iter().enumerate().any(|(i, &c)| {
        c == '단'
            && (i == 0 || matches!(chars[i - 1], ' ' | '(' | '※' | '*' | '-' | '\t'))
            && chars
                .get(i + 1)
                .is_none_or(|n| matches!(n, ',' | ')' | ':' | ' ' | '：' | '.'))
    })
}

/// Every matcher hit on one line, filtered by local context.
fn line_candidates(field: &str, line: &str) -> Vec<Cand> {
    let ctx = Ctx { line, field };
    let mut cands: Vec<Cand> = Vec::new();
    for matcher in MATCHERS.iter() {
        for caps in matcher.regex.captures_iter(line) {
            let whole = caps.get(0).expect("group 0");
            if let Some(value) = (matcher.build)(&caps, &ctx) {
                cands.push(Cand {
                    kind: matcher.kind,
                    start: whole.start(),
                    end: whole.end(),
                    value,
                    rule: matcher.rule,
                });
            }
        }
    }
    cands.sort_by_key(|c| (c.start, std::cmp::Reverse(c.end - c.start)));
    let mut kept: Vec<Cand> = Vec::new();
    for c in cands {
        if kept
            .iter()
            .any(|k| k.kind == c.kind && overlaps((k.start, k.end), (c.start, c.end)))
        {
            continue;
        }
        kept.push(c);
    }
    let mut out = Vec::new();
    for mut c in kept {
        let (a, b) = (c.start, c.end);
        if EXCEPTION.is_match(before(line, a, 12))
            || (in_parens(line, a) && EXCEPTION.is_match(&line[..a]))
        {
            continue;
        }
        let (ca, _) = clause_bounds(line, a, b);
        if SUBJECT_OTHER.is_match(&line[ca.min(a)..a])
            || SUBJECT_OTHER.is_match(before(line, a, 15))
        {
            continue;
        }
        if c.kind == "age" {
            if line[..a]
                .chars()
                .next_back()
                .is_some_and(|ch| ch.is_ascii_digit())
            {
                continue;
            }
            let tail = after(line, b, 8);
            if AGE_TAIL_OTHER.is_match(tail)
                || after(line, b, 20).contains("동의")
                || after(line, b, 6).contains("도래")
            {
                continue;
            }
            if in_parens(line, a) && !AGE_PAREN_HEAD.is_match(&line[..a]) {
                continue;
            }
            // A 연 나이 band is not the 만나이 the profile's age reads.
            if AGE_CALENDAR.is_match(before(line, a, 6))
                || AGE_CALENDAR.is_match(after(line, b, 10))
            {
                c.value["basis"] = json!("연");
            }
        }
        if c.kind == "prior_receipt" && PRIOR_TAIL.is_match(&line[b..]) {
            continue;
        }
        out.push(c);
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineKind {
    Header,
    ExclCtx,
    Body,
}

struct Line<'a> {
    pos: usize,
    raw: &'a str,
    stripped: String,
    kind: LineKind,
    cands: Vec<Cand>,
}

fn marker(raw: &str) -> Option<char> {
    BULLET
        .is_match(raw)
        .then(|| raw.trim().chars().next())
        .flatten()
}

/// Facts of one field, whether the field is complete, and the residual text.
fn extract_field(field: &str, text: &str) -> (Vec<Fact>, bool, String) {
    // A placeholder ("-", "없음", "해당없음") is missing data, never a
    // statement that there are no conditions.
    if EXPLICIT_NONE.is_match(text) {
        return (Vec::new(), false, String::new());
    }
    let mut lines: Vec<Line<'_>> = Vec::new();
    let mut offset = 0;
    for raw in text.split('\n') {
        let pos = offset;
        offset += raw.len() + 1;
        let stripped = BULLET.replace(raw, "").trim().to_string();
        if !stripped.is_empty() {
            lines.push(Line {
                pos,
                raw,
                stripped,
                kind: LineKind::Body,
                cands: Vec::new(),
            });
        }
    }
    let excl_field = EXCLUSION_FIELDS.contains(&field);
    let mut excl_ctx = false;
    for line in &mut lines {
        if HEADER.is_match(line.raw) || HEADER.is_match(&line.stripped) {
            excl_ctx = false;
            line.kind = LineKind::Header;
            continue;
        }
        if EXCL_HEADER.is_match(&line.stripped) {
            excl_ctx = true;
            line.kind = LineKind::Header;
            continue;
        }
        line.kind = if excl_ctx {
            LineKind::ExclCtx
        } else {
            LineKind::Body
        };
        line.cands = line_candidates(field, line.raw);
    }
    // An inclusion note ("- 등록 외국인 포함") widens the line above; it is
    // not an item.
    for line in lines.iter_mut().skip(1) {
        if line.kind == LineKind::Body && line.cands.is_empty() && INCLUSION_NOTE.is_match(line.raw)
        {
            line.kind = LineKind::Header;
        }
    }
    let body: Vec<usize> = (0..lines.len())
        .filter(|i| lines[*i].kind == LineKind::Body)
        .collect();
    let markers: HashSet<String> = body
        .iter()
        .map(|i| {
            let raw = lines[*i].raw;
            if BULLET.is_match(raw) {
                raw.trim_start()
                    .chars()
                    .next()
                    .map(String::from)
                    .unwrap_or_default()
            } else {
                String::new()
            }
        })
        .collect();
    // "요건을 모두 충족" covers its own line and the sub-items under it, not
    // every line of the field.
    let level = |raw: &str| marker(raw).is_some_and(|m| !TOP_MARK.contains(m));
    let mut all_of = vec![false; lines.len()];
    let mut owner: Option<bool> = None;
    for (i, line) in lines.iter().enumerate() {
        if ALL_OF.is_match(line.raw) {
            owner = Some(level(line.raw));
            all_of[i] = true;
            continue;
        }
        match owner {
            Some(owner_sub) if level(line.raw) && !owner_sub => all_of[i] = true,
            _ => owner = None,
        }
    }
    let short_list = body.len() >= 2
        && markers.len() == 1
        && body.iter().all(|i| {
            let l = &lines[*i];
            char_len(&l.stripped) <= 30
                && !REL_CLAUSE_END.is_match(&l.stripped)
                && !ATTR_END.is_match(&l.stripped)
                && matches!(line_shape(l.raw, &l.cands), Shape::Single | Shape::Enum)
        });
    let tops: Vec<usize> = body
        .iter()
        .copied()
        .filter(|i| marker(lines[*i].raw).is_none_or(|m| TOP_MARK.contains(m)))
        .collect();
    let one_parent = tops.len() == 1 && body.len() > 1 && body.first() == tops.first();
    let labelled = body.len() >= 2
        && body.iter().all(|i| LABELLED.is_match(lines[*i].raw))
        && !body
            .iter()
            .any(|i| LABEL_INNER_COLON.is_match(&LABELLED.replace(lines[*i].raw, "")));

    let mut facts = Vec::new();
    let mut spans_used: Vec<(usize, usize)> = Vec::new();
    let mut complete = true;
    for (index, line) in lines.iter().enumerate() {
        let (pos, raw) = (line.pos, line.raw);
        if line.kind == LineKind::Header {
            spans_used.push((pos, pos + raw.len()));
            continue;
        }
        let shape = line_shape(raw, &line.cands);
        let by_shape = match shape {
            Shape::Single | Shape::Conj => Some(Role::Req),
            Shape::Enum => Some(Role::Alt),
            Shape::Mixed => None,
        };
        let role = if excl_field || line.kind == LineKind::ExclCtx {
            Some(Role::Excl)
        } else if body.len() == 1 || all_of[index] || labelled {
            by_shape
        } else if short_list {
            Some(Role::Alt)
        } else if one_parent {
            if tops.first() == Some(&index) {
                by_shape
            } else {
                None
            }
        } else {
            None
        };
        if role.is_none() {
            complete = false;
        }
        // A 신혼부부 next to its own year window on the line is the same
        // fact: consumed, not repeated.
        let has_years = line
            .cands
            .iter()
            .any(|c| c.rule == "marital.newlywed_years");
        let mut seen_prior = false;
        for c in &line.cands {
            spans_used.push((pos + c.start, pos + c.end));
            if c.rule == "marital.newlywed" && has_years {
                continue;
            }
            let mut r = role;
            let (_, cb) = clause_bounds(raw, c.start, c.end);
            let em = EXCEPT_PHRASE.find(raw);
            if matches!(r, Some(Role::Req | Role::Alt)) && em.is_some() {
                if em.is_some_and(|em| c.end <= em.start()) {
                    r = Some(Role::Excl);
                }
            } else if matches!(r, Some(Role::Req | Role::Alt)) {
                let probe = format!(
                    "{}{}",
                    &raw[c.end..cb.max(c.end)],
                    after(raw, cb.max(c.end), 12)
                );
                if LINE_EXCL.is_match(&probe) {
                    r = Some(Role::Excl);
                }
            }
            // Inside a "(… 우선)" or "(… 포함)" note: a priority or a widening,
            // not a requirement. Abstain (ages there are already filtered).
            if c.kind != "age"
                && paren_group(raw, c.start, c.end).is_some_and(|g| PRIORITY_WORD.is_match(g))
            {
                complete = false;
                continue;
            }
            // An inclusion fact next to an exclusion or negation cue: abstain.
            if matches!(r, Some(Role::Req | Role::Alt))
                && c.kind != "prior_receipt"
                && exclusion_near(raw, c.start, c.end)
            {
                complete = false;
                continue;
            }
            if r == Some(Role::Excl) {
                let allowed: &[&str] = if line.kind == LineKind::ExclCtx {
                    &["prior_receipt"]
                } else {
                    &EXCLUSION_FIELD_KINDS
                };
                if !allowed.contains(&c.kind) && c.kind != "prior_receipt" {
                    complete = false;
                    continue;
                }
            }
            if c.kind == "prior_receipt" {
                r = Some(Role::Excl);
                if seen_prior {
                    continue;
                }
                seen_prior = true;
            }
            facts.push(Fact {
                kind: c.kind,
                value: c.value.clone(),
                role: r.unwrap_or(Role::Unknown),
                field: field.to_string(),
                quote: raw[c.start..c.end].to_string(),
                rule: c.rule,
                start: pos + c.start,
            });
        }
    }
    let mut residual = text.to_string();
    spans_used.sort();
    for (a, b) in spans_used.iter().rev() {
        let (a, b) = ((*a).min(residual.len()), (*b).min(residual.len()));
        if a < b && residual.is_char_boundary(a) && residual.is_char_boundary(b) {
            residual.replace_range(a..b, " ");
        }
    }
    let has_age = facts.iter().any(|f| f.kind == "age") || YOUTH_CODE_AGE_FIELDS.contains(&field);
    let empty = residual_empty(&residual, has_age);
    (facts, complete && empty, residual)
}

/// The parenthetical group around `[start, end)`, if the span is inside one.
fn paren_group(line: &str, start: usize, end: usize) -> Option<&str> {
    let mut depth = 0i32;
    let mut open = None;
    for (i, ch) in line[..start].char_indices().rev() {
        match ch {
            ')' => depth += 1,
            '(' if depth == 0 => {
                open = Some(i);
                break;
            }
            '(' => depth -= 1,
            _ => {}
        }
    }
    let open = open?;
    let close = line[end..].find(')').map_or(line.len(), |i| end + i + 1);
    Some(&line[open..close])
}

/// No condition-like content left: no [`CONDITION_CUES`] word, no leftover
/// number or head count, and only filler tokens. The cues are checked before
/// priority notes are dropped, so "(재산 포함)" still counts.
fn residual_empty(residual: &str, has_age: bool) -> bool {
    if CONDITION_CUES.iter().any(|cue| residual.contains(cue))
        || standalone_dan(residual)
        || LEFTOVER_COUNT.is_match(residual)
        || residual.chars().any(|c| c.is_ascii_digit())
    {
        return false;
    }
    // Priority notes without a condition word ("(우선 선정)") are not
    // conditions.
    let r = PRIORITY_NOTE.replace_all(residual, " ");
    let r = BULLET.replace(&r, "");
    let r = RESIDUAL_PUNCT.replace_all(&r, " ");
    for tok in r.split_whitespace() {
        let mut t = tok;
        for p in PARTICLES.iter().filter(|p| !p.is_empty()) {
            if t.ends_with(p) && t.len() > p.len() {
                t = &t[..t.len() - p.len()];
                break;
            }
        }
        if STOP.contains(t) || STOP.contains(tok) {
            continue;
        }
        if has_age && (AGE_NOUNS.contains(&t) || AGE_NOUNS.contains(&tok)) {
            continue;
        }
        return false;
    }
    true
}

/// All facts from a programme's eligibility fields, plus completeness.
pub fn extract(source_type: &str, fields: &BTreeMap<String, String>) -> Extraction {
    let mut out = Vec::new();
    let mut complete = true;
    let mut has_text = false;
    for field in eligibility_fields(source_type) {
        // Blank or placeholder text is missing data.
        let Some(text) = fields
            .get(*field)
            .filter(|t| !t.trim().is_empty() && !EXPLICIT_NONE.is_match(t))
        else {
            continue;
        };
        has_text = true;
        let (facts, field_complete, _) = extract_field(field, text);
        complete = complete && field_complete && facts.iter().all(|f| f.role != Role::Unknown);
        out.extend(facts);
    }
    let context_free = context_fields(source_type).iter().all(|field| {
        fields
            .get(*field)
            .is_none_or(|t| t.trim().is_empty() || EXPLICIT_NONE.is_match(t))
    });
    Extraction {
        facts: out,
        complete: complete && has_text && context_free,
        has_text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(source: &str, field: &str, text: &str) -> Extraction {
        extract(
            source,
            &BTreeMap::from([(field.to_string(), text.to_string())]),
        )
    }

    #[test]
    fn helpers_index_characters_not_bytes() {
        let s = "가나다라마";
        let at = s.find('다').unwrap();
        assert_eq!(before(s, at, 1), "나");
        assert_eq!(before(s, at, 9), "가나");
        assert_eq!(after(s, at, 2), "다라");
        assert_eq!(mask("가(나다)라").len(), "가(나다)라".len());
        assert_eq!(mask("1,000(원)"), "10000(___)");
    }

    #[test]
    fn a_single_requirement_line_is_complete_with_its_quote() {
        let e = one("gov_benefits", "지원대상", "기준 중위소득 100% 이하인 자");
        assert!(e.complete, "{e:?}");
        assert_eq!(e.facts.len(), 1);
        let f = &e.facts[0];
        assert_eq!((f.kind, f.role), ("income_median", Role::Req));
        assert_eq!(f.value, json!({"max_pct": 100}));
        assert_eq!(f.quote, "기준 중위소득 100% 이하");
    }

    #[test]
    fn leftover_condition_text_keeps_the_programme_incomplete() {
        // The residence and the 세대주 status are not structured kinds.
        let e = one(
            "gov_benefits",
            "지원대상",
            "부산시 거주 기준 중위소득 100% 이하인 자",
        );
        assert!(!e.complete);
        let e = one("gov_benefits", "지원대상", "만 19세~39세 거주자");
        assert!(!e.complete, "거주자 alone is a residence condition");
        let e = one(
            "gov_benefits",
            "지원대상",
            "청년 중 학자금 지원구간 8구간 이하",
        );
        assert!(!e.complete, "청년 with no age is a condition");
    }

    #[test]
    fn exclusion_or_negation_context_makes_a_fact_abstain() {
        for text in [
            "무주택 세대구성원 (단, 만 30세 미만 단독세대주는 제외)",
            "기준 중위소득 100% 이하인 자 다만, 기초생활수급자는 지원",
            "미혼자 이외의 자",
        ] {
            let e = one("gov_benefits", "지원대상", text);
            assert!(!e.complete, "{text}: {e:?}");
            assert!(
                e.facts
                    .iter()
                    .all(|f| f.role != Role::Req || f.kind == "prior_receipt"),
                "{text}: {:?}",
                e.facts
            );
        }
        assert!(standalone_dan("(단, 예외)"));
        assert!(!standalone_dan("지원단 단체 판단"));
    }

    #[test]
    fn new_kinds_read_only_explicit_phrasing() {
        let e = one("gov_benefits", "지원대상", "무주택세대구성원");
        assert!(e.complete, "{e:?}");
        assert_eq!(e.facts[0].value, json!({"level": "household"}));
        let e = one("gov_benefits", "지원대상", "무주택 세대주");
        assert_eq!(e.facts[0].value, json!({"level": "head"}));
        let e = one(
            "gov_benefits",
            "지원대상",
            "혼인신고일 기준 7년 이내 신혼부부",
        );
        assert_eq!(e.facts.len(), 1, "{:?}", e.facts);
        assert_eq!(
            e.facts[0].value,
            json!({"status": "married", "newlywed_years": 7})
        );
        assert!(e.complete, "{e:?}");
        let e = one("gov_benefits", "지원대상", "미혼모");
        assert!(e.facts.is_empty());
        let e = one("gov_benefits", "지원대상", "만 18세 이하 자녀 2명 이상");
        assert!(
            e.facts.iter().all(|f| f.kind != "children"),
            "{:?}",
            e.facts
        );
        let e = one("gov_benefits", "지원대상", "3자녀 이상 가구");
        assert!(e
            .facts
            .iter()
            .any(|f| f.kind == "children" && f.value == json!({"min": 3})));
    }

    #[test]
    fn placeholders_are_missing_data() {
        // A placeholder is missing data, never "no conditions".
        for placeholder in ["제한없음", "-", ".", "없음", "해당없음"] {
            let e = one("gov_benefits", "지원대상", placeholder);
            assert!(
                !e.complete && e.facts.is_empty() && !e.has_text,
                "{placeholder}"
            );
        }
        // An 온통청년 description may name the target: never complete then.
        let e = extract(
            "youth_center",
            &BTreeMap::from([
                ("addAplyQlfcCndCn".into(), "-".into()),
                (
                    "plcyExplnCn".into(),
                    "인천광역시 청년농업인4-H연합회 활동 지원".into(),
                ),
            ]),
        );
        assert!(!e.complete && e.facts.is_empty());
        let e = extract("youth_center", &BTreeMap::new());
        assert!(!e.complete && !e.has_text);
        let e = extract(
            "local_welfare",
            &BTreeMap::from([("servDgst".into(), "제한없음".into())]),
        );
        assert!(!e.complete, "welfare summaries are never read");
    }
}
