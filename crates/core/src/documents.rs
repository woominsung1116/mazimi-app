//! Required documents split from a source's own documents field (v1
//! feedback F6.2a): 온통청년 `sbmsnDcmntCn`, 한국장학재단 `제출서류 상세내용`
//! and 보조금24 `serviceDetail.구비서류`, all served as
//! `programs.submission_documents`.
//!
//! The text is used only when it is a list: at least two items, each
//! started by the same kind of bullet or enumerator (`○`, `①`, `1.`, `가.`, …),
//! or, without any bullet, one per line. Lines under an item that start with
//! another kind of bullet (or none) are that item's description. `※` lines
//! are notes. A heading such as "<공통제출서류>" or "1차 제출서류" is not a
//! document. Grouped texts ("○ 신청인 제출서류" with `-` lines under it, then
//! "○ 공무원 확인 서류(신청인 미제출 서류)" with its own) list the lines under
//! every group title, and each item's description starts with its group
//! title (or the heading line above it), so a qualifier such as "미제출" or
//! "… 신청 시" stays visible next to the document.
//! Names are the source's words minus the leading bullet; `(필수)` /
//! `(선택)`, before or after the name, set `is_required`. Anything else
//! ("별도 문의", one sentence) gives no list, and the raw text stays the
//! guidance. "해당없음", "없음" and "-" are [`states_none`]: the notice says
//! nothing is to be submitted.
use crate::display_text::{clean, enumerator_len, is_circled_number, leading_symbol, pieces};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentItem {
    pub name: String,
    /// Detail lines written under the item, joined with `\n`.
    pub description: Option<String>,
    /// `Some(true)` for "(필수)", `Some(false)` for "(선택)".
    pub is_required: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DocumentList {
    pub items: Vec<DocumentItem>,
    /// `※` notes of the text.
    pub notes: Vec<String>,
}

/// The kind of mark a piece starts with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mark {
    Symbol(char),
    Circled,
    Number,
    Paren,
    Hangul,
    Note,
}

fn mark(piece: &str) -> Option<Mark> {
    let s = piece.trim_start();
    let first = s.chars().next()?;
    if first == '※' {
        return Some(Mark::Note);
    }
    if leading_symbol(s).is_some() {
        return Some(Mark::Symbol(first));
    }
    if is_circled_number(first) {
        return Some(Mark::Circled);
    }
    enumerator_len(s).or_else(|| unspaced_number_len(s))?;
    Some(if first.is_ascii_digit() {
        Mark::Number
    } else if first == '(' {
        Mark::Paren
    } else {
        Mark::Hangul
    })
}

/// "1.신청서", "3.(해당 시) 임대차계약서": a list number written without
/// the space, straight before a Hangul syllable or `(`. Only a documents
/// list reads it this way (display text keeps "25.하반기" as written), and
/// only numbers up to 20.
fn unspaced_number_len(s: &str) -> Option<usize> {
    let digits = s.chars().take_while(char::is_ascii_digit).count();
    let number: u32 = s.get(..digits)?.parse().ok()?;
    let mut rest = s[digits..].chars();
    let closes = matches!(rest.next(), Some('.' | ')'));
    let word = rest
        .next()
        .is_some_and(|c| ('\u{AC00}'..='\u{D7A3}').contains(&c) || c == '(');
    ((1..=20).contains(&number) && closes && word).then_some(digits + 1)
}

/// [`clean`], also removing an [`unspaced_number_len`] number.
fn clean_item(piece: &str) -> String {
    let s = piece.trim_start();
    match unspaced_number_len(s) {
        Some(len) if enumerator_len(s).is_none() => clean(&s[len..]),
        _ => clean(piece),
    }
}

