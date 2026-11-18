//! Rule parser for cross-programme questions ("부산 사는 휴학생이 받을 수
//! 있는 장학금?"): region, enrollment status, programme types and the
//! remaining content keywords. No model.
use super::text::{self, Term};
use serde::Serialize;

/// Programme-type cues, matched against stemmed question words.
const TYPE_CUES: &[(&str, &[&str])] = &[
    ("장학", &["scholarship"]),
    ("학자금", &["scholarship"]),
    ("정책", &["policy"]),
    ("지원금", &["policy"]),
    ("지원사업", &["policy"]),
    ("수당", &["policy"]),
    ("바우처", &["policy"]),
    ("캠퍼스", &["campus_benefit"]),
    ("교내", &["campus_benefit"]),
    ("학교혜택", &["campus_benefit"]),
    ("학생할인", &["campus_benefit", "corporate_benefit"]),
    ("할인", &["corporate_benefit", "campus_benefit"]),
    ("기업혜택", &["corporate_benefit"]),
    ("구독", &["corporate_benefit"]),
];

/// Words that frame a search but do not narrow it.
const FRAME_WORDS: &[&str] = &[
    "사는",
    "살고",
    "살아",
    "거주",
    "거주하는",
    "청년",
    "학생",
    "대학생",
    "혜택",
    "지원",
    "추천",
    "목록",
    "종류",
    "찾아",
    "찾아줘",
    "있는",
    "받을",
    "받는",
    "뭐",
    "뭐가",
    "어떤",
    "무엇",
    "알려줘",
    "보여줘",
    "전부",
    "모두",
    "다",
    "같은",
    "프로그램",
    "제도",
    "사업",
];

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct SearchIntent {
    pub region: Option<&'static str>,
    pub region_label: Option<&'static str>,
    pub enrollment_status: Option<&'static str>,
    pub enrollment_label: Option<&'static str>,
    pub program_types: Vec<&'static str>,
    pub keywords: Vec<String>,
    #[serde(skip)]
    pub keyword_forms: Vec<Vec<String>>,
    /// Field tabs the question names (`mazimi_core::categories::fields_named`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<&'static str>,
    /// Only rows closing at most this many days from today, deadline first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_within_days: Option<u16>,
    /// ... and at least this many (`SearchArgs::deadline_from_days`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_from_days: Option<u16>,
    /// Only the row that closes first ([`mazimi_core::agent::SearchArgs::earliest`]).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub earliest: bool,
    /// Only rows paying at least this many won ([`amount_bound`]), for
    /// `amount_period` (`None` = any stored period).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amount_min: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amount_period: Option<&'static str>,
    /// Regions the question excludes (canonical codes).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excluded_regions: Vec<&'static str>,
    /// Words the question asks to leave out ("부산 말고"): never a filter,
    /// and no exclusion is applied for them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub negated: Vec<String>,
}

impl SearchIntent {
    pub fn is_empty(&self) -> bool {
        self.region.is_none()
            && self.enrollment_status.is_none()
            && self.program_types.is_empty()
            && self.keywords.is_empty()
            && self.fields.is_empty()
            && self.deadline_within_days.is_none()
    }
}

/// What may follow a status phrase, whole: a particle or the copula with
/// its endings (a closed class, not a list of nouns).
const STATUS_TAILS: &[&str] = &[
    "이",
    "가",
    "은",
    "는",
    "도",
    "을",
    "를",
    "의",
    "만",
    "로",
    "으로",
    "와",
    "과",
    "랑",
    "이랑",
    "에게",
    "한테",
    "부터",
    "까지",
    "에서",
    "이라",
    "이라서",
    "이라도",
    "이고",
    "이면",
    "이니",
    "이니까",
    "이에요",
    "이야",
    "인데",
    "인데요",
    "입니다",
    "이지만",
    "이어도",
    "이어서",
    "인",
];
/// Words a status noun is composed with across a space ([`status_words`]).
const STATUS_JOINERS: &[&str] = &["예정자", "예정", "중", "후", "이후", "유예", "앞둔"];
/// Prospective forms: the status is still ahead.
const PROSPECTIVE: &[&str] = &[
    "할예정자",
    "할예정",
    "예정자",
    "예정",
    "을앞둔",
    "앞둔",
    "유예",
];

fn tail_ok(rest: &str) -> bool {
    rest.is_empty() || STATUS_TAILS.contains(&rest)
}

/// The enrollment status a question word states ([`status_words`] composes
/// the phrase first), read by aspect; when not certain, none (unknown is
/// the safe side):
/// - 졸업 is an event: `graduated` only from a completed form (졸업생,
///   졸업자, 졸업한/졸업했-/졸업함, 졸업 후/이후); `enrolled` from the
///   prospective (졸업 예정, 졸업할 예정, 졸업을 앞둔, 졸업 유예); anything
///   else (졸업하는데, 졸업 안 했는데, 졸업 전) none;
/// - 휴학/복학/재학/수료: the noun, then optionally its person (생/자, 들) or
///   progressive (중) form, then nothing, the 하다 verb or a closed-class
///   tail; a prospective form (휴학 예정, 복학 예정자) is none.
///
/// Another noun after the status noun (재학증명서, 졸업작품) reads none; a
/// bare "대학생" names the target.
pub(crate) fn enrollment_cue(word: &str) -> Option<&'static str> {
    const CUES: &[(&str, &str)] = &[
        ("휴학", "on_leave"),
        ("복학", "enrolled"),
        ("재학", "enrolled"),
        ("졸업", "graduated"),
        ("수료", "research_student"),
    ];
    let (cue, code) = CUES.iter().find(|(cue, _)| word.starts_with(cue))?;
    let rest = &word[cue.len()..];
    if let Some(after) = PROSPECTIVE.iter().find_map(|p| rest.strip_prefix(p)) {
        let after = after.strip_prefix('중').unwrap_or(after);
        return (*cue == "졸업" && tail_ok(after)).then_some("enrolled");
    }
    if *cue == "졸업" {
        if let Some(after) = ["생", "자"].iter().find_map(|p| rest.strip_prefix(p)) {
            return tail_ok(after.strip_prefix('들').unwrap_or(after)).then_some("graduated");
        }
        let completed = rest.starts_with('후')
            || rest.starts_with("이후")
            || ["한", "했", "함"].iter().any(|v| rest.starts_with(v));
        return completed.then_some("graduated");
    }
    let mut rest = rest;
    for part in ["생", "자"] {
        if let Some(r) = rest.strip_prefix(part) {
            rest = r;
            break;
        }
    }
    rest = rest.strip_prefix("들").unwrap_or(rest);
    rest = rest.strip_prefix("중").unwrap_or(rest);
    const VERB: &[&str] = &["하", "한", "했", "해", "할", "함"];
    (tail_ok(rest) || VERB.iter().any(|v| rest.starts_with(v))).then_some(code)
}

/// The status a question word states: read on the word as written (its
/// tense is in the ending: "졸업했는데"), else on its stem.
pub(crate) fn status_of(word: &str) -> Option<&'static str> {
    enrollment_cue(word).or_else(|| enrollment_cue(text::stem(word)))
}

/// Question words with a status phrase composed across a space: a status
/// noun (bare, or with 할/을 before a prospective: "졸업할 예정", "졸업을
/// 앞둔") followed by a word that is exactly a joiner less its tail
/// ([`STATUS_JOINERS`]: "졸업 예정자", "휴학 중이라서", "졸업 후", "졸업 유예").
/// "휴학 중요한" is not joined.
pub(crate) fn status_words(words: &[String]) -> Vec<String> {
    status_phrases(words)
        .into_iter()
        .map(|(word, _)| word)
        .collect()
}

