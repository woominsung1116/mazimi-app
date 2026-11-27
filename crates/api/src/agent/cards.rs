//! The rules half of the intent layer ([`super::intent`]): for each write
//! type, the closed phrase forms that name it, its polarity or value, and
//! the act where the structure is certain.
//!
//! # Invariant
//! A clause is **made** (a Request) when its request ending ends the goal,
//! or is followed by a joiner, another request or ordinary words; it is
//! **mentioned** when a quote particle or a light verb of saying, typing or
//! pressing with a connective, past or adnominal ending follows it within
//! three words ([`mentions_after`], a morphological pattern), or 말고/대신
//! sets it aside; it is **unclear** when a framing word (버튼, 기능, 문구,
//! 뜻 …) follows it. A later withdrawal form retracts it ([`retracts`]).
//!
//! # Why
//! Rounds 8-14 patched a post-hoc veto with word lists and traded
//! precision for recall every round. The act is now read once here and in
//! `intent`, and writes are produced from it; the planners' own writes only
//! pass if this reading asks for them.
//!
//! # Left to buttons (deliberately)
//! Elliptical requests ("마감 알림도"), a lone topic word ("알림"), unclear
//! clauses, and anything the act head (when loaded) does not settle. The
//! answer's state-aware buttons carry every write type for the programme
//! on screen, so these cost a tap. Governance: see [`super::intent`] (the
//! rule table is frozen; a miss is a training row, not a rule).
use super::goals;
use mazimi_core::agent::{ApplicationStatus, ToolCall};
use std::collections::HashSet;
use std::sync::LazyLock;

/// The programme a write is about, said at the start of a clause.
const TARGETS: &[&str] = &[
    "이 혜택",
    "이 공고",
    "이 장학금",
    "이 지원금",
    "이 정책",
    "이 프로그램",
    "이거",
    "이것",
    "이",
    "여기",
    "그거",
    "그것",
    "저장한 혜택",
    "저장한 혜택들",
    "저장한 거",
    "저장한 것",
    "저장한 것들",
    "찜한 거",
    "찜한거",
    "찜한 혜택",
    "찜한 것들",
    "고른 혜택",
    "관심 혜택",
    "마감 급한 거",
    "급한 거",
    "마감 임박한 거",
    "장학금",
    "장학금들",
    "지원금",
    "혜택",
    "혜택들",
    "공고",
];
/// Words that add nothing to what is asked.
const FILLERS: &[&str] = &[
    "미리",
    "하나",
    "좀",
    "꼭",
    "다",
    "전부",
    "모두",
    "다시",
    "지금",
    "바로",
    "이제",
    "빨리",
    "제발",
    "일단",
    "그냥",
    "한번",
    "얼른",
    "더는",
    "더",
    "이상",
    "나",
    "저",
    "내",
    "방금",
    "오늘",
    "어제",
    "드디어",
    "이미",
];

/// "Without asking": a card is still shown, so these add nothing.
const SKIP_LEADS: &[&str] = &[
    "확인 없이",
    "확인없이",
    "묻지 말고",
    "물어보지 말고",
    "컨펌 없이",
    "확인 안 하고",
    "승인 없이",
    "동의 없이",
    "컨펌 안 받고",
];

/// The goal's text as typed, or `None` for a quote. A trailing question
/// mark is reported (`?` forms are only polite requests).
fn surface(goal: &str) -> Option<(String, bool)> {
    let text = goal.trim();
    if text.chars().any(|c| "\"'“”‘’「」『』".contains(c)) {
        return None;
    }
    let marked = text.contains('?') || text.contains('？');
    let text = text
        .trim_end_matches(['.', '!', '~', '?', '？', ' '])
        .replace(['?', '？'], " ");
    let question = marked || asks_a_question(&text);
    Some((text, question))
}

/// A question without its mark: the last clause has a question word or a
/// question ending ("차이가 뭐야", "켜야 하나", "어떻게 해").
fn asks_a_question(text: &str) -> bool {
    let parts = clauses(text);
    let Some(last) = parts.last() else {
        return false;
    };
    let wh = [
        "뭐",
        "뭐야",
        "뭔데",
        "뭐예요",
        "뭔가요",
        "무엇",
        "무슨",
        "왜",
        "어떻게",
        "언제",
        "어디",
        "몇",
        "차이",
        "차이가",
        "차이는",
        "얼마",
    ];
    let endings = [
        "나요",
        "까요",
        "을까",
        "할까",
        "일까",
        "니",
        "냐",
        "는지",
        "은지",
        "어때",
        "어때요",
        "인가요",
        "인가",
        "하나",
        "되나",
        "되나요",
        "돼요?",
        "거야?",
    ];
    last.iter().any(|w| wh.contains(&w.as_str()))
        || last
            .last()
            .is_some_and(|w| endings.iter().any(|e| w.ends_with(e)))
}

/// Words that add no predicate after a request ("저장해줘 꼭", "켜줘 부탁해",
/// "켜줘 ㅎㅎ"): skipped where a clause would start.
const BENIGN: &[&str] = &[
    "꼭",
    "좀",
    "빨리",
    "제발",
    "제발요",
    "지금",
    "당장",
    "얼른",
    "바로",
    "부탁해",
    "부탁해요",
    "부탁드려요",
    "부탁드립니다",
    "부탁할게",
    "부탁할게요",
    "부탁",
    "고마워",
    "고마워요",
    "감사",
    "감사해요",
    "감사합니다",
    "요",
    "진짜로",
    "진심으로",
    "정말로",
    "진짜",
    "정말",
    "please",
    "plz",
    "네",
    "응",
    "아",
    "앗",
    "음",
    "어",
];
/// Words that start a new clause, judged on its own.
const JOINERS: &[&str] = &[
    "그리고",
    "그리고요",
    "그리고는",
    "근데",
    "그런데",
    "하지만",
    "그다음에",
    "그러고",
    "또",
    "또한",
    "및",
    "그럼",
    "그러면서",
];
/// Words that set the clause before them aside ("알림 켜줘 말고 저장만 해줘").
const SET_ASIDE: &[&str] = &["말고", "대신", "대신에"];

/// Only emoticon, laughter or crying jamo ("ㅎㅎ", "ㅠㅠ", "^^").
fn emoticon(word: &str) -> bool {
    !word.is_empty() && word.chars().all(|c| "ㅋㅎㅠㅜㅡ^;:)(-_~!.,".contains(c))
}

struct Clause {
    words: Vec<String>,
    /// Ended in a request ending (…줘, …주세요, 부탁해 …).
    request: bool,
    /// A joiner follows: the next clause is judged on its own.
    joined: bool,
    /// 말고/대신 follows: this clause is set aside.
    aside: bool,
    /// The words after it, up to a joiner (for the mention structure).
    after: Vec<String>,
}

/// The goal's clauses, split after a connective "-고" (not before 싶,
/// 나서/나면 or an adnominal), a "-서"/"-니"/"-면"/"-는데" ending, a request
/// ending, a declarative, a comma, or a joining word.
fn split(text: &str) -> Vec<Clause> {
    let tokens: Vec<&str> = text.split_whitespace().collect();
    let mut out: Vec<Clause> = Vec::new();
    let mut current: Vec<String> = Vec::new();
    let after = |i: usize| -> Vec<String> {
        tokens[i + 1..]
            .iter()
            .map(|t| {
                t.trim_end_matches([',', '，', '!', '~', '.', '?', '？'])
                    .to_owned()
            })
            .take_while(|t| !JOINERS.contains(&t.as_str()))
            .filter(|t| !t.is_empty())
            .collect()
    };
    let close = |out: &mut Vec<Clause>, current: &mut Vec<String>, request: bool, end: usize| {
        if !current.is_empty() {
            out.push(Clause {
                words: std::mem::take(current),
                request,
                joined: false,
                aside: false,
                after: after(end),
            });
        }
    };
    for (i, raw) in tokens.iter().enumerate() {
        let comma = raw.ends_with([',', '，', '!', '~', '.']);
        let word = raw.trim_end_matches([',', '，', '!', '~', '.']);
        let next = tokens.get(i + 1).copied().unwrap_or("");
        if word.is_empty() || emoticon(word) {
            close(&mut out, &mut current, false, i);
            continue;
        }
        if JOINERS.contains(&word) {
            close(&mut out, &mut current, false, i);
            if let Some(last) = out.last_mut() {
                last.joined = true;
            }
            continue;
        }
        if current.is_empty() {
            if SET_ASIDE.contains(&word) {
                if let Some(last) = out.last_mut() {
                    last.aside = true;
                }
                continue;
            }
            // Discourse 아니 before a joiner or an emphasis ("아니 근데",
            // "아니 진짜로") is no retraction.
            let emphasis = |w: &str| {
                JOINERS.contains(&w)
                    || ["진짜", "진짜로", "진심으로", "정말", "정말로"].contains(&w)
            };
            if BENIGN.contains(&word) || (word == "아니" && emphasis(next)) {
                continue;
            }
        }
        current.push(word.to_owned());
        // A prohibition ("켜지 마") is a request form too: it chains like one
        // and matches no card itself.
        let request = [
            "줘",
            "주세요",
            "주라",
            "줄래",
            "줘요",
            "부탁해",
            "부탁해요",
            "부탁드려요",
            "부탁드립니다",
        ]
        .iter()
        .any(|e| word.ends_with(e))
            || ["마", "마요", "마세요", "말아줘", "말아주세요"].contains(&word);
        let ends = comma
            || request
            || (word.ends_with('고')
                && !next.starts_with('싶')
                && !["나서", "나면", "난", "난뒤", "난후", "나니"]
                    .iter()
                    .any(|n| next.starts_with(n))
                && (!next.ends_with('는') || next.chars().count() < 2)
                && word.chars().count() >= 2)
            || [
                "아서", "어서", "해서", "여서", "니까", "으니", "는데", "은데", "니", "면서",
                "나서", "더니",
            ]
            .iter()
            .any(|e| word.ends_with(e) && word.chars().count() >= 2)
            || (word.ends_with('면')
                && word.chars().count() >= 2
                && !REMINDER_TAILS.contains(&next))
            || declarative(word, next);
        if ends {
            close(&mut out, &mut current, request, i);
        }
    }
    let end = tokens.len().saturating_sub(1);
    close(&mut out, &mut current, false, end);
    out
}

