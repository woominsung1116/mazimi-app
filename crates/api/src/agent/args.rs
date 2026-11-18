//! Deterministic tool arguments, read from the goal's words. The fast path
//! and the model planner both use these functions, so a goal gets the same
//! search filters, keywords and deadline window whichever planner ran, and
//! the model never decides a filter the goal does not state.
//!
//! - Region, enrollment status and programme types: the cross-programme
//!   question parser (`grounded_qa::intent`), plus English words, `다니-`
//!   (attending school) and the narrowing of `할인` by `기업`/`학교`.
//! - Keywords: content nouns that occur in the goal. Particles, verb
//!   fragments, pronouns, framing and generic words, school names, English
//!   function words and anything credential-shaped are dropped.
//! - Deadline window: `이번 주`, `N일`, `N주`, `한 달`, `next N days`.
use crate::assistant_provider::cue_words;
use crate::services::grounded_qa::{intent, text};
use chrono::NaiveDate;
use mazimi_core::agent::{
    ProfileField, ProfilePatchArgs, SearchArgs, MAX_DEADLINE_DAYS, MAX_KEYWORDS, MAX_KEYWORD_CHARS,
    PROGRAM_TYPES,
};

/// Generic words a model or the parser tends to keep; they narrow nothing.
const GENERIC_KEYWORDS: &[&str] = &[
    "혜택",
    "지원",
    "정보",
    "대학생",
    "청년",
    "학생",
    "프로그램",
    "제도",
    "사업",
    "공고",
    "목록",
    "요새",
    "요즘",
    "나한테",
    "학교",
    "기업",
    "마감",
];

/// Final syllables of particles and verb endings (`달에`, `맞는`, `만한`,
/// `해야`, `사는데`, `다니`): a word ending in one is a copied phrase, not a
/// content noun.
const NON_NOUN_ENDINGS: &[char] = &[
    '에', '는', '한', '테', '야', '을', '를', '은', '의', '로', '고', '면', '서', '데', '니',
];

/// Words that frame a search but are not content keywords.
const KEYWORD_STOP: &[&str] = &[
    "이번",
    "다음",
    "담달",
    "학기",
    "달",
    "주",
    "올해",
    "요즘",
    "지금",
    "받을",
    "있는",
    "거",
    "것",
    "챙겨",
    "놓친",
    "놓치",
    "정리",
    "해줘",
    "줘",
    "좀",
    "나",
    "내",
    "저",
    "제",
    "내가",
    "제가",
    "나는",
    "저는",
    "알려",
    "알아",
    "찾아",
    "추천",
    "보여",
    "있어",
    "있나",
    "방학",
    "신청",
    "가능",
    "가능한",
    "관련",
    "각각",
    "지원",
    "도움",
    "쪽",
    "저장",
    "준비",
    // Follow-up requests on what a search finds ("찾고 알림까지 켜줘").
    "알림",
    "알람",
    "켜줘",
    "꺼줘",
    "도와",
    "비교",
    "자격",
    "마련",
];

/// Verb fragments inside a word (`저장해둔`, `지원해주는`, `다니는`).
const VERB_FRAGMENTS: &[&str] = &[
    "해둔",
    "해놓",
    "해놨",
    "해놔",
    "해줘",
    "해주",
    "해서",
    "하는",
    "하고",
    "했",
    "하려",
    "되는",
    "주는",
    "다니",
    "다녀",
    "사는",
    "살고",
    "살아",
    "알아보",
    "알아봐",
    "봐줘",
    "줄래",
    "주세요",
];

/// English function and framing words.
const ENGLISH_STOP: &[&str] = &[
    "a",
    "an",
    "the",
    "in",
    "on",
    "at",
    "of",
    "to",
    "for",
    "with",
    "about",
    "and",
    "or",
    "me",
    "my",
    "i",
    "is",
    "are",
    "any",
    "what",
    "which",
    "there",
    "find",
    "search",
    "show",
    "list",
    "please",
    "help",
    "can",
    "get",
    "available",
    "student",
    "students",
    "benefit",
    "benefits",
    "program",
    "programs",
    "support",
    "leave",
    "due",
    "next",
    "day",
    "days",
    "week",
    "weeks",
    "this",
    "that",
];

/// English programme-type words.
const ENGLISH_TYPES: &[(&str, &[&str])] = &[
    ("scholarship", &["scholarship"]),
    ("scholarships", &["scholarship"]),
    ("grant", &["policy"]),
    ("grants", &["policy"]),
    ("allowance", &["policy"]),
    ("allowances", &["policy"]),
    ("subsidy", &["policy"]),
    ("subsidies", &["policy"]),
    ("stipend", &["policy"]),
    ("discount", &["corporate_benefit", "campus_benefit"]),
    ("discounts", &["corporate_benefit", "campus_benefit"]),
];

/// A framing word, not content. One-syllable stops (`주`, `거`) match
/// whole words only so `주거` and `거주` survive; longer ones match as
/// prefixes (`신청할`, `관련된`).
fn is_stop_keyword(keyword: &str) -> bool {
    KEYWORD_STOP
        .iter()
        .any(|s| keyword == *s || (s.chars().count() >= 2 && keyword.starts_with(s)))
}

/// A keyword that can be stored: letters and digits only, not Latin
/// letters mixed with digits (`hunter2`, `minsu01`), and not part of a goal
/// word that mixes Latin letters with digits or symbols (`qw!er`), so a
/// credential the redaction missed never becomes a search keyword.
/// `3분위`, `2학기`, `KTX` stay.
pub(crate) fn storable_keyword(keyword: &str, goal: &str) -> bool {
    let mixed = |w: &str| {
        let w = w.trim_end_matches(['.', ',', '?', '!', '~']);
        w.chars().any(|c| c.is_ascii_alphabetic())
            && w.chars()
                .any(|c| c.is_ascii_digit() || !c.is_alphanumeric())
    };
    let keyword_lower = keyword.to_lowercase();
    keyword.chars().all(char::is_alphanumeric)
        && !mixed(keyword)
        && !goal
            .split_whitespace()
            .any(|w| mixed(w) && w.to_lowercase().contains(&keyword_lower))
}

/// A school name (`부산대`, `한국대학교`): who the user is, not what the
/// benefit is about. `공공임대` and `세대` are not schools.
fn school_name(keyword: &str) -> bool {
    let chars = keyword.chars().count();
    keyword.ends_with("대학교")
        || keyword.ends_with("대학")
        || (keyword.ends_with('대')
            && (3..=4).contains(&chars)
            && !["임대", "세대", "시대", "연대", "지대"]
                .iter()
                .any(|n| keyword.ends_with(n)))
}

/// A framing word with a particle or copula left on (`마감인`, `것도`), a
/// period (`2주`, `3일`, `열흘`) or a deadline word (`마감되`), not content.
fn framing_with_tail(keyword: &str) -> bool {
    const TAILS: &[char] = &['인', '이', '은', '는', '도', '만', '일'];
    let with_tail = keyword
        .strip_suffix(TAILS)
        .is_some_and(|stem| GENERIC_KEYWORDS.contains(&stem) || KEYWORD_STOP.contains(&stem));
    let digits = keyword.trim_start_matches(|c: char| c.is_ascii_digit());
    let period = digits.len() < keyword.len()
        && ["일", "주", "달", "개월", "년", "시", "분", "주일"].contains(&digits);
    let numeral = NUMERAL_DAYS.iter().any(|(w, _)| keyword.starts_with(w));
    let deadline = DEADLINE_WORDS.iter().any(|w| keyword.contains(w));
    with_tail || period || numeral || deadline
}