/// "<공통제출서류>", "[추가서류]", "제출서류:", "1차 제출서류": a section
/// title, not a document. A titled entry with a note ("추가서류(필요시)(선택)")
/// is an item the user prepares.
fn heading(name: &str) -> bool {
    let n = name.trim();
    if (n.starts_with('<') && n.ends_with('>')) || (n.starts_with('[') && n.ends_with(']')) {
        return true;
    }
    if n.ends_with(':') {
        return true;
    }
    if n.contains('(') {
        return false;
    }
    let core = n
        .trim_start_matches(|c: char| c.is_ascii_digit() || c == '차')
        .trim();
    matches!(
        core,
        "제출서류"
            | "구비서류"
            | "추가서류"
            | "공통서류"
            | "공통제출서류"
            | "필수서류"
            | "기타서류"
            | "증빙서류"
            | "제출 서류"
            | "추가 서류"
    )
}

/// "신청인 제출서류", "공무원 확인 가능 서류(신청인 미제출 서류)": a title
/// naming a group of documents. Only a piece with its own marked lines
/// under it is read this way; a lone "임업인 증빙서류" is a document.
fn group_title(name: &str) -> bool {
    if heading(name) {
        return true;
    }
    let n = name.trim();
    // "50% 감면 : 신분증 등 관련 증빙서류": a label and its content.
    if n.split_once(':')
        .is_some_and(|(_, after)| !after.trim().is_empty())
    {
        return false;
    }
    // A trailing "(…)" qualifies the group, it does not name a document.
    let n = match n.strip_suffix(')').and_then(|rest| rest.rfind('(')) {
        Some(open) if open > 0 => n[..open].trim_end(),
        _ => n,
    };
    n.ends_with("서류")
}

fn required(name: &str) -> Option<bool> {
    let n = name.trim();
    if n.ends_with("(필수)") || n.starts_with("(필수)") {
        Some(true)
    } else if n.ends_with("(선택)") || n.starts_with("(선택)") {
        Some(false)
    } else {
        None
    }
}

/// The explicit "nothing to submit" values a documents field holds instead
/// of a list: "해당없음", "없음" or a lone "-", with any bullet, spacing or
/// final full stop. Other wording ("별도 문의", "구비서류 없음") is guidance
/// and stays the source's text.
const NONE_STATEMENTS: &[&str] = &["해당없음", "없음"];

/// Whether a documents text states that nothing has to be submitted
/// ([`NONE_STATEMENTS`]). This is the one definition of "공고상 별도 제출
/// 서류 없음": the API serves it as a flag and the app only renders it. An
/// absent or blank text is missing data, never this state.
pub fn states_none(text: Option<&str>) -> bool {
    let Some(text) = text.map(str::trim).filter(|t| !t.is_empty()) else {
        return false;
    };
    if text
        .chars()
        .all(|c| matches!(c, '-' | '–' | '—') || c.is_whitespace())
    {
        return true;
    }
    let compact: String = clean(text).chars().filter(|c| !c.is_whitespace()).collect();
    NONE_STATEMENTS.contains(&compact.trim_end_matches('.'))
}

/// Mark choice: an enumerator kind used at least twice, else the first
/// bullet kind used at least twice, else the first mark.
fn choose(marks: &[Mark]) -> Option<Mark> {
    let repeated = |m: &Mark| marks.iter().filter(|o| *o == m).count() >= 2;
    let enumerator = |m: &Mark| !matches!(m, Mark::Symbol(_));
    marks
        .iter()
        .find(|m| enumerator(m) && repeated(m))
        .or_else(|| marks.iter().find(|m| repeated(m)))
        .or(marks.first())
        .copied()
}