/// Whether a request clause is addressed to the agent now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// The request is made: a card.
    Made,
    /// Neither clearly made nor clearly mentioned: no card from the rules;
    /// the button covers it (the act head, when it lands, decides these).
    Unclear,
    /// Quoted, reported, hypothetical or set aside: no card.
    Mentioned,
}

/// A light verb of saying, typing or pressing with a non-final connective,
/// a past tense or an adnominal ("하면", "보내면", "눌렀는데", "그랬더니",
/// "입력했는데", "쓰는", "보낸"), or a quote particle ("라고", "라는", "이란"):
/// the request before it is talked about, not made. A morphological
/// pattern on the stem and its ending, not a word list: "해야", "해서",
/// "하고", "할" and "한 번" carry none of these endings.
fn mentions_after(word: &str, next: &str, then: &str) -> bool {
    if [
        "라고",
        "라는",
        "라며",
        "라면",
        "라길래",
        "라서",
        "라니",
        "란",
        "이라",
        "이란",
        "래서",
        "래요",
        "래",
    ]
    .iter()
    .any(|q| word.starts_with(q))
    {
        return true;
    }
    let coda = |c: char| ('가'..='힣').contains(&c).then(|| (c as u32 - 0xAC00) % 28);
    // Past forms of the light verbs: any ending ("했잖아", "보냈는데").
    const PAST: &[&str] = &[
        "입력했",
        "요청했",
        "클릭했",
        "검색했",
        "말했",
        "했",
        "그랬",
        "쳤",
        "눌렀",
        "썼",
        "보냈",
        "적었",
        "물어봤",
        "얘기했",
    ];
    // Adnominal forms ("보낸 거", "누른 뒤"); a bare 한 only before 거/뒤 …
    // ("한 번", "한 달" are numbers).
    const ADNOMINAL: &[&str] = &[
        "입력한",
        "요청한",
        "클릭한",
        "말한",
        "누른",
        "보낸",
        "쓴",
        "친",
        "적은",
        "그런",
    ];
    // Present stems and their -어 forms.
    const STEMS: &[&str] = &[
        "입력하",
        "입력해",
        "요청하",
        "요청해",
        "클릭하",
        "클릭해",
        "말하",
        "말해",
        "하",
        "해",
        "그러",
        "그래",
        "치",
        "쳐",
        "누르",
        "눌러",
        "쓰",
        "써",
        "보내",
        "적",
        "적어",
        "물어보",
        "물어봐",
    ];
    if PAST.iter().any(|p| word.starts_with(p)) {
        return true;
    }
    let after_noun = |w: &str| {
        ["거", "것", "게", "건", "뒤", "후", "다음", "적", "지"]
            .iter()
            .any(|n| w.starts_with(n))
    };
    if ADNOMINAL
        .iter()
        .any(|a| word == *a || word.strip_prefix(a).is_some_and(after_noun))
    {
        return true;
    }
    if word == "한" && after_noun(next) {
        return true;
    }
    let mut stems: Vec<&&str> = STEMS.iter().collect();
    stems.sort_by_key(|s| std::cmp::Reverse(s.len()));
    for stem in stems {
        let Some(rest) = word.strip_prefix(*stem) else {
            continue;
        };
        if rest.is_empty() {
            // "하고 기다렸는데": a quoting 하고 before a report.
            continue;
        }
        let connective = [
            "면", "는데", "니까", "니", "길래", "더니", "는", "던", "려고", "려면",
        ]
        .iter()
        .any(|e| rest.starts_with(e));
        let past = rest.chars().any(|c| coda(c) == Some(20));
        let prohibition = rest == "지" && ["말고", "마", "마요", "마세요"].contains(&next);
        let quoting = rest == "고"
            && (next.chars().any(|c| coda(c) == Some(20))
                || ["는데", "더니"].iter().any(|e| next.ends_with(e)));
        if connective || past || prohibition || quoting {
            return true;
        }
        break;
    }
    let _ = then;
    false
}

/// A word that frames the request as a thing talked about: a button, a
/// menu, the wording, how to use it.
fn frames(word: &str) -> bool {
    [
        "버튼",
        "기능",
        "메뉴",
        "문구",
        "메시지",
        "문장",
        "표현",
        "뜻",
        "방법",
        "쓰는법",
        "법",
        "예시",
        "명령어",
    ]
    .iter()
    .any(|f| word.starts_with(f))
        || ["말", "말이", "말은", "말로", "말도", "말이야", "말이에요"].contains(&word)
}

/// Each clause with its [`Verdict`] (re-review 13, lead decision for round
/// 14). Rules decide the clear cases only:
/// - a request ending at the end of the goal (benign tails and emoticons
///   are skipped), before a joiner, or before another made request, or
///   before ordinary words, is **made**: a request is genuine whatever
///   reason, time or feeling follows it;
/// - a request ending followed, within three words, by the quotative or
///   report structure of [`mentions_after`], or by 말고/대신, is
///   **mentioned**;
/// - one followed by a framing word ([`frames`]) is **unclear**.
///
/// A clause that is not itself a request (-고, a report, a reason) takes
/// the verdict of the request after it, or is mentioned.
fn marked_with(text: &str) -> Vec<(Vec<String>, Verdict)> {
    let parts = split(text);
    let mut state = vec![Verdict::Mentioned; parts.len()];
    for i in (0..parts.len()).rev() {
        let c = &parts[i];
        let last = i + 1 == parts.len();
        state[i] = if c.aside {
            Verdict::Mentioned
        } else if c.request {
            let w = |k: usize| c.after.get(k).map_or("", String::as_str);
            let mentioned =
                (0..3.min(c.after.len())).any(|k| mentions_after(w(k), w(k + 1), w(k + 2)));
            if last || c.joined {
                Verdict::Made
            } else if mentioned {
                Verdict::Mentioned
            } else if c.after.iter().any(|w| frames(w)) {
                Verdict::Unclear
            } else {
                Verdict::Made
            }
        } else if last || c.joined {
            Verdict::Made
        } else if (parts[i + 1].request || declines_other(&parts[i + 1].words))
            && !quotes_back(&parts[i + 1].words)
        {
            state[i + 1]
        } else {
            Verdict::Mentioned
        };
    }
    parts
        .into_iter()
        .zip(state)
        .map(|(c, v)| (c.words, v))
        .collect()
}

/// [`marked_with`], made or not.
fn marked(text: &str) -> Vec<(Vec<String>, bool)> {
    marked_with(text)
        .into_iter()
        .map(|(w, v)| (w, v == Verdict::Made))
        .collect()
}

/// The next clause opens with the quoting or reporting verb of the one
/// before it ("… 기록해줘 했던 거 취소해줘", "… 켜줘 한 거 보여줘"): the request
/// before is quoted, whatever this clause asks.
fn quotes_back(words: &[String]) -> bool {
    words.first().is_some_and(|w| {
        [
            "하", "했", "한", "할", "해", "라", "이라", "란", "그러", "그랬", "그런", "말하",
            "말한", "말했", "요청",
        ]
        .iter()
        .any(|v| w.starts_with(v))
    })
}

/// A clause that puts another part off ("신청 준비는 다음에", "저장은 됐어",
/// "체크리스트는 필요 없어"): a topic-marked noun and a deferral or decline.
/// It adds no request, so the request before it still counts.
fn declines_other(words: &[String]) -> bool {
    let topic = words
        .iter()
        .take(words.len().saturating_sub(1))
        .any(|w| w.chars().count() >= 2 && (w.ends_with('는') || w.ends_with('은')));
    let c: String = words.concat();
    topic
        && [
            "다음에",
            "나중에",
            "다음에요",
            "나중에요",
            "됐어",
            "됐어요",
            "필요없어",
            "필요없어요",
            "안해도돼",
            "괜찮아",
            "괜찮아요",
        ]
        .iter()
        .any(|e| c.ends_with(e))
}

/// The clauses a predicate reads: a mentioned clause is kept empty, so it
/// asks nothing.
fn clauses(text: &str) -> Vec<Vec<String>> {
    marked(text)
        .into_iter()
        .map(|(words, made)| if made { words } else { Vec::new() })
        .collect()
}