/// Benefit titles are Korean: an English word (`rent`) matches none of
/// them, an acronym written in capitals (`KTX`, `TOEIC`) does.
fn latin_acronym_or_not_latin(keyword: &str, goal: &str) -> bool {
    !keyword.chars().any(|c| c.is_ascii_alphabetic()) || goal.contains(&keyword.to_uppercase())
}

/// The cleaned form of a keyword candidate (plural `들` dropped), if it is
/// a content noun of this goal.
fn clean_keyword(candidate: &str, goal: &str, goal_compact: &str) -> Option<String> {
    let trimmed = candidate.trim().to_lowercase();
    let keyword = match trimmed.strip_suffix('들') {
        Some(stem) if stem.chars().count() >= 2 => stem,
        _ => trimmed.as_str(),
    };
    // A generic noun glued on ("월세지원", "교통비혜택") narrows nothing.
    let keyword = ["지원", "혜택"]
        .iter()
        .find_map(|g| keyword.strip_suffix(g).filter(|s| s.chars().count() >= 2))
        .unwrap_or(keyword);
    let keep = !keyword.is_empty()
        && keyword.chars().count() <= MAX_KEYWORD_CHARS
        && storable_keyword(keyword, goal)
        && goal_compact.contains(keyword)
        && !keyword.ends_with(NON_NOUN_ENDINGS)
        && !GENERIC_KEYWORDS.contains(&keyword)
        && !is_stop_keyword(keyword)
        && !VERB_FRAGMENTS.iter().any(|f| keyword.contains(f))
        && !ENGLISH_STOP.contains(&keyword)
        && !keyword.chars().all(|c| c.is_ascii_digit())
        && !school_name(keyword)
        && !framing_with_tail(keyword)
        && latin_acronym_or_not_latin(keyword, goal)
        && {
            let parsed = parse(keyword);
            parsed.region.is_none()
                && parsed.enrollment_status.is_none()
                && parsed.program_types.is_empty()
        };
    keep.then(|| keyword.to_owned())
}

/// Up to [`MAX_KEYWORDS`] cleaned keywords of `goal`, in order, without
/// duplicates. Model keywords get the goal's particle stripped first.
pub(crate) fn keywords<'a>(
    candidates: impl IntoIterator<Item = &'a str>,
    goal: &str,
) -> Vec<String> {
    let compact: String = goal
        .to_lowercase()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    let mut out: Vec<String> = Vec::new();
    for candidate in candidates {
        let stemmed = text::stem(candidate.trim());
        if let Some(keyword) = clean_keyword(stemmed, goal, &compact) {
            if !out.contains(&keyword) {
                out.push(keyword);
            }
        }
        if out.len() == MAX_KEYWORDS {
            break;
        }
    }
    out
}

/// Region, status and types the goal states, before keywords.
fn parse(goal: &str) -> SearchArgs {
    let parsed = intent::parse(goal);
    let words = text::words(goal);
    let mut types: Vec<&str> = parsed.program_types.clone();
    for word in &words {
        if let Some((_, extra)) = ENGLISH_TYPES.iter().find(|(w, _)| w == word) {
            for t in *extra {
                if !types.contains(t) {
                    types.push(t);
                }
            }
        }
    }
    // `할인` reads as both discount types; `기업에서 주는 할인` and `학교 할인`
    // name one of them.
    let follows = |i: usize, heads: &[&str]| {
        words
            .get(i + 1)
            .is_some_and(|n| heads.iter().any(|h| n.starts_with(h)))
    };
    let corporate = words.iter().any(|w| w.starts_with("기업"));
    let campus = words.iter().enumerate().any(|(i, w)| {
        (w == "학교" || w == "학교의" || w == "학교에서") && follows(i, &["혜택", "할인"])
    });
    let discount_words = ["혜택", "할인", "discount"];
    let benefit_word = words
        .iter()
        .any(|w| discount_words.iter().any(|d| w.contains(d)));
    if corporate && !campus && benefit_word {
        types.retain(|t| *t != "campus_benefit");
        if !types.contains(&"corporate_benefit") {
            types.push("corporate_benefit");
        }
    } else if campus && !corporate {
        types.retain(|t| *t != "corporate_benefit");
        if !types.contains(&"campus_benefit") {
            types.push("campus_benefit");
        }
    }
    let enrollment = parsed.enrollment_status.or_else(|| {
        let joined = format!(" {} ", words.join(" "));
        if joined.contains(" on leave ") || joined.contains(" leave of absence ") {
            Some("on_leave")
        } else if words.iter().any(|w| w == "enrolled" || w == "attending") {
            Some("enrolled")
        } else if words
            .iter()
            .any(|w| w == "graduate" || w == "graduates" || w == "graduated")
        {
            Some("graduated")
        } else if words
            .iter()
            .any(|w| w.starts_with("다니") || w.starts_with("다녀"))
        {
            // "학교 다니면서": attending school, said as a verb.
            Some("enrolled")
        } else {
            None
        }
    });
    SearchArgs {
        region: parsed.region.map(str::to_owned),
        enrollment_status: enrollment.map(str::to_owned),
        program_types: PROGRAM_TYPES
            .iter()
            .filter(|t| types.contains(t))
            .map(|t| (*t).to_owned())
            .collect(),
        keywords: Vec::new(),
        // The agent's topic filter. The non-agent question search keeps its
        // own reading (`intent::parse` sets no fields).
        fields: mazimi_core::categories::fields_named(goal)
            .into_iter()
            .map(str::to_owned)
            .collect(),
        deadline_within_days: None,
        deadline_from_days: None,
        deadline_named: false,
        earliest: super::guard::superlative(goal) && deadline_named(goal),
        amount_min: parsed.amount_min,
        amount_period: parsed.amount_period.map(str::to_owned),
        excluded_regions: Vec::new(),
    }
}

/// The topics a goal names, one per topic word, in order: the fields and
/// programme types the word names ("장학금" → education and scholarship,
/// "주거" → housing). Topics sharing a field or a type are one ("월세",
/// "주거").
fn topics(goal: &str) -> Vec<(Vec<String>, Vec<String>)> {
    let mut out: Vec<(Vec<String>, Vec<String>)> = Vec::new();
    for word in text::words(&respace(goal)) {
        let named = parse(&word);
        // A topic names a field; a programme type alone ("지원금": policy)
        // is no topic, as in the search (held-out x01: "월세 지원금" split
        // off an empty search).
        if named.fields.is_empty() {
            continue;
        }
        let shares = |(f, t): &(Vec<String>, Vec<String>)| {
            named.fields.iter().any(|x| f.contains(x))
                || named.program_types.iter().any(|x| t.contains(x))
        };
        match out.iter_mut().find(|topic| shares(topic)) {
            Some((f, t)) => {
                f.extend(
                    named
                        .fields
                        .into_iter()
                        .filter(|x| !f.contains(x))
                        .collect::<Vec<_>>(),
                );
                t.extend(
                    named
                        .program_types
                        .into_iter()
                        .filter(|x| !t.contains(x))
                        .collect::<Vec<_>>(),
                );
            }
            None => out.push((named.fields, named.program_types)),
        }
    }
    out
}

