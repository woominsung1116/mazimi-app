//! FROZEN DESIGN, FEATURE OFF. Three review rounds each found a new family
//! of answers this guard marks valid. Known open classes:
//! - fragment passages are quotable: the passage builder cuts long
//!   sentences into clauses, so a consequent without its condition, or a
//!   clause without its exception, is a "whole source sentence" here;
//! - noun-final lines and list items (a bullet under a heading such as
//!   "[제외 대상]") read as standalone statements;
//! - a true but misleading quote (a notice sentence about another group).
//!
//! The generated summary (`ASSISTANT_AI_ENABLED`, default off) must not be
//! enabled until the passage builder marks whole sentences with their
//! heading, or the summary becomes a selection of passage ids the server
//! renders. Do not extend this file with new cases.
//!
//! Grounding check for a generated answer, default-deny: the answer is
//! accepted only when every one of its sentences is a whole source
//! sentence (a retrieved passage, the verdict summary or the not-found
//! sentence), and, when the question has a verdict, the verdict's own
//! sentences are among them. Nothing is detected as a "claim": a sentence
//! the sources do not state is rejected whatever it says, so no list of
//! eligibility words, polarity words, units or names stands between a
//! reworded answer and the user. A violation replaces the text with
//! [`FALLBACK_TEXT`] while the quotes stay.
//!
//! One normalisation for both sides: NFKC; then whitespace is ignored and
//! trailing punctuation dropped. Every other character counts, and one
//! outside the plain set (Hangul syllables, ASCII letters and digits, a
//! closed punctuation set) must occur in a source, else the answer fails
//! closed. The one allowed difference is the polite ending of the last
//! predicate ("아닙니다" / "아니에요"), read by Hangul decomposition so the
//! stem, and with it the polarity, must be equal ([`ending_key`]). A
//! sentence-initial attribution of the notice ("공고에 따르면") may be left
//! out. A source question or a verbless line (a heading, a bullet) is no
//! quotable sentence.
use unicode_normalization::UnicodeNormalization;

pub const FALLBACK_TEXT: &str = "근거 문장을 확인해 주세요";
/// Answer characters beyond the verdict's own sentences.
const MAX_CHARS: usize = 400;
/// Answer sentences beyond the verdict's own.
const MAX_SENTENCES: usize = 4;
/// The sentence the model writes when the passages do not answer
/// (the answer prompt names it); a server sentence, always quotable.
pub const NOT_FOUND_SENTENCE: &str = "공고 문장에서 확인되지 않아요.";
/// Punctuation a sentence may carry without a source having it.
const PUNCTUATION: &str = ".,!?~-:;()'\"‘’“”·/%…[]";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Violation {
    Empty,
    TooLong,
    Url,
    HiddenCharacter,
    /// An answer sentence is no whole source sentence.
    NotQuoted,
    /// The question has a verdict and the answer leaves its sentences out.
    MissingVerdict,
}

pub struct Grounding<'a> {
    /// Passages and the programme title.
    pub sources: Vec<&'a str>,
    /// The verdict summary shown beside the answer, when the question has
    /// a verdict: its sentences must be in the answer, in order.
    pub verdict: Option<&'a str>,
}

fn plain(c: char) -> bool {
    ('\u{AC00}'..='\u{D7A3}').contains(&c)
        || c.is_ascii_alphanumeric()
        || c == ' '
        || c == '\n'
        || PUNCTUATION.contains(c)
}

/// The sentences of `text`, split before normalisation by the feature's one
/// boundary rule ([`super::text::ends_sentence`]) and at newlines, each then
/// NFKC-normalised, with whether it ends with a question mark. Commas,
/// decimals and dates never split.
fn sentences(text: &str) -> Vec<(String, bool)> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut current = String::new();
    let mut push = |current: &mut String, question: bool| {
        let normal: String = current.trim().nfkc().collect();
        if !normal.trim().is_empty() {
            out.push((normal.trim().to_owned(), question));
        }
        current.clear();
    };
    for (i, &c) in chars.iter().enumerate() {
        if c == '\n' {
            push(&mut current, false);
        } else if super::text::ends_sentence(&chars, i) {
            push(&mut current, c == '?');
        } else {
            current.push(c);
        }
    }
    push(&mut current, false);
    out
}