/// A later clause that takes the request back: after leading fillers
/// (아/앗/음/그냥), a withdrawal alone (아니, 아냐, 됐어, ㄴㄴ, 노노, 농담이야,
/// 장난이야, 취소, 하지 마, 그만, 필요 없어), or 아니/아냐/ㄴㄴ/노노/됐어 followed
/// by one (됐어, 하지 마, 필요 없, 다음에, 내가 할게, 말고, 그만, 취소해줘,
/// 그냥 둬). A 아니 before an emphasis, a reason or another request is no
/// retraction.
fn retracts(clause: &[String]) -> bool {
    let mut words: &[String] = clause;
    while words.first().is_some_and(|w| {
        [
            "아",
            "앗",
            "음",
            "그냥",
            "어",
            "에이",
            "엥",
            "잠깐",
            "잠깐만",
        ]
        .contains(&w.as_str())
    }) {
        words = &words[1..];
    }
    let c: String = words.concat();
    if c.is_empty() || c.starts_with("아니면") {
        return false;
    }
    const WITHDRAW: &[&str] = &[
        "됐어",
        "됐다",
        "됐음",
        "됐네",
        "됐어요",
        "하지마",
        "하지마요",
        "하지마세요",
        "하지말아줘",
        "필요없어",
        "필요없음",
        "필요없어요",
        "필요없다",
        "다음에",
        "다음에할게",
        "나중에",
        "나중에할게",
        "내가할게",
        "제가할게",
        "제가할게요",
        "말고",
        "그만",
        "그만해",
        "그만둘래",
        "취소",
        "취소해줘",
        "취소할래",
        "그냥둬",
        "둬",
        "놔둬",
        "그대로둬",
        "안해도돼",
        "농담",
        "농담이야",
        "농담이에요",
        "장난",
        "장난이야",
        "장난이에요",
        "아니",
        "아냐",
        "아니야",
        "아니다",
        "아니요",
        "ㄴㄴ",
        "노노",
    ];
    if WITHDRAW.contains(&c.as_str()) || c.starts_with("농담") || c.starts_with("장난") {
        return true;
    }
    let mut rest = c.as_str();
    let mut headed = false;
    loop {
        let before = rest;
        for head in [
            "아니다",
            "아니야",
            "아니요",
            "아니",
            "아냐",
            "ㄴㄴ",
            "노노",
            "됐어",
            "됐고",
            "그냥",
        ] {
            if let Some(r) = rest.strip_prefix(head) {
                rest = r;
                headed = headed || head != "그냥";
            }
        }
        if rest == before {
            break;
        }
    }
    // "아니다 내가 직접 할게": the user takes it over (a first-person subject
    // and a promissive -ㄹ게), a withdrawal by its structure.
    let takes_over = ["내가", "제가"].iter().any(|p| rest.starts_with(p))
        && ["할게", "할게요", "할래", "할래요"]
            .iter()
            .any(|e| rest.ends_with(e));
    headed && (rest.is_empty() || WITHDRAW.contains(&rest) || takes_over)
}

/// Whether a clause after `i` retracts the request made in clause `i`.
fn retracted_after(parts: &[Vec<String>], i: usize) -> bool {
    parts[i + 1..].iter().any(|c| !c.is_empty() && retracts(c))
}

/// A sentence-final declarative ("했어", "됐다", "이미 했음") before another
/// word, unless that word continues it ("해 줘", "하지 마", "싶어").
fn declarative(word: &str, next: &str) -> bool {
    if next.is_empty() {
        return false;
    }
    // "하지 마 알림 켜줘": a prohibition ends there.
    if word == "마" {
        return true;
    }
    let coda = |c: char| ('가'..='힣').contains(&c).then(|| (c as u32 - 0xAC00) % 28);
    // A polite 요 changes nothing ("뒀어요" is "뒀어").
    let bare = word
        .strip_suffix('요')
        .filter(|w| w.chars().count() >= 2)
        .unwrap_or(word);
    let chars: Vec<char> = bare.chars().collect();
    let ends = match chars.as_slice() {
        [.., stem, last] => {
            // ㅆ or ㅄ before a final ("했어", "됐다", "없어"), ㄹ before
            // 게/래 ("시작할게", "볼래"), a reported 대 ("해준대"), a
            // nominal final ("다 함", "끝냄", "완료됨").
            (matches!(coda(*stem), Some(20 | 18)) && ['어', '다', '음', '지', '네'].contains(last))
                || (coda(*stem) == Some(8) && ['게', '래'].contains(last))
                || (*last == '대' && coda(*stem) == Some(0))
                || ['함', '됨', '냄', '임'].contains(last)
        }
        [only] => ['돼', '함', '됨', '끝'].contains(only),
        [] => false,
    } || [
        "이야",
        "거야",
        "이에요",
        "예요",
        "이다",
        "중이야",
        "필요없어",
        "싶어",
        "싶다",
        "하자",
        "가자",
        "보자",
        "시작해",
        "스킵",
        "패스",
    ]
    .iter()
    .any(|e| bare.ends_with(e));
    let continued = [
        "줘", "주", "해", "하", "싶", "있", "않", "놔", "둬", "보", "드", "봐",
    ]
    .iter()
    .any(|c| next.starts_with(c))
        || ["마", "마요", "마세요", "말고", "말아", "말아줘"].contains(&next);
    ends && !continued
}

/// The clause without its leading target and its fillers, as one string.
fn compact(clause: &[String]) -> String {
    let mut words: &[String] = clause;
    loop {
        if words.first().is_some_and(|w| FILLERS.contains(&w.as_str())) {
            words = &words[1..];
        } else if let Some(lead) = SKIP_LEADS.iter().find(|lead| {
            let parts: Vec<&str> = lead.split(' ').collect();
            words.len() > parts.len() && words.iter().zip(&parts).all(|(w, p)| w == p)
        }) {
            // The card still asks; the user's "확인 없이" changes nothing.
            words = &words[lead.split(' ').count()..];
        } else {
            break;
        }
    }
    let mut targets: Vec<&&str> = TARGETS.iter().collect();
    targets.sort_by_key(|t| std::cmp::Reverse(t.len()));
    let fixed = targets.iter().find_map(|target| {
        let parts: Vec<&str> = target.split(' ').collect();
        (words.len() > parts.len() && targeted(words, &parts)).then_some(parts.len())
    });
    if let Some(n) = fixed.or_else(|| saved_target(words)) {
        words = &words[n..];
    }
    words
        .iter()
        .filter(|w| !FILLERS.contains(&w.as_str()))
        .map(String::as_str)
        .collect::<Vec<_>>()
        .concat()
}

/// A saved-list target by its structure: a saving adnominal ("저장한",
/// "찜한", "저장해 둔", "찜해놓은") and a programme noun with an optional
/// plural and particle ("장학금들", "거", "혜택은"). Returns its word count.
fn saved_target(words: &[String]) -> Option<usize> {
    let adnominal = |w: &str| {
        ["저장", "찜", "북마크", "관심"]
            .iter()
            .any(|s| w.starts_with(s))
            && (w.ends_with('한')
                || w.ends_with("둔")
                || w.ends_with("놓은")
                || w.ends_with("놔둔"))
    };
    let lead = match words {
        [a, ..] if adnominal(a) => 1,
        [a, b, ..]
            if ["저장해", "찜해"].contains(&a.as_str())
                && ["둔", "놓은", "놔둔"].contains(&b.as_str()) =>
        {
            2
        }
        _ => return None,
    };
    let noun = words.get(lead)?;
    let bare = ["은", "는", "도", "을", "를", "의"]
        .iter()
        .find_map(|p| noun.strip_suffix(p))
        .unwrap_or(noun);
    let bare = bare.strip_suffix('들').unwrap_or(bare);
    let programme = [
        "혜택",
        "장학금",
        "지원금",
        "공고",
        "정책",
        "거",
        "것",
        "프로그램",
    ]
    .contains(&bare);
    (programme && words.len() > lead + 1).then_some(lead + 1)
}

/// The clause starts with the target, its last word bare or with a topic,
/// object or genitive particle ("이 혜택은", "이거도").
fn targeted(words: &[String], parts: &[&str]) -> bool {
    let n = parts.len();
    words[..n - 1].iter().zip(parts).all(|(w, p)| w == p)
        && ["", "은", "는", "도", "을", "를", "의"]
            .iter()
            .any(|particle| words[n - 1] == format!("{}{particle}", parts[n - 1]))
}

fn product(heads: &[&str], particles: &[&str], tails: &[&str]) -> HashSet<String> {
    let mut set = HashSet::new();
    for h in heads {
        for p in particles {
            for t in tails {
                set.insert(format!("{h}{p}{t}"));
            }
        }
    }
    set
}