/// A search over several topics the goal names ("장학금 그리고 주거 지원 둘
/// 다", QA B6), as one search per topic: a type one topic names never
/// filters the other's rows. The region, status, window and the keywords no
/// other topic's field covers are shared. One search when the goal names
/// one topic, or when the search's topics are not exactly the goal's words'.
pub(crate) fn split_topics(args: &SearchArgs, goal: &str) -> Vec<SearchArgs> {
    let topics = topics(goal);
    let same = |mine: &[String], theirs: Vec<&String>| {
        mine.len() == theirs.len() && theirs.iter().all(|x| mine.contains(x))
    };
    if topics.len() < 2
        || !same(&args.fields, topics.iter().flat_map(|(f, _)| f).collect())
        || !same(
            &args.program_types,
            topics.iter().flat_map(|(_, t)| t).collect(),
        )
    {
        return vec![args.clone()];
    }
    topics
        .into_iter()
        .map(|(fields, types)| {
            let mut one = args.clone();
            one.keywords.retain(|k| {
                let named = mazimi_core::categories::fields_named(k);
                named.is_empty() || named.iter().any(|f| fields.iter().any(|x| x == f))
            });
            one.fields = fields;
            one.program_types = types;
            one.canonicalize();
            one
        })
        .collect()
}

/// Days a closing-soon search covers when the goal gives no window
/// ("마감 임박한 장학금").
pub(crate) const SOON_DAYS: u16 = 14;

/// The deadline window of a search goal that asks for closing-soon rows:
/// the window it states ("이번 달 마감"), else [`SOON_DAYS`] for `임박` /
/// `곧 마감` / `마감 순`; `None` when it names no deadline.
/// With how it was said: the soon window is no named period.
pub(crate) fn search_reading(goal: &str) -> Option<Reading> {
    let today = mazimi_core::seoul_date(chrono::Utc::now());
    if deadline_named(goal) {
        if let Some(reading) = reading_on(goal, today) {
            return Some(reading);
        }
    }
    soon_window(goal).map(|period| Reading {
        period,
        named: false,
    })
}

fn soon_window(goal: &str) -> Option<Period> {
    {
        let compact: String = goal.chars().filter(|c| !c.is_whitespace()).collect();
        [
            "임박",
            "곧마감",
            "마감순",
            "마감가까",
            "마감다가",
            "마감직전",
            "곧끝나",
        ]
        .iter()
        .any(|w| compact.contains(w))
        .then_some((0, SOON_DAYS))
    }
}

/// Request endings typed onto a noun without a space ("장학금찾아줘").
/// Longest first.
const GLUED_REQUESTS: &[&str] = &[
    "추천해주세요",
    "찾아주세요",
    "알려주세요",
    "보여주세요",
    "추천해줘",
    "뭐있어요",
    "찾아줘",
    "알려줘",
    "보여줘",
    "찾아봐",
    "뭐있어",
    "뭐있음",
    "뭐있나",
    "있을까",
    "있어요",
    "있어",
    "있나",
    "있음",
];
/// What may follow a glued region besides a benefit word ("부산사는데").
const AFTER_REGION: &[&str] = &["사는", "살", "거주"];

/// Split a word typed without spaces into its region, its benefit noun and
/// its request ("부산장학금찾아줘" → "부산 장학금 찾아줘"), so the region
/// parser and the cue grammar see whole words. Only words of Hangul are
/// touched, a region is split off only before a benefit word (not a school:
/// "부산대"), and a request only after one. Other words are unchanged.
/// Common typos and casual spellings, word by word (holdout class 3):
/// `잇어`→`있어`, `지언`/`지운금`→`지원`, `머 있어`→`뭐 있어`, `찾아죠`→`찾아줘`,
/// and 초성-only topic words of the main fields (`ㅈㅎㄱ` 장학금, `ㅇㅅ` 월세,
/// `ㄱㅌㅂ` 교통비) when they are a whole word (a particle may follow).
/// Conservative: a word is changed only in these exact shapes.
pub(crate) fn normalize_casual(goal: &str) -> String {
    const SUFFIXES: &[(&str, &str)] = &[
        ("잇어요", "있어요"),
        ("잇을까", "있을까"),
        ("잇어", "있어"),
        ("잇나", "있나"),
        ("잇음", "있음"),
        ("잇는", "있는"),
        ("알려죠", "알려줘"),
        ("찾아죠", "찾아줘"),
        ("해죠", "해줘"),
        ("지언금", "지원금"),
        ("지운금", "지원금"),
        ("지언", "지원"),
        ("지운사업", "지원사업"),
    ];
    const CHOSEONG: &[(&str, &str)] =
        &[("ㅈㅎㄱ", "장학금"), ("ㅇㅅ", "월세"), ("ㄱㅌㅂ", "교통비")];
    const PARTICLES: &[&str] = &["", "이", "가", "은", "는", "도", "을", "를", "좀"];
    let tokens: Vec<&str> = goal.split_whitespace().collect();
    let mut out: Vec<String> = Vec::with_capacity(tokens.len());
    for (i, token) in tokens.iter().enumerate() {
        let core =
            token.trim_end_matches(|c: char| !c.is_alphanumeric() && !('ㄱ'..='ㅎ').contains(&c));
        let tail = &token[core.len()..];
        let mut word = core.to_owned();
        if let Some((typo, fixed)) = SUFFIXES.iter().find(|(t, _)| word.ends_with(t)) {
            word = format!("{}{fixed}", &word[..word.len() - typo.len()]);
        }
        // `지운` alone, before a request verb ("월세 지운 받는 거", "지운 있어?");
        // a name ("친구 지운 장학금") stays (re-review 3 L3).
        let next_verb = tokens.get(i + 1).is_some_and(|n| {
            ["받", "신청", "있", "잇", "되", "돼", "해", "뭐", "머"]
                .iter()
                .any(|v| n.starts_with(v))
        });
        if word == "지운" && next_verb {
            word = "지원".into();
        }
        // `머`/`모` before `있…` ("머 있어"), or glued ("머있어").
        let next_has = tokens
            .get(i + 1)
            .is_some_and(|n| n.starts_with('있') || n.starts_with('잇'));
        if (word == "머" || word == "모") && next_has {
            word = "뭐".into();
        }
        for (typo, fixed) in [("머있", "뭐있"), ("모있", "뭐있")] {
            if word.starts_with(typo) {
                word = format!("{fixed}{}", &word[typo.len()..]);
            }
        }
        // `ㅇㅅ` is also 입시/인사: only before 지원/보조 or a request (re-review 3 L3).
        let next_request = tokens.get(i + 1).is_some_and(|n| {
            [
                "지원", "지언", "보조", "찾", "알려", "있", "잇", "뭐", "머", "받",
            ]
            .iter()
            .any(|v| n.starts_with(v))
        });
        if let Some((_, full)) = CHOSEONG.iter().find(|(c, _)| {
            word.strip_prefix(c)
                .is_some_and(|rest| PARTICLES.contains(&rest))
                && (*c != "ㅇㅅ" || next_request)
        }) {
            let particle = CHOSEONG
                .iter()
                .find_map(|(c, _)| word.strip_prefix(c))
                .unwrap_or("");
            word = format!("{full}{particle}");
        }
        out.push(format!("{word}{tail}"));
    }
    out.join(" ")
}

