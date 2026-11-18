//! How a programme is applied for, read from its own notice text, and how far
//! one user has come (the detail page's 신청 방법 and 신청 진행 단계).
//!
//! [`procedure_steps`] segments the notice's application-method text, then its
//! screening text, by the structure the text itself has. Nothing is generated.
//! A step is the source's words without the list mark.
//!
//! [`stages`] is the single state function behind the Figma stepper. It covers
//! 발견, 자격 확인, 서류 준비 and 신청, each `done`, `current` or `upcoming`,
//! read from what is already recorded for the user.
use crate::display_text::{clean, enumerator_len, is_circled_number, leading_symbol, pieces};
use serde::Serialize;

/// One step of the notice's procedure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Step {
    pub title: String,
    pub detail: Option<String>,
    /// The source field the step was read from.
    pub source: &'static str,
}

/// A segmented procedure text.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Steps {
    /// Text before the first step, e.g. "농림축산식품부 사업 추진일정에 따라 운영".
    pub intro: Option<String>,
    pub steps: Vec<Step>,
    /// The source numbered the steps or chained them with arrows. Bullet
    /// lists are not ordered.
    pub ordered: bool,
}

/// The kind of mark a piece starts with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mark {
    Number,
    Paren,
    Hangul,
    Circled,
    Arrow,
    Symbol(char),
    Note,
}

impl Mark {
    /// Enumerators and arrows state an order; a bullet does not.
    fn ordered(self) -> bool {
        !matches!(self, Mark::Symbol(_) | Mark::Note)
    }
}

/// Arrows that start a line ("→ 사업 결과 및 정산").
const LEADING_ARROWS: &[&str] = &["→", "⇒", "➡", "->"];
/// Arrows that chain steps inside a line. `▷`/`▶` chain only mid-line; at
/// the start of a line they are bullets. A spaced `>` is a menu path
/// ("고용24 > 취업지원"), a location rather than steps, so it does not chain.
const INLINE_ARROWS: &[&str] = &["→", "⇒", "➡", "▷", "▶", "->"];

fn mark(piece: &str) -> Option<Mark> {
    let s = piece.trim_start();
    let first = s.chars().next()?;
    if first == '※' {
        return Some(Mark::Note);
    }
    if LEADING_ARROWS.iter().any(|a| s.starts_with(a)) {
        return Some(Mark::Arrow);
    }
    if leading_symbol(s).is_some() {
        return Some(Mark::Symbol(first));
    }
    if is_circled_number(first) {
        return Some(Mark::Circled);
    }
    enumerator_len(s)?;
    Some(if first.is_ascii_digit() {
        Mark::Number
    } else if first == '(' {
        Mark::Paren
    } else {
        Mark::Hangul
    })
}

/// The HTML entities collected notices carry ("&lsquo;청년도전지원사업&rsquo;").
fn decode_entities(text: &str) -> String {
    const ENTITIES: &[(&str, &str)] = &[
        ("&lsquo;", "‘"),
        ("&rsquo;", "’"),
        ("&ldquo;", "“"),
        ("&rdquo;", "”"),
        ("&middot;", "·"),
        ("&bull;", "•"),
        ("&nbsp;", " "),
        ("&quot;", "\""),
        ("&#39;", "'"),
        ("&apos;", "'"),
        ("&lt;", "<"),
        ("&gt;", ">"),
        ("&amp;", "&"),
    ];
    ENTITIES
        .iter()
        .fold(text.to_owned(), |t, (entity, c)| t.replace(entity, c))
}

/// A line split at inline bullets ([`pieces`]); an inline `▷`/`▶` is an
/// arrow, so it stays inside its piece.
fn line_pieces(line: &str) -> Vec<&str> {
    let mut out: Vec<&str> = Vec::new();
    let mut start: Option<usize> = None;
    let base = line.as_ptr() as usize;
    for piece in pieces(line) {
        let at = piece.as_ptr() as usize - base;
        let arrow = piece.trim_start().starts_with(['▷', '▶']);
        match start {
            Some(s) if arrow => {
                out.pop();
                out.push(&line[s..at + piece.len()]);
            }
            _ => {
                start = Some(at);
                out.push(piece);
            }
        }
    }
    out
}

/// Pieces of a line between inline arrows, empty ones dropped.
fn arrow_segments(line: &str) -> Vec<String> {
    let mut parts = vec![line.to_owned()];
    for arrow in INLINE_ARROWS {
        parts = parts
            .iter()
            .flat_map(|p| p.split(arrow).map(str::to_owned).collect::<Vec<_>>())
            .collect();
    }
    parts
        .into_iter()
        .map(|p| clean(&p))
        .filter(|p| !p.is_empty())
        .collect()
}

/// An enumerator kind used at least twice (first by appearance), else the
/// first bullet kind used at least twice.
fn choose(marks: &[Mark]) -> Option<Mark> {
    let repeated = |m: &Mark| marks.iter().filter(|o| *o == m).count() >= 2;
    marks
        .iter()
        .find(|m| m.ordered() && repeated(m))
        .or_else(|| {
            marks
                .iter()
                .find(|m| matches!(m, Mark::Symbol(_)) && repeated(m))
        })
        .copied()
}