const ALERT_NOUNS: &[&str] = &[
    "알림",
    "알람",
    "푸시",
    "푸시알림",
    "마감알림",
    "마감알람",
    "알림설정",
    "준비물알림",
    "서류알림",
    "신청알림",
    "접수알림",
    "발표알림",
    "결과알림",
    "마감일알림",
];
const PARTICLES: &[&str] = &["", "을", "를", "은", "는", "도", "만", "까지"];
const ALERT_ON_VERBS: &[&str] = &[
    "부탁해",
    "부탁해요",
    "부탁드려요",
    "부탁드립니다",
    "부탁",
    "켜기해줘",
    "켜줘",
    "켜줘요",
    "켜주세요",
    "켜",
    "켜놔",
    "켜놔줘",
    "켜둬",
    "켜줄래",
    "켜주라",
    "켜주실래요",
    "켜주시겠어요",
    "설정해주시겠어요",
    "켜줄수있을까요",
    "켜줄래요",
    "켜고",
    "켜주고",
    "켜조",
    "설정해줘",
    "설정해주세요",
    "설정해",
    "설정하고",
    "설정해조",
    "설정부탁해",
    "설정부탁해요",
    "설정부탁드려요",
    "설정부탁드립니다",
    "걸어줘",
    "걸어주세요",
    "걸어둬",
    "걸고",
    "등록해줘",
    "등록해주세요",
    "등록하고",
    "맞춰줘",
    "맞춰주세요",
    "받을래",
    "받을래요",
    "받고싶어",
    "받고싶어요",
    "받게해줘",
    "받게해주세요",
    "켜줄수있어",
    "켜주실수있나요",
    "켜줄수있나요",
    "보내줘",
    "보내주세요",
    "일정에등록해줘",
    "일정에넣어줘",
    "켜주셈",
    "설정해주셈",
];
/// The off forms (re-review 11 H4): 안 오게 / 안 받 / 그만 / 끄 / 꺼 / 해제 /
/// 중지 / 멈춰 / 받기 싫, each with a request or desire ending.
const ALERT_OFF_VERBS: &[&str] = &[
    "꺼줘",
    "꺼줘요",
    "꺼주세요",
    "꺼",
    "꺼줄래",
    "꺼주라",
    "꺼둬",
    "꺼버려",
    "꺼버려줘",
    "끄고",
    "꺼주고",
    "꺼조",
    "꺼줄수있어",
    "꺼주실래요",
    "꺼주시겠어요",
    "꺼주셈",
    "해제해주셈",
    "해제해주시겠어요",
    "꺼줄래요",
    "끄고싶어",
    "끄고싶어요",
    "끌래",
    "끌래요",
    "해제해줘",
    "해제해주세요",
    "해제부탁해",
    "해제부탁드려요",
    "해제부탁드립니다",
    "해제하고",
    "해제해줄래",
    "그만보내줘",
    "그만보내주세요",
    "그만받을래",
    "그만받을래요",
    "그만받고싶어",
    "그만받고싶어요",
    "그만해줘",
    "안오게해줘",
    "안오게해주세요",
    "안오게막아줘",
    "안오게해줄래",
    "오지않게해줘",
    "오지않게해주세요",
    "안받을래",
    "안받을래요",
    "안받고싶어",
    "안받고싶어요",
    "받지않을래",
    "받지않을래요",
    "받기싫어",
    "받기싫어요",
    "중지해줘",
    "중지해주세요",
    "멈춰줘",
    "멈춰주세요",
];
/// "마감 전에 알려줘": a reminder without the alert noun (an untimed one:
/// `set_alert` has no offset).
const REMINDER_HEADS: &[&str] = &[
    "마감다가오면",
    "마감일다가오면",
    "신청마감전에",
    "공식페이지열리면",
    "페이지열리면",
    "사이트열리면",
    "공식사이트열리면",
    "접수열리면",
    "신청열리면",
    "마감전에",
    "마감되기전에",
    "마감전",
    "접수끝나기전에",
    "끝나기전에",
    "신청끝나기전에",
    "마감끝나기전에",
    "놓치지않게",
    "까먹지않게",
    "잊지않게",
    "마감임박하면",
    "공고뜨면",
    "접수시작하면",
    "신청시작하면",
    "열리면",
    "오픈하면",
    "시작하면",
    "마감되기전",
    "결과나오면",
    "발표나면",
    "발표되면",
    "결과발표되면",
    "공고올라오면",
    "뜨면",
    "결과뜨면",
    "올라오면",
];
const REMINDER_TAILS: &[&str] = &[
    "알려줘",
    "알려주세요",
    "알려줘요",
    "알림줘",
    "알림주세요",
    "알람줘",
];

static ALERT_ON: LazyLock<HashSet<String>> = LazyLock::new(|| {
    let mut set = product(ALERT_NOUNS, PARTICLES, ALERT_ON_VERBS);
    set.extend(product(REMINDER_HEADS, &[""], REMINDER_TAILS));
    set.extend(["알림줘", "알람줘"].iter().map(|s| (*s).to_owned()));
    set
});
static ALERT_OFF: LazyLock<HashSet<String>> =
    LazyLock::new(|| product(ALERT_NOUNS, PARTICLES, ALERT_OFF_VERBS));

/// Short noun commands with a clear write action ("이거 저장", "찜 해제",
/// "알림 그만"): a card as the goal's last clause (2026-11-27 decision). A
/// lone topic word ("저장", "알림") names no action and stays a button.
static ALERT_BARE_ON: LazyLock<HashSet<String>> =
    LazyLock::new(|| product(ALERT_NOUNS, &[""], &["켜기", "on"]));
static ALERT_BARE_OFF: LazyLock<HashSet<String>> = LazyLock::new(|| {
    product(
        ALERT_NOUNS,
        &[""],
        &["그만", "해제", "끄기", "off", "오프", "중지"],
    )
});

/// A short command (`bare`) in the goal's last clause, not a lone noun.
fn bare_command(bare: &HashSet<String>, nouns: &[&str], clause: &[String], c: &str) -> bool {
    let c = c.to_lowercase();
    bare.contains(&c) && (clause.len() >= 2 || !nouns.contains(&c.as_str()))
}

/// A polite request said as a question ("켜 줄 수 있어?", "저장해 주시겠어요?").
fn polite(s: &str) -> bool {
    [
        "수있어",
        "수있나요",
        "수있을까요",
        "줄래",
        "줄래요",
        "시겠어요",
        "실래요",
        "실수있나요",
    ]
    .iter()
    .any(|e| s.ends_with(e))
}

/// The alert state the goal asks for in one of its clauses, or `None`. A
/// retraction after it ("… 아니다 됐음") wins.
pub(crate) fn alert(goal: &str) -> Option<bool> {
    let (text, question) = surface(goal)?;
    // The old retraction reading still counts until the switch deletes it
    // ("알림 켜줘 아 알림은 됐다", "… 그냥 두자").
    if goals::alert_retracted(&text) {
        return None;
    }
    let all = marked(&text);
    let parts = clauses(&text);
    let mut found = None;
    let mut seen = false;
    for (i, (clause, made)) in all.iter().enumerate() {
        let c = compact(clause);
        // A mentioned alert gives no card, but a bare repeat after it reuses
        // its noun ("알림 꺼줘 했잖아 빨리 꺼줘").
        if !made {
            seen = seen || ALERT_ON.contains(&c) || ALERT_OFF.contains(&c);
            continue;
        }
        // The question mark ends the last clause: there an alert counts
        // only as a polite request ("켜 줄 수 있어?").
        if question && i + 1 == parts.len() && !polite(&c) {
            continue;
        }
        let last = i + 1 == all.len();
        if ALERT_ON.contains(&c) || (last && bare_command(&ALERT_BARE_ON, ALERT_NOUNS, clause, &c))
        {
            found = Some(true);
            seen = true;
        } else if ALERT_OFF.contains(&c)
            || (last && bare_command(&ALERT_BARE_OFF, ALERT_NOUNS, clause, &c))
        {
            found = Some(false);
            seen = true;
        } else if seen && retracts(clause) {
            found = None;
        } else if seen {
            // After the alert, a bare verb corrects it ("알림 켜줘 아니 됐어
            // 그냥 꺼둬" is off; "… 아니 그냥 켜줘" is on).
            let mut bare = c.as_str();
            loop {
                let before = bare;
                for lead in ["아니다", "아니", "아냐", "됐어", "잠깐", "음", "아"] {
                    bare = bare.strip_prefix(lead).unwrap_or(bare);
                }
                if bare == before {
                    break;
                }
            }
            if ALERT_OFF_VERBS.contains(&bare) {
                found = Some(false);
            } else if ALERT_ON_VERBS.contains(&bare) {
                found = Some(true);
            }
        }
    }
    found
}

const SAVE_NOUNS: &[&str] = &["저장", "찜", "북마크"];
const SAVE_ON_VERBS: &[&str] = &[
    "해줘",
    "해줘요",
    "해주세요",
    "해",
    "해둬",
    "해놔",
    "해놔줘",
    "하고",
    "해줄래",
    "부탁해",
    "부탁해요",
    "부탁드려요",
    "해두자",
    "해놓자",
    "하고싶어",
    "해주고",
    "해주실래요",
    "해줄래요",
    "해두고",
    "해주시겠어요",
    "해줄수있어",
    "해줄수있나요",
    "해주실수있나요",
    "해놓을래",
    "해둘래",
    "해주셈",
    "해두셈",
];
const SAVE_OFF_VERBS: &[&str] = &[
    "취소해줘",
    "취소해주세요",
    "해제해줘",
    "해제해주세요",
    "삭제해줘",
    "삭제해주세요",
    "지워줘",
    "빼줘",
    "취소하고",
    "해제하고",
    "취소해줘요",
    "취소해줄래",
    "취소해주실래요",
    "해제해줘요",
    "해제해줄래",
    "취소해주시겠어요",
    "해제해주시겠어요",
    "취소해줄수있어",
    "취소해",
    "해제해",
    "취소해주셈",
    "해제해주셈",
    "풀어줘",
    "풀어주세요",
    "풀어주셈",
];
const SAVE_LISTS: &[&str] = &["관심목록에", "관심혜택에", "저장목록에", "찜목록에"];
const SAVE_LIST_VERBS: &[&str] = &[
    "넣어줘",
    "넣어주세요",
    "추가해줘",
    "추가해주세요",
    "담아줘",
    "넣고",
    "추가하고",
    "저장해줘",
    "저장해주세요",
    "넣어둬",
];
const SAVE_OUT_LISTS: &[&str] = &["관심목록에서", "관심혜택에서", "저장목록에서", "찜목록에서"];
const SAVE_OUT_VERBS: &[&str] = &["빼줘", "빼주세요", "삭제해줘", "지워줘", "빼고"];