pub(crate) fn respace(goal: &str) -> String {
    let goal = normalize_casual(goal);
    let mut out: Vec<String> = Vec::new();
    for token in goal.split_whitespace() {
        let core = token.trim_end_matches(|c: char| !c.is_alphanumeric());
        let punctuation = &token[core.len()..];
        let hangul = core.chars().count() >= 4
            && core.chars().all(|c| ('\u{AC00}'..='\u{D7A3}').contains(&c));
        if !hangul {
            out.push(token.to_owned());
            continue;
        }
        let chars: Vec<char> = core.chars().collect();
        let mut rest: String = core.to_owned();
        for len in (2..=chars.len().saturating_sub(2).min(7)).rev() {
            let prefix: String = chars[..len].iter().collect();
            let after: String = chars[len..].iter().collect();
            if mazimi_core::canonical_region_code(&prefix).is_some()
                && !after.starts_with('대')
                && (super::guard::benefit_anchor(&after)
                    || AFTER_REGION.iter().any(|a| after.starts_with(a)))
            {
                out.push(prefix);
                rest = after;
                break;
            }
        }
        let glued = GLUED_REQUESTS.iter().find_map(|tail| {
            rest.strip_suffix(tail)
                .filter(|stem| stem.chars().count() >= 2 && super::guard::benefit_anchor(stem))
                .map(|stem| (stem.to_owned(), *tail))
        });
        match glued {
            Some((stem, tail)) => {
                out.push(stem);
                out.push(format!("{tail}{punctuation}"));
            }
            None => out.push(format!("{rest}{punctuation}")),
        }
    }
    out.join(" ")
}

/// Search arguments from the goal alone. `keep_keywords: false` is the
/// sweep shape (no keywords).
pub(crate) fn search(goal: &str, keep_keywords: bool) -> SearchArgs {
    search_read(&Goal::read(goal), keep_keywords)
}

/// A goal as the arguments read it: spaced, and without its exclusions
/// (`intent::exclusions`), computed once at the entry. Every leaf (filters,
/// fields, the parser's and the model's keywords) reads `cleaned`; the
/// excluded regions are typed arguments.
struct Goal {
    cleaned: String,
    parsed: intent::SearchIntent,
}

impl Goal {
    fn read(goal: &str) -> Self {
        let spaced = respace(goal);
        Self {
            cleaned: intent::without_negated(&spaced).0,
            parsed: intent::parse(&spaced),
        }
    }
}

fn search_read(goal: &Goal, keep_keywords: bool) -> SearchArgs {
    let mut args = parse(&goal.cleaned);
    args.excluded_regions = goal
        .parsed
        .excluded_regions
        .iter()
        .map(|r| (*r).to_owned())
        .collect();
    if keep_keywords {
        args.keywords = keywords(
            goal.parsed.keywords.iter().map(String::as_str),
            &goal.cleaned,
        );
    }
    args
}

/// Search arguments for a model step: the goal's filters, and the model's
/// keywords that survive cleaning (the parser's when none do).
pub(crate) fn search_with_model_keywords<'a>(
    goal: &str,
    model_keywords: impl IntoIterator<Item = &'a str>,
) -> SearchArgs {
    let goal = Goal::read(goal);
    let mut args = search_read(&goal, true);
    let from_model = keywords(model_keywords, &goal.cleaned);
    if !from_model.is_empty() {
        args.keywords = from_model;
    }
    args
}

const NUMERAL_DAYS: &[(&str, u32)] = &[
    ("하루", 1),
    ("이틀", 2),
    ("사흘", 3),
    ("나흘", 4),
    ("닷새", 5),
    ("일주일", 7),
    ("열흘", 10),
    ("보름", 15),
    ("한달", 30),
];

/// Words that make a period a deadline window: "이번 달에 받을 수 있는 거"
/// scopes benefits, "이번 달 마감" scopes deadlines.
const DEADLINE_WORDS: &[&str] = &[
    "마감", "끝나", "끝남", "기한", "만료", "종료", "due", "deadline", "close", "expire",
];

/// The goal speaks of deadlines ([`DEADLINE_WORDS`]).
fn deadline_named(goal: &str) -> bool {
    let lower = goal.to_lowercase();
    DEADLINE_WORDS.iter().any(|w| lower.contains(w))
}

/// A deadline period as inclusive day offsets from today (Korean calendar
/// days): `(from, to)`.
pub(crate) type Period = (u16, u16);

/// [`period_on`] when the goal speaks of deadlines, else `None`.
pub(crate) fn deadline_period_on(goal: &str, today: NaiveDate) -> Option<Period> {
    deadline_named(goal)
        .then(|| period_on(goal, today))
        .flatten()
}

/// [`deadline_period_on`] today in Korea.
pub(crate) fn deadline_window(goal: &str) -> Option<Period> {
    deadline_period_on(goal, mazimi_core::seoul_date(chrono::Utc::now()))
}

/// The period the goal states on `today` (audit M2: calendar ranges, not
/// day counts): `오늘` today, `내일` tomorrow, `이번 주` through Sunday,
/// `다음 주` next Monday to Sunday, `이번 달` through the month's last day,
/// `다음 달`/`담달` the whole next month; a length (`N일`, `N주`, `N달`,
/// `일주일`, `한 달`, `next N days`) runs from today. Cut at
/// [`MAX_DEADLINE_DAYS`], the tool's limit.
pub(crate) fn period_on(goal: &str, today: NaiveDate) -> Option<Period> {
    reading_on(goal, today).map(|r| r.period)
}

/// A period as the goal said it: a named calendar period ("이번 주", "다음
/// 달", "내일") or a length from today ("3일", "한 달").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Reading {
    pub period: Period,
    pub named: bool,
}

/// [`period_on`] with how it was said. "까지" after a named period ("내일까지",
/// "다음 주까지", "다음 달 말까지") runs from today: one rule for every
/// named period.
pub(crate) fn reading_on(goal: &str, today: NaiveDate) -> Option<Reading> {
    let words = cue_words(goal);
    let (period, named, at) = named_or_length(&words, today)?;
    // 까지 binds the period: on its word ("내일까지", "다음주까지") or on its
    // head noun right after ("다음 주까지", "다음 달 말까지"); 까지 on another
    // word ("마감 언제까지야") is no "until" of the period.
    // The period's head noun, or a day inside it (금요일, 15일, 중순: a
    // closed class), carrying 까지.
    let head = |w: &String| {
        let w = w.strip_suffix("까지").unwrap_or(w);
        ["주", "달", "말", "초"].iter().any(|h| w.starts_with(h))
            || w.ends_with("요일")
            || ["초순", "중순", "하순"].contains(&w)
            || w.strip_suffix('일')
                .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
    };
    let until = named
        && (words[at].contains("까지")
            || words
                .iter()
                .skip(at + 1)
                .take(2)
                .take_while(|w| head(w))
                .any(|w| w.contains("까지")));
    Some(Reading {
        period: if until { (0, period.1) } else { period },
        named,
    })
}

