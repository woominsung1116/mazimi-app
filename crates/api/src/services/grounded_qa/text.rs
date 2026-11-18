//! Korean-aware normalisation for grounded retrieval: whitespace-free
//! matching (spacing in notices is inconsistent: `주민등록 등본` /
//! `주민등록등본`), one-step particle and ending stripping, small synonym
//! sets, and character bigrams as a secondary ranking signal.

/// Particles and endings stripped from the end of a question word, longest
/// first. One strip per word; the stem must keep at least two characters so
/// `참가` never becomes `참`.
const ENDINGS: &[&str] = &[
    "했는데요",
    "하려면요",
    "이라도",
    "에서는",
    "에게는",
    "으로는",
    "에서도",
    "까지는",
    "부터는",
    "인가요",
    "이에요",
    "했는데",
    "하는데",
    "하려면",
    "하나요",
    "되나요",
    "인데요",
    "이면요",
    "에서",
    "에게",
    "한테",
    "으로",
    "까지",
    "부터",
    "이나",
    "이랑",
    "처럼",
    "보다",
    "마다",
    "라도",
    "하고",
    "는요",
    "은요",
    "도요",
    "이요",
    "인데",
    "인지",
    "이면",
    "이야",
    "예요",
    "에는",
    "에도",
    "했어",
    "하면",
    "할까",
    "하나",
    "해야",
    "해요",
    "해도",
    "되나",
    "돼요",
    "되면",
    "은",
    "는",
    "이",
    "가",
    "을",
    "를",
    "에",
    "의",
    "와",
    "과",
    "로",
    "도",
    "만",
    "랑",
    "요",
    "야",
    "면",
    "해",
    "돼",
];

/// Question words that carry no content after stripping.
const STOPWORDS: &[&str] = &[
    "이거",
    "그거",
    "저거",
    "이것",
    "그것",
    "거",
    "것",
    "건",
    "게",
    "나",
    "저",
    "내",
    "제",
    "너",
    "우리",
    "이번",
    "그럼",
    "그러면",
    "그리고",
    "또",
    "좀",
    "혹시",
    "정말",
    "진짜",
    "제도",
    "사업",
    "가능",
    "필요",
    "경우",
    "때",
    "수",
    "등",
    "및",
    "네",
    "예",
    "아니",
    "혹은",
    "관련",
    "대해",
    "대한",
    "정도",
    "같은",
    "같이",
    "다",
    "모두",
    "전부",
    "그냥",
    "나도",
    "저도",
    "이혜택",
    "그혜택",
    "이건",
    "그건",
    "이게",
    "뭐",
    "뭘",
    "뭐야",
    "뭔가요",
    "뭐예요",
    "뭐에요",
    "무엇",
    "어때",
    "어떻게",
    "알려줘",
    "알려주세요",
    "궁금해",
    "궁금",
    "말해줘",
    "해줘",
    "주세요",
    "있어",
    "있나",
    "있나요",
    "있어요",
    "없어",
    "없나",
    "되",
    "됨",
    "하",
    "할",
    "한",
    "하는",
    // The question mask (`[개인정보 생략]`).
    "개인정보",
    "생략",
];

/// Word prefixes of verbs and question words that never carry content.
const STOP_PREFIXES: &[&str] = &[
    "받을",
    "받으",
    "받나",
    "받아",
    "받는",
    "받기",
    "받고",
    "받게",
    "있",
    "없",
    "알려",
    "궁금",
    "어떻",
    "어떤",
    "무슨",
    "무엇",
    "어디",
    "뭐",
    "뭔",
    "될",
    "되는",
    "될까",
    "되요",
    "됩",
    "필요",
    "가능",
    "줘",
    "주세요",
];

/// Topic vocabulary: words that name what kind of fact is asked, not which
/// one. A question made only of these is answered from its topic's sections.
const GENERIC: &[&str] = &[
    "서류",
    "제출서류",
    "구비서류",
    "준비물",
    "마감",
    "기간",
    "기한",
    "일정",
    "언제",
    "신청",
    "접수",
    "방법",
    "절차",
    "순서",
    "조건",
    "자격",
    "대상",
    "지원",
    "지원금",
    "금액",
    "얼마",
    "내용",
    "요약",
    "심사",
    "선발",
    "준비",
    "신청서",
    "청년",
    "혜택",
    "해당",
    "신청방법",
    "지원내용",
];