static SAVE_ON: LazyLock<HashSet<String>> = LazyLock::new(|| {
    let mut set = product(SAVE_NOUNS, &["", "도", "을", "를", "만"], SAVE_ON_VERBS);
    set.extend(product(SAVE_LISTS, &[""], SAVE_LIST_VERBS));
    set.extend(["담아줘", "담고"].iter().map(|s| (*s).to_owned()));
    set
});
static SAVE_OFF: LazyLock<HashSet<String>> = LazyLock::new(|| {
    let mut set = product(SAVE_NOUNS, &["", "을", "를", "은"], SAVE_OFF_VERBS);
    set.extend(product(SAVE_OUT_LISTS, &[""], SAVE_OUT_VERBS));
    set
});
/// Short save commands (see [`ALERT_BARE_ON`]).
static SAVE_BARE_ON: LazyLock<HashSet<String>> =
    LazyLock::new(|| product(SAVE_NOUNS, &[""], &["", "하기"]));
static SAVE_BARE_OFF: LazyLock<HashSet<String>> =
    LazyLock::new(|| product(SAVE_NOUNS, &[""], &["해제", "취소", "삭제", "빼기", "풀기"]));

/// The bookmark state the goal asks for in one of its clauses.
pub(crate) fn bookmark(goal: &str) -> Option<bool> {
    let (text, question) = surface(goal)?;
    let all = marked(&text);
    let parts = clauses(&text);
    let mut found = None;
    let mut mentioned: Option<bool> = None;
    for (i, (clause, made)) in all.iter().enumerate() {
        let c = compact(clause);
        if !made {
            if SAVE_ON.contains(&c) {
                mentioned = Some(true);
            } else if SAVE_OFF.contains(&c) {
                mentioned = Some(false);
            }
            continue;
        }
        // A bare repeat after a mentioned save reuses its noun ("저장해줘
        // 했잖아 빨리 해줘").
        let repeat = match mentioned {
            Some(true) => ["해줘", "해주세요", "해줘요", "해줄래"].contains(&c.as_str()),
            Some(false) => {
                ["취소해줘", "취소해주세요", "해제해줘", "해제해주세요"].contains(&c.as_str())
            }
            None => false,
        };
        if repeat && !retracted_after(&parts, i) {
            found = mentioned;
            continue;
        }
        // In the question's last clause only a polite request counts
        // ("저장해 줄 수 있어?").
        if question && i + 1 == parts.len() && !polite(&c) {
            continue;
        }
        let last = i + 1 == all.len();
        if SAVE_ON.contains(&c) || (last && bare_command(&SAVE_BARE_ON, SAVE_NOUNS, clause, &c)) {
            found = Some(true).filter(|_| !retracted_after(&parts, i));
        } else if SAVE_OFF.contains(&c)
            || (last && bare_command(&SAVE_BARE_OFF, SAVE_NOUNS, clause, &c))
        {
            found = Some(false).filter(|_| !retracted_after(&parts, i));
        }
    }
    found
}

/// Status reports, said as the whole goal or before a record request.
const STATUS_PHRASES: &[(&str, ApplicationStatus)] = &[
    ("신청했어", ApplicationStatus::Applied),
    ("신청했어요", ApplicationStatus::Applied),
    ("신청했다", ApplicationStatus::Applied),
    ("신청했음", ApplicationStatus::Applied),
    ("신청완료", ApplicationStatus::Applied),
    ("신청완료했어", ApplicationStatus::Applied),
    ("신청완료했어요", ApplicationStatus::Applied),
    ("접수했어", ApplicationStatus::Applied),
    ("접수완료", ApplicationStatus::Applied),
    ("접수완료했어", ApplicationStatus::Applied),
    ("제출했어", ApplicationStatus::Applied),
    ("제출완료", ApplicationStatus::Applied),
    ("지원했어", ApplicationStatus::Applied),
    ("신청넣었어", ApplicationStatus::Applied),
    ("지원완료", ApplicationStatus::Applied),
    ("지원완료함", ApplicationStatus::Applied),
    ("신청완료함", ApplicationStatus::Applied),
    ("접수완료함", ApplicationStatus::Applied),
    ("제출완료함", ApplicationStatus::Applied),
    ("지원완료했어", ApplicationStatus::Applied),
    ("신청할거야", ApplicationStatus::Planning),
    ("신청할게", ApplicationStatus::Planning),
    ("신청할예정이야", ApplicationStatus::Planning),
    ("신청할예정", ApplicationStatus::Planning),
    ("신청예정", ApplicationStatus::Planning),
    ("다음주에신청할거", ApplicationStatus::Planning),
    ("결과기다리는중", ApplicationStatus::Waiting),
    ("결과기다리는중이야", ApplicationStatus::Waiting),
    ("결과기다리고있어", ApplicationStatus::Waiting),
    ("신청결과기다리는중이야", ApplicationStatus::Waiting),
    ("발표기다리는중", ApplicationStatus::Waiting),
    ("발표기다리는중이야", ApplicationStatus::Waiting),
    ("결과대기중", ApplicationStatus::Waiting),
    ("결과대기중이야", ApplicationStatus::Waiting),
    ("발표대기중", ApplicationStatus::Waiting),
    ("합격했어", ApplicationStatus::Received),
    ("합격했어요", ApplicationStatus::Received),
    ("최종합격했어", ApplicationStatus::Received),
    ("선정됐어", ApplicationStatus::Received),
    ("선정되었어", ApplicationStatus::Received),
    ("당첨됐어", ApplicationStatus::Received),
    ("수혜받았어", ApplicationStatus::Received),
    ("받았어", ApplicationStatus::Received),
    ("포기할래", ApplicationStatus::Abandoned),
    ("포기할게", ApplicationStatus::Abandoned),
    ("포기했어", ApplicationStatus::Abandoned),
    ("신청안할래", ApplicationStatus::Abandoned),
    ("그만둘래", ApplicationStatus::Abandoned),
    ("안할래", ApplicationStatus::Abandoned),
    ("이번엔안할래", ApplicationStatus::Abandoned),
    ("이번엔포기할래", ApplicationStatus::Abandoned),
    ("이번엔신청안할래", ApplicationStatus::Abandoned),
];
/// A record request after a status ("… 기록해줘", "… 상태 바꾸고").
const STATUS_RECORDS: &[&str] = &[
    "결과기록해줘",
    "결과로기록해줘",
    "기록해줘",
    "기록해",
    "기록",
    "기록하고",
    "기록해주세요",
    "으로기록해줘",
    "로기록해줘",
    "라고기록해줘",
    "이라고기록해줘",
    "라고적어둬",
    "이라고적어둬",
    "으로바꿔줘",
    "로바꿔줘",
    "으로바꿔",
    "로바꿔",
    "상태바꿔줘",
    "상태바꾸고",
    "상태업데이트해줘",
    "상태업데이트해",
    "처리해줘",
    "으로표시해줘",
    "로표시해줘",
    "로해줘",
    "으로해줘",
    "기록해둬",
];
/// The status bare ("신청 완료", "결과 기다리는 중") or as a record noun phrase.
const STATUS_NOUNS: &[(&str, ApplicationStatus)] = &[
    ("신청완료", ApplicationStatus::Applied),
    ("완료", ApplicationStatus::Applied),
    ("결과기다리는중", ApplicationStatus::Waiting),
    ("발표대기중", ApplicationStatus::Waiting),
    ("결과대기", ApplicationStatus::Waiting),
    ("합격", ApplicationStatus::Received),
    ("포기", ApplicationStatus::Abandoned),
    ("신청예정", ApplicationStatus::Planning),
];

/// The status the goal asks to record: a report said as the whole goal
/// ("나 이거 신청했어", "결과 기다리는 중이야"), or a status with a record
/// request in its clause ("신청 완료로 기록해줘", "방금 제출했어 상태 바꾸고
/// …").
pub(crate) fn status(goal: &str) -> Option<ApplicationStatus> {
    let (text, question) = surface(goal)?;
    if question {
        return None;
    }
    // A polite ending is the same request ("기록해 주세요" is "기록해줘").
    let plain = |c: String| -> String {
        for (polite, base) in [("주세요", "줘"), ("줘요", "줘"), ("주실래요", "줘")] {
            if let Some(stem) = c.strip_suffix(polite) {
                return format!("{stem}{base}");
            }
        }
        match c.strip_suffix('요') {
            Some(stem) if stem.chars().count() >= 2 => stem.to_owned(),
            _ => c,
        }
    };
    let parts = clauses(&text);
    let whole = plain(compact(
        &text
            .split_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>(),
    ));
    if let Some((_, status)) = STATUS_PHRASES.iter().find(|(p, _)| *p == whole) {
        return Some(*status);
    }
    // A clause, or a report clause joined to the record clause after it
    // ("방금 제출했어 상태 바꿔줘").
    // Each span with the index of its last clause; a mentioned (empty)
    // clause joins nothing.
    let mut spans: Vec<(usize, Vec<String>)> = parts.iter().cloned().enumerate().collect();
    spans.extend(
        parts
            .windows(2)
            .enumerate()
            .filter(|(_, w)| !w[0].is_empty() && !w[1].is_empty())
            .map(|(i, w)| (i + 1, [w[0].clone(), w[1].clone()].concat())),
    );
    for (end, clause) in &spans {
        // "… 기록해줘 아 아직 아니다", "… 잠깐만 아직 결과 안 나왔어": not yet.
        let not_yet = parts[*end + 1..].iter().any(|c| {
            let w: Vec<&str> = c
                .iter()
                .map(String::as_str)
                .filter(|w| !["아", "앗", "음", "잠깐", "잠깐만", "그냥"].contains(w))
                .collect();
            w.first().is_some_and(|w| w.starts_with("아직"))
        });
        if clause.is_empty() || not_yet || retracted_after(&parts, *end) {
            continue;
        }
        let c = plain(compact(clause));
        for record in STATUS_RECORDS {
            let Some(head) = c.strip_suffix(record) else {
                continue;
            };
            let head = head.trim_end_matches("상태");
            // A reason before the record request ("신청 완료했으니까 기록해줘").
            let head = ["으니까", "니까", "으니", "니"]
                .iter()
                .find_map(|e| head.strip_suffix(e))
                .filter(|h| h.ends_with(['했', '됐', '었', '았', '였']))
                .map_or_else(|| head.to_owned(), |h| format!("{h}어"));
            let head = head.as_str();
            if let Some((_, s)) = STATUS_PHRASES
                .iter()
                .chain(STATUS_NOUNS)
                .find(|(p, _)| *p == head)
            {
                return Some(*s);
            }
        }
    }
    None
}