/// [`status_words`], each with the question words it joined.
fn status_phrases(words: &[String]) -> Vec<(String, Vec<String>)> {
    const NOUNS: &[&str] = &["휴학", "복학", "재학", "졸업", "수료"];
    let joins = |word: &str, next: &str| {
        let noun = NOUNS
            .iter()
            .any(|n| word == *n || word.strip_prefix(n).is_some_and(|r| r == "할" || r == "을"));
        noun && STATUS_JOINERS
            .iter()
            .any(|j| next.strip_prefix(j).is_some_and(tail_ok))
    };
    let mut out = Vec::with_capacity(words.len());
    let mut i = 0;
    while i < words.len() {
        if words.get(i + 1).is_some_and(|n| joins(&words[i], n)) {
            out.push((
                format!("{}{}", words[i], words[i + 1]),
                vec![words[i].clone(), words[i + 1].clone()],
            ));
            i += 2;
        } else {
            out.push((words[i].clone(), Vec::new()));
            i += 1;
        }
    }
    out
}

/// Marker stems that exclude the value before them, with free endings
/// (빼고, 빼면, 빼곤, 빼주세요, 빼놓고, 제외한, 아니고 …). The one exclusion
/// lexicon: the guard and the argument parser both read it
/// ([`exclusions`]). 말- only as 말고/말구 (말 alone is a noun). A
/// disjunction ("아니면") excludes nothing; "뿐(만) 아니라" adds.
const MARKER_STEMS: &[&str] = &[
    "말고",
    "말구",
    "빼",
    "뺀",
    "뺄",
    "제외",
    "아니고",
    "아니라",
    "아님",
    "이외",
    "없이",
];
/// Markers that are only whole words ("부산 외 지역", "아닌 곳"): glued they
/// end other words (해외, …).
const MARKER_WORDS: &[&str] = &["외", "외에", "외의", "아닌"];
/// Coordination a value phrase may carry ("부산이랑 서울 말고").
const COORDINATION: &[&str] = &["이랑", "랑", "하고", "와", "과", "및"];
/// Dependent nouns a value may carry before its marker ("부산 지역 말고",
/// "부산 쪽 말고", "부산 거 말고"), bare: the one word the reader steps back over.
const DEPENDENT_NOUNS: &[&str] = &["지역", "쪽", "거", "것", "곳", "데"];

/// A value exclusion in a goal: the marker word, the words of the excluded
/// phrase, and the excluded values (filter values when recognised, else
/// the word right before the marker).
pub(crate) struct Exclusion {
    pub marker: usize,
    pub phrase: Vec<usize>,
    pub values: Vec<String>,
}

fn bare(w: &str) -> String {
    w.trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase()
}

/// The marker a word is or ends with, and the value glued in front of it.
fn marker_in(word: &str) -> Option<(&'static str, &str)> {
    if MARKER_WORDS.contains(&word) {
        let marker = MARKER_WORDS.iter().find(|m| **m == word)?;
        return Some((marker, ""));
    }
    // "아닌데", "아닌가요": read (and set aside) as the 아닌 marker.
    if word.starts_with("아닌") {
        return Some(("아닌", ""));
    }
    MARKER_STEMS
        .iter()
        .filter_map(|m| word.find(m).map(|at| (at, *m)))
        .min_by_key(|(at, m)| (*at, std::cmp::Reverse(m.len())))
        // Glued markers need two letters ("빼" glued is too short to read).
        .filter(|(at, m)| *at == 0 || m.chars().count() >= 2)
        .map(|(at, m)| (m, &word[..at]))
}

/// Heads that negate the verb before them ("켜지 마", "빼지 말고",
/// "제외하지 않고"): the one negation lexicon, read by the exclusion reader
/// and the guard.
const NEGATION_HEADS: &[&str] = &[
    "마요",
    "마세요",
    "마라",
    "말고",
    "말아",
    "말구",
    "말라",
    "말자",
    "않",
    "못하",
];

pub(crate) fn is_negation_head(s: &str) -> bool {
    s == "마" || NEGATION_HEADS.iter().any(|h| s.starts_with(h))
}

/// Endings of 빼- that state an exclusion: a closed list (빼고, 빼면, 빼곤,
/// 빼서, 빼줘, 빼주세요, 빼놓고, a bare 빼). Any other ending is another verb
/// or a negation ("빼먹은", "빼지").
const BBAE_ENDINGS: &[&str] = &["고", "면", "곤", "서", "줘", "주", "놓"];
/// Light verbs a 제외 marker carries before its ending (제외하고, 제외시켜).
const LIGHT_VERBS: &[&str] = &["시키", "시켜", "하", "해", "할", "한"];

/// A marker verb (빼/뺀/뺄/제외) excludes only in an affirmative form: its
/// own ending after a light verb starting with 지 ("빼지 마", "제외하지
/// 않고"), a negation after the bare marker ("제외 안 하고": the guard's
/// negation heads, or 안/못), or "필요 없-" after it ("뺄 필요 없이")
/// leave the value as the user asserted it.
fn negated_marker(marker: &str, tail: &str, next: Option<&String>, after: Option<&String>) -> bool {
    if !matches!(marker, "빼" | "뺀" | "뺄" | "제외") {
        return false;
    }
    if marker == "빼" && !tail.is_empty() && !BBAE_ENDINGS.iter().any(|e| tail.starts_with(e)) {
        return true;
    }
    let ending = LIGHT_VERBS
        .iter()
        .find_map(|v| tail.strip_prefix(v))
        .unwrap_or(tail);
    if ending.starts_with('지') {
        return true;
    }
    let negation = next.is_some_and(|n| n == "안" || n == "못" || is_negation_head(n));
    let needless = next.is_some_and(|n| {
        n.strip_prefix("필요").is_some_and(|rest| {
            rest.starts_with('없') || after.is_some_and(|a| a.starts_with('없'))
        })
    });
    (ending.is_empty() && negation) || needless
}

/// The words a denial sets aside, with its own index: a region or status
/// value (stepping back over one dependent noun: "졸업한 건 아닌데") before
/// a copula negation ("아닌데", "아니야", "아닙니다", "아니어도"). The
/// search reads no status, no region and no exclusion from it. 아니고,
/// 아니라, 아님 and a whole 아닌 are exclusion markers; 아니면 is a
/// disjunction. 안/못 never deny: "휴학생은 안 돼?" asks about the value.
/// The single-programme verdict reads no denial: a question about the
/// programme asks the value whatever its polarity.
fn denial<S: AsRef<str>>(words: &[S], i: usize) -> Option<Vec<usize>> {
    let word = words[i].as_ref();
    let copula = ["아니", "아닌", "아닙", "아냐"]
        .iter()
        .any(|p| word.starts_with(p))
        && !["아니고", "아니라", "아니면"]
            .iter()
            .any(|p| word.starts_with(p))
        && !matches!(word, "아님" | "아닌");
    if !copula || i == 0 {
        return None;
    }
    let mut k = i - 1;
    let mut set = vec![i, k];
    // "건" is 것 with its topic marker ("졸업한 건 아닌데").
    if (DEPENDENT_NOUNS.contains(&words[k].as_ref()) || words[k].as_ref() == "건") && k > 0 {
        k -= 1;
        set.push(k);
    }
    let value = words[k].as_ref();
    let denied = mazimi_core::canonical_region_code(text::stem(value)).is_some()
        || status_of(value).is_some();
    denied.then_some(set)
}