/// The mark kind of group titles, when the text is grouped: a kind whose
/// pieces with marked lines of another kind under them are at least two, or
/// all of its pieces, every such piece being a [`group_title`] ("○ 신청인 제출서류\n- 신분증\n-
/// 신청서\n○ 공무원 확인 서류\n- 주민등록등본"). Pieces of that kind
/// without lines under them stay items.
fn group_kind(pieces: &[(&str, Option<Mark>)]) -> Option<Mark> {
    let mut kinds: Vec<Mark> = Vec::new();
    for (_, m) in pieces {
        if let Some(m) = m.filter(|m| *m != Mark::Note && !kinds.contains(m)) {
            kinds.push(m);
        }
    }
    kinds.into_iter().find(|kind| {
        let parents: Vec<&str> = pieces
            .iter()
            .enumerate()
            .filter(|(i, (_, m))| {
                *m == Some(*kind)
                    && pieces[i + 1..]
                        .iter()
                        .find(|(_, next)| *next != Some(Mark::Note))
                        .is_some_and(|(_, next)| next.is_some_and(|n| n != *kind))
            })
            .map(|(_, (p, _))| *p)
            .collect();
        let all = pieces.iter().filter(|(_, m)| *m == Some(*kind)).count();
        // Groups come in twos or more, or every piece of the kind is one: a
        // single titled entry among plain items ("3. 증빙서류(아래 중
        // 1개)" with ① cases under it) is an item with its description.
        (parents.len() >= 2 || parents.len() == all)
            && parents.iter().all(|p| group_title(&clean(p)))
    })
}