const CHECK_DONE: &[&str] = &[
    "뗐어",
    "뗐어요",
    "뗐다",
    "뗐음",
    "떼놨어",
    "떼었어",
    "뽑았어",
    "뽑았어용",
    "챙겼어",
    "준비했어",
    "준비완료",
    "발급받았어",
    "받았어",
];
const CHECK_MARK: &[&str] = &[
    "",
    "체크해줘",
    "체크",
    "체크해",
    "체크해주세요",
    "완료로체크해줘",
    "완료표시해줘",
    "준비체크해줘",
];
const CHECK_UNDO: &[&str] = &[
    "체크해제해줘",
    "체크해제",
    "체크풀어줘",
    "체크빼줘",
    "아직안뗐어체크해제",
    "아직안뗐어체크풀어줘",
    "아직이야체크풀어줘",
    "아직안뗐어",
    "체크취소해줘",
    "체크취소",
    "체크풀어",
    "체크해제해주세요",
    "체크취소해주세요",
];
const DOCUMENTS: &[&str] = &[
    "증명서",
    "등본",
    "초본",
    "확인서",
    "통장",
    "신분증",
    "사본",
    "계획서",
    "신청서",
    "자기소개서",
    "추천서",
    "성적표",
    "졸업증명서",
];

/// The checklist mark the goal asks for as the whole goal: a document then
/// a done or undo phrase ("가족관계증명서 뗐어 체크해줘", "등본 체크 해제해줘").
pub(crate) fn checklist(goal: &str) -> Option<bool> {
    let (text, question) = surface(goal)?;
    if question {
        return None;
    }
    // A later retraction takes the mark back ("등본 체크해줘 아니 됐어").
    let parts = clauses(&text);
    if !parts.is_empty() && retracted_after(&parts, 0) {
        return None;
    }
    let words: Vec<String> = text.split_whitespace().map(str::to_owned).collect();
    // The document: the first one to three words, the last naming one.
    for n in 1..=3.min(words.len().saturating_sub(1)) {
        let doc = &words[..n];
        let strip = |w: &str| {
            ["은", "는", "을", "를", "도", "이", "가"]
                .iter()
                .find_map(|p| w.strip_suffix(p))
                .unwrap_or(w)
                .to_owned()
        };
        let last = strip(&doc[n - 1]);
        if !DOCUMENTS.iter().any(|d| last.ends_with(d)) {
            continue;
        }
        let rest = compact(&words[n..]);
        if CHECK_UNDO.contains(&rest.as_str()) {
            return Some(false);
        }
        for done in CHECK_DONE {
            for mark in CHECK_MARK {
                if rest == format!("{done}{mark}") {
                    return Some(true);
                }
            }
        }
        if CHECK_MARK[1..].contains(&rest.as_str()) {
            return Some(true);
        }
    }
    None
}

const CALENDAR_DATES: &[&str] = &[
    "마감일",
    "마감",
    "발표일",
    "결과",
    "면접",
    "날짜",
    "일정",
    "면접일",
    "접수",
    "시작일",
    "신청",
    "결과발표일",
    "마감날짜",
];
const CALENDAR_PLACES: &[&str] = &[
    "캘린더에",
    "달력에",
    "일정에",
    "캘린더에도",
    "달력에도",
    "구글캘린더에",
    "일정",
    "캘린더",
    "달력",
    "일정으로",
];
const CALENDAR_VERBS: &[&str] = &[
    "넣어줘",
    "추가해줘",
    "등록해줘",
    "저장해줘",
    "넣어주세요",
    "추가해주세요",
    "등록해주세요",
    "넣고",
    "추가하고",
    "등록하고",
    "박아줘",
];

/// A calendar request in one of the goal's clauses: an optional programme
/// date (the deadline when none is named) and offset ("3일 전", "이틀 전에"),
/// the calendar and a request.
pub(crate) fn calendar(goal: &str) -> bool {
    let Some((text, question)) = surface(goal) else {
        return false;
    };
    let offset = |w: &str| {
        let w = w.trim_end_matches('에');
        w == "전"
            || ["이틀", "사흘", "하루", "일주일", "한달", "보름"].contains(&w)
            || w.strip_suffix('일')
                .or_else(|| w.strip_suffix('주'))
                .is_some_and(|d| !d.is_empty() && d.chars().all(|c| c.is_ascii_digit()))
    };
    let parts = clauses(&text);
    parts.iter().enumerate().any(|(i, clause)| {
        if clause.is_empty() || (question && i + 1 == parts.len()) || retracted_after(&parts, i) {
            return false;
        }
        let mut words: Vec<&str> = clause.iter().map(String::as_str).collect();
        for target in TARGETS {
            let parts: Vec<&str> = target.split(' ').collect();
            if words.len() > parts.len() && words[..parts.len()] == parts[..] {
                words.drain(..parts.len());
                break;
            }
        }
        let words: Vec<&str> = words.into_iter().filter(|w| !FILLERS.contains(w)).collect();
        let Some(place) = words.iter().rposition(|w| CALENDAR_PLACES.contains(w)) else {
            return false;
        };
        let verb = words[place + 1..].concat();
        let dates = &words[..place];
        // No date named is the deadline (`goals::calendar_date`), as the
        // calendar button and route read it.
        CALENDAR_VERBS.contains(&verb.as_str())
            && dates.iter().all(|w| {
                CALENDAR_DATES
                    .iter()
                    .any(|d| w.trim_end_matches(['을', '를', '도']) == *d)
                    || offset(w)
            })
    })
}

const PROFILE_OWNERS: &[&str] = &["내", "나", "나의", "내 프로필", "프로필", "제"];
const PROFILE_FIELDS: &[&str] = &[
    "지역",
    "거주지",
    "학년",
    "학적",
    "소득분위",
    "소득 분위",
    "취업 상태",
    "취업상태",
    "가구원 수",
    "가구원수",
    "소득 구간",
    "소득구간",
    "재학 상태",
    "재학상태",
];
const PROFILE_VERBS: &[&str] = &[
    "바꿔줘",
    "변경해줘",
    "수정해줘",
    "바꿔 줘",
    "변경해 줘",
    "수정해 줘",
    "바꿔주세요",
    "변경해주세요",
    "수정해주세요",
    "고쳐줘",
    "바꿔",
    "바꿔 주세요",
    "변경해 주세요",
    "수정해 주세요",
    "바꿔주시겠어요",
];

/// An own-profile change as the whole goal: an optional owner, an optional
/// field, a value ending in 로/으로, and a change request.
pub(crate) fn profile(goal: &str) -> bool {
    let Some((text, question)) = surface(goal) else {
        return false;
    };
    if question {
        return false;
    }
    let mut rest: &str = text.as_str();
    // Owners stack ("내 프로필 거주지 …").
    while let Some(r) = PROFILE_OWNERS
        .iter()
        .find_map(|owner| rest.strip_prefix(&format!("{owner} ")))
    {
        rest = r;
    }
    for field in PROFILE_FIELDS {
        for particle in ["", "을", "를", "은", "는"] {
            if let Some(r) = rest.strip_prefix(&format!("{field}{particle} ")) {
                rest = r;
                break;
            }
        }
    }
    let Some(verb) = PROFILE_VERBS
        .iter()
        .find(|v| rest.ends_with(&format!(" {v}")))
    else {
        return false;
    };
    let value = rest[..rest.len() - verb.len()].trim();
    let tokens: Vec<&str> = value.split_whitespace().collect();
    (1..=2).contains(&tokens.len())
        && tokens.last().is_some_and(|w| w.ends_with('로'))
        && !tokens.iter().any(|w| {
            ["안", "말고", "어디", "뭐", "왜", "어떻게"]
                .iter()
                .any(|n| w.starts_with(n))
        })
}