/// "신청·접수: 신보 영업점 …" → ("신청·접수", "신보 영업점 …"). The label
/// is at most 20 characters; a URL scheme or a time is not a label.
fn split_label(text: &str) -> (String, Option<String>) {
    let chars: Vec<char> = text.chars().collect();
    let at = chars.iter().enumerate().find(|(i, c)| {
        matches!(c, ':' | '：')
            && *i <= 20
            && chars.get(i + 1) != Some(&'/')
            && !(*i > 0 && chars[i - 1].is_ascii_digit())
    });
    if let Some((i, _)) = at {
        let head: String = chars[..i].iter().collect::<String>().trim().to_owned();
        let tail: String = chars[i + 1..].iter().collect::<String>().trim().to_owned();
        if !head.is_empty() && !tail.is_empty() {
            return (head, Some(tail));
        }
    }
    (text.to_owned(), None)
}

/// An item under construction: its first text and the lines under it.
struct Item {
    text: String,
    /// (text, starts a sub-item): a wrapped line continues the text before it.
    under: Vec<(String, bool)>,
}

impl Item {
    fn step(self, source: &'static str) -> Step {
        let (title, first) = split_label(&self.text);
        let mut detail = first.unwrap_or_default();
        for (line, sub) in self.under {
            if !detail.is_empty() {
                detail.push_str(if sub { " · " } else { " " });
            }
            detail.push_str(&line);
        }
        Step {
            title,
            detail: (!detail.is_empty()).then_some(detail),
            source,
        }
    }
}

/// An intro that is one word ("신청방법") is a heading of the list, not text.
fn intro_of(parts: Vec<String>) -> Option<String> {
    let intro = parts.join(" ");
    (intro.split_whitespace().count() > 1).then_some(intro)
}

/// Segment one procedure text by its own structure: a list mark used at
/// least twice, else an arrow chain, else the whole text as one step.
pub fn segment(text: &str, source: &'static str) -> Steps {
    let text = decode_entities(text)
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    // (piece, mark, indent). A piece split off mid-line takes its line's
    // indent when it carries the line's own mark ("① 신청 ② 심사" on one
    // line); any other mid-line piece is never top level.
    let mut marked: Vec<(&str, Option<Mark>, usize)> = Vec::new();
    let mut first_line_end = 0;
    for (line_index, line) in lines.iter().enumerate() {
        let indent = line.chars().take_while(|c| c.is_whitespace()).count();
        let parts: Vec<&str> = line_pieces(line)
            .into_iter()
            .filter(|p| !p.trim().is_empty())
            .collect();
        let line_mark = parts.first().and_then(|p| mark(p));
        for (i, piece) in parts.into_iter().enumerate() {
            let m = mark(piece);
            let at = if i == 0 || (m.is_some() && m == line_mark) {
                indent
            } else {
                usize::MAX
            };
            marked.push((piece, m, at));
        }
        if line_index == 0 {
            first_line_end = marked.len();
        }
    }
    // A source that trimmed the field leaves its first line at indent 0 while
    // the rest of the list sits deeper ("1. 신청\n  2. 심사"): the first line
    // then joins the level of the next lines that carry its mark.
    let structural = |m: &Option<Mark>| m.is_some_and(|m| m != Mark::Note);
    let first_mark = marked.first().and_then(|(_, m, _)| *m);
    if marked.first().is_some_and(|(_, _, indent)| *indent == 0) {
        if let Some(level) = marked[first_line_end..]
            .iter()
            .filter(|(_, m, indent)| structural(m) && *m == first_mark && *indent != usize::MAX)
            .map(|(_, _, indent)| *indent)
            .min()
        {
            let below = marked[first_line_end..]
                .iter()
                .any(|(_, m, indent)| structural(m) && *indent < level);
            if !below {
                for piece in marked[..first_line_end]
                    .iter_mut()
                    .filter(|(_, m, _)| *m == first_mark)
                {
                    piece.2 = level;
                }
            }
        }
    }
    // Only marks at the shallowest indentation start steps; deeper ones
    // ("- 방문 : …" under "❍ 신청방법") are details of the step above.
    let top = marked
        .iter()
        .filter(|(_, m, _)| structural(m))
        .map(|(_, _, indent)| *indent)
        .min();
    let marks: Vec<Mark> = marked
        .iter()
        .filter(|(_, _, indent)| Some(*indent) == top)
        .filter_map(|(_, m, _)| *m)
        .collect();
    let note = |piece: &str| format!("※ {}", clean(piece));
    let cleaned = |piece: &str, m: Option<Mark>| {
        if m == Some(Mark::Note) {
            note(piece)
        } else {
            clean(piece)
        }
    };

    if let Some(kind) = choose(&marks) {
        let (mut intro, mut items) = (Vec::new(), Vec::<Item>::new());
        for (piece, m, indent) in marked {
            let text = cleaned(piece, m);
            if text.is_empty() || text == "※ " {
                continue;
            }
            match items.last_mut() {
                _ if m == Some(kind) && Some(indent) == top => items.push(Item {
                    text,
                    under: Vec::new(),
                }),
                None => intro.push(text),
                Some(item) => item.under.push((text, m.is_some())),
            }
        }
        return Steps {
            intro: intro_of(intro),
            steps: items.into_iter().map(|i| i.step(source)).collect(),
            ordered: kind.ordered(),
        };
    }

    let chained: Vec<Vec<String>> = lines.iter().map(|l| arrow_segments(l)).collect();
    let arrow_line = |i: usize| {
        chained[i].len() >= 2
            || LEADING_ARROWS
                .iter()
                .any(|a| lines[i].trim_start().starts_with(a))
    };
    if (0..lines.len())
        .filter(|i| arrow_line(*i))
        .map(|i| chained[i].len())
        .sum::<usize>()
        >= 2
    {
        let (mut intro, mut items) = (Vec::new(), Vec::<Item>::new());
        for (i, line) in lines.iter().enumerate() {
            if arrow_line(i) {
                for text in &chained[i] {
                    items.push(Item {
                        text: text.clone(),
                        under: Vec::new(),
                    });
                }
                continue;
            }
            let m = mark(line);
            let cleaned = if m == Some(Mark::Note) {
                note(line)
            } else {
                clean(line)
            };
            match items.last_mut() {
                None => intro.push(cleaned),
                Some(item) => item.under.push((cleaned, m.is_some())),
            }
        }
        return Steps {
            intro: intro_of(intro),
            steps: items.into_iter().map(|i| i.step(source)).collect(),
            ordered: true,
        };
    }

    // No list: one step. Its text runs until the first marked line or note;
    // what follows is its detail.
    let (mut head, mut under) = (Vec::new(), Vec::new());
    for (piece, m, _) in marked {
        let text = cleaned(piece, m);
        if text.is_empty() || text == "※ " {
            continue;
        }
        if under.is_empty() && m != Some(Mark::Note) && (m.is_none() || head.is_empty()) {
            head.push(text);
        } else {
            under.push((text, m.is_some()));
        }
    }
    if head.is_empty() {
        // Only notes ("※ 홈페이지 참조"): the first note is the step.
        if under.is_empty() {
            return Steps::default();
        }
        head.push(under.remove(0).0);
    }
    Steps {
        intro: None,
        steps: vec![Item {
            text: head.join(" "),
            under,
        }
        .step(source)],
        ordered: true,
    }
}