/// Every value exclusion of `words` (whitespace words of the goal), and the
/// marker words that excluded nothing (an addition, a statement, a verb
/// negation) with the values a denial sets aside ([`denial`]), which are no
/// keywords and no filter values either. The value is the word right
/// before the marker when it is a filter value (with a value coordinated
/// to it), or the bare value before a bare dependent noun ("부산 지역
/// 말고"); a recognised value is excluded whatever its last letter ("복지
/// 말고"). Any other adjacent word is the excluded thing: it leaves with
/// the marker and nothing is excluded. When the reading is not certain, no
/// filter changes: a value the user asserted is never excluded.
pub(crate) fn exclusions<S: AsRef<str>>(words: &[S]) -> (Vec<Exclusion>, Vec<usize>) {
    let words: Vec<String> = words.iter().map(|w| bare(w.as_ref())).collect();
    let mut out = Vec::new();
    let mut idle = Vec::new();
    for (i, word) in words.iter().enumerate() {
        if let Some(set) = denial(&words, i) {
            idle.extend(set);
            continue;
        }
        let Some((marker, glued)) = marker_in(word) else {
            continue;
        };
        let tail = &word[glued.len() + marker.len()..];
        // "아닌데", "아닌가요": a statement or a question about oneself.
        if marker == "아닌" && !tail.is_empty() {
            idle.push(i);
            continue;
        }
        if negated_marker(marker, tail, words.get(i + 1), words.get(i + 2)) {
            idle.push(i);
            continue;
        }
        // The passive (제외되-, 제외돼-, 제외됨), a copula (제외인가요, 제외야,
        // 제외예요) or a quotative (제외래, 제외라던데) states the programme's
        // rule, never the user's exclusion; a question about it stays a
        // question.
        if marker == "제외"
            && tail.starts_with([
                '되', '돼', '됨', '된', '될', '이', '인', '야', '예', '에', '입', '래', '라',
            ])
        {
            idle.push(i);
            continue;
        }
        let mut phrase: Vec<usize> = Vec::new();
        let mut values: Vec<String> = Vec::new();
        if !glued.is_empty() {
            values.push(glued.to_owned());
        } else if i == 0 {
            idle.push(i);
            continue;
        } else {
            // The value is the word the marker governs: the word right
            // before it (round-4 review H-1: a value asserted earlier in the
            // sentence is never reached).
            let adjacent = &words[i - 1];
            phrase.push(i - 1);
            if filter_value(adjacent).is_some() {
                values.push(adjacent.clone());
                // A value coordinated to it ("부산이랑 서울").
                if let Some(k) = (i - 1).checked_sub(1) {
                    let prev = &words[k];
                    if let Some(stem) = COORDINATION.iter().find_map(|c| prev.strip_suffix(c)) {
                        if filter_value(stem).is_some() {
                            phrase.insert(0, k);
                            values.insert(0, stem.to_owned());
                        }
                    }
                }
            } else if matches!(marker, "말고" | "말구") && adjacent.ends_with('지') {
                // "-지 말고": a verb negation ("놓치지 말고", "켜지 말고"); the
                // guard reads it as a negated request.
                idle.push(i);
                continue;
            } else if DEPENDENT_NOUNS.contains(&adjacent.as_str()) {
                // One step back over a bare dependent noun, onto a bare value.
                let value = (i - 1)
                    .checked_sub(1)
                    .map(|k| (k, &words[k]))
                    .filter(|(_, w)| text::stem(w) == w.as_str() && filter_value(w).is_some());
                if let Some((k, w)) = value {
                    phrase.insert(0, k);
                    values.push(w.clone());
                }
            }
            // Otherwise the adjacent word is the excluded thing ("게임 말고",
            // "서류 없이"): it leaves with the marker, nothing is excluded.
        }
        let additive = marker == "아니라"
            && values
                .first()
                .or(words.get(i.wrapping_sub(1)))
                .is_some_and(|v| v.ends_with('뿐') || v.ends_with("뿐만"));
        if additive {
            idle.push(i);
            continue;
        }
        out.push(Exclusion {
            marker: i,
            phrase,
            values,
        });
    }
    (out, idle)
}