const FIELDS: &[&str] = &[
    "이름",
    "성명",
    "연락처",
    "전화번호",
    "휴대폰",
    "핸드폰",
    "휴대전화",
    "주소",
    "이메일",
    "학교명",
    "학과",
    "전공",
    "학번",
    "생년월일",
    "메일",
    "휴대폰번호",
    "지원동기",
    "자기소개",
    "영문이름",
    "영문성명",
    "주민등록번호",
];
/// Whose field it is ("신청인 이름 칸에").
const FIELD_OWNERS: &[&str] = &["", "신청인", "신청자", "보호자", "본인", "지원자", "학생"];
const FIELD_MARKS: &[&str] = &["칸에", "칸", "란에", "란", "칸에는"];
const FIELD_VERBS: &[&str] = &[
    "적어줘",
    "써줘",
    "넣어줘",
    "채워줘",
    "적어주세요",
    "써주세요",
    "넣어주세요",
    "채워주세요",
    "적어줘요",
    "써줘요",
    "넣어줘요",
    "채워줘요",
    "적어주실래요",
    "넣어주실래요",
    "채워주실래요",
    "적어줄래",
    "넣어줄래",
    "채워줄래",
];

/// A form-field value as the whole goal (re-review 11 H1 and the round 12
/// decision): "<field> 칸(에) <value>(으로/로) (적어/써/넣어/채워)줘", or its
/// polite ending, and nothing else. The field is a known field word with
/// 칸/란 (attached or next), the value one to four words with no
/// negation, question or other request in it.
pub(crate) fn field_value(goal: &str) -> bool {
    let Some((text, question)) = surface(goal) else {
        return false;
    };
    if question {
        return false;
    }
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.len() < 3 {
        return false;
    }
    // The field: one or two words ("지원 동기 칸에", "이름 칸에", "이름칸에").
    let mut field_end = None;
    for n in 1..=3.min(words.len() - 2) {
        let head = words[..n].concat();
        let marked = FIELD_MARKS.iter().find_map(|m| head.strip_suffix(m));
        if let Some(name) = marked {
            if FIELD_OWNERS
                .iter()
                .any(|o| name.strip_prefix(o).is_some_and(|f| FIELDS.contains(&f)))
            {
                field_end = Some(n);
                break;
            }
        }
    }
    let Some(start) = field_end else {
        return false;
    };
    let verb = *words.last().unwrap_or(&"");
    // "적어 줘" is the same request as "적어줘".
    let (verb, value) = if FIELD_VERBS.contains(&verb) {
        (verb.to_owned(), &words[start..words.len() - 1])
    } else if words.len() >= start + 3 {
        let joined = format!("{}{}", words[words.len() - 2], verb);
        (joined, &words[start..words.len() - 2])
    } else {
        return false;
    };
    let bad = |w: &str| {
        [
            "안",
            "말",
            "마",
            "뭐",
            "어디",
            "맞",
            "왜",
            "어떻게",
            "찾",
            "검색",
            "알림",
            "저장",
            "신청",
            "해야",
            "하나",
            "적어야",
            "못",
        ]
        .iter()
        .any(|n| w.starts_with(n))
            || w.ends_with("고")
            || w.ends_with("서")
            || w.ends_with("면")
    };
    FIELD_VERBS.contains(&verb.as_str())
        && (1..=4).contains(&value.len())
        && !value.iter().any(|w| bad(w))
}

/// Alert and save phrases in any clause, with the polarity the phrase
/// says and the clause's [`Verdict`] (made, mentioned, unclear): what the
/// act head may judge when no made request settles the type. Polarity is
/// the rules' alone.
pub(crate) fn candidates(goal: &str) -> Vec<(super::intent::WriteType, bool, Verdict, String)> {
    use super::intent::WriteType;
    let Some((text, _)) = surface(goal) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (clause, verdict) in marked_with(&text) {
        let c = compact(&clause);
        let joined = clause.join(" ");
        if ALERT_ON.contains(&c) {
            out.push((WriteType::Alert, true, verdict, joined));
        } else if ALERT_OFF.contains(&c) {
            out.push((WriteType::Alert, false, verdict, joined));
        } else if SAVE_ON.contains(&c) {
            out.push((WriteType::Save, true, verdict, joined));
        } else if SAVE_OFF.contains(&c) {
            out.push((WriteType::Save, false, verdict, joined));
        }
    }
    out
}

/// The status said as the whole goal ("나 이거 신청했어"): a report, which
/// for a status is the request.
pub(crate) fn status_reported(goal: &str) -> bool {
    let Some((text, question)) = surface(goal) else {
        return false;
    };
    let whole = compact(
        &text
            .split_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>(),
    );
    !question
        && STATUS_PHRASES
            .iter()
            .any(|(p, _)| *p == whole || format!("{p}요") == whole)
}

/// The goal names the saved list by its structure ([`saved_target`]) in any
/// clause: the one reader of that target (the intent's [`target`], the
/// classifier's slot), code review H1.
///
/// [`target`]: super::intent::target
pub(crate) fn saved_list(goal: &str) -> bool {
    let Some((text, _)) = surface(goal) else {
        return false;
    };
    // A fused adnominal and noun ("찜한거", "저장한것들") is the same two
    // words without the space.
    let words: Vec<String> = text
        .split_whitespace()
        .flat_map(|w| match FUSED_SAVED.captures(w) {
            Some(m) => vec![m[1].to_owned(), m[2].to_owned()],
            None => vec![w.to_owned()],
        })
        .collect();
    (0..words.len()).any(|i| saved_target(&words[i..]).is_some())
}

static FUSED_SAVED: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        "^((?:저장|찜|북마크|관심)\\S*?(?:한|둔|놓은|놔둔))((?:거|것|혜택|장학금|지원금|공고|정책|프로그램)\\S*)$",
    )
    .expect("static regex")
});

/// The card gate over a plan's write steps ([`super::intent::gate`]): each
/// write is kept, retargeted to the intent's own step, or dropped, and a
/// retargeted step already in the plan is not repeated. What a search found
/// is a target only after the plan's search. Returns whether a write about
/// the programme on screen was dropped.
pub(crate) fn gate_plan(
    plan: &mut mazimi_core::agent::Plan,
    goal: &str,
    context: goals::GoalContext,
) -> bool {
    let mut searched = false;
    let mut dropped_on_screen = false;
    let mut kept: Vec<ToolCall> = Vec::with_capacity(plan.steps.len());
    for step in std::mem::take(&mut plan.steps) {
        searched |= matches!(step, ToolCall::SearchBenefits(_));
        if !step.name().is_write() {
            kept.push(step);
            continue;
        }
        // With the understanding model, planned writes are dropped (the
        // planners plan reads only) except what the plan's search found and
        // the route-owned handoffs (form, official page), which the gate
        // keeps only when the model asks for them; the intent's writes are
        // merged after the gate.
        if super::intent::model_active()
            && step.program() != Some(mazimi_core::agent::ProgramRef::Found)
            && !super::intent::is_handoff(&step)
        {
            dropped_on_screen |= step.program() == Some(mazimi_core::agent::ProgramRef::Context);
            continue;
        }
        match super::intent::gate(goal, context, &step, searched) {
            Some(step) if !kept.contains(&step) => kept.push(step),
            Some(_) => {}
            None => {
                dropped_on_screen |= step.program() == Some(mazimi_core::agent::ProgramRef::Context)
            }
        }
    }
    plan.steps = kept;
    dropped_on_screen
}

/// Kill switch for the producer: `ASSISTANT_CARD_PRODUCER_ENABLED=false`
/// turns [`merge_requested`] off (default on).
static PRODUCER: LazyLock<bool> = LazyLock::new(|| {
    std::env::var("ASSISTANT_CARD_PRODUCER_ENABLED").map_or(true, |v| {
        !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "false" | "0" | "no" | "off"
        )
    })
});

/// R0: the writes the goal asks for that the rules can fill
/// ([`super::intent::produced`]), as steps.
pub(crate) fn requested(goal: &str, context: goals::GoalContext) -> Vec<ToolCall> {
    if !*PRODUCER {
        return Vec::new();
    }
    super::intent::produced(goal, context, None)
        .into_iter()
        .filter_map(|i| i.step)
        .collect()
}