/// Pseudo-form matching an age in the text: digits followed by 세 or 살.
const AGE_FORM: &str = "<age>";

/// Extra surface forms a question word also matches, compacted (no spaces).
const SYNONYMS: &[(&str, &[&str])] = &[
    ("마감", &["까지", "기한", "기간"]),
    ("언제", &["기간", "마감", "까지", "기한", "일정"]),
    ("기간", &["까지"]),
    ("얼마", &["금액", "만원", "지원금", "최대"]),
    ("금액", &["만원", "지원금"]),
    ("지원금", &["금액", "만원"]),
    ("서류", &["증명서", "등본", "사본", "신분증", "구비"]),
    ("준비물", &["서류", "증명서"]),
    ("나이", &["연령", AGE_FORM]),
    ("연령", &["나이", AGE_FORM]),
    ("소득", &["분위", "중위", "수급", "차상위"]),
    ("외국인", &["국적"]),
    ("온라인", &["홈페이지", "누리집", "인터넷"]),
    ("방문", &["오프라인", "센터", "창구"]),
    ("방법", &["절차", "신청", "접수"]),
    ("절차", &["방법", "단계", "순서"]),
    ("신청", &["접수"]),
    ("조건", &["자격", "대상"]),
    ("자격", &["조건", "대상"]),
    ("대상", &["자격", "조건"]),
    ("심사", &["선발", "선정", "평가", "면접"]),
    ("선발", &["선정", "심사"]),
    ("월세", &["임대료", "주거"]),
    ("학생", &["재학", "대학생"]),
    ("대학생", &["재학", "대학"]),
];

/// Lowercased words split on anything that is not a letter or digit.
/// Whether `chars[i]` ends a sentence: `.`, `!`, `?` or `。` before
/// whitespace or the end of the text, so a decimal ("1.5%") or a date
/// ("2026.12.31.까지") never splits. The one boundary rule of the feature:
/// the passage builder and the summary guard both read it. An ellipsis
/// (U+2026) is no end; read it before any normalisation turns it into dots.
pub(crate) fn ends_sentence(chars: &[char], i: usize) -> bool {
    matches!(chars.get(i), Some('.' | '!' | '?' | '。'))
        && chars.get(i + 1).is_none_or(|n| n.is_whitespace())
}

pub(crate) fn words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_owned)
        .collect()
}

fn is_hangul(c: char) -> bool {
    ('\u{AC00}'..='\u{D7A3}').contains(&c)
}

/// Strip one trailing particle or ending from a Hangul word.
pub(crate) fn stem(word: &str) -> &str {
    if !word.chars().last().is_some_and(is_hangul) {
        return word;
    }
    let length = word.chars().count();
    for ending in ENDINGS {
        if let Some(rest) = word.strip_suffix(ending) {
            if length - ending.chars().count() >= 2 {
                return rest;
            }
        }
    }
    word
}

fn is_stop(word: &str) -> bool {
    STOPWORDS.contains(&word) || STOP_PREFIXES.iter().any(|p| word.starts_with(p))
}

/// Lowercased text without whitespace, with the original char offset of
/// every kept char so a match can be highlighted in the original text.
pub(crate) fn compact(text: &str) -> (String, Vec<usize>) {
    let mut out = String::with_capacity(text.len());
    let mut map = Vec::new();
    for (index, c) in text.chars().enumerate() {
        if c.is_whitespace() {
            continue;
        }
        for lower in c.to_lowercase() {
            out.push(lower);
            map.push(index);
        }
    }
    (out, map)
}

/// Character bigrams of letters and digits, ignoring punctuation.
pub(crate) fn bigrams(compacted: &str) -> Vec<String> {
    let chars: Vec<char> = compacted.chars().filter(|c| c.is_alphanumeric()).collect();
    chars.windows(2).map(|w| w.iter().collect()).collect()
}