/// The notice's application procedure: the method text's steps, then the
/// screening text's (screening follows the application). A screening text
/// that repeats the method adds nothing.
pub fn procedure_steps(
    method: Option<(&str, &'static str)>,
    screening: Option<(&str, &'static str)>,
) -> Steps {
    let compact = |t: &str| t.split_whitespace().collect::<String>();
    // "해당없음", "-": the field states nothing to do (same rule as documents).
    let method = method.filter(|(t, _)| !crate::documents::states_none(Some(t)));
    let screening = screening.filter(|(t, _)| !crate::documents::states_none(Some(t)));
    let first = method.map(|(t, s)| segment(t, s)).unwrap_or_default();
    let second = screening
        .filter(|(t, _)| method.is_none_or(|(m, _)| compact(m) != compact(t)))
        .map(|(t, s)| {
            let mut part = segment(t, s);
            if let Some(intro) = part.intro.take() {
                part.steps.insert(
                    0,
                    Step {
                        title: intro,
                        detail: None,
                        source: s,
                    },
                );
            }
            part
        })
        .unwrap_or_default();
    if first.steps.is_empty() && second.steps.is_empty() {
        return Steps::default();
    }
    let ordered = |s: &Steps| s.steps.is_empty() || s.ordered;
    Steps {
        ordered: ordered(&first) && ordered(&second),
        intro: first.intro,
        steps: first.steps.into_iter().chain(second.steps).collect(),
    }
}

/// 복지로 중앙's `applmetList` stage names (`servSeDetailNm`), in the order of
/// the 복지 process. The API's own closed vocabulary; the source lists them
/// out of order now and then.
const LISTED_STAGES: &[&str] = &[
    "신청기관연락처목록",
    "조사기관연락처목록",
    "결정기관연락처목록",
    "이의신청접수기관연락처목록",
    "지급기관연락처목록",
    "사후관리기관목록",
];

/// Steps from 복지로 중앙's `applmetList` (`(stage name, text)` in source
/// order). Each text reads "{institution}에서 {action}": the action (after
/// the first "에서 ") is the title, the institution the detail. Entries of one stage with the same
/// action are one step. Stage names outside the API's list keep their source
/// order after the known ones.
pub fn listed_stages(entries: &[(Option<&str>, &str)], source: &'static str) -> Steps {
    let rank = |stage: Option<&str>| {
        stage
            .and_then(|name| LISTED_STAGES.iter().position(|known| *known == name.trim()))
            .unwrap_or(LISTED_STAGES.len())
    };
    let mut ordered: Vec<(usize, &str)> = entries
        .iter()
        .filter(|(_, text)| !text.trim().is_empty())
        .map(|(stage, text)| (rank(*stage), text.trim()))
        .collect();
    ordered.sort_by_key(|(rank, _)| *rank);
    let mut steps: Vec<Step> = Vec::new();
    for (_, text) in ordered {
        let (title, place) = match text.find("에서 ") {
            Some(at) if at > 0 => (
                text[at + "에서 ".len()..]
                    .trim()
                    .trim_matches(['‘', '’', '\'', '"'])
                    .trim()
                    .to_owned(),
                Some(text[..at].trim().to_owned()),
            ),
            _ => (text.to_owned(), None),
        };
        if title.is_empty() {
            continue;
        }
        match steps.iter_mut().find(|s| s.title == title) {
            Some(step) => {
                if let Some(place) = place {
                    let detail = step.detail.get_or_insert_with(String::new);
                    if !detail.split(" · ").any(|d| d == place) {
                        if !detail.is_empty() {
                            detail.push_str(" · ");
                        }
                        detail.push_str(&place);
                    }
                }
            }
            None => steps.push(Step {
                title,
                detail: place,
                source,
            }),
        }
    }
    Steps {
        intro: None,
        ordered: !steps.is_empty(),
        steps,
    }
}

/// The Figma stepper's four stages (34065:10997), in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Discover,
    Eligibility,
    Documents,
    Apply,
}

impl Stage {
    pub const ALL: [Stage; 4] = [
        Stage::Discover,
        Stage::Eligibility,
        Stage::Documents,
        Stage::Apply,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Stage::Discover => "발견",
            Stage::Eligibility => "자격 확인",
            Stage::Documents => "서류 준비",
            Stage::Apply => "신청",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StageState {
    Done,
    Current,
    Upcoming,
}

/// A connector submission's state (`application_submissions.state`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Submission {
    /// queued, submitting, or unknown (sent, acknowledgement lost).
    InFlight,
    /// accepted: the institution issued a receipt.
    Accepted,
    /// rejected, cancelled or expired: nothing was received.
    Ended,
}

impl Submission {
    pub fn parse(state: &str) -> Option<Self> {
        match state {
            "queued" | "submitting" | "unknown" => Some(Self::InFlight),
            "accepted" => Some(Self::Accepted),
            "rejected" | "cancelled" | "expired" => Some(Self::Ended),
            _ => None,
        }
    }
}

/// What is recorded for one user and programme. The default is a guest.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Progress {
    /// A signed-in viewer of the programme's detail page.
    pub discovered: bool,
    /// The checklist's 신청 조건 확인 task is ticked.
    pub eligibility_confirmed: bool,
    /// Ticked and total document tasks of the checklist.
    pub documents_done: usize,
    pub documents_total: usize,
    /// The checklist has no document task: nothing is to be submitted.
    pub documents_not_required: bool,
    /// The user recorded "신청 준비 중" (`user_program_states.applying`).
    pub preparing: bool,
    /// The user went to the official site (`application_journeys.handoff_at`).
    pub handed_off: bool,
    /// The user reported applying, entered a receipt number, or recorded
    /// applied, waiting or received.
    pub reported_applied: bool,
    /// The latest connector submission, if any.
    pub submission: Option<Submission>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StageView {
    pub stage: Stage,
    pub label: &'static str,
    pub state: StageState,
}

/// The state of each stage. A stage is done when its own record says so, or
/// when the user has already started a later one (ticking a document passes
/// 자격 확인; going to the official site passes 서류 준비). The first stage not
/// done is current; the rest are upcoming. A guest is at 발견.
pub fn stages(progress: &Progress) -> Vec<StageView> {
    let p = progress;
    let applied = p.reported_applied || p.submission == Some(Submission::Accepted);
    let done = |stage: Stage| match stage {
        Stage::Discover => p.discovered,
        Stage::Eligibility => p.eligibility_confirmed,
        Stage::Documents => {
            p.documents_not_required
                || (p.documents_total > 0 && p.documents_done >= p.documents_total)
        }
        Stage::Apply => applied,
    };
    let started = |stage: Stage| match stage {
        Stage::Documents => p.documents_done > 0 || p.preparing || done(stage),
        Stage::Apply => applied || p.handed_off || p.submission == Some(Submission::InFlight),
        _ => done(stage),
    };
    let reached = Stage::ALL.iter().rposition(|s| started(*s));
    let mut current_given = false;
    Stage::ALL
        .iter()
        .enumerate()
        .map(|(i, stage)| {
            let state = if reached.is_some_and(|r| i < r) || done(*stage) {
                StageState::Done
            } else if !current_given {
                current_given = true;
                StageState::Current
            } else {
                StageState::Upcoming
            };
            StageView {
                stage: *stage,
                label: stage.label(),
                state,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn titles(s: &Steps) -> Vec<&str> {
        s.steps.iter().map(|s| s.title.as_str()).collect()
    }

    // 온통청년 (문체부) 청년예술인 예술활동 적립계좌: plcyAplyMthdCn 1-3, srngMthdCn 4-9.
    const ART_METHOD: &str = "1. 예술활동증명확인\n예술인경력정보시스템(https://www.kawfartist.kr)을 접속하시어\n경력지원 > 예술활동증명 > 신청내역 진행 상태에서 신청일 현재 예술활동증명 유효 여부를 확인합니다.\n\n2. 구비서류 확인\n상품 및 구비서류를 확인하여 준비합니다.\n\n3. 온라인 신청\n공지사항에 공지된 신청기간 내 적립계좌 신청하기를 통해 신청 정보 동의 및 인적사항, 신청자료를 입력합니다.";
    const ART_SCREENING: &str = "4. 심사\n접수하신 서류를 토대로 심사자가 심사를 진행합니다.\n\n5. 승인 대상자 발표\n심사를 거쳐 승인된 결과가 고객님께 문자메시지로 발송됩니다.\n적립계좌 신청 내역은 마이페이지를 통해 확인 가능합니다.\n\n6. 적금 계좌 개설\n승인 후 지정 기간 내에 협력 은행의 비대면 계좌를 개설하여 적금에 가입합니다.\n\n7. 예술활동 확인내역서 제출\n24회차 내 총 4회간 예술활동 확인내역서를 제출하여야 합니다.\n\n8. 총 24회차 납입\n적립계좌를 개설한 청년예술인이 매달 적금 입금 시 지원금이 적립됩니다.\n\n9. 만기 시 지원금 지급\n※ 24개월 간 정상 납입한 회차에 비례하여 지원금을 지급합니다.";

    #[test]
    fn numbered_method_and_screening_read_as_one_ordered_procedure() {
        let s = procedure_steps(
            Some((ART_METHOD, "plcyAplyMthdCn")),
            Some((ART_SCREENING, "srngMthdCn")),
        );
        assert!(s.ordered);
        assert_eq!(
            titles(&s),
            [
                "예술활동증명확인",
                "구비서류 확인",
                "온라인 신청",
                "심사",
                "승인 대상자 발표",
                "적금 계좌 개설",
                "예술활동 확인내역서 제출",
                "총 24회차 납입",
                "만기 시 지원금 지급"
            ]
        );
        assert_eq!(
            s.steps[1].detail.as_deref(),
            Some("상품 및 구비서류를 확인하여 준비합니다.")
        );
        assert_eq!(s.steps[0].source, "plcyAplyMthdCn");
        assert_eq!(s.steps[3].source, "srngMthdCn");
        // A ※ note stays with its step, marked as a note.
        assert_eq!(
            s.steps[8].detail.as_deref(),
            Some("※ 24개월 간 정상 납입한 회차에 비례하여 지원금을 지급합니다.")
        );
    }

    // 온통청년 부산 전세피해 임차인 주거안정지원금 지원 (CRLF, circled numbers).
    #[test]
    fn circled_numbers_then_a_single_screening_line() {
        let s = procedure_steps(
            Some((
                "\r\n① 신청대상자 안내 문자\r\n② 주거안정지원금 신청\r\n③ 적합여부 및 서류 검토\r\n④ 지급결정 통보\r\n⑤ 계좌 입금\r\n",
                "plcyAplyMthdCn",
            )),
            Some(("- 지원금 지급대상자로 결정 시, 결정 통보 및 지급 안내 문자 발송", "srngMthdCn")),
        );
        assert!(s.ordered);
        assert_eq!(
            titles(&s),
            [
                "신청대상자 안내 문자",
                "주거안정지원금 신청",
                "적합여부 및 서류 검토",
                "지급결정 통보",
                "계좌 입금",
                "지원금 지급대상자로 결정 시, 결정 통보 및 지급 안내 문자 발송"
            ]
        );
    }

    // 온통청년 유망청년창업기업 보증 지원: bullets with "label: text".
    #[test]
    fn bullets_are_steps_but_not_ordered_and_split_at_the_label() {
        let s = segment(" ㅇ 신청·접수: 신보 영업점 방문 또는 온라인 플랫폼\n ㅇ 보증 심사: 기술성, 사업성, 대표자 경영 역량 및 신용·재무 상태 등 종합 평가\n ㅇ 보증서 발급 및 대출: 보증 승인 후 채권은행을 통해 대출 실행", "plcyAplyMthdCn");
        assert!(!s.ordered);
        assert_eq!(
            titles(&s),
            ["신청·접수", "보증 심사", "보증서 발급 및 대출"]
        );
        assert_eq!(
            s.steps[0].detail.as_deref(),
            Some("신보 영업점 방문 또는 온라인 플랫폼")
        );
    }

    // 온통청년 영암군 청년 자격증 응시료 지원사업: one ❍ per field, "-" alternatives under it.
    #[test]
    fn sub_bullets_stay_details_of_their_step() {
        let s = procedure_steps(
            Some(("  ❍ 신청방법 : 방문 및 이메일 접수\n   - 방  문 : 영암군청 인구청년과\n   - 이메일 : 담당자 이메일", "plcyAplyMthdCn")),
            Some(("  ❍ 서류심사\n     - 신청조건에 부합할 경우 매월 말 문자로 선정 안내", "srngMthdCn")),
        );
        assert_eq!(titles(&s), ["신청방법", "서류심사"]);
        assert_eq!(
            s.steps[0].detail.as_deref(),
            Some("방문 및 이메일 접수 · 방 문 : 영암군청 인구청년과 · 이메일 : 담당자 이메일")
        );
        assert_eq!(
            s.steps[1].detail.as_deref(),
            Some("신청조건에 부합할 경우 매월 말 문자로 선정 안내")
        );
        // Each field is one step: apply, then screening, in that order.
        assert!(s.ordered);
    }

    // 온통청년 청년전용 보증부 월세대출 srngMthdCn: an arrow chain.
    #[test]
    fn an_arrow_chain_is_an_ordered_procedure() {
        let s = segment("대출조건 확인 ▷ 대출신청 ▷ 자산심사 ▷ 자산심사 결과 정보 송신 ▷ 서류제출 및 추가심사 진행 ▷ 대출승인 및 실행", "srngMthdCn");
        assert!(s.ordered);
        assert_eq!(
            titles(&s),
            [
                "대출조건 확인",
                "대출신청",
                "자산심사",
                "자산심사 결과 정보 송신",
                "서류제출 및 추가심사 진행",
                "대출승인 및 실행"
            ]
        );
    }

    // 온통청년 대학생 천원의 아침밥 지원 확대: intro line, then an arrow chain over two lines.
    #[test]
    fn arrow_chain_across_lines_keeps_its_intro() {
        let s = segment("농림축산식품부 사업 추진일정에 따라 운영\n- (농식품부) 전국 대학 신청접수(전년도 ~12월) → 사업대상자 확정(전년도 12월 중) \n  → 사업대상 대학교 천원의 아침밥 운영(1.1.~12.18.) → 사업 결과 및 정산(당해년도 12월)", "plcyAplyMthdCn");
        assert_eq!(
            s.intro.as_deref(),
            Some("농림축산식품부 사업 추진일정에 따라 운영")
        );
        assert_eq!(
            titles(&s),
            [
                "(농식품부) 전국 대학 신청접수(전년도 ~12월)",
                "사업대상자 확정(전년도 12월 중)",
                "사업대상 대학교 천원의 아침밥 운영(1.1.~12.18.)",
                "사업 결과 및 정산(당해년도 12월)"
            ]
        );
        assert!(s.ordered);
    }

    // 온통청년 장기간부 도약적금: two ㅇ items; ①② cases (with arrows) and "-" lines nested under them.
    #[test]
    fn nested_cases_stay_under_their_top_level_item() {
        let s = segment("ㅇ 가입방법(군인공제회는 3.3.부터, IBK기업은행은 4.1.부터, 신한은행은 4.3.부터, 그외 금융기관은 26.5월부터 비대면가입 가능)\n ① 비대면 가입 : iMND복지포털 접속 가입자격확인서 발급→금융기관 앱 접속 후 적금 가입\n ② 대면 가입 : iMND복지포털 접속 가입자격확인서 발급→금융기관 방문 후 적금 가입\n군인공제회 회원 자격자만(회원퇴직급여 1구좌 1회 이상 납부한 사람) 가입 가능\nㅇ 대리가입\n  - 대리자 : 가족관계증명서 상 가족 중 1명\n  - 지참서류 : 가족관계증명서(주민번호 13자리 포함), 방문자 신분증, 가입자격확인서\n※ 가입자격확인서는 대상자가 직접 발행받아 가족에게 PDF파일 또는 출력본을 전달해야 합니다.", "plcyAplyMthdCn");
        assert!(!s.ordered);
        assert_eq!(titles(&s).len(), 2);
        assert!(s.steps[0].title.starts_with("가입방법(군인공제회는"));
        assert_eq!(
            s.steps[0].detail.as_deref(),
            Some("비대면 가입 : iMND복지포털 접속 가입자격확인서 발급→금융기관 앱 접속 후 적금 가입 · 대면 가입 : iMND복지포털 접속 가입자격확인서 발급→금융기관 방문 후 적금 가입 군인공제회 회원 자격자만(회원퇴직급여 1구좌 1회 이상 납부한 사람) 가입 가능")
        );
        assert_eq!(s.steps[1].title, "대리가입");
        assert_eq!(
            s.steps[1].detail.as_deref(),
            Some("대리자 : 가족관계증명서 상 가족 중 1명 · 지참서류 : 가족관계증명서(주민번호 13자리 포함), 방문자 신분증, 가입자격확인서 · ※ 가입자격확인서는 대상자가 직접 발행받아 가족에게 PDF파일 또는 출력본을 전달해야 합니다.")
        );
    }

    // 온통청년 청년어업인 어촌정착지원: one wrapped sentence, no structure.
    #[test]
    fn plain_text_is_one_step_never_split_at_line_wraps() {
        let s = segment("사업 신청자격과 요건을 갖춘 청년어업인은 신청서와 사업계획서를 작성하여\n주민등록 기준 거주지 관할 시·군·구에 제출", "plcyAplyMthdCn");
        assert_eq!(
            titles(&s),
            ["사업 신청자격과 요건을 갖춘 청년어업인은 신청서와 사업계획서를 작성하여 주민등록 기준 거주지 관할 시·군·구에 제출"]
        );
    }

    #[test]
    fn a_heading_word_before_the_list_is_dropped_and_entities_are_decoded() {
        // 온통청년 청년도전지원사업 (abridged at a step boundary).
        let s = segment("신청방법\n① 고용24 홈페이지 접속하여 회원가입 후 구직회원 전환\n② 고용24 홈페이지 접속하여 로그인 후 &lsquo;청년도전지원사업&rsquo; 신청", "plcyAplyMthdCn");
        assert_eq!(s.intro, None);
        assert_eq!(
            s.steps[1].title,
            "고용24 홈페이지 접속하여 로그인 후 ‘청년도전지원사업’ 신청"
        );
    }

    #[test]
    fn empty_dash_and_repeated_screening_add_nothing() {
        let method = "홈페이지 참조\n(https://www.arko.or.kr/main/arko)";
        let s = procedure_steps(
            Some((method, "plcyAplyMthdCn")),
            Some((method, "srngMthdCn")),
        );
        assert_eq!(s.steps.len(), 1);
        let s = procedure_steps(
            Some(("온라인 신청", "plcyAplyMthdCn")),
            Some(("-", "srngMthdCn")),
        );
        assert_eq!(titles(&s), ["온라인 신청"]);
        assert_eq!(procedure_steps(None, None), Steps::default());
        // 온통청년 천안형 채용연계프로그램: srngMthdCn "해당없음" is not a step.
        let s = procedure_steps(
            Some(("홈페이지 신청", "plcyAplyMthdCn")),
            Some(("해당없음", "srngMthdCn")),
        );
        assert_eq!(titles(&s), ["홈페이지 신청"]);
    }

    fn states(p: &Progress) -> Vec<StageState> {
        stages(p).into_iter().map(|s| s.state).collect()
    }
    use StageState::{Current as C, Done as D, Upcoming as U};

    #[test]
    fn a_guest_sees_every_stage_ahead_and_the_first_highlighted() {
        assert_eq!(states(&Progress::default()), [C, U, U, U]);
        let labels: Vec<_> = stages(&Progress::default())
            .into_iter()
            .map(|s| s.label)
            .collect();
        assert_eq!(labels, ["발견", "자격 확인", "서류 준비", "신청"]);
    }

    #[test]
    fn a_signed_in_viewer_has_found_it_and_checks_eligibility_next() {
        let p = Progress {
            discovered: true,
            ..Default::default()
        };
        assert_eq!(states(&p), [D, C, U, U]);
    }

    #[test]
    fn documents_are_current_while_partly_ticked_and_done_when_all_are() {
        let mut p = Progress {
            discovered: true,
            eligibility_confirmed: true,
            documents_total: 3,
            documents_done: 1,
            ..Default::default()
        };
        assert_eq!(states(&p), [D, D, C, U]);
        p.documents_done = 3;
        assert_eq!(states(&p), [D, D, D, C]);
    }

    #[test]
    fn ticking_documents_before_eligibility_moves_the_journey_on() {
        // Progress in a later stage means the earlier one was passed.
        let p = Progress {
            discovered: true,
            documents_total: 2,
            documents_done: 1,
            ..Default::default()
        };
        assert_eq!(states(&p), [D, D, C, U]);
    }

    #[test]
    fn handoff_makes_apply_current_and_a_report_completes_it() {
        let mut p = Progress {
            discovered: true,
            handed_off: true,
            ..Default::default()
        };
        assert_eq!(states(&p), [D, D, D, C]);
        p.reported_applied = true;
        assert_eq!(states(&p), [D, D, D, D]);
    }

    /// The future auto-apply path: a connector submission row drives 신청
    /// through the same function as a user's report.
    #[test]
    fn a_connector_submission_advances_apply_without_a_user_report() {
        let fixture = [
            ("queued", [D, D, D, C]),
            ("submitting", [D, D, D, C]),
            ("unknown", [D, D, D, C]),
            ("accepted", [D, D, D, D]),
        ];
        for (state, expected) in fixture {
            let p = Progress {
                discovered: true,
                submission: Submission::parse(state),
                ..Default::default()
            };
            assert_eq!(states(&p), expected, "{state}");
        }
        // A rejected, cancelled or expired submission received nothing.
        for state in ["rejected", "cancelled", "expired"] {
            let p = Progress {
                discovered: true,
                submission: Submission::parse(state),
                ..Default::default()
            };
            assert_eq!(states(&p), [D, C, U, U], "{state}");
        }
    }

    #[test]
    fn without_document_tasks_documents_stay_current_until_the_user_moves_on() {
        let p = Progress {
            discovered: true,
            eligibility_confirmed: true,
            ..Default::default()
        };
        assert_eq!(states(&p), [D, D, C, U]);
    }

    /// 복지로 중앙 공공분양 (NationalWelfaredetailedV001, 2026-10-31).
    #[test]
    fn listed_stages_read_action_and_institution() {
        let entries = [
            (Some("신청기관연락처목록"), "거주지 읍/면/동 주민센터, 한국토지주택공사, 지방공사(SH공사, 경기도시공사 등)에서 ‘서비스 신청’"),
            (Some("조사기관연락처목록"), "담당 시/군/구청 또는 한국토지주택공사, 지방공사(SH공사, 경기도시공사 등)에서 조사 및 심사"),
            (Some("결정기관연락처목록"), "담당 시/군/구청 또는 한국토지주택공사, 지방공사(SH공사, 경기도시공사 등)에서 보장 결정"),
            (Some("지급기관연락처목록"), "담당 시/군/구청 또는 한국토지주택공사, 지방공사(SH공사, 경기도시공사 등)에서 대상자에게 서비스 제공"),
            (Some("사후관리기관목록"), "담당 시/군/구청 또는 한국토지주택공사, 지방공사(SH공사, 경기도시공사 등)에서 서비스 제공 이후 대상자의 상황 관리"),
        ];
        let s = listed_stages(&entries, "applmetList");
        assert!(s.ordered);
        assert_eq!(
            titles(&s),
            [
                "서비스 신청",
                "조사 및 심사",
                "보장 결정",
                "대상자에게 서비스 제공",
                "서비스 제공 이후 대상자의 상황 관리"
            ]
        );
        assert_eq!(
            s.steps[0].detail.as_deref(),
            Some("거주지 읍/면/동 주민센터, 한국토지주택공사, 지방공사(SH공사, 경기도시공사 등)")
        );
        assert_eq!(s.steps[0].source, "applmetList");
    }

    /// 복지로 중앙 대중교통비 환급 지원(모두의카드): listed out of order, no 신청 stage.
    #[test]
    fn listed_stages_follow_the_process_order_not_the_source_order() {
        let entries = [
            (
                Some("사후관리기관목록"),
                "담당 시/군/구청 또는 한국교통안전공단에서 서비스 제공 이후 대상자의 상황 관리",
            ),
            (
                Some("조사기관연락처목록"),
                "담당 시/군/구청 또는 한국교통안전공단에서 조사 및 심사",
            ),
            (
                Some("결정기관연락처목록"),
                "담당 시/군/구청 또는 한국교통안전공단에서 보장 결정",
            ),
            (
                Some("지급기관연락처목록"),
                "담당 시/군/구청 또는 국가 ,지방자치단체에서 대상자에게 서비스 제공",
            ),
        ];
        let s = listed_stages(&entries, "applmetList");
        assert_eq!(
            titles(&s),
            [
                "조사 및 심사",
                "보장 결정",
                "대상자에게 서비스 제공",
                "서비스 제공 이후 대상자의 상황 관리"
            ]
        );
    }

    #[test]
    fn listed_stages_merge_one_action_from_two_institutions_and_skip_blanks() {
        let entries = [
            (
                Some("지급기관연락처목록"),
                "담당 시/군/구청 또는 광역노후준비지원센터에서 대상자에게 서비스 제공",
            ),
            (
                Some("지급기관연락처목록"),
                "담당 시/군/구청 또는 지역노후준비지원센터에서 대상자에게 서비스 제공",
            ),
            (None, "  "),
        ];
        let s = listed_stages(&entries, "applmetList");
        assert_eq!(titles(&s), ["대상자에게 서비스 제공"]);
        assert_eq!(s.steps[0].detail.as_deref(), Some("담당 시/군/구청 또는 광역노후준비지원센터 · 담당 시/군/구청 또는 지역노후준비지원센터"));
        assert_eq!(listed_stages(&[], "applmetList"), Steps::default());
    }

    /// Review findings: a one-line circled list, a list whose first line was
    /// trimmed by the source, a note-only text, a breadcrumb, and the first
    /// "에서" of a stage text.
    #[test]
    fn one_line_circled_list_is_a_list() {
        let s = segment("① 온라인 신청 ② 서류심사 ③ 선정", "plcyAplyMthdCn");
        assert_eq!(titles(&s), ["온라인 신청", "서류심사", "선정"]);
        assert!(s.ordered);
    }

    #[test]
    fn a_trimmed_first_line_stays_in_its_list() {
        let s = segment(
            "1. 신청서 작성\n  2. 서류 심사\n  3. 선정 통보",
            "plcyAplyMthdCn",
        );
        assert_eq!(titles(&s), ["신청서 작성", "서류 심사", "선정 통보"]);
    }

    #[test]
    fn a_note_only_text_is_kept_as_one_step() {
        let s = segment("※ 홈페이지 참조", "plcyAplyMthdCn");
        assert_eq!(titles(&s), ["※ 홈페이지 참조"]);
    }

    #[test]
    fn a_menu_breadcrumb_is_one_step_not_a_procedure() {
        let s = segment(
            "고용24 > 취업지원 > 청년취업역량 강화 프로그램",
            "plcyAplyMthdCn",
        );
        assert_eq!(s.steps.len(), 1);
    }

    #[test]
    fn listed_stage_splits_at_the_first_place_marker() {
        let s = listed_stages(
            &[(
                Some("신청기관연락처목록"),
                "행정복지센터에서 접수 후 온라인에서 제출",
            )],
            "applmetList",
        );
        assert_eq!(titles(&s), ["접수 후 온라인에서 제출"]);
        assert_eq!(s.steps[0].detail.as_deref(), Some("행정복지센터"));
    }

    #[test]
    fn a_programme_with_nothing_to_submit_does_not_hold_at_documents() {
        let p = Progress {
            discovered: true,
            eligibility_confirmed: true,
            documents_not_required: true,
            ..Default::default()
        };
        assert_eq!(states(&p), [D, D, D, C]);
    }

    #[test]
    fn a_recorded_preparing_status_puts_the_user_at_documents() {
        let p = Progress {
            discovered: true,
            preparing: true,
            ..Default::default()
        };
        assert_eq!(states(&p), [D, D, C, U]);
    }
}