/// Hangul syllable parts (lead, vowel, tail index; tail 0 = none).
fn parts(c: char) -> Option<(u32, u32, u32)> {
    let code = (c as u32).checked_sub(0xAC00).filter(|n| *n < 11_172)?;
    Some((code / 588, (code % 588) / 28, code % 28))
}

fn syllable(lead: u32, vowel: u32, tail: u32) -> char {
    char::from_u32(0xAC00 + lead * 588 + vowel * 28 + tail).unwrap_or('?')
}

/// The tail index of ㅂ.
const TAIL_B: u32 = 17;
/// Lead index of ㅎ; vowel indices of ㅏ, ㅐ, ㅙ, ㅚ.
const L_H: u32 = 18;
const V_A: u32 = 0;
const V_AE: u32 = 1;
const V_WAE: u32 = 10;
const V_OE: u32 = 11;

/// A sentence body with its final polite ending reduced to the stem, by
/// Hangul decomposition: the formal "-ㅂ니다/-습니다" and the polite
/// "-어요/-아요/-에요/-예요" (and the contracted 해요 = 하+여요, 돼요 = 되+어요)
/// of one stem give the same key. Only ending syllables are touched, never
/// a syllable of the stem, so "대상이" and "대상이 아니" stay apart. Any other
/// ending is kept as written: then only the same ending matches.
fn ending_key(body: &str) -> String {
    let mut c: Vec<char> = body.chars().filter(|c| !c.is_whitespace()).collect();
    let ends = |c: &[char], s: &str| c.ends_with(&s.chars().collect::<Vec<_>>());
    let cut = |c: &mut Vec<char>, n: usize| {
        let keep = c.len().saturating_sub(n);
        c.truncate(keep);
    };
    if ends(&c, "습니다") && c.len() > 3 {
        cut(&mut c, 3);
    } else if ends(&c, "니다") && c.len() > 2 {
        // -ㅂ니다: the stem's last syllable carries ㅂ as its tail.
        let at = c.len() - 3;
        if let Some((lead, vowel, TAIL_B)) = parts(c[at]) {
            cut(&mut c, 2);
            c[at] = syllable(lead, vowel, 0);
        }
    } else if ends(&c, "예요") && c.len() > 2 {
        // 학교예요 = 학교 + 이에요.
        cut(&mut c, 2);
        c.push('이');
    } else if ends(&c, "에요") && c.len() > 2 {
        cut(&mut c, 2);
    } else if ends(&c, "어요") || ends(&c, "아요") {
        if c.len() > 2 {
            cut(&mut c, 2);
        }
    } else if ends(&c, "요") && c.len() > 1 {
        // A contracted stem + 어: 해요 (하+여), 돼요 (되+어), 가요, 와요 …
        let at = c.len() - 2;
        if let Some((lead, vowel, 0)) = parts(c[at]) {
            let stem = match vowel {
                V_AE if lead == L_H => Some(syllable(L_H, V_A, 0)), // 해 → 하
                V_WAE => Some(syllable(lead, V_OE, 0)),             // 돼 → 되
                _ => None,
            };
            if let Some(stem) = stem {
                cut(&mut c, 2);
                c.push(stem);
            }
        }
    }
    c.into_iter().collect()
}

/// Nouns a sentence-initial attribution names ("공고에 따르면").
const SOURCE_NOUNS: &[&str] = &[
    "공고",
    "공고문",
    "안내",
    "안내문",
    "원문",
    "모집공고",
    "모집요강",
];

/// `sentence` without a sentence-initial attribution of the notice: a
/// [`SOURCE_NOUNS`] noun, a particle (에/에서/을/를), then 따르면/의하면/보면,
/// and an optional comma. Anywhere else nothing is cut.
fn without_attribution(sentence: &str) -> &str {
    for noun in SOURCE_NOUNS {
        let Some(rest) = sentence.strip_prefix(noun) else {
            continue;
        };
        for particle in ["에서", "에", "을", "를"] {
            let Some(rest) = rest.strip_prefix(particle) else {
                continue;
            };
            let rest = rest.trim_start();
            for marker in ["따르면", "의하면", "보면"] {
                if let Some(rest) = rest.strip_prefix(marker) {
                    return rest.trim_start_matches([',', ' ']);
                }
            }
        }
    }
    sentence
}