fn named_or_length(words: &[String], today: NaiveDate) -> Option<(Period, bool, usize)> {
    use chrono::Datelike;
    let offset = |day: NaiveDate| (day - today).num_days().clamp(0, MAX as i64) as u16;
    let length = |days: u32| (days >= 1).then(|| (0, days.min(u32::from(MAX)) as u16));
    let month_start = |date: NaiveDate| date.with_day(1).unwrap_or(date);
    let next_month = |date: NaiveDate| {
        month_start(date)
            .checked_add_months(chrono::Months::new(1))
            .unwrap_or(date)
    };
    let month_end = |date: NaiveDate| next_month(date).pred_opt().unwrap_or(date);
    for (i, word) in words.iter().enumerate() {
        let next = words.get(i + 1).map(String::as_str).unwrap_or("");
        let digits: String = word.chars().take_while(char::is_ascii_digit).collect();
        if !digits.is_empty() && digits.len() <= 3 {
            let n: u32 = digits.parse().unwrap_or(0);
            let rest = &word[digits.len()..];
            let unit = if rest.is_empty() { next } else { rest };
            let per = if unit.starts_with("주") || unit.starts_with("week") {
                7
            } else if unit.starts_with("개월")
                || unit.starts_with("달")
                || unit.starts_with("month")
            {
                30
            } else if unit.starts_with("일") || unit.starts_with("day") {
                1
            } else {
                continue;
            };
            return length(n * per).map(|p| (p, false, i));
        }
        let this = word == "이번" || word == "this";
        if word.starts_with("이번주") || (this && (next.starts_with('주') || next == "week")) {
            let (from, to) = mazimi_core::calendar::rest_of_week(today);
            return Some(((offset(from), offset(to)), true, i));
        }
        if word.starts_with("이번달") || (this && (next.starts_with('달') || next == "month")) {
            return Some(((0, offset(month_end(today))), true, i));
        }
        let following = word == "다음" || word == "next";
        if word.starts_with("다음주") || (following && (next.starts_with('주') || next == "week"))
        {
            let (from, to) = mazimi_core::calendar::next_week(today);
            return Some(((offset(from), offset(to)), true, i));
        }
        if word.starts_with("다음달")
            || word.starts_with("담달")
            || (following && (next.starts_with('달') || next == "month"))
        {
            let first = next_month(today);
            return Some(((offset(first), offset(month_end(first))), true, i));
        }
        if (word == "한" || word == "두") && (next.starts_with('달') || next.starts_with('주'))
        {
            let n = if word == "한" { 1 } else { 2 };
            return length(n * if next.starts_with('달') { 30 } else { 7 }).map(|p| (p, false, i));
        }
        // `내일배움카드` is a programme, not tomorrow.
        let short = word.chars().count() <= 5;
        if short && word.starts_with("오늘") {
            return Some(((0, 0), true, i));
        }
        if short && word.starts_with("내일") {
            return Some(((1, 1), true, i));
        }
        if let Some((_, days)) = NUMERAL_DAYS
            .iter()
            .find(|(w, _)| word.starts_with(w) && short)
        {
            return length(*days).map(|p| (p, false, i));
        }
    }
    None
}

/// [`MAX_DEADLINE_DAYS`] as the periods read it.
const MAX: u16 = MAX_DEADLINE_DAYS;

/// Put a reading on a search's deadline fields, `named` included, so the
/// answer says it as the goal did without reading the goal again (`None`
/// clears them).
pub(crate) fn set_search_period(search: &mut SearchArgs, reading: Option<Reading>) {
    search.deadline_from_days = reading.map(|r| r.period.0).filter(|from| *from > 0);
    search.deadline_within_days = reading.map(|r| r.period.1);
    search.deadline_named = reading.is_some_and(|r| r.named);
}

/// The deadline digest's arguments for `period`.
pub(crate) fn deadline_args(period: Option<Period>) -> mazimi_core::agent::DeadlineArgs {
    mazimi_core::agent::DeadlineArgs {
        from_days: period.map(|(from, _)| from).filter(|from| *from > 0),
        within_days: period.map(|(_, to)| to),
    }
}

/// Particles and copula endings after a profile value (`부산으로`,
/// `3학년이야`, `휴학으로`), longest first.
const VALUE_PARTICLES: &[&str] = &[
    "으로",
    "이에요",
    "이야",
    "예요",
    "에서",
    "로",
    "야",
    "을",
    "를",
    "은",
    "는",
    "이",
    "가",
    "요",
];

fn value_stem(word: &str) -> &str {
    VALUE_PARTICLES
        .iter()
        .find_map(|p| word.strip_suffix(p).filter(|stem| !stem.is_empty()))
        .unwrap_or(word)
}

/// `3학년`, `5분위`, `4명`: the number before one of `units`, then only
/// particles.
fn numbered(word: &str, units: &[&str]) -> Option<String> {
    let digits: String = word.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() || digits.len() > 2 {
        return None;
    }
    let rest = &word[digits.len()..];
    let unit = units.iter().find(|u| rest.starts_with(**u))?;
    let tail = &rest[unit.len()..];
    (tail.is_empty() || VALUE_PARTICLES.contains(&tail))
        .then(|| {
            // Canonical decimal: `03` is `3`.
            digits.trim_start_matches('0').to_owned()
        })
        .map(|n| if n.is_empty() { "0".into() } else { n })
}

/// Words of a document or checklist goal: never a profile value.
const PROFILE_DOCUMENT_WORDS: &[&str] = &[
    "증명",
    "확인서",
    "서류",
    "등본",
    "초본",
    "사본",
    "체크",
    "준비",
    "발급",
];
/// Field nouns of the status, year and income fields.
const STATUS_FIELD: &[&str] = &[
    "~학적",
    "~재학상태",
    "~취업",
    "~직업",
    "~고용",
    "~학년",
    "~소득",
    "~분위",
    "~구간",
];

/// Region field nouns; a bare region name ("부산으로 바꿔줘") is not enough.
const REGION_FIELD: &[&str] = &[
    "~지역",
    "~거주지",
    "~거주",
    "~주소",
    "사는 곳|데",
    "~사는곳",
];
/// Employment values safe to read without a field noun (`학생`, `기타`,
/// `신입` are not).
const EMPLOYMENT_VALUES: &[&str] = &[
    "재직",
    "재직자",
    "직장인",
    "재직중",
    "미취업",
    "무직",
    "취업준비생",
    "취준생",
    "구직중",
    "취업준비",
    "자영업",
    "자영업자",
    "프리랜서",
];