/// The checklist of a documents text, or `None` when it is not a list.
pub fn split(text: Option<&str>) -> Option<DocumentList> {
    let text = text?.replace("\r\n", "\n").replace('\r', "\n");
    // (piece, split off inside a line)
    let raw: Vec<(&str, bool)> = text
        .lines()
        .flat_map(|line| {
            pieces(line)
                .into_iter()
                .enumerate()
                .map(|(i, piece)| (piece, i > 0))
        })
        .filter(|(p, _)| !p.trim().is_empty())
        .collect();
    let marked_pieces: Vec<(&str, Option<Mark>)> = raw.iter().map(|(p, _)| (*p, mark(p))).collect();
    let group = group_kind(&marked_pieces);
    // A group title with lines under it is not an item; the item mark is
    // chosen among the other kinds.
    let is_group_title = |i: usize| {
        group.is_some()
            && marked_pieces[i].1 == group
            && marked_pieces[i + 1..]
                .iter()
                .find(|(_, next)| *next != Some(Mark::Note))
                .is_some_and(|(_, next)| next.is_some_and(|n| Some(n) != group))
    };
    let marks: Vec<Mark> = marked_pieces
        .iter()
        .filter_map(|(_, m)| *m)
        .filter(|m| *m != Mark::Note && Some(*m) != group)
        .collect();
    let primary = choose(&marks).or(group);
    let mut list = DocumentList::default();
    // (name, description lines) of the items so far.
    let mut items: Vec<(String, Vec<String>)> = Vec::new();
    // The group title the next items sit under. It opens each item's
    // description, so "공무원 확인 가능 서류(신청인 미제출 서류)" or "양수
    // 검사비 지원 신청 시 제출 서류" stays with the documents it qualifies.
    let mut under: Option<String> = None;
    // The item later unmarked lines describe; a group title or a heading
    // line ends it.
    let mut current: Option<usize> = None;
    // An inline ※ note that ends with (필수)/(선택) marks the item it follows.
    let mut marked: Vec<(usize, bool)> = Vec::new();
    for (index, (piece, inline)) in raw.into_iter().enumerate() {
        let cleaned = clean_item(piece);
        if cleaned.is_empty() {
            continue;
        }
        let m = marked_pieces[index].1;
        if m == Some(Mark::Note) {
            if let (true, Some(flag), false) = (inline, required(&cleaned), items.is_empty()) {
                marked.push((items.len() - 1, flag));
            }
            list.notes.push(cleaned);
            continue;
        }
        if is_group_title(index) {
            current = None;
            under = Some(cleaned);
            continue;
        }
        let starts_item = match primary {
            Some(kind) => m == Some(kind) || (m.is_some() && m == group),
            // No bullets at all: one item per line.
            None => true,
        };
        if starts_item {
            let context = if m.is_some() && m == group {
                // A childless piece of the group kind is not under a group.
                under = None;
                Vec::new()
            } else {
                under.iter().cloned().collect()
            };
            items.push((cleaned, context));
            current = Some(items.len() - 1);
        } else if m.is_none() && heading(&cleaned) {
            // "<해당 세대 제출>": a heading line describes no item; it
            // qualifies the items after it, as a group title does.
            current = None;
            under = Some(
                cleaned
                    .trim_matches(|c| matches!(c, '<' | '>' | '[' | ']'))
                    .trim()
                    .to_owned(),
            );
        } else if let Some(description) = current.and_then(|i| items.get_mut(i)) {
            description.1.push(cleaned);
        }
        // Text before the first item (an intro or a heading) is not a
        // document; the raw text is still served.
    }
    list.items = items
        .into_iter()
        .enumerate()
        .filter(|(_, (name, _))| !heading(name))
        .map(|(index, (name, description))| DocumentItem {
            is_required: required(&name).or_else(|| {
                marked
                    .iter()
                    .find(|(i, _)| *i == index)
                    .map(|(_, flag)| *flag)
            }),
            description: (!description.is_empty()).then(|| description.join("\n")),
            name,
        })
        .collect();
    // A name listed twice is one item (the first, with its details).
    let mut seen: Vec<String> = Vec::new();
    list.items.retain(|item| {
        let fresh = !seen.contains(&item.name);
        if fresh {
            seen.push(item.name.clone());
        }
        fresh
    });
    (list.items.len() >= 2).then_some(list)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(text: &str) -> Vec<String> {
        split(Some(text))
            .map(|l| l.items.into_iter().map(|i| i.name).collect())
            .unwrap_or_default()
    }

    #[test]
    fn prod_lists_split_into_items() {
        // 한국장학재단: inline ○ list with a trailing ※ note.
        let list = split(Some("○ 장학생 신청서(개인정보이용동의서 포함)○ 재학증명서○ 부모 또는 본인의 통장사본※ 자세한 사항은 첨부파일 또는 홈페이지 참고")).unwrap();
        assert_eq!(
            list.items
                .iter()
                .map(|i| i.name.as_str())
                .collect::<Vec<_>>(),
            [
                "장학생 신청서(개인정보이용동의서 포함)",
                "재학증명서",
                "부모 또는 본인의 통장사본"
            ]
        );
        assert_eq!(list.notes, ["자세한 사항은 첨부파일 또는 홈페이지 참고"]);
        // 온통청년: numbered items with - detail lines.
        let list = split(Some("1. 주민등록초본\n- 2026년 발급분\n- 발급 시 발급대상자 본인 및 전체 발급\n\n2. 소득금액증명원\n- 2026년 발급분")).unwrap();
        assert_eq!(list.items[0].name, "주민등록초본");
        assert_eq!(
            list.items[0].description.as_deref(),
            Some("2026년 발급분\n발급 시 발급대상자 본인 및 전체 발급")
        );
        assert_eq!(list.items[1].name, "소득금액증명원");
        // One per line, with (필수)/(선택).
        let list = split(Some(
            "시험 결제영수증(필수)\n통장사본(본인명의)(필수)\n주민등록등본(선택)\n추가서류(필요시)(선택)",
        ))
        .unwrap();
        // "추가서류(필요시)(선택)" is an item the user prepares.
        assert_eq!(list.items.len(), 4);
        assert_eq!(list.items[3].name, "추가서류(필요시)(선택)");
        assert_eq!(list.items[3].is_required, Some(false));
        assert_eq!(list.items[0].is_required, Some(true));
        assert_eq!(list.items[2].is_required, Some(false));
        // A heading line and circled numbers.
        assert_eq!(
            names(
                "○ 제출서류\n① 신청서(서식1)\n② 서약서 및 개인정보 동의서(서식2)\n③ 주민등록등본"
            ),
            [
                "신청서(서식1)",
                "서약서 및 개인정보 동의서(서식2)",
                "주민등록등본"
            ]
        );
        assert_eq!(
            names("<공통제출서류>\n ① 임대 신청서(서식 1)\n ② 영농계획서(서식 2)"),
            ["임대 신청서(서식 1)", "영농계획서(서식 2)"]
        );
        // Hangul enumerators under a ○ intro: the intro is not a document.
        assert_eq!(
            names("○ 지원 시: 해당 서류(가~마) 모두 제출한 경우에만 접수 가능\n가. 우울증 치료비 지원 신청서 1부(서식 1)\n나. 개인정보 수집·이용·제공 동의서 1부(서식 2)"),
            ["우울증 치료비 지원 신청서 1부(서식 1)", "개인정보 수집·이용·제공 동의서 1부(서식 2)"]
        );
        // ○ items with - details: the first repeated kind leads.
        let list = split(Some("○ 신청서\n- 서식 1\n- 날인\n○ 통장 사본")).unwrap();
        assert_eq!(
            list.items
                .iter()
                .map(|i| i.name.as_str())
                .collect::<Vec<_>>(),
            ["신청서", "통장 사본"]
        );
        assert_eq!(list.items[0].description.as_deref(), Some("서식 1\n날인"));
        // Plain - list.
        assert_eq!(
            names("- 국/영문 신청서 각 1부\n- 재학(졸업) 증명서(국/영문 각 1부)"),
            ["국/영문 신청서 각 1부", "재학(졸업) 증명서(국/영문 각 1부)"]
        );
    }

    #[test]
    fn review_cases_split_cleanly() {
        // "⑥-1." is one enumerator.
        assert_eq!(
            names("⑤ 주민등록등본\n⑥ 가족관계증명서\n⑥-1. 혼인관계증명서"),
            ["주민등록등본", "가족관계증명서", "혼인관계증명서"]
        );
        // An inline ※ note keeps the item's (필수).
        let list = split(Some("○ 신청서 ※ 서식 1 (필수)\n○ 통장 사본")).unwrap();
        assert_eq!(list.items[0].name, "신청서");
        assert_eq!(list.items[0].is_required, Some(true));
        assert_eq!(list.items[1].is_required, None);
        assert_eq!(list.notes, ["서식 1 (필수)"]);
        // A bare heading line is still not a document.
        assert_eq!(
            names("○ 추가서류\n○ 재학증명서\n○ 성적증명서"),
            ["재학증명서", "성적증명서"]
        );
    }

    #[test]
    fn a_repeated_item_is_listed_once() {
        assert_eq!(
            names("신청서(필수)\n추가서류(필요시)(선택)\n통장 사본(필수)\n추가서류(필요시)(선택)"),
            ["신청서(필수)", "추가서류(필요시)(선택)", "통장 사본(필수)"]
        );
    }

    #[test]
    fn grouped_lists_give_the_lines_under_each_title() {
        assert_eq!(
            names("○ 필수서류\n- 신청서\n- 통장 사본\n○ 추가서류(해당자)\n- 가족관계증명서"),
            ["신청서", "통장 사본", "가족관계증명서"]
        );
        // A group kind piece without lines under it is still an item.
        assert_eq!(
            names(
                "○ 신청서\n○ 통장사본\n○ 추가서류\n - 가족관계증명서\n○ 해당자 서류\n - 재학증명서"
            ),
            ["신청서", "통장사본", "가족관계증명서", "재학증명서"]
        );
        // One titled entry among plain items is an item with a description
        // (here the ① lines are cases of people, not documents).
        assert_eq!(
            names("1. 신청서\n2. 동의서\n3. 증빙서류(아래 중 1개)\n① 상담센터가 인정하는 자\n② 의료기관이 인정하는 자"),
            ["신청서", "동의서", "증빙서류(아래 중 1개)"]
        );
        // "label : content" is not a group title.
        assert_eq!(
            names("○ 50% 감면 : 유족증 등 관련 증빙서류\n-국가유공자\n-장애인\n○ 30% 감면 : 주민등록등본 등 관련 증빙서류\n-다자녀"),
            ["50% 감면 : 유족증 등 관련 증빙서류", "30% 감면 : 주민등록등본 등 관련 증빙서류"]
        );
        // A piece with lines that is not a group title keeps them as its
        // description (the ae42… sample: "출산 … 증빙자료" with - lines).
        let list = split(Some(
            "① 신분 확인서류\n - 신분증\n② 소득 증빙자료\n - 건강보험료 납부확인서",
        ))
        .unwrap();
        assert_eq!(list.items.len(), 2);
        assert_eq!(
            list.items[1].description.as_deref(),
            Some("건강보험료 납부확인서")
        );
        // An unmarked heading line ends the item before it.
        let list = split(Some(
            "- 신청서\n- 통장사본\n<해당 세대 제출>\n- 가족관계증명서",
        ))
        .unwrap();
        assert_eq!(list.items[1].description, None);
        assert_eq!(list.items.len(), 3);
        // ... and qualifies the items after it.
        assert_eq!(list.items[2].description.as_deref(), Some("해당 세대 제출"));
        // The group title opens each item's description.
        let list = split(Some("○ 신청인 제출서류\n- 신분증\n  · 원본 지참\n○ 공무원 확인 서류(신청인 미제출 서류)\n- 주민등록등본\n- 장애인증명서")).unwrap();
        let described: Vec<_> = list
            .items
            .iter()
            .map(|i| i.description.as_deref())
            .collect();
        assert_eq!(
            described,
            [
                Some("신청인 제출서류\n원본 지참"),
                Some("공무원 확인 서류(신청인 미제출 서류)"),
                Some("공무원 확인 서류(신청인 미제출 서류)")
            ]
        );
    }

    #[test]
    fn numbers_without_a_space_enumerate() {
        assert_eq!(
            names("1.신청서\n2.통장 사본\n3.(해당 시) 임대차계약서"),
            ["신청서", "통장 사본", "(해당 시) 임대차계약서"]
        );
        // A number above 20 is not one, and display text is untouched.
        assert_eq!(
            names("25.하반기 모집\n26.상반기 모집"),
            ["25.하반기 모집", "26.상반기 모집"]
        );
        assert_eq!(
            crate::display_text::normalize(Some("25.하반기 모집")).lines,
            ["25.하반기 모집"]
        );
        // A decimal is not an enumerator.
        assert_eq!(
            names("1.5배 지원\n2.5배 지원"),
            ["1.5배 지원", "2.5배 지원"]
        );
    }

    #[test]
    fn a_leading_required_mark_counts() {
        let list = split(Some("- (필수) 신청서\n- (선택) 통장 사본\n- 등본")).unwrap();
        let flags: Vec<_> = list.items.iter().map(|i| i.is_required).collect();
        assert_eq!(flags, [Some(true), Some(false), None]);
    }

    #[test]
    fn none_statements() {
        for text in [
            "해당없음",
            "해당 없음",
            "해당없음\u{3000}",
            "○ 해당없음",
            "없음",
            "없음.",
            "-",
            " - ",
            "–",
        ] {
            assert!(states_none(Some(text)), "{text:?}");
        }
        for text in [
            "",
            "  ",
            "별도 문의",
            "구비서류 없음",
            "신청서 외 추가서류 없음",
            "해당없음\n○ 신청서",
            "- 신청서",
            "○ 신분증\n○ 해당없음",
        ] {
            assert!(!states_none(Some(text)), "{text:?}");
        }
        assert!(!states_none(None));
        assert_eq!(split(Some("해당없음")), None);
    }

    #[test]
    fn non_lists_give_nothing() {
        for text in [
            "해당없음",
            "별도 문의",
            "상환유예 신청 증빙서류 등",
            "아산시 거주 확인(등초본) 및 관내 대학 재학증명서",
            "※ 홈페이지 참고",
            "",
        ] {
            assert_eq!(split(Some(text)), None, "{text}");
        }
        assert_eq!(split(None), None);
    }
}