/// The comparison key of a sentence: trailing punctuation dropped, the
/// polite ending reduced ([`ending_key`]).
fn key(sentence: &str) -> String {
    ending_key(sentence.trim_end_matches(|c: char| PUNCTUATION.contains(c) || c.is_whitespace()))
}

/// Endings that close a statement; a source line without one is a heading
/// or a bullet, not a quotable sentence.
const STATEMENT_ENDS: [char; 6] = ['다', '요', '함', '됨', '음', '임'];
/// Endings that ask.
const QUESTION_ENDS: &[&str] = &["까", "까요", "나요", "가요", "는지", "는지요", "는가"];

fn quotable(sentence: &str, question: bool) -> bool {
    let body = sentence.trim_end_matches(|c: char| PUNCTUATION.contains(c) || c.is_whitespace());
    !question && body.ends_with(STATEMENT_ENDS) && !QUESTION_ENDS.iter().any(|q| body.ends_with(q))
}

fn has_url(lower: &str) -> bool {
    if lower.contains("http") || lower.contains("www.") || lower.contains('@') {
        return true;
    }
    let chars: Vec<char> = lower.chars().collect();
    chars
        .windows(3)
        .any(|w| w[0].is_ascii_alphanumeric() && w[1] == '.' && w[2].is_ascii_alphabetic())
}