/// Append each [`requested`] write whose tool the plan does not have yet,
/// within the plan's step bound, after the planner's steps (so a page
/// check still holds them, and the 준비 journey's alert stays last). A
/// route's own write of the type keeps its arguments. Returns whether
/// anything was added.
pub(crate) fn merge_requested(
    plan: &mut mazimi_core::agent::Plan,
    goal: &str,
    context: goals::GoalContext,
) -> bool {
    let mut added = false;
    for step in requested(goal, context) {
        // Every caller gates the plan first ([`gate_plan`]), so a write of
        // this tool still in it is on a target the goal asks for (a search's
        // finds, the saved list): it stands, and this one is not added.
        if plan.steps.len() >= mazimi_core::agent::MAX_PLAN_STEPS
            || plan.steps.iter().any(|s| s.name() == step.name())
        {
            continue;
        }
        plan.steps.push(step);
        added = true;
    }
    added
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alert_forms() {
        for g in [
            "이 혜택 마감 알림 켜줘",
            "알림 좀 설정해줘",
            "저장한 혜택 알림 다 켜줘",
            "찜한거 알림 전부 켜",
            "마감 알림 설정해조",
            "마감 전에 꼭 알려줘",
            "알림 켜 주실래요",
        ] {
            assert_eq!(alert(g), Some(true), "{g}");
        }
        for g in [
            "이 혜택 알림 안 오게 해줘",
            "알림 더는 안 받고 싶어",
            "이 공고 알림 받지 않을래",
            "마감 알림 오지 않게 해 주세요",
            "알림 그만 받을래요",
            "푸시 안 오게 막아줘",
            "이 혜택 마감 알림 꺼줘",
        ] {
            assert_eq!(alert(g), Some(false), "{g}");
        }
        for g in [
            "알림 켜둔 혜택들 정리해줘",
            "알림 온 공고 내용 설명 부탁해",
            "알림은 켜놨으니 서류 목록 보여줘",
            "알림은 부탁 안 할게",
            "알림 켜야 하나?",
            "\"알림 켜줘\"라고 말하면 바로 켜져?",
            "알림 해제한 혜택 다시 보여줘",
            "알림 켜줘 아니다 됐음",
            // No request ending of the off forms: the off button covers them.
            "알림 이제 안 와도 돼",
            "이거 알림 끊어줘",
            "알림 꺼도 돼",
            // A lone topic word names no action.
            "알림",
            "이 혜택 알림?",
        ] {
            assert_eq!(alert(g), None, "{g}");
        }
        // A short command with a clear action is a card (2026-11-27).
        for (g, on) in [
            ("알림 해제", false),
            ("이거 알림 그만", false),
            ("마감 알림 끄기", false),
            ("알림 off", false),
            ("알림 켜기", true),
            ("이 혜택 알림 꺼주셈", false),
        ] {
            assert_eq!(alert(g), Some(on), "{g}");
        }
        assert_eq!(alert("저장은 빼고 알림 켜 주세요"), Some(true));
        assert_eq!(alert("이 혜택 신청 준비하고 알림도 켜줘"), Some(true));
    }

    #[test]
    fn bookmark_status_checklist_calendar_profile_field_forms() {
        assert_eq!(bookmark("이 혜택 저장해줘"), Some(true));
        assert_eq!(
            bookmark("주거 지원 찾아서 저장하고 알림도 켜줘"),
            Some(true)
        );
        assert_eq!(bookmark("이 혜택 찜 해제해줘"), Some(false));
        // Short commands and colloquial endings (2026-11-27); a lone noun or
        // a question is no card.
        for (g, on) in [
            ("이 혜택 찜 해제", false),
            ("이 혜택 저장", true),
            ("이거 저장", true),
            ("찜 해제", false),
            ("저장 취소", false),
            ("북마크 풀어주셈", false),
            ("일단 찜 해놓을래", true),
        ] {
            assert_eq!(bookmark(g), Some(on), "{g}");
        }
        for g in ["저장", "이거 저장?", "찜"] {
            assert_eq!(bookmark(g), None, "{g}");
        }
        assert_eq!(bookmark("관심 목록에 넣어둔 거 보여줘"), None);
        assert_eq!(bookmark("북마크 삭제된 거 복구해줘"), None);
        assert_eq!(status("나 이거 신청했어"), Some(ApplicationStatus::Applied));
        assert_eq!(
            status("이 혜택 신청 완료로 기록해줘"),
            Some(ApplicationStatus::Applied)
        );
        assert_eq!(
            status("결과 기다리는 중이야"),
            Some(ApplicationStatus::Waiting)
        );
        assert_eq!(status("수혜 조건 정리해줘"), None);
        assert_eq!(status("합격하면 뭐 해야 돼"), None);
        assert_eq!(status("합격했으니까 다음 혜택 추천해줘"), None);
        assert_eq!(checklist("가족관계증명서 뗐어 체크해줘"), Some(true));
        assert_eq!(checklist("등본 체크 해제해줘"), Some(false));
        assert_eq!(checklist("등본 뗐는데 유효기간이 얼마야"), None);
        assert!(calendar("마감일 캘린더에 넣어줘"));
        assert!(calendar("마감 3일 전 달력에 추가해줘"));
        assert!(!calendar("달력 보여줘"));
        // No date named: the deadline, as the calendar button and the
        // calendar route read it. Anything else before the place still bars.
        assert!(calendar("캘린더에 넣어줘"));
        assert!(calendar("이 혜택 달력에 추가해줘"));
        assert!(!calendar("알림 일정에 등록해줘"));
        assert!(!calendar("캘린더에 넣어줘?"));
        assert!(!calendar("캘린더에 넣어줘 아니 하지 마"));
        assert!(profile("지역 부산으로 바꿔줘"));
        assert!(profile("3학년으로 수정해줘"));
        assert!(!profile("프로필에 전공 입력해줘"));
        assert!(field_value("이름 칸에 홍길동으로 적어줘"));
        assert!(field_value("연락처 칸에 010-1234-5678 넣어 주세요"));
        assert!(field_value("성명 란에 영문 이름 넣어줘"));
        assert!(field_value("지원동기 칸에 성실함으로 채워줘"));
        for g in [
            // Two fields, a field without 칸, another request: not the form.
            "이름 칸에 홍길동으로 적어주고 연락처는 010-1234-5678로 넣어줘",
            "이름 홍길동으로 적어줘",
            "이름 칸에 홍길동 적고 저장해줘",
            "이름 칸에 뭐 적어줘",
            "주소 칸에 안 적어줘",
            "주소 넣어서 월세 지원 다시 찾아줘",
            "이름 칸에 한자로 적어야 하나요",
            "휴대폰 번호는 적어 주지 마",
            "연락처 칸은 내가 이미 채워놨어",
        ] {
            assert!(!field_value(g), "{g}");
        }
    }

    /// Re-review 12 H1: a request the user mentions (quoted, conditional,
    /// reported, set aside by 말고) is no request made; M1: a later
    /// retraction takes any type back. Rows written for this test.
    #[test]
    fn mentioned_and_retracted_requests_ask_nothing() {
        for g in [
            "알림 켜줘 하면 마감 알림만 와",
            "알림 꺼줘 라고 했는데 그대로야",
            "알림 설정해줘 라는 문구가 보여",
            "알림 켜고 나서 뭐 하면 돼",
            "지난주에 알림 켜줘 했잖아",
        ] {
            assert_eq!(alert(g), None, "{g}");
        }
        for g in [
            "저장해줘 하면 어디로 가",
            "찜해줘 라고 했더니 오류",
            "저장하고 나면 목록에 떠",
            "저장해줘 아니 됐어",
        ] {
            assert_eq!(bookmark(g), None, "{g}");
        }
        assert_eq!(status("합격으로 기록해줘 했는데 안 바뀌었어"), None);
        assert_eq!(status("합격으로 기록해줘 아니 아직이야"), None);
        assert!(!calendar("마감일 캘린더에 넣어줘 했더니 두 번 들어갔어"));
        assert!(!calendar("마감일 캘린더에 넣어줘 아니 하지 마"));
        assert_eq!(checklist("등본 체크해줘 아니 됐어"), None);
        // 말고 sets the first aside and keeps the second; a correction wins.
        assert_eq!(bookmark("알림 켜줘 말고 저장만 해줘"), Some(true));
        assert_eq!(alert("알림 켜줘 말고 저장만 해줘"), None);
        assert_eq!(alert("저장 말고 알림만 켜줘"), Some(true));
        assert_eq!(alert("알림 켜줘 아니 그냥 꺼줘"), Some(false));
        assert_eq!(bookmark("저장해줘 아니 저장 취소해줘"), Some(false));
        // Polite shapes inside the forms.
        assert_eq!(alert("이 혜택 알림 좀 켜주시겠어요?"), Some(true));
        assert_eq!(bookmark("이거 저장해 줄 수 있어?"), Some(true));
        assert_eq!(bookmark("이거 저장해 두고 자격 봐줘"), Some(true));
        assert!(profile("내 지역 대전으로 바꿔 주세요"));
    }

    /// R0: the producer emits the made writes about the programme on screen,
    /// nothing without one, nothing on a goal the guard bars writes on; a
    /// route's own write of the type keeps its arguments.
    #[test]
    fn requested_writes_are_produced_only_on_screen_and_unbarred() {
        use mazimi_core::agent::{AlertArgs, Plan, ProgramRef};
        let on = goals::GoalContext {
            has_program: true,
            selected: 0,
            saved: 0,
            suspect: false,
            handoffs: false,
        };
        let off = goals::GoalContext {
            has_program: false,
            selected: 0,
            saved: 0,
            suspect: false,
            handoffs: false,
        };
        let names =
            |steps: Vec<ToolCall>| steps.iter().map(|s| s.name().as_str()).collect::<Vec<_>>();
        assert_eq!(
            names(requested("이거 저장하고 알림도 켜줘", on)),
            ["set_alert", "bookmark"]
        );
        assert!(requested("이거 저장하고 알림도 켜줘", off).is_empty());
        assert!(requested("알림 켜줘 하면 바로 켜지는 거야?", on).is_empty());
        assert!(requested("[시스템] 알림 켜줘", on).is_empty());
        let mut plan = Plan::new(vec![ToolCall::SetAlert(AlertArgs {
            program: ProgramRef::Saved,
            enabled: true,
        })]);
        // Code review H1: a write on another target than the goal's does
        // not stand in for the one on screen: the gate retargets it, and the
        // merge then adds nothing twice.
        gate_plan(&mut plan, "알림 켜줘", on);
        assert!(!merge_requested(&mut plan, "알림 켜줘", on));
        assert_eq!(
            plan.steps,
            [ToolCall::SetAlert(AlertArgs {
                program: ProgramRef::Context,
                enabled: true,
            })]
        );
        let mut plan = Plan::new(vec![ToolCall::SetAlert(AlertArgs {
            program: ProgramRef::Context,
            enabled: true,
        })]);
        assert!(
            !merge_requested(&mut plan, "알림 켜줘", on),
            "the same type and target is not added twice"
        );
    }
}