/// What a negated word is as a filter value, if anything.
enum Excluded {
    Region(&'static str),
    Other,
}

fn filter_value(word: &str) -> Option<Excluded> {
    let stemmed = text::stem(word);
    if let Some(code) = mazimi_core::canonical_region_code(stemmed) {
        return Some(Excluded::Region(code));
    }
    let named = status_of(word).is_some()
        || TYPE_CUES.iter().any(|(cue, _)| stemmed.contains(cue))
        || !mazimi_core::categories::fields_named(word).is_empty();
    named.then_some(Excluded::Other)
}

/// `question` without its exclusions (each marker and its value), the
/// excluded words that are filter values other than a region (said in the
/// search notice), and the excluded regions (typed: the search decides on
/// them). An excluded word that is no filter value is dropped silently.
pub(crate) fn without_negated(question: &str) -> (String, Vec<String>) {
    let (kept, negated, _) = read_exclusions(question);
    (kept, negated)
}

fn read_exclusions(question: &str) -> (String, Vec<String>, Vec<&'static str>) {
    let words: Vec<&str> = question.split_whitespace().collect();
    let mut drop = vec![false; words.len()];
    let mut negated = Vec::new();
    let mut regions = Vec::new();
    let (found, idle) = exclusions(&words);
    for i in idle {
        drop[i] = true;
    }
    for e in found {
        drop[e.marker] = true;
        for i in &e.phrase {
            drop[*i] = true;
        }
        for value in &e.values {
            match filter_value(value) {
                Some(Excluded::Region(code)) if !regions.contains(&code) => regions.push(code),
                Some(Excluded::Region(_)) | None => {}
                Some(Excluded::Other) => negated.push(text::stem(value).to_owned()),
            }
        }
    }
    let kept: Vec<&str> = words
        .iter()
        .zip(&drop)
        .filter(|(_, d)| !**d)
        .map(|(w, _)| *w)
        .collect();
    (kept.join(" "), negated, regions)
}

/// Lower-bound words after an amount, matched whole ("넘는데", "넘어도" are
/// concessive, not bounds).
const AMOUNT_BOUNDS: &[&str] = &[
    "이상",
    "이상인",
    "이상의",
    "넘는",
    "넘게",
    "넘고",
    "초과",
    "초과하는",
    "부터",
];
/// The period word before an amount, by stored column.
const AMOUNT_PERIOD_WORDS: &[(&str, &[&str])] = &[
    (
        "monthly",
        &["월", "매달", "매월", "달에", "한달에", "월마다"],
    ),
    (
        "semester",
        &["학기", "학기당", "학기에", "한학기", "학기마다"],
    ),
    ("once", &["일시금", "한번에", "1회"]),
];
/// Nouns whose amount is the benefit's (the noun before the amount or the
/// word after its bound), and the giving verbs after a bound.
const BENEFIT_NOUNS: &[&str] = &[
    "지원금",
    "장학금",
    "수당",
    "지원",
    "혜택",
    "보조금",
    "금액",
    "장학",
];
/// An adnominal form ("주는", "받을", "지원되는"): its last syllable closes in
/// ㄴ or ㄹ, read by Hangul decomposition.
fn adnominal(word: &str) -> bool {
    word.chars().last().is_some_and(|c| {
        let code = (c as u32).wrapping_sub(0xAC00);
        code < 11_172 && matches!(code % 28, 4 | 8)
    })
}

/// Concessive and contrastive endings: what they close is the speaker's.
const CONCESSIVE: &[&str] = &[
    "는데",
    "은데",
    "ㄴ데",
    "어도",
    "아도",
    "해도",
    "여도",
    "지만",
    "더라도",
];
/// What may follow an amount without 원: particles, or nothing.
const AMOUNT_PARTICLES: &[&str] = &[
    "", "을", "를", "이", "가", "은", "는", "씩", "의", "정도", "까지",
];

fn concessive(word: &str) -> bool {
    CONCESSIVE.iter().any(|c| word.ends_with(c))
}

/// What an amount word is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Money<'a> {
    /// A money amount within [`mazimi_core::agent::MAX_AMOUNT_WON`], with
    /// what follows it.
    Amount(i32, &'a str),
    /// Money, but no amount a search takes (past the limit, or zero).
    Unread(&'a str),
    None,
}

/// The won an amount word states, its whole numeral run composed
/// ("30만원", "300,000원", "3만5천원", "5천만원", "1억2천만원", "천만원",
/// "30만"), with checked arithmetic. A count ("2만명") or a partial read is
/// no money.
fn won(word: &str) -> Money<'_> {
    let mut total: i64 = 0;
    let mut section: i64 = 0;
    let mut number: i64 = 0;
    let mut digits = false;
    let mut unit_seen = false;
    let mut end = 0;
    let mut wonned = false;
    let mut overflow = false;
    let checked = |r: Option<i64>, overflow: &mut bool| {
        r.unwrap_or_else(|| {
            *overflow = true;
            0
        })
    };
    for (at, c) in word.char_indices() {
        match c {
            '0'..='9' => {
                number = checked(
                    number
                        .checked_mul(10)
                        .and_then(|n| n.checked_add(i64::from(c as u8 - b'0'))),
                    &mut overflow,
                );
                digits = true;
            }
            ',' if digits => {}
            '천' | '백' | '만' | '억' => {
                let scale = match c {
                    '천' => 1_000,
                    '백' => 100,
                    '만' => 10_000,
                    _ => 100_000_000,
                };
                // "천만원": a magnitude with no digit before it counts once.
                let n = if digits { number } else { 1 };
                if matches!(c, '천' | '백') {
                    section = checked(section.checked_add(n.saturating_mul(scale)), &mut overflow);
                } else {
                    let part = if digits || section == 0 {
                        section + n
                    } else {
                        section
                    };
                    total = checked(
                        part.checked_mul(scale).and_then(|p| total.checked_add(p)),
                        &mut overflow,
                    );
                    section = 0;
                }
                number = 0;
                digits = false;
                unit_seen = true;
            }
            '원' if unit_seen || digits => {
                wonned = true;
                end = at + c.len_utf8();
                break;
            }
            _ => {
                end = at;
                break;
            }
        }
        end = at + c.len_utf8();
    }
    if !(unit_seen || wonned) {
        return Money::None;
    }
    let rest = &word[end..];
    // An amount ends the word or takes a particle or bound word only, with
    // 원 or without: a counter (명, 개, 대) or a name ("천원의아침밥") is no
    // money.
    if !AMOUNT_PARTICLES.contains(&rest) && !AMOUNT_BOUNDS.contains(&rest) {
        return Money::None;
    }
    if overflow {
        return Money::Unread(rest);
    }
    let total = total.saturating_add(section).saturating_add(number);
    match i32::try_from(total) {
        Ok(won) if mazimi_core::agent::valid_amount(won) => Money::Amount(won, rest),
        _ => Money::Unread(rest),
    }
}

/// `question` without its amount words, and the lower bound they state.
/// An amount word, read or not, is never a keyword, nor is its bound word.
/// The amount is a bound only when it is the benefit's, by one rule for
/// the bound word and the word after it: the bound is a whole bound word
/// ([`AMOUNT_BOUNDS`]) without a concessive ending, and either a benefit
/// noun comes right before the amount (a period word between allowed), or
/// no other noun does and a benefit noun or a giving verb
/// ([`GIVING_VERBS`]) follows the bound. Anything else ("월세 50만원 넘는데",
/// "월 300만원 이상 받는데") reads no bound: an unfiltered list is right more
/// often than a wrong filter.
pub(crate) fn amount_bound(question: &str) -> (String, Option<(i32, Option<&'static str>)>) {
    // "100만 원": one amount word across a space.
    let mut words: Vec<String> = Vec::new();
    for word in question.split_whitespace() {
        // Only when the first word is money and the second is 원 with a
        // particle or bound word ("100만 원 이상", not "청년만 원하는").
        let joins = words.last().is_some_and(|prev: &String| {
            prev.ends_with(['만', '천', '억'])
                && won(prev) != Money::None
                && word.strip_prefix('원').is_some_and(|r| {
                    let r = r.trim_matches(|c: char| !c.is_alphanumeric());
                    AMOUNT_PARTICLES.contains(&r) || AMOUNT_BOUNDS.contains(&r)
                })
        });
        if joins {
            let prev = words.pop().unwrap_or_default();
            words.push(format!("{prev}{word}"));
        } else {
            words.push(word.to_owned());
        }
    }
    let bare = |w: &str| {
        w.trim_matches(|c: char| !c.is_alphanumeric() && c != ',')
            .to_owned()
    };
    let is_bound = |w: &str| {
        !concessive(w) && (AMOUNT_BOUNDS.contains(&w) || AMOUNT_BOUNDS.contains(&text::stem(w)))
    };
    let names_bound = |w: &str| {
        AMOUNT_BOUNDS
            .iter()
            .any(|b| w.starts_with(b) || text::stem(w).starts_with(b))
    };
    let period_of = |w: &str| {
        AMOUNT_PERIOD_WORDS
            .iter()
            .find(|(_, ws)| ws.contains(&w) || ws.contains(&text::stem(w)))
            .map(|(p, _)| *p)
    };
    let benefit = |w: &str| {
        let w = text::stem(w);
        BENEFIT_NOUNS.iter().any(|n| w.ends_with(n))
    };
    let mut drop = vec![false; words.len()];
    let mut bound = None;
    for (i, raw) in words.iter().enumerate() {
        let word = bare(raw);
        // "월30만원": the period glued in front.
        let glued = AMOUNT_PERIOD_WORDS.iter().find_map(|(p, ws)| {
            ws.iter().find_map(|w| {
                word.strip_prefix(w)
                    .filter(|r| won(r) != Money::None)
                    .map(|r| (*p, r.to_owned()))
            })
        });
        let (glued_period, amount_word) = match &glued {
            Some((p, rest)) => (Some(*p), rest.clone()),
            None => (None, word.clone()),
        };
        let (min, rest) = match won(&amount_word) {
            Money::None => continue,
            // A digitless magnitude ("천원", "만원") is an amount only with a
            // bound word ("만원의 행복" is a name, "만 19세" an age).
            _ if !amount_word.chars().any(|c| c.is_ascii_digit())
                && !words
                    .get(i + 1)
                    .is_some_and(|n| AMOUNT_BOUNDS.iter().any(|b| bare(n).starts_with(b)))
                && !AMOUNT_BOUNDS.iter().any(|b| amount_word.ends_with(b)) =>
            {
                continue
            }
            Money::Amount(min, rest) => (Some(min), rest.to_owned()),
            Money::Unread(rest) => (None, rest.to_owned()),
        };
        drop[i] = true;
        let next = words.get(i + 1).map(|w| bare(w)).unwrap_or_default();
        // The bound word, glued or the next word, never a keyword.
        let (bounded, head_at) = if names_bound(&rest) {
            (is_bound(&rest), i + 1)
        } else if names_bound(&next) {
            drop[i + 1] = true;
            (is_bound(&next), i + 2)
        } else {
            (false, words.len())
        };
        let following: Vec<String> = words.iter().skip(head_at).map(|w| bare(w)).collect();
        let mut j = i;
        let mut period = glued_period;
        if period.is_none() {
            if let Some(p) = i.checked_sub(1).and_then(|b| period_of(&bare(&words[b]))) {
                period = Some(p);
                drop[i - 1] = true;
                j = i - 1;
            }
        }
        // The amount's owner: no noun before it, or a benefit noun.
        let noun_before = j.checked_sub(1).map(|b| bare(&words[b]));
        let owned = noun_before.as_deref().is_none_or(benefit);
        // The head of the phrase after the bound: nothing, a benefit noun or
        // 거/것 directly, or after one adnominal ("주는 지원금", "받는 거").
        // A person or any other noun, or a concessive anywhere after it,
        // means the amount is the speaker's (round-4 review M-1).
        let head = |w: &str| benefit(w) || matches!(text::stem(w), "거" | "것");
        let headed = match following.first() {
            None => true,
            Some(w) if head(w) => true,
            Some(w) if adnominal(w) => following.get(1).is_none_or(|h| head(h)),
            Some(_) => false,
        };
        let speakers = following.iter().any(|w| concessive(w));
        if let (Some(min), true, true, true, false, None) =
            (min, bounded, owned, headed, speakers, bound)
        {
            bound = Some((min, period));
        }
    }
    let kept: Vec<&str> = words
        .iter()
        .zip(&drop)
        .filter(|(_, d)| !**d)
        .map(|(w, _)| w.as_str())
        .collect();
    (kept.join(" "), bound)
}

/// "월 30만원 이상": an amount bound as the answer and the notice say it.
pub(crate) fn amount_label(min: i64, period: Option<&str>) -> String {
    let period = match period {
        Some("monthly") => "월 ",
        Some("semester") => "학기 ",
        Some("once") => "1회 ",
        _ => "",
    };
    let amount = if min % 10_000 == 0 {
        format!("{}만원", min / 10_000)
    } else {
        format!("{min}원")
    };
    format!("{period}{amount} 이상")
}

pub fn parse(question: &str) -> SearchIntent {
    let (question, negated, excluded_regions) = read_exclusions(question);
    let (question, amount) = amount_bound(&question);
    let question = question.as_str();
    let mut intent = SearchIntent {
        negated,
        excluded_regions,
        amount_min: amount.map(|(min, _)| min),
        amount_period: amount.and_then(|(_, period)| period),
        ..SearchIntent::default()
    };
    let mut consumed: Vec<String> = Vec::new();
    for (word, joined) in status_phrases(&text::words(question)) {
        let stemmed = text::stem(&word).to_owned();
        if intent.region.is_none() {
            if let Some(code) = mazimi_core::canonical_region_code(&stemmed) {
                intent.region = Some(code);
                intent.region_label = Some(mazimi_core::region_label(code));
                consumed.push(stemmed.clone());
                continue;
            }
        }
        if let Some(code) = status_of(&word) {
            if intent.enrollment_status.is_none() {
                intent.enrollment_status = Some(code);
                intent.enrollment_label = Some(mazimi_core::enrollment_status_label(code));
            }
            consumed.push(stemmed.clone());
            // The words a composed phrase joined are consumed too (M-4).
            for part in joined {
                consumed.push(text::stem(&part).to_owned());
                consumed.push(part);
            }
            continue;
        }
        let mut typed = false;
        for (cue, types) in TYPE_CUES {
            if stemmed.contains(cue) {
                typed = true;
                for t in *types {
                    if !intent.program_types.contains(t) {
                        intent.program_types.push(t);
                    }
                }
            }
        }
        if typed {
            consumed.push(stemmed);
        }
    }
    let keywords: Vec<Term> = text::query_terms(question)
        .into_iter()
        .filter(|t| t.specific)
        .filter(|t| !consumed.contains(&t.surface))
        .filter(|t| !FRAME_WORDS.contains(&t.surface.as_str()))
        .collect();
    intent.keywords = keywords.iter().map(|t| t.surface.clone()).collect();
    intent.keyword_forms = keywords.into_iter().map(|t| t.forms).collect();
    intent
}

/// An intent from already-validated codes (the agent's `search_benefits`
/// arguments). Unknown codes are dropped; keywords get the same matching
/// forms a parsed question would.
pub fn from_parts(args: &mazimi_core::agent::SearchArgs) -> SearchIntent {
    let region = args.region.as_deref();
    let enrollment_status = args.enrollment_status.as_deref();
    let program_types = &args.program_types;
    let keywords = &args.keywords;
    let region = region.and_then(mazimi_core::canonical_region_code);
    let enrollment_status = enrollment_status.and_then(mazimi_core::canonical_enrollment_status);
    let mut intent = SearchIntent {
        region,
        region_label: region.map(mazimi_core::region_label),
        enrollment_status,
        enrollment_label: enrollment_status.map(mazimi_core::enrollment_status_label),
        fields: args
            .fields
            .iter()
            .filter_map(|f| mazimi_core::categories::parse_field(f))
            .collect(),
        deadline_within_days: args
            .deadline_within_days
            .filter(|d| *d <= mazimi_core::agent::MAX_DEADLINE_DAYS),
        deadline_from_days: args
            .deadline_from_days
            .filter(|from| args.deadline_within_days.is_some_and(|to| from <= &to)),
        earliest: args.earliest,
        excluded_regions: args
            .excluded_regions
            .iter()
            .filter_map(|r| mazimi_core::canonical_region_code(r))
            .collect(),
        amount_min: args
            .amount_min
            .filter(|min| mazimi_core::agent::valid_amount(*min)),
        amount_period: args
            .amount_min
            .and(args.amount_period.as_deref())
            .and_then(|p| {
                mazimi_core::agent::AMOUNT_PERIODS
                    .iter()
                    .copied()
                    .find(|known| *known == p)
            }),
        ..SearchIntent::default()
    };
    for t in program_types {
        if let Some(known) = mazimi_core::agent::PROGRAM_TYPES
            .iter()
            .find(|known| **known == t.as_str())
        {
            if !intent.program_types.contains(known) {
                intent.program_types.push(known);
            }
        }
    }
    for keyword in keywords {
        if let Some(term) = text::query_terms(keyword).into_iter().next() {
            if !intent.keywords.contains(&term.surface) {
                intent.keywords.push(term.surface);
                intent.keyword_forms.push(term.forms);
            }
        }
    }
    intent
}

/// Keywords of `intent` that `title` or `summary` contain.
pub fn matched_keywords(intent: &SearchIntent, title: &str, summary: Option<&str>) -> Vec<String> {
    let (haystack, _) = text::compact(&format!("{title} {}", summary.unwrap_or("")));
    intent
        .keywords
        .iter()
        .zip(&intent.keyword_forms)
        .filter(|(_, forms)| forms.iter().any(|f| haystack.contains(f.as_str())))
        .map(|(k, _)| k.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn region_enrollment_type_and_keywords_are_parsed() {
        let intent = parse("부산 사는 휴학생이 받을 수 있는 장학금?");
        assert_eq!(intent.region, Some("busan"));
        assert_eq!(intent.enrollment_status, Some("on_leave"));
        assert_eq!(intent.program_types, ["scholarship"]);
        assert!(intent.keywords.is_empty(), "{:?}", intent.keywords);

        let intent = parse("서울 월세 지원 알려줘");
        assert_eq!(intent.region, Some("seoul"));
        assert_eq!(intent.keywords, ["월세"]);
        assert_eq!(
            matched_keywords(&intent, "청년 주거비 지원", Some("월 임대료를 지원")),
            ["월세"]
        );
        assert!(parse("뭐 있어?").is_empty());
    }

    /// Audit M6: an amount with a lower bound is a filter; an amount is
    /// never a keyword.
    #[test]
    fn amount_bounds_are_filters_not_keywords() {
        for (question, bound) in [
            (
                "월 30만원 이상 주는 지원금",
                Some((300_000, Some("monthly"))),
            ),
            ("월30만원이상 지원", Some((300_000, Some("monthly")))),
            ("30만원 넘는 장학금", Some((300_000, None))),
            (
                "학기당 200만원 이상 장학금",
                Some((2_000_000, Some("semester"))),
            ),
            ("300,000원 이상 받는 거", Some((300_000, None))),
            ("30만원 지원", None),
        ] {
            let intent = parse(question);
            assert_eq!(
                intent.amount_min.map(|m| (m, intent.amount_period)),
                bound,
                "{question}"
            );
            assert!(
                !intent
                    .keywords
                    .iter()
                    .any(|k| k.chars().any(|c| c.is_ascii_digit()) || k == "이상"),
                "{question}: {:?}",
                intent.keywords
            );
        }
    }

    /// Round-2 review (code M-4, M-5, M-6; arch M4): one reading of value
    /// exclusions, glued and inflected forms included; a disjunction is no
    /// exclusion; only a filter value is reported; an excluded region is
    /// typed.
    #[test]
    fn exclusions_are_read_once_and_typed() {
        assert_eq!(parse("휴학생 제외한 장학금").enrollment_status, None);
        assert_eq!(parse("부산 뺀 지역 장학금").region, None);
        assert_eq!(parse("부산 말구 서울 장학금").region, Some("seoul"));
        let either = parse("장학금 아니면 월세 지원 찾아줘");
        assert!(either.program_types.contains(&"scholarship"), "{either:?}");
        assert!(either.negated.is_empty(), "{either:?}");
        let both = parse("장학금뿐만 아니라 생활비도");
        assert!(both.program_types.contains(&"scholarship"), "{both:?}");
        assert!(parse("그거 말고 다른 장학금").negated.is_empty());
        assert_eq!(parse("휴학생 제외한 장학금").negated, ["휴학생"]);
        let elsewhere = parse("부산 말고 장학금 찾아줘");
        assert_eq!(elsewhere.excluded_regions, ["busan"]);
        assert!(
            elsewhere.negated.is_empty(),
            "a region is handled, not noticed"
        );
    }

    /// Round 4 (code L-1, L-2, regressions): an amount is joined across a
    /// space only when the first word is money, a 원 amount takes only a
    /// particle or bound word, and a digitless 천원/만원 is an amount only with
    /// a bound.
    #[test]
    fn words_are_not_read_as_amounts_by_accident() {
        let glued = parse("청년만 원하는 지원").keywords;
        assert!(!glued.iter().any(|k| k.contains("청년만원")), "{glued:?}");
        let campus = parse("천원의아침밥 알려줘").keywords;
        assert!(campus.contains(&"천원의아침밥".to_owned()), "{campus:?}");
        let happy = parse("만원의 행복 알려줘").keywords;
        assert!(happy.iter().any(|k| k.contains("만원")), "{happy:?}");
        assert_eq!(parse("천만원 이상 지원").amount_min, Some(10_000_000));
    }

    /// Round 4 (code M-3): a passive states the programme's rule, never the
    /// user's exclusion; a question about exclusion stays a question.
    #[test]
    fn a_passive_exclusion_is_the_programmes_rule() {
        let intent = parse("휴학생도 제외되나요");
        assert_eq!(intent.enrollment_status, Some("on_leave"));
        assert!(intent.negated.is_empty(), "{intent:?}");
        assert!(exclusions(&["대상에서", "제외돼"]).0.is_empty());
        assert!(exclusions(&["휴학생", "제외됨"]).0.is_empty());
        assert_eq!(parse("휴학생 제외한 장학금").negated, ["휴학생"]);
        // Final review L-3: 제외 with a copula or a quotative is the
        // programme's rule too (main's reading: the status stays).
        for question in [
            "휴학생 제외인가요",
            "휴학생 제외야?",
            "휴학생은 제외래 맞아?",
            "휴학생 제외라던데 맞아?",
            "휴학생 제외예요?",
        ] {
            let intent = parse(question);
            assert_eq!(intent.enrollment_status, Some("on_leave"), "{question}");
            assert!(intent.negated.is_empty(), "{question}: {intent:?}");
        }
        assert_eq!(parse("휴학생 제외하고 장학금").negated, ["휴학생"]);
    }

    /// Round 4 (code M-2): 졸업 is an event. Graduated only from a completed
    /// form, enrolled from the prospective, anything else no status; a
    /// prospective 휴학/복학 is no status either.
    #[test]
    fn graduation_is_read_by_aspect() {
        for (question, status) in [
            ("졸업할 예정인데 받을 수 있어?", Some("enrolled")),
            ("졸업을 앞둔 4학년도 돼?", Some("enrolled")),
            ("졸업 유예 중인데 받을 수 있어?", Some("enrolled")),
            ("졸업 예정자도 받을 수 있는 장학금", Some("enrolled")),
            ("졸업하지 않은 사람도 돼?", None),
            ("졸업 안 했는데 돼?", None),
            ("졸업 못 했는데 신청 가능?", None),
            ("졸업 전에 신청 가능?", None),
            ("내년에 졸업하는데 돼?", None),
            ("곧 졸업해요", None),
            ("졸업생도 받을 수 있는 장학금", Some("graduated")),
            ("졸업한 청년 지원", Some("graduated")),
            ("졸업했는데 받을 수 있어?", Some("graduated")),
            ("졸업 후 2년 이내 청년 지원", Some("graduated")),
            ("휴학 예정인데 장학금", None),
            ("휴학할 예정인데 장학금", None),
            ("복학 예정자 장학금", None),
            ("휴학 중인데 장학금", Some("on_leave")),
        ] {
            assert_eq!(parse(question).enrollment_status, status, "{question}");
        }
    }

    /// Round 4 (code M-1): the amount's role is decided by the head after
    /// the bound: a benefit noun, nothing or 거/것 bounds; a person or any
    /// other noun, or a concessive anywhere, does not.
    #[test]
    fn an_amount_bound_is_read_by_its_head() {
        for (question, bound) in [
            ("월 200만원 이상 받는 직장인도 받을 수 있는 지원금", None),
            ("300만원 이상 받는 사람도 신청 가능한 장학금", None),
            ("장학금 100만원 이상 받는데 다른 장학금 있어?", None),
            ("지원금 30만원 넘게 받고 있는데 더 받을 수 있어?", None),
            ("200만원 이상 받는 직장인", None),
            ("월 30만원 이상 주는 지원금", Some(300_000)),
            ("300,000원 이상 받는 거", Some(300_000)),
            ("30만원 넘는 장학금", Some(300_000)),
            ("지원금 50만원 넘는 거", Some(500_000)),
            ("장학금 100만원 이상", Some(1_000_000)),
        ] {
            assert_eq!(parse(question).amount_min, bound, "{question}");
        }
    }

    /// Round 4 (code H-1): the excluded value is the word the marker
    /// governs. A value the user asserted earlier in the sentence is never
    /// excluded; an adjacent non-value word leaves with the marker and
    /// nothing is excluded. Readings are round 2's.
    #[test]
    fn an_asserted_value_is_never_excluded() {
        let rows: [(&str, Option<&str>, Option<&str>); 9] = [
            ("부산 사는데 게임 말고 장학금", Some("busan"), None),
            (
                "부산 대학생 서류 없이 신청 가능한 장학금",
                Some("busan"),
                None,
            ),
            ("부산 거주 제한 없이 받는 장학금", Some("busan"), None),
            (
                "휴학생인데 소득 제한 없이 받는 장학금",
                None,
                Some("on_leave"),
            ),
            (
                "졸업생도 나이 제한 없이 되는 지원금",
                None,
                Some("graduated"),
            ),
            (
                "서울 휴학생 대상 공모전 빼고 알려줘",
                Some("seoul"),
                Some("on_leave"),
            ),
            (
                "부산 살고 휴학 중인데 부담 없이 받을 수 있는 장학금",
                Some("busan"),
                Some("on_leave"),
            ),
            (
                "휴학생은 대상에서 제외되는 장학금 있어?",
                None,
                Some("on_leave"),
            ),
            ("장학금 놓치지 말고 알려줘", None, None),
        ];
        for (question, region, status) in rows {
            let intent = parse(question);
            assert_eq!(intent.region, region, "{question}");
            assert_eq!(intent.enrollment_status, status, "{question}");
            assert!(intent.excluded_regions.is_empty(), "{question}: {intent:?}");
            assert!(intent.negated.is_empty(), "{question}: {intent:?}");
        }
        assert!(parse("장학금 놓치지 말고 알려줘")
            .program_types
            .contains(&"scholarship"));
    }

    /// Final review M-3: a status or region the user denies (a copula
    /// negation after it) reads no status, no region and no exclusion.
    /// "아니고"/"아니라" stay exclusion markers.
    #[test]
    fn a_denied_status_or_region_is_not_read() {
        for (question, status) in [
            ("휴학생 아닌데 받을 수 있는 장학금", None),
            ("휴학생은 아닌데 장학금 있어?", None),
            ("휴학생 아니야 재학생이야", Some("enrolled")),
            ("휴학생 아닙니다 장학금 알려줘", None),
            ("재학생이 아니어도 되는 장학금", None),
            ("졸업한 건 아닌데 받을 수 있어?", None),
        ] {
            let intent = parse(question);
            assert_eq!(intent.enrollment_status, status, "{question}: {intent:?}");
            assert!(intent.negated.is_empty(), "{question}: {intent:?}");
        }
        for question in ["나 부산 아닌데 받을 수 있어?", "부산 아니어도 되는 장학금"]
        {
            let intent = parse(question);
            assert_eq!(intent.region, None, "{question}: {intent:?}");
            assert!(intent.excluded_regions.is_empty(), "{question}: {intent:?}");
            assert!(intent.program_types.contains(&"scholarship") || !question.contains("장학금"));
        }
        // Exclusion markers keep their reading.
        let intent = parse("부산 아니고 서울 장학금");
        assert_eq!(
            (intent.region, intent.excluded_regions),
            (Some("seoul"), vec!["busan"])
        );
        assert_eq!(parse("부산 아닌 곳 장학금").excluded_regions, ["busan"]);
    }

    /// Final review F-1: 안/못 never deny. "X는 안 돼?" asks about X, so
    /// the value reads as on main.
    #[test]
    fn a_negative_question_keeps_its_value() {
        for (question, status) in [
            ("휴학생은 안 돼?", "on_leave"),
            ("휴학생은 못 받아?", "on_leave"),
            ("졸업생은 안 돼?", "graduated"),
            ("휴학생은 못 받는 장학금 있어?", "on_leave"),
            ("휴학생도 안 되나요", "on_leave"),
        ] {
            let intent = parse(question);
            assert_eq!(
                intent.enrollment_status,
                Some(status),
                "{question}: {intent:?}"
            );
            assert!(intent.negated.is_empty(), "{question}: {intent:?}");
        }
        for question in ["부산은 안 돼?", "부산은 안 되는 거야?"] {
            let intent = parse(question);
            assert_eq!(intent.region, Some("busan"), "{question}: {intent:?}");
            assert!(intent.excluded_regions.is_empty(), "{question}: {intent:?}");
        }
    }

    /// Final review M-4: a status phrase composed across a space consumes
    /// both of its words; neither becomes a keyword.
    #[test]
    fn a_composed_status_phrase_leaves_no_keywords() {
        for (question, status) in [
            ("휴학 중인데 장학금 알려줘", "on_leave"),
            ("졸업 예정인데 받을 수 있는 장학금", "enrolled"),
            ("재학 중인데 받을 수 있는 장학금", "enrolled"),
        ] {
            let intent = parse(question);
            assert_eq!(intent.enrollment_status, Some(status), "{question}");
            assert!(intent.keywords.is_empty(), "{question}: {intent:?}");
        }
    }

    /// Pre-merge (mutation j): the excluded thing is the word right before
    /// the marker; a bare value two words back is never reached.
    #[test]
    fn a_value_two_words_back_is_never_excluded() {
        for question in ["부산 게임 말고 장학금", "부산 서류 없이 되는 장학금"]
        {
            let intent = parse(question);
            assert_eq!(intent.region, Some("busan"), "{question}: {intent:?}");
            assert!(intent.excluded_regions.is_empty(), "{question}: {intent:?}");
            assert!(intent.negated.is_empty(), "{question}: {intent:?}");
        }
        let intent = parse("휴학생 게임 말고 장학금");
        assert_eq!(intent.enrollment_status, Some("on_leave"), "{intent:?}");
        assert!(intent.negated.is_empty(), "{intent:?}");
    }

    /// Pre-merge (mutation l): a concessive after the bound, or as the bound
    /// word itself, makes the amount the speaker's: no bound.
    #[test]
    fn a_concessive_amount_is_the_speakers() {
        for question in [
            "100만원 이상 받지만 장학금 찾아줘",
            "100만원 이상 받는데 장학금 찾아줘",
            "100만원 이상 받더라도 신청 가능한 장학금",
            "월 100만원 이상 저축해도 받는 장학금",
            "월 100만원 이상 벌어도 되는 지원금",
            "30만원 넘는데 장학금 있어?",
            "50만원 넘어도 되는 장학금",
            // The bound word itself is concessive ("초과해도" stems to 초과).
            "월 100만원 초과해도 되는 지원금",
        ] {
            assert_eq!(parse(question).amount_min, None, "{question}");
        }
        assert_eq!(
            parse("월 30만원 이상 주는 지원금").amount_min,
            Some(300_000)
        );
    }

    /// Final review M-1: a marker verb excludes only in an affirmative form.
    /// "-지 (말고/마/않고)", a negation after it, or "필요 없-" leave the
    /// value asserted (main's readings); 빼- takes closed endings only.
    #[test]
    fn a_negated_marker_verb_excludes_nothing() {
        for question in [
            "부산도 빼지 말고 알려줘",
            "부산 빼지 마",
            "부산 빼먹지 말고 알려줘",
            "부산 뺄 필요 없이 다 보여줘",
            "부산 제외하지 말고 장학금",
            "부산은 제외하지 않고 알려줘",
            "부산 제외시키지 마",
            "부산 제외 안 하고 알려줘",
            "부산 제외할 필요 없어",
        ] {
            let intent = parse(question);
            assert_eq!(intent.region, Some("busan"), "{question}: {intent:?}");
            assert!(intent.excluded_regions.is_empty(), "{question}: {intent:?}");
            assert!(intent.negated.is_empty(), "{question}: {intent:?}");
        }
        for question in [
            "장학금 빼지 말고 다 알려줘",
            "장학금 제외 안 하고 다 보여줘",
            "장학금 빼먹은 거 없어?",
        ] {
            let intent = parse(question);
            assert!(
                intent.program_types.contains(&"scholarship"),
                "{question}: {intent:?}"
            );
            assert!(intent.negated.is_empty(), "{question}: {intent:?}");
        }
        let intent = parse("휴학생도 빼지 말고 보여줘");
        assert_eq!(intent.enrollment_status, Some("on_leave"), "{intent:?}");
        assert!(intent.negated.is_empty(), "{intent:?}");
        // The affirmative forms still exclude.
        for question in [
            "부산 빼고 알려줘",
            "부산 빼면 뭐 있어?",
            "부산 빼줘",
            "부산 빼서 보여줘",
            "부산 빼놓고 알려줘",
            "부산 뺀 지역 장학금",
            "부산 제외하고 알려줘",
            "부산 제외한 장학금",
            "부산 제외시켜줘",
            "부산 제외해줘",
        ] {
            let intent = parse(question);
            assert_eq!(intent.excluded_regions, ["busan"], "{question}: {intent:?}");
            assert_eq!(intent.region, None, "{question}");
        }
    }

    /// Round 3 (code M-4, L-8): marker stems take free endings, the value
    /// is the nearest filter value of the phrase before the marker, a
    /// recognised value is excluded whatever its last letter, and marker
    /// words never become keywords.
    #[test]
    fn exclusions_read_the_phrase_not_one_word() {
        for question in [
            "부산 지역 말고 장학금",
            "부산 쪽 말고 다른 지역 장학금",
            "부산 거 말고 장학금",
            "부산 빼면 뭐 있어",
            "부산 빼곤 다 알려줘",
            "부산 외 지역 장학금",
            "부산 이외 장학금",
        ] {
            let intent = parse(question);
            assert_eq!(intent.region, None, "{question}");
            assert_eq!(intent.excluded_regions, ["busan"], "{question}");
        }
        let both = parse("부산이랑 서울 말고 장학금");
        assert_eq!(both.region, None);
        assert_eq!(both.excluded_regions, ["busan", "seoul"]);
        for question in [
            "취업 빼주세요 생활비",
            "취업은 빼 줘 생활비",
            "취업 빼놓고 생활비",
        ] {
            assert_eq!(parse(question).negated, ["취업"], "{question}");
        }
        assert_eq!(parse("복지 말고 취업 지원").negated, ["복지"]);
        assert_eq!(parse("휴학생 아님 장학금").enrollment_status, None);
        assert!(!parse("장학금뿐만 아니라 생활비도")
            .keywords
            .contains(&"아니라".to_owned()));
        assert!(!parse("대학생 아닌데 받을 수 있는 장학금")
            .keywords
            .contains(&"아닌데".to_owned()));
    }

    /// Round 3 (code M-5, L-1, L-7): one role rule for the bound word and
    /// the verb (a concessive ending makes the amount the speaker's), a
    /// benefit noun before the amount suffices, "N만 원" across a space, and
    /// an unread money amount never becomes a keyword.
    #[test]
    fn an_amount_is_the_benefits_by_one_rule() {
        for (question, bound) in [
            ("월 300만원 이상 받는데 받을 수 있는 지원금 있어?", None),
            ("한달에 300만원 이상 받는데 신청 가능한 지원 있어?", None),
            ("300만원 넘게 받는데 지원금 있어?", None),
            ("장학금 100만원 이상", Some(1_000_000)),
            ("지원금 50만원 넘는 거", Some(500_000)),
            ("월 100만 원 이상 주는 지원금", Some(1_000_000)),
            ("30만원 이상 주거 지원", None),
        ] {
            assert_eq!(parse(question).amount_min, bound, "{question}");
        }
        for question in [
            "1억2천만원 이상 지원",
            "2억 이상 주는 지원금",
            "천만원 이상 지원",
            "월 100만 원 이상 주는 지원금",
        ] {
            let keywords = parse(question).keywords;
            assert!(
                !keywords
                    .iter()
                    .any(|k| k.chars().any(|c| c.is_ascii_digit())
                        || k.contains("만원")
                        || k == "이상"
                        || k == "원"),
                "{question}: {keywords:?}"
            );
        }
    }

    /// Round-2 review (code M-1, M-2, security L1): a bound only for the
    /// benefit's own amount, a numeral read whole, never past the limit.
    #[test]
    fn an_amount_bound_is_the_benefits_and_read_whole() {
        for (question, bound) in [
            ("월세 50만원 넘는데 받을 수 있는 지원 있어?", None),
            ("소득 300만원 넘어도 받을 수 있는 지원금", None),
            ("2만명 이상 뽑는 사업", None),
            ("239807672958224171만원 이상 지원", None),
            ("20000만원 이상 지원금", None),
            ("5천만원 이상 지원", Some(50_000_000)),
            ("3만5천원 이상 지원", Some(35_000)),
            // Past the limit: no bound, never arguments the validator rejects.
            ("1억2천만원 이상 지원", None),
            ("5천만원 이상 지원금", Some(50_000_000)),
        ] {
            assert_eq!(parse(question).amount_min, bound, "{question}");
        }
    }

    /// Audit M1: a negated value ("부산 말고") is never set.
    #[test]
    fn a_negated_value_is_never_set() {
        let intent = parse("부산 말고 서울 장학금 찾아줘");
        assert_eq!(intent.region, Some("seoul"));
        let intent = parse("서울 빼고 부산 휴학생 장학금");
        assert_eq!(intent.region, Some("busan"));
        let intent = parse("부산 말고 장학금");
        assert_eq!(intent.region, None);
        assert_eq!(parse("휴학생 제외 장학금").enrollment_status, None);
        assert!(parse("장학금 말고 월세").program_types.is_empty());
    }

    /// Round 3 (code M-7): the status phrase is composed before the lookup,
    /// and a status noun takes any closed-class tail (copula, particle).
    #[test]
    fn status_phrases_are_composed_and_take_their_grammar() {
        for (question, status) in [
            ("졸업 예정자도 받을 수 있는 장학금", Some("enrolled")),
            ("졸업 예정인데 받을 수 있는 장학금", Some("enrolled")),
            ("휴학 중이라서 받을 수 있는 거", Some("on_leave")),
            ("휴학생이라 받을 수 있는 장학금", Some("on_leave")),
            ("재학생입니다 장학금 알려줘", Some("enrolled")),
            ("휴학중이라서 받을 수 있는 거", Some("on_leave")),
            ("졸업작품 지원금", None),
            ("재학인증 방법", None),
            ("졸업일 지나도 되는 장학금", None),
        ] {
            assert_eq!(parse(question).enrollment_status, status, "{question}");
        }
    }

    /// Audit H3: a status cue is a status word ("재학", "재학생", "휴학 중",
    /// "졸업한"), never the start of another noun (a certificate).
    #[test]
    fn status_cues_are_status_words_not_document_names() {
        for (question, status) in [
            ("재학증명서 없이 받을 수 있는 장학금", None),
            ("수료증 필요한 지원", None),
            ("졸업증명서 내야 하는 장학금", None),
            ("휴학생도 받을 수 있는 장학금", Some("on_leave")),
            ("재학생 장학금 뭐 있어", Some("enrolled")),
            ("휴학 중인데 받을 수 있는 거", Some("on_leave")),
            ("졸업한 청년 지원", Some("graduated")),
            ("졸업예정자 취업 지원", Some("enrolled")),
        ] {
            assert_eq!(parse(question).enrollment_status, status, "{question}");
        }
        assert_eq!(
            parse("대학생 할인").program_types,
            ["corporate_benefit", "campus_benefit"]
        );
    }
}