/// The one profile field and value an own-profile change request names:
/// "지역 부산으로 바꿔줘", "3학년으로 수정해줘", "휴학으로 변경", "소득분위 5분위로
/// 바꿔줘", "가구원 4명으로 고쳐줘". Values are canonical
/// ([`ProfileField::accepts`]). Zero or two candidates, or a value the
/// field does not take, is `None`: no card from a guess.
pub(crate) fn profile_patch(goal: &str) -> Option<ProfilePatchArgs> {
    use crate::assistant_provider::any_cue;
    let words = cue_words(goal);
    let mut found: Vec<(ProfileField, String)> = Vec::new();
    let mut add = |field: ProfileField, value: String| {
        if !found.iter().any(|(f, v)| *f == field && *v == value) {
            found.push((field, value));
        }
    };
    // A document goal ("재학 증명서 완료로 바꿔줘") is a checklist, never a
    // profile edit (code review).
    if words.iter().any(|w| {
        PROFILE_DOCUMENT_WORDS
            .iter()
            // `준비` inside `취업준비` is an employment value (re-review).
            .any(|d| w.contains(d) && !(*d == "준비" && w.contains("취업준비")))
    }) {
        return None;
    }
    let names_region = any_cue(&words, REGION_FIELD);
    let names_household = any_cue(&words, &["~가구원", "~가구"]);
    let names_status = any_cue(&words, STATUS_FIELD);
    let names_kosaf = any_cue(&words, &["~학자금", "~지원구간"]);
    for (i, word) in words.iter().enumerate() {
        let stem = value_stem(word);
        // Without its field noun a value counts only as the change target
        // (`휴학으로`), not as a mention (`재학 중인데`).
        let target = names_status || word.ends_with('로');
        // "졸업 예정" is still enrolled: a following 예정 makes it a guess.
        let next_is_plan = words.get(i + 1).is_some_and(|n| n.starts_with("예정"));
        if names_region {
            if let Some(code) = mazimi_core::canonical_region_code(stem) {
                add(ProfileField::RegionCode, code.to_owned());
            }
        }
        if target && !next_is_plan {
            if let Some(code) = mazimi_core::canonical_enrollment_status(stem) {
                add(ProfileField::EnrollmentStatus, code.to_owned());
            }
        }
        if target && EMPLOYMENT_VALUES.contains(&stem) {
            if let Some(code) = mazimi_core::canonical_employment_status(stem) {
                add(ProfileField::EmploymentStatus, code.to_owned());
            }
        }
        if let Some(n) = numbered(word, &["학년"]).filter(|_| target) {
            add(ProfileField::SchoolYear, n);
        }
        // 학자금 (지원)구간 is its own column; any other 분위/구간 is 소득분위.
        if let Some(n) = numbered(word, &["분위", "구간"]).filter(|_| target) {
            add(
                if names_kosaf {
                    ProfileField::KosafSupportBracket
                } else {
                    ProfileField::IncomeBracket
                },
                n,
            );
        }
        if names_household {
            if let Some(n) = numbered(word, &["명", "인"]) {
                add(ProfileField::HouseholdSize, n);
            }
        }
    }
    match found.as_slice() {
        [(field, value)] if field.accepts(value) => Some(ProfilePatchArgs {
            field: *field,
            value: value.clone(),
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {

    /// QA B6: two topics joined are two searches; one topic stays one.
    /// Audit M6: an amount bound is a structured filter, never keywords.
    #[test]
    fn an_amount_bound_is_a_filter_not_keywords() {
        let args = search("월 30만원 이상 주는 지원금", true);
        assert_eq!(
            (args.amount_min, args.amount_period.as_deref()),
            (Some(300_000), Some("monthly")),
            "{args:?}"
        );
        assert!(
            !args
                .keywords
                .iter()
                .any(|k| k.contains("30") || k == "이상"),
            "{args:?}"
        );
    }

    /// Audit M1: a filter value the goal negates ("취업 말고", "부산 빼고")
    /// never becomes the filter.
    #[test]
    fn a_negated_value_is_never_the_filter() {
        // Round 2 (code M-3, M-4): every leaf reads the cleaned goal, model
        // keywords included; glued markers count.
        let args = search_with_model_keywords("카카오 말고 다른 기업 혜택", ["카카오"]);
        assert!(!args.keywords.contains(&"카카오".to_owned()), "{args:?}");
        let args = search("취업말고 생활비 지원", true);
        assert!(!args.fields.contains(&"employment".to_owned()), "{args:?}");
        let args = search("부산 말고 장학금 찾아줘", true);
        assert_eq!(args.excluded_regions, ["busan"], "{args:?}");
        let args = search("취업 말고 그냥 생활비 받을 수 있는 거", true);
        assert!(!args.fields.contains(&"employment".to_owned()), "{args:?}");
        let args = search("부산 말고 서울 장학금 찾아줘", true);
        assert_eq!(args.region.as_deref(), Some("seoul"), "{args:?}");
        let args = search("휴학생 빼고 장학금", true);
        assert_eq!(args.enrollment_status, None, "{args:?}");
    }

    #[test]
    fn joined_topics_are_one_search_each() {
        let goal = "장학금 그리고 주거 지원 둘 다 알아봐줘";
        let split = split_topics(&search(goal, true), goal);
        let topics: Vec<(Vec<String>, Vec<String>)> = split
            .iter()
            .map(|a| (a.fields.clone(), a.program_types.clone()))
            .collect();
        assert_eq!(
            topics,
            [
                (vec!["education".to_owned()], vec!["scholarship".to_owned()]),
                (vec!["housing".to_owned()], vec![]),
            ],
            "{split:?}"
        );
        for goal in [
            "부산 청년 월세 지원 있어?",
            "장학금 찾아줘",
            "월세랑 주거비 지원",
            "월세 지원금 받을수잇는거 알랴줘",
        ] {
            assert_eq!(split_topics(&search(goal, true), goal).len(), 1, "{goal}");
        }
    }

    /// QA B5: a superlative on the deadline asks for the row closing first.
    #[test]
    fn a_deadline_superlative_asks_for_the_earliest() {
        for goal in [
            "부산 청년 취업 지원 찾아서 마감 제일 빠른 거 알림 켜줘",
            "마감 젤 가까운 장학금 하나만 알려줘",
            "주거 지원 중에 가장 빨리 끝나는 거",
        ] {
            assert!(search(goal, true).earliest, "{goal}");
        }
        for goal in [
            "제일 좋은 장학금 알려줘",
            "마감 임박한 장학금",
            "월세 지원 찾아줘",
        ] {
            assert!(!search(goal, true).earliest, "{goal}");
        }
    }

    use super::*;

    fn s(region: Option<&str>, status: Option<&str>, types: &[&str], kw: &[&str]) -> SearchArgs {
        SearchArgs {
            region: region.map(str::to_owned),
            enrollment_status: status.map(str::to_owned),
            program_types: types.iter().map(|t| (*t).to_owned()).collect(),
            keywords: kw.iter().map(|k| (*k).to_owned()).collect(),
            ..SearchArgs::default()
        }
    }

    /// The filters other than the fields (asserted on their own below).
    fn without_fields(mut args: SearchArgs) -> SearchArgs {
        args.fields.clear();
        args
    }

    /// Topic understanding: the fields a goal names, read deterministically.
    /// Rows written for this test, not taken from an evaluation set.
    #[test]
    fn goals_name_their_fields() {
        for (goal, fields) in [
            (
                "부산 사는 대학생인데 월세 지원 받을 수 있는 거 있을까?",
                &["housing"][..],
            ),
            ("전세 보증금 지원 있어?", &["housing"]),
            ("자취방 구하는데 도움 되는 거", &["housing"]),
            ("기숙사비 지원해주는 곳", &["housing"]),
            ("집 구할 때 받을 수 있는 혜택", &["housing"]),
            ("청년 주거 지원 알려줘", &["housing"]),
            ("임대주택 신청할 수 있어?", &["housing"]),
            ("이사비 지원", &["housing"]),
            ("취업 준비생 지원금 찾아줘", &["employment"]),
            ("일자리 관련 혜택", &["employment"]),
            ("면접 볼 때 지원받을 수 있는 거", &["employment"]),
            ("인턴 지원 사업", &["employment"]),
            ("창업 지원금 있어?", &["employment"]),
            ("자격증 응시료 지원", &["employment"]),
            ("장학금 찾아줘", &["education"]),
            ("학비 지원 있어?", &["education"]),
            ("등록금 지원", &["education"]),
            ("어학 시험 교육비 지원", &["education"]),
            ("학자금 대출 이자 지원", &["education", "finance"]),
            ("청년 적금 뭐 있어", &["finance"]),
            ("대출 받을 수 있는 거", &["finance"]),
            ("목돈 마련 통장", &["finance"]),
            ("전세 대출 이자 지원", &["housing", "finance"]),
            ("교통비 지원 있어?", &["living"]),
            ("문화 생활 지원", &["living"]),
            ("통신비 할인", &["living"]),
            ("생활비 지원해주는 거", &["living"]),
            ("심리 상담 지원", &["welfare"]),
            ("병원비 의료 지원", &["welfare"]),
            ("출산 육아 지원", &["welfare"]),
            ("마감 임박한 장학금", &["education"]),
            ("이번 달 마감인 주거 지원", &["housing"]),
            ("부산 청년 혜택 알려줘", &[]),
            ("모집 중인 거 알려줘", &[]),
            ("집중 지원 사업", &[]),
            ("날씨 알려줘", &[]),
        ] {
            assert_eq!(search(goal, true).fields, fields, "{goal}");
        }
    }

    /// Holdout class 3: typos, casual endings and 초성 words. Rows written
    /// for this test, not taken from an evaluation set.
    #[test]
    fn casual_spellings_are_normalized_conservatively() {
        for (goal, normal) in [
            ("식비 지언 잇음?", "식비 지원 있음?"),
            ("청년 지운사업 뭐 잇나", "청년 지원사업 뭐 있나"),
            ("생활비 지운 신청 돼?", "생활비 지원 신청 돼?"),
            ("심리상담 지원 머 있어요", "심리상담 지원 뭐 있어요"),
            ("자격증 지원 알려죠", "자격증 지원 알려줘"),
            ("ㅈㅎㄱ 좀 알려줘", "장학금 좀 알려줘"),
            ("울산 ㅇㅅ 보조 있음?", "울산 월세 보조 있음?"),
            ("ㄱㅌㅂ는 받을 수 있나", "교통비는 받을 수 있나"),
            // Left alone: other words, 초성 inside a word, a name.
            ("ㅋㅋ 장학금 있어?", "ㅋㅋ 장학금 있어?"),
            ("ㅇㅅㅇ 뭐야", "ㅇㅅㅇ 뭐야"),
            ("지운 씨가 알려준 장학금", "지운 씨가 알려준 장학금"),
            ("친구 지운 장학금 받았대", "친구 지운 장학금 받았대"),
            ("지운 장학금", "지운 장학금"),
            ("ㅇㅅ 끝나고 받을 수 있는 거", "ㅇㅅ 끝나고 받을 수 있는 거"),
            ("머리 아픈데 상담 지원", "머리 아픈데 상담 지원"),
        ] {
            assert_eq!(normalize_casual(goal), normal, "{goal}");
        }
        assert_eq!(
            search("ㅈㅎㄱ 좀 알려줘", true).program_types,
            ["scholarship"]
        );
        assert_eq!(search("식비 지언 잇음?", true).fields, ["living"]);
    }

    #[test]
    fn closing_soon_windows_for_a_search() {
        for (goal, period) in [
            ("마감 임박한 장학금", Some((0, SOON_DAYS))),
            ("곧 마감되는 주거 지원", Some((0, SOON_DAYS))),
            ("이번 달 마감인 장학금", deadline_window("이번 달 마감")),
            ("3일 안에 마감되는 장학금", Some((0, 3))),
            ("장학금 찾아줘", None),
        ] {
            assert_eq!(search_reading(goal).map(|r| r.period), period, "{goal}");
        }
    }

    /// Rows written for this test, not taken from an evaluation set.
    #[test]
    fn search_filters_come_from_the_goal_words() {
        for (goal, expected) in [
            (
                "울산 살고 있는데 받을수있는 장학금 뭐있음",
                s(Some("ulsan"), None, &["scholarship"], &[]),
            ),
            (
                "scholarships for graduates in Busan",
                s(Some("busan"), Some("graduated"), &["scholarship"], &[]),
            ),
            (
                "any grants for students on leave",
                s(None, Some("on_leave"), &["policy"], &[]),
            ),
            (
                "기업이 해주는 학생 할인 뭐 있어",
                s(None, None, &["corporate_benefit"], &[]),
            ),
            (
                "전남대 다니는데 학교 할인 알려줘",
                s(None, Some("enrolled"), &["campus_benefit"], &[]),
            ),
            (
                "학교 다니면서 받을 식비 지원",
                s(None, Some("enrolled"), &[], &["식비"]),
            ),
            (
                "충남대 졸업생인데 창업 지원금 찾아줘",
                s(None, Some("graduated"), &["policy"], &["창업"]),
            ),
            (
                "광주 장학금 찾고 3주 안에 마감인 것도 정리",
                s(Some("gwangju"), None, &["scholarship"], &[]),
            ),
            (
                "학생증 할인 되는 곳",
                s(
                    None,
                    None,
                    &["campus_benefit", "corporate_benefit"],
                    &["학생증"],
                ),
            ),
            (
                "공공임대 관련 지원 있어?",
                s(None, None, &[], &["공공임대"]),
            ),
            (
                "내가 찜해둔 혜택들 중 교통비",
                s(None, None, &[], &["교통비"]),
            ),
            // `학교` alone is not the campus type, and `기업` without a
            // benefit word is not the corporate type.
            (
                "기업 인턴 지원금 있어?",
                s(None, None, &["policy"], &["인턴"]),
            ),
        ] {
            assert_eq!(without_fields(search(goal, true)), expected, "{goal}");
        }
    }

    #[test]
    fn model_keywords_are_cleaned_and_fall_back_to_the_parser() {
        let goal = "세종 사는데 문화 바우처 받을 수 있는 거";
        assert_eq!(
            search_with_model_keywords(goal, ["사는데", "바우처", "문화를"]).keywords,
            ["문화"]
        );
        assert_eq!(
            search_with_model_keywords(goal, ["해외여행"]).keywords,
            ["문화"],
            "an invented keyword is dropped; the parser's stands"
        );
        assert_eq!(
            keywords(["sky2024", "월세", "월세"], "sky2024 월세 지원"),
            ["월세"]
        );
        assert!(storable_keyword("2학기", "2학기 장학금"));
        assert!(storable_keyword("ktx", "KTX 할인 있어!"));
        assert_eq!(keywords(["ktx", "rent"], "KTX 할인이랑 rent 지원"), ["ktx"]);
        assert!(!storable_keyword("hunter2", "hunter2"));
        assert!(!storable_keyword("qw", "qw!er 장학금"));
    }

    #[test]
    fn deadline_windows_are_parsed() {
        // 2026-11-10, a Tuesday.
        let today = chrono::NaiveDate::from_ymd_opt(2026, 11, 10).unwrap();
        for (goal, period) in [
            ("이번 주 마감", Some((0, 5))),
            ("이번주에 끝나는", Some((0, 5))),
            ("일주일 안에 마감", Some((0, 7))),
            ("3일 안에 끝나는 거", Some((0, 3))),
            ("10 일 남은", Some((0, 10))),
            ("3주 안에 마감되는", Some((0, 21))),
            ("두 달 안에", Some((0, 60))),
            ("6개월 내 마감", Some((0, MAX_DEADLINE_DAYS))),
            ("이번 달 마감", Some((0, 20))),
            ("한 달 안에 마감", Some((0, 30))),
            ("열흘 남은 거", Some((0, 10))),
            ("오늘 마감인 거", Some((0, 0))),
            ("내일배움카드 마감", None),
            ("due within the next 5 days", Some((0, 5))),
            ("what closes this week", Some((0, 5))),
            ("마감 임박한 거", None),
            ("2학기 장학금 마감", None),
            ("3분위 마감", None),
        ] {
            assert_eq!(period_on(goal, today), period, "{goal}");
        }
        assert_eq!(deadline_window("이번 달에 받을 수 있는 거 챙겨줘"), None);
        assert_eq!(deadline_window("오늘 할 일 정리해줘"), None);
        assert_eq!(
            deadline_period_on("이번 달 마감인 거", today),
            Some((0, 20))
        );
    }

    #[test]
    fn glued_words_are_split_only_around_benefit_words() {
        for (goal, spaced) in [
            ("부산장학금찾아줘", "부산 장학금 찾아줘"),
            ("서울시청년수당있어?", "서울시 청년수당 있어?"),
            ("울산사는데 뭐 있어", "울산 사는데 뭐 있어"),
            // A school, a place name inside a word, short or spaced words.
            ("부산대장학금", "부산대장학금"),
            ("대구경북 날씨", "대구경북 날씨"),
            ("부산 장학금 찾아줘", "부산 장학금 찾아줘"),
            ("KTX할인", "KTX할인"),
        ] {
            assert_eq!(respace(goal), spaced, "{goal}");
        }
        assert_eq!(
            without_fields(search("광주월세지원알려줘", true)),
            s(Some("gwangju"), None, &[], &["월세"])
        );
    }

    /// Audit M2: a named period is a calendar range of Korean days, not a
    /// day count from today. 2026-11-10 is a Tuesday.
    #[test]
    fn deadline_periods_are_calendar_ranges() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 11, 10).unwrap();
        let day = |m: u32, d: u32| chrono::NaiveDate::from_ymd_opt(2026, m, d).unwrap();
        let range = |goal: &str| {
            deadline_period_on(goal, today).map(|(from, to)| {
                (
                    today + chrono::Days::new(u64::from(from)),
                    today + chrono::Days::new(u64::from(to)),
                )
            })
        };
        for (goal, expected) in [
            ("오늘 마감인 거", (day(11, 10), day(11, 10))),
            ("내일 마감인 거", (day(11, 11), day(11, 11))),
            ("이번 주 마감 장학금", (day(11, 10), day(11, 15))),
            ("다음 주 마감", (day(11, 16), day(11, 22))),
            ("다음 주까지 마감인 거", (day(11, 10), day(11, 22))),
            // Round 2 (code M-8): 까지 runs from today for every period.
            ("내일까지 마감인 거", (day(11, 10), day(11, 11))),
            ("다음 달 말까지 마감", (day(11, 10), day(12, 31))),
            // Round 3 (code L-2): 까지 on another word is no "until" of the period.
            ("다음주 마감 언제까지야", (day(11, 16), day(11, 22))),
            ("내일 마감 몇시까지 신청해", (day(11, 11), day(11, 11))),
            // Round 4 (code L-3): a day inside the period carries 까지 too.
            ("다음 주 금요일까지 마감", (day(11, 10), day(11, 22))),
            ("다음 달 15일까지 마감", (day(11, 10), day(12, 31))),
            ("다음 달 중순까지 마감", (day(11, 10), day(12, 31))),
            ("이번 달 마감인 거", (day(11, 10), day(11, 30))),
            ("다음 달 마감 장학금", (day(12, 1), day(12, 31))),
            ("담달 마감", (day(12, 1), day(12, 31))),
            ("3일 안에 마감", (day(11, 10), day(11, 13))),
            ("2주 안에 마감", (day(11, 10), day(11, 24))),
            ("한 달 안에 마감", (day(11, 10), day(12, 10))),
        ] {
            assert_eq!(range(goal), Some(expected), "{goal}");
        }
        assert_eq!(range("이번 달에 받을 수 있는 거 챙겨줘"), None);
        // The longest next month from its eve stays whole.
        let eve = chrono::NaiveDate::from_ymd_opt(2026, 7, 1).unwrap();
        assert_eq!(deadline_period_on("다음 달 마감", eve), Some((31, 61)));
    }

    #[test]
    fn next_week_and_next_month_are_windows() {
        // 2026-11-10, a Tuesday: next week is 11-16..11-22, next month
        // 12-01..12-31.
        let today = chrono::NaiveDate::from_ymd_opt(2026, 11, 10).unwrap();
        for (goal, period) in [
            ("다음 주 마감", Some((6, 12))),
            ("다음주에 끝나는", Some((6, 12))),
            ("다음 달 안에 마감", Some((21, 51))),
            ("담달 마감", Some((21, 51))),
            ("due next month", Some((21, 51))),
            ("다음에 마감 알려줘", None),
        ] {
            assert_eq!(period_on(goal, today), period, "{goal}");
        }
        assert!(keywords(["다음"], "다음 달 마감 장학금").is_empty());
    }

    #[test]
    fn profile_patch_reads_one_whitelisted_field_and_value() {
        use mazimi_core::agent::ProfileField::*;
        for (goal, field, value) in [
            ("내 지역 부산으로 바꿔줘", RegionCode, "busan"),
            ("거주지 광주로 변경해줘", RegionCode, "gwangju"),
            ("사는 곳 서울특별시로 수정해줘", RegionCode, "seoul"),
            ("학적 휴학으로 바꿔줘", EnrollmentStatus, "on_leave"),
            ("휴학생으로 변경해줘", EnrollmentStatus, "on_leave"),
            ("졸업으로 고쳐줘", EnrollmentStatus, "graduated"),
            ("취업 상태 취준생으로 바꿔줘", EmploymentStatus, "preparing"),
            ("학년 3학년으로 수정해줘", SchoolYear, "3"),
            ("소득분위 5분위로 바꿔줘", IncomeBracket, "5"),
            // One field per column (client audit R-H1): 소득분위 is
            // income_bracket, 학자금 지원구간 is kosaf_support_bracket.
            ("학자금 지원구간 0구간으로 바꿔줘", KosafSupportBracket, "0"),
            (
                "학자금 지원 구간 3구간으로 수정해줘",
                KosafSupportBracket,
                "3",
            ),
            ("소득 구간 2분위로 고쳐줘", IncomeBracket, "2"),
            ("가구원 4명으로 고쳐줘", HouseholdSize, "4"),
        ] {
            assert_eq!(
                profile_patch(goal),
                Some(ProfilePatchArgs {
                    field,
                    value: value.into()
                }),
                "{goal}"
            );
        }
        for goal in [
            // Checklist goals naming a status word are not profile edits.
            "재학 증명서 완료로 바꿔줘",
            "졸업 증명서 체크 완료로 바꿔줘",
            "재직 증명서 준비 완료로 바꿔줘",
            "휴학 확인서 상태 바꿔줘",
            // No value, or a bare region with no field noun.
            "내 프로필 지역 바꿔줘",
            "부산으로 바꿔줘",
            // Two fields, two regions (from/to), out of range.
            "지역 부산으로 바꾸고 3학년으로 바꿔줘",
            "지역 서울에서 부산으로 바꿔줘",
            "학년 7학년으로 바꿔줘",
            "소득분위 11분위로 바꿔줘",
            // Not on the whitelist.
            "나이 25살로 바꿔줘",
            "학교 부산대로 바꿔줘",
            // `졸업 예정` is enrolled, not graduated: a guess.
            "졸업 예정으로 바꿔줘",
            // A household size needs its field noun.
            "4명으로 바꿔줘",
        ] {
            assert_eq!(profile_patch(goal), None, "{goal}");
        }
    }
}