/// One content word of a question and the forms it matches.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Term {
    pub surface: String,
    pub forms: Vec<String>,
    /// Not topic vocabulary: the passage must contain it to answer.
    pub specific: bool,
}

/// Content terms of a question, in order, deduplicated.
pub(crate) fn query_terms(question: &str) -> Vec<Term> {
    let mut terms: Vec<Term> = Vec::new();
    for word in words(question) {
        // Bare numbers (years, counts, leftovers of masked numbers) match
        // too loosely to decide anything.
        if is_stop(&word) || word.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let stemmed = stem(&word).to_owned();
        if is_stop(&stemmed) || stemmed.chars().count() < 2 {
            continue;
        }
        if terms.iter().any(|t| t.surface == stemmed) {
            continue;
        }
        let mut forms = vec![stemmed.clone()];
        // 휴학생 → 휴학, 대학원생 → 대학원: notices name the status.
        if stemmed.chars().count() >= 3 {
            if let Some(base) = stemmed.strip_suffix('생') {
                forms.push(base.to_owned());
            }
        }
        if let Some((_, extra)) = SYNONYMS.iter().find(|(key, _)| *key == stemmed) {
            forms.extend(extra.iter().map(|s| (*s).to_owned()));
        }
        forms.dedup();
        terms.push(Term {
            specific: !GENERIC.contains(&stemmed.as_str()),
            surface: stemmed,
            forms,
        });
    }
    terms
}

/// Non-overlapping occurrences of `form` in `haystack` (both compacted), as
/// char ranges.
pub(crate) fn occurrences(haystack: &str, form: &str) -> Vec<(usize, usize)> {
    if form.is_empty() {
        return Vec::new();
    }
    if form == AGE_FORM {
        let chars: Vec<char> = haystack.chars().collect();
        return (1..chars.len())
            .filter(|&i| matches!(chars[i], '세' | '살') && chars[i - 1].is_ascii_digit())
            .map(|i| {
                let digits = chars[..i]
                    .iter()
                    .rev()
                    .take_while(|c| c.is_ascii_digit())
                    .count();
                (i - digits, i + 1)
            })
            .collect();
    }
    let length = form.chars().count();
    haystack
        .match_indices(form)
        .map(|(byte, _)| {
            let start = haystack[..byte].chars().count();
            (start, start + length)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn particles_and_endings_are_stripped_once_and_keep_two_chars() {
        for (word, expected) in [
            ("휴학생도", "휴학생"),
            ("서류는", "서류"),
            ("부산에서", "부산"),
            ("외국인이면", "외국인"),
            ("졸업했는데", "졸업"),
            ("신청하려면", "신청"),
            ("대학원생이", "대학원생"),
            ("참가", "참가"),
            ("학생이", "학생"),
            ("이", "이"),
            ("github", "github"),
        ] {
            assert_eq!(stem(word), expected, "{word}");
        }
    }

    #[test]
    fn question_terms_drop_question_words_and_mark_topic_vocabulary() {
        let terms = query_terms("휴학생도 받을 수 있어?");
        assert_eq!(terms.len(), 1);
        assert_eq!(terms[0].surface, "휴학생");
        assert!(terms[0].specific);
        assert!(terms[0].forms.contains(&"휴학".to_owned()));
        let terms = query_terms("필요한 서류는 뭐야?");
        assert_eq!(
            terms.iter().map(|t| t.surface.as_str()).collect::<Vec<_>>(),
            ["서류"]
        );
        let terms = query_terms("그럼 서류는?");
        assert_eq!(terms.len(), 1);
        assert!(!terms[0].specific);
        assert!(query_terms("이거 뭐야?").is_empty());
    }

    #[test]
    fn compact_maps_back_to_original_offsets() {
        let (text, map) = compact("주민등록 등본 1부");
        assert_eq!(text, "주민등록등본1부");
        let (start, end) = occurrences(&text, "등록등본")[0];
        assert_eq!((map[start], map[end - 1] + 1), (2, 7));
        assert_eq!(bigrams("만19세~34세").len(), 6);
    }
}