pub fn validate(answer: &str, grounding: &Grounding) -> Result<(), Violation> {
    let raw = answer;
    let answer: String = answer.nfkc().collect();
    let sources: Vec<String> = grounding
        .sources
        .iter()
        .map(|s| s.nfkc().collect())
        .collect();
    let verdict: Option<String> = grounding.verdict.map(|v| v.nfkc().collect());
    if answer.trim().is_empty() {
        return Err(Violation::Empty);
    }
    // A character outside the plain set must be a source's; the sentence
    // rule below then requires it in the very sentence quoted.
    let known = |c: char| sources.iter().chain(verdict.iter()).any(|s| s.contains(c));
    if answer
        .chars()
        .any(|c| !plain(c) && !c.is_whitespace() && !known(c))
        || answer
            .chars()
            .any(|c| c.is_whitespace() && c != ' ' && c != '\n' && !known(c))
    {
        return Err(Violation::HiddenCharacter);
    }
    // Links stay out even when a passage has one (the client links the
    // official page itself).
    if has_url(&answer.to_lowercase()) {
        return Err(Violation::Url);
    }
    let quotable_keys: Vec<String> = grounding
        .sources
        .iter()
        .chain(grounding.verdict.iter())
        .flat_map(|s| sentences(s))
        .filter(|(s, question)| quotable(s, *question))
        .map(|(s, _)| key(&s))
        .chain(std::iter::once(key(NOT_FOUND_SENTENCE)))
        .collect();
    let answer_keys: Vec<String> = sentences(raw)
        .into_iter()
        .map(|(s, _)| key(without_attribution(&s)))
        .collect();
    // The verdict's sentences, in order, before anything else is counted.
    let verdict_keys: Vec<String> = grounding
        .verdict
        .iter()
        .flat_map(|v| sentences(v))
        .map(|(s, _)| key(&s))
        .collect();
    if !verdict_keys.is_empty()
        && !answer_keys
            .windows(verdict_keys.len())
            .any(|w| w == verdict_keys.as_slice())
    {
        return Err(Violation::MissingVerdict);
    }
    let verdict_chars = verdict.as_ref().map_or(0, |v| v.chars().count());
    if answer.chars().count() > MAX_CHARS + verdict_chars
        || answer_keys.len() > MAX_SENTENCES + verdict_keys.len()
    {
        return Err(Violation::TooLong);
    }
    if answer_keys.iter().any(|k| !quotable_keys.contains(k)) {
        return Err(Violation::NotQuoted);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(answer: &str, sources: &[&str], verdict: Option<&str>) -> bool {
        validate(
            answer,
            &Grounding {
                sources: sources.to_vec(),
                verdict,
            },
        )
        .is_ok()
    }

    #[test]
    fn limits_and_links_hold_for_quoted_sentences() {
        let long = "외국인은 신청할 수 없습니다. ".repeat(6);
        assert_eq!(
            validate(
                &long,
                &Grounding {
                    sources: vec!["외국인은 신청할 수 없습니다."],
                    verdict: None
                }
            ),
            Err(Violation::TooLong),
            "quoted sentences, but too many"
        );
        assert_eq!(
            validate(
                "",
                &Grounding {
                    sources: vec![],
                    verdict: None
                }
            ),
            Err(Violation::Empty)
        );
        assert!(check(NOT_FOUND_SENTENCE, &[], None));
    }

    // ---- Round 3: every row of the round-1 and round-2 reviews. ----
    // `check(answer, sources, verdict summary)`; `true` = accepted.

    const P_AGE: &str = "부산 거주 만 19~34세 청년";
    const P_MONTH: &str = "1인당 월 최대 20만원을 12개월간 지원합니다.";
    const P_DEPOSIT: [&str; 2] = ["월 20만원 지원", "보증금 500만원"];

    /// Rejected under every verdict summary (and none).
    #[test]
    fn review_rows_that_must_be_rejected() {
        let v_fail_age = "저장한 나이 정보가 공고에 등록된 나이 조건과 맞지 않아요.";
        let v_pass_age = "저장한 나이 정보가 공고에 등록된 나이 조건과 맞아요. 최종 자격은 기관 확인이 필요해요.";
        let v_fail_enrol = "질문한 ‘휴학생’은 공고에 등록된 학적 조건에 포함되지 않아요.";
        let v_unknown_income =
            "공고에 소득 조건이 등록돼 있지만 저장한 정보로는 확정하지 못했어요.";
        let v_fail_region = "질문한 ‘부산’은 공고에 등록된 대상 지역이 아니에요.";
        let rows: Vec<(&str, Vec<&str>, Option<&str>)> = vec![
            // Round 1, security H1 / code H-1.
            (
                "휴학생은 지원 대상이에요.",
                vec!["휴학생은 지원 대상이 아닙니다."],
                None,
            ),
            (
                "부산 거주 청년이 지원 대상이 아니에요.",
                vec!["부산 거주 청년이 지원 대상입니다."],
                None,
            ),
            (
                "부산은 공고에 등록된 대상 지역이에요.",
                vec![],
                Some(v_fail_region),
            ),
            (
                "휴학생도 온라인 신청 가능해요.",
                vec!["재학생은 온라인 신청 가능합니다."],
                None,
            ),
            (
                "휴학생은 지원 대상이에요.",
                vec!["휴학생은 지원 대상에서 제외됩니다."],
                None,
            ),
            (
                "휴학생은 해당돼요.",
                vec!["휴학생은 해당되지 않습니다."],
                None,
            ),
            (
                "회원님은 소득 기준을 충족해요.",
                vec!["소득 기준을 충족하지 못하면 선발에서 제외됩니다."],
                None,
            ),
            ("회원님도 받을 순 있어요.", vec![], None),
            ("회원님은 받지 못해요.", vec![], None),
            ("회원님도 신청하면 돼요.", vec![], None),
            ("회원님은 이 혜택의 대상이에요.", vec![], None),
            ("네, 신청 가능합니다.", vec![], None),
            // Round 1, M2 (amounts).
            ("달마다 500만원을 지원해요.", P_DEPOSIT.to_vec(), None),
            (
                "월 기준으로 계산해 보면 500만원이에요.",
                P_DEPOSIT.to_vec(),
                None,
            ),
            ("500만원을 월 단위로 지원해요.", P_DEPOSIT.to_vec(), None),
            ("월 500만원을 지원해요.", P_DEPOSIT.to_vec(), None),
            // Round 2, security H1 (a): the server's own sentences flipped.
            (
                "저장한 나이 정보가 공고에 등록된 나이 조건과 맞아요.",
                vec![],
                Some(v_fail_age),
            ),
            (
                "저장한 나이 정보가 공고에 등록된 나이 조건과 맞지 않아요.",
                vec![],
                Some(v_pass_age),
            ),
            (
                "질문한 휴학생은 공고에 등록된 학적 조건에 포함돼요.",
                vec![],
                Some(v_fail_enrol),
            ),
            ("소득 조건도 문제없어요.", vec![], Some(v_unknown_income)),
            // (b) claims with no listed stem.
            ("회원님도 지원하실 수 있어요.", vec![], None),
            ("회원님도 돼요.", vec![], None),
            ("회원님은 안 돼요.", vec![], None),
            ("회원님도 받으실 거예요.", vec![], None),
            ("회원님도 받아요.", vec![], None),
            ("회원님도 포함돼요.", vec![], None),
            ("회원님은 조건을 만족해요.", vec![], None),
            ("회원님은 기준에 부합해요.", vec![], None),
            ("회원님은 탈락이에요.", vec![], None),
            ("회원님도 이용할 수 있어요.", vec![], None),
            ("나이는 안 맞지만 회원님도 돼요.", vec![], None),
            (
                "휴학생은 제외 안 됩니다.",
                vec!["휴학생은 제외됩니다."],
                None,
            ),
            (
                "졸업생도 선발합니다.",
                vec!["졸업생은 선발하지 않습니다."],
                None,
            ),
            // (c) attribution cut.
            (
                "휴학생은 지원 대상이라고 보면 돼요.",
                vec!["휴학생은 지원 대상이 아닙니다."],
                None,
            ),
            (
                "휴학생은 지원 대상이니 서류를 살펴보면 좋아요.",
                vec!["휴학생은 지원 대상이 아닙니다."],
                None,
            ),
            (
                "부산 거주 청년이 지원 대상이 아니라고 보면 됩니다.",
                vec!["부산 거주 청년이 지원 대상입니다."],
                None,
            ),
            (
                "부산은 공고에 등록된 대상 지역이라고 보면 돼요.",
                vec![],
                Some(v_fail_region),
            ),
            (
                "휴학생도 온라인 신청 가능하다고 보면 돼요.",
                vec!["재학생은 온라인 신청 가능합니다."],
                None,
            ),
            // (e) clause boundaries.
            (
                "회원님도, 청년 지원 대상입니다.",
                vec!["청년 지원 대상"],
                None,
            ),
            (
                "휴학생은 지원 대상이 아닙니다, 라는 말은 틀려요.",
                vec!["휴학생은 지원 대상이 아닙니다."],
                None,
            ),
            (
                "휴학생은 지원 대상이 아닙니다, 하지만 회원님은 괜찮아요.",
                vec!["휴학생은 지원 대상이 아닙니다."],
                None,
            ),
            (
                "부산 거주 청년이 지원 대상입니다, 그런데 회원님은 아니에요.",
                vec!["부산 거주 청년이 지원 대상입니다."],
                None,
            ),
            (
                "회원님처럼 부산에 사시면, 부산 거주 청년이 지원 대상이에요.",
                vec!["부산 거주 청년이 지원 대상입니다."],
                None,
            ),
            (
                "청년은 지원 대상이에요.",
                vec!["다음 각 호에 해당하지 않는 청년은 지원 대상입니다."],
                None,
            ),
            (
                "휴학생도 신청 가능해요.",
                vec!["Q. 휴학생도 신청 가능? A. 아니요, 재학생만 됩니다."],
                None,
            ),
            // Round 2, code H-1: a conditional's consequent.
            (
                "신청할 수 있어요.",
                vec!["만 19~34세 청년이면, 신청할 수 있습니다."],
                None,
            ),
            (
                "지원 대상이에요.",
                vec!["소득 기준을 충족하는 경우, 지원 대상입니다."],
                None,
            ),
            (
                "타 지역 거주자도 신청할 수 있어요.",
                vec!["부모가 부산에 거주하는 경우에만 타 지역 거주자도 신청할 수 있습니다."],
                None,
            ),
            (
                "휴학생은 지원 대상이에요.",
                vec!["졸업생과 달리 군 복무로 인한 휴학생은 지원 대상입니다."],
                None,
            ),
            (
                "신청할 수 없어요.",
                vec!["부산 거주자가 아닌 경우, 신청할 수 없습니다."],
                None,
            ),
            // Code M-1, M-2.
            (
                "휴학생도 받을 수 있다고 보면 돼요.",
                vec!["휴학생은 지원 대상에서 제외됩니다."],
                None,
            ),
            (
                "휴학생도 지원 대상이라고 보면 됩니다.",
                vec!["휴학생은 지원 대상에서 제외됩니다."],
                None,
            ),
            (
                "회원님은 받을 수 없다고 보면 돼요.",
                vec!["휴학생은 지원 대상에서 제외됩니다."],
                None,
            ),
            (
                "회원님도 신청하시면 돼요.",
                vec!["휴학생은 제외됩니다."],
                None,
            ),
            (
                "회원님도 받으실 순 있어요.",
                vec!["휴학생은 제외됩니다."],
                None,
            ),
            (
                "회원님도 참여할 수 있어요.",
                vec!["휴학생은 제외됩니다."],
                None,
            ),
            (
                "회원님은 지원하셔도 됩니다.",
                vec!["휴학생은 제외됩니다."],
                None,
            ),
            ("휴학생도 포함돼요.", vec!["휴학생은 제외됩니다."], None),
            ("네, 됩니다.", vec!["휴학생은 제외됩니다."], None),
            (
                "회원님은 조건을 만족하지 못해요.",
                vec!["휴학생은 제외됩니다."],
                None,
            ),
            // Round 2, security M1: letters and blanks outside the allowlist.
            ("회원님은 이 혜택의 대\u{02BC}상이에요.", vec![], None),
            ("대\u{0345}상이에요.", vec![], None),
            ("대\u{0640}상이에요.", vec![], None),
            ("대\u{30FC}상이에요.", vec![], None),
            ("대상\u{0456}이에요.", vec![], None),
            ("무\u{2800}조건 받아요.", vec![], None),
            ("내\u{0001}일까지 신청하세요.", vec![], None),
            ("20만원 이\u{2800}상 지원해요.", vec![], None),
            ("월\u{007F} 지원해요.", vec![], None),
            ("월\u{FFF9} 지원해요.", vec![], None),
            ("월\u{1D173} 지원해요.", vec![], None),
            ("월\u{FFFC} 지원해요.", vec![], None),
            ("월\u{0301} 지원해요.", vec![], None),
            ("월\u{302E} 지원해요.", vec![], None),
            ("월\u{00B7} 지원해요.", vec![], None),
            ("월 최대 三十万원을 지원합니다.", vec![P_MONTH], None),
            ("월 최대 20億원을 지원합니다.", vec![P_MONTH], None),
            ("월 최대 20萬원을 지원합니다.", vec![P_MONTH], None),
            ("월 최대 20조원을 지원합니다.", vec![P_MONTH], None),
            // Round 1 M1 (Unicode).
            ("신청 가\u{3164}능합니다.", vec![], None),
            ("무\u{200B}조건 받아요.", vec![], None),
            // Round 2, security M2: amounts rebound.
            ("연 최대 20만원을 드려요.", vec![P_MONTH], None),
            ("한 해에 최대 20만원을 드려요.", vec![P_MONTH], None),
            ("총 최대 20만원을 드려요.", vec![P_MONTH], None),
            ("하루 최대 20만원을 드려요.", vec![P_MONTH], None),
            ("가구당 월 최대 20만원을 드려요.", vec![P_MONTH], None),
            ("최대 20만원을 빌려줘요.", vec![P_MONTH], None),
            ("최대 20만원을 내야 해요.", vec![P_MONTH], None),
            ("본인 부담 최대 20만원이에요.", vec![P_MONTH], None),
            ("최대 20만원 넘게 드려요.", vec![P_MONTH], None),
            ("달마다 보증금 500만원을 드려요.", P_DEPOSIT.to_vec(), None),
            ("보증금 500만원씩 다달이 드려요.", P_DEPOSIT.to_vec(), None),
            ("월 20만원 넘게 지원해요.", P_DEPOSIT.to_vec(), None),
            // Round 2, security M3: numbers rebound to another unit.
            ("신청은 19일까지예요.", vec![P_MONTH, P_AGE], None),
            ("12월 20일에 마감돼요.", vec![P_MONTH, P_AGE], None),
            ("20일까지 신청하세요.", vec![P_MONTH, P_AGE], None),
            ("34일 안에 신청하세요.", vec![P_MONTH, P_AGE], None),
            ("20년 거주해야 해요.", vec![P_MONTH, P_AGE], None),
            ("삼 개월 동안 드려요.", vec![P_MONTH], None),
            ("두 번 드려요.", vec![P_MONTH], None),
            ("월 만원을 드려요.", vec![P_MONTH], None),
            ("월세 전액을 드려요.", vec![P_MONTH], None),
            // Round 2, code M-3: paraphrases of real passages (fallback accepted).
            (
                "등록된 지원 금액은 월 200,000원이에요.",
                vec!["등록된 지원 금액: 월 200,000원"],
                None,
            ),
            ("월 최대 20만원이 지원되는 사업이에요.", vec![P_MONTH], None),
            (
                "부산에 사는 청년에게 월세를 지원하고 있어요.",
                vec![P_MONTH],
                None,
            ),
            // Round 1 originals: paraphrase, numbers, names, URLs.
            (
                "부산에 사는 만 19~34세 청년은 월세를 한 달에 최대 20만원까지 지원받는 사업이에요.",
                vec![P_MONTH, P_AGE],
                None,
            ),
            ("월 최대 30만원을 지원해요.", vec![P_MONTH], None),
            ("희망사다리장학금도 함께 신청하세요.", vec![P_MONTH], None),
            (
                "https://young.busan.go.kr 에서 신청하세요.",
                vec![P_MONTH],
                None,
            ),
            (
                "young.busan.go.kr 에서 신청합니다.",
                vec!["young.busan.go.kr 에서 신청합니다."],
                None,
            ),
            ("무조건 받을 수 있어요.", vec![], None),
            // A source question or heading is not a quotable sentence.
            ("청년 지원 대상이에요.", vec!["청년 지원 대상"], None),
            ("신청 가능한가요.", vec!["신청 가능한가요?"], None),
        ];
        for (answer, sources, verdict) in rows {
            assert!(
                !check(answer, &sources, verdict),
                "accepted: {answer} | {sources:?} | {verdict:?}"
            );
        }
    }

    /// Accepted: whole source sentences, a polite ending changed without
    /// changing the stem, a sentence-initial attribution; with the verdict's
    /// own sentence when there is a verdict.
    #[test]
    fn whole_source_sentences_are_accepted() {
        let verdict = "아직 확인할 조건이 있어요.";
        let rows: Vec<(&str, Vec<&str>, Option<&str>)> = vec![
            (
                "휴학생은 지원 대상이 아니에요.",
                vec!["휴학생은 지원 대상이 아닙니다."],
                None,
            ),
            (
                "부산 거주 청년이 지원 대상이에요.",
                vec!["부산 거주 청년이 지원 대상입니다."],
                None,
            ),
            (
                "외국인은 신청할 수 없어요.",
                vec!["외국인은 신청할 수 없습니다."],
                None,
            ),
            (
                "공고에 따르면 외국인은 신청할 수 없어요.",
                vec!["외국인은 신청할 수 없습니다."],
                None,
            ),
            (
                "공고에 따르면, 외국인은 신청할 수 없습니다.",
                vec!["외국인은 신청할 수 없습니다."],
                None,
            ),
            (
                "1인당 월 최대 20만원을 12개월간 지원해요.",
                vec![P_MONTH],
                None,
            ),
            (
                "1인당 월 최대 20만원을 12개월간 지원합니다.",
                vec![P_MONTH],
                None,
            ),
            (
                "휴학생은 지원 대상에서 제외돼요.",
                vec!["휴학생은 지원 대상에서 제외됩니다."],
                None,
            ),
            (
                "아직 확인할 조건이 있어요. 외국인은 신청할 수 없어요.",
                vec!["외국인은 신청할 수 없습니다."],
                Some(verdict),
            ),
            (
                "외국인은 신청할 수 없어요. 아직 확인할 조건이 있어요.",
                vec!["외국인은 신청할 수 없습니다."],
                Some(verdict),
            ),
        ];
        for (answer, sources, verdict) in rows {
            assert!(
                check(answer, &sources, verdict),
                "rejected: {answer} | {sources:?}"
            );
        }
    }

    /// The verdict layer: with a verdict, its own sentence must be in the
    /// answer; a true notice quote alone is not enough.
    #[test]
    fn a_verdict_sentence_is_never_left_out() {
        let fail = "현재 저장된 정보와 맞지 않는 조건이 있어요. 신청 전에 확인해 주세요.";
        let source = vec!["부산 거주 청년이 지원 대상입니다."];
        assert!(!check(
            "부산 거주 청년이 지원 대상이에요.",
            &source,
            Some(fail)
        ));
        assert!(check(
            "현재 저장된 정보와 맞지 않는 조건이 있어요. 신청 전에 확인해 주세요. 부산 거주 청년이 지원 대상이에요.",
            &source,
            Some(fail)
        ));
        // Only part of the verdict is not the verdict.
        assert!(!check(
            "신청 전에 확인해 주세요. 부산 거주 청년이 지원 대상이에요.",
            &source,
            Some(fail)
        ));
    }

    /// The polite-ending equivalence keeps the stem whole: it can never
    /// turn an affirmation into a denial or back.
    #[test]
    fn a_polite_ending_never_flips_polarity() {
        for (a, b) in [
            ("대상이 아닙니다", "대상이 아니에요"),
            ("대상입니다", "대상이에요"),
            ("있습니다", "있어요"),
            ("가능합니다", "가능해요"),
            ("제외됩니다", "제외돼요"),
            ("학교입니다", "학교예요"),
        ] {
            assert_eq!(ending_key(a), ending_key(b), "{a} ~ {b}");
        }
        for (a, b) in [
            ("대상입니다", "대상이 아니에요"),
            ("대상이 아닙니다", "대상이에요"),
            ("있습니다", "없어요"),
            ("됩니다", "안 돼요"),
            ("합니다", "하지 않아요"),
            ("가능합니다", "불가능해요"),
            ("받을 수 있습니다", "받을 수 없어요"),
        ] {
            assert_ne!(ending_key(a), ending_key(b), "{a} !~ {b}");
        }
    }

    /// Round 4 (security H2): one sentence boundary for passages and the
    /// guard; a decimal's tail is no sentence, and the decimal sentence
    /// itself is quotable.
    #[test]
    fn a_number_never_splits_a_sentence() {
        let rows: [(&str, &str); 8] = [
            (
                "청년 전월세 대출이자를 연 1.5% 지원합니다.",
                "5% 지원합니다.",
            ),
            ("교통비로 월 3.5만원을 지급합니다.", "5만원을 지급합니다."),
            (
                "보증금은 최대 1.5억원까지 대출됩니다.",
                "5억원까지 대출돼요.",
            ),
            (
                "직전 학기 평점 3.0 이상인 재학생은 신청할 수 있습니다.",
                "0 이상인 재학생은 신청할 수 있어요.",
            ),
            (
                "중위소득 150.5% 이하인 가구는 제외됩니다.",
                "5% 이하인 가구는 제외돼요.",
            ),
            (
                "2026.12.31.까지 신청할 수 있습니다.",
                "까지 신청할 수 있습니다.",
            ),
            (
                "재학생은 지원 대상입니다.(단, 휴학생과 졸업유예자는 제외)",
                "재학생은 지원 대상이에요.",
            ),
            (
                "휴학생도 신청할 수 있다…고 생각하면 안 됩니다.",
                "휴학생도 신청할 수 있다.",
            ),
        ];
        for (source, tail) in rows {
            assert!(!check(tail, &[source], None), "accepted: {tail} | {source}");
        }
        for source in [
            "청년 전월세 대출이자를 연 1.5% 지원합니다.",
            "2026.12.31.까지 신청할 수 있습니다.",
        ] {
            assert!(check(source, &[source], None), "rejected: {source}");
        }
    }
}
