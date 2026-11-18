//! The final message, assembled from tool results with fixed templates.
//! No model wording: verdicts, counts and titles are copied from results,
//! and [`validate`] drops any citation or action that does not point at a
//! result of this run (spec §10).
use super::{
    goals::{GoalKind, Refusal, GOAL_CHIPS},
    AgentMessage, Citation, ConfirmationView, StepStatus, SuggestedAction, ToolResult,
};
use mazimi_core::agent::ToolName;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

const ACTION_TYPES: &[&str] = &[
    "open_program",
    "prepare_application",
    "confirm",
    "goal",
    "open_form_fill",
    "edit_profile",
    "pick_program",
    "ask_user",
];

/// The journey's form turn; the fast path routes it to `open_form_fill`.
pub const FORM_GOAL: &str = "이 혜택 서식 작성 도와줘";
/// The journey's official-page turn; the fast path routes it to the
/// `open_official_application` card (a client without the journey
/// capability gets the read-only page check).
pub const OFFICIAL_GOAL: &str = "이 혜택 공식 신청 페이지 열어줘";
/// The journey's last turn, after the official site; routed to
/// `record_application_status` (applied).
pub const APPLIED_GOAL: &str = "이 혜택 신청 완료로 기록해줘";
/// The state-aware alert button when the alert is on.
pub const PROGRAM_ALERT_OFF_GOAL: &str = "이 혜택 마감 알림 꺼줘";
/// The save toggle's turns (the programme on screen).
pub const SAVE_GOAL: &str = "이 혜택 저장해줘";
pub const UNSAVE_GOAL: &str = "이 혜택 저장 취소해줘";
/// The saved-list alert turns (no programme on screen).
pub const SAVED_ALERTS_ON_GOAL: &str = "저장한 혜택 알림 다 켜줘";
pub const SAVED_ALERTS_OFF_GOAL: &str = "저장한 혜택 알림 다 꺼줘";
/// The calendar button's turn.
pub const CALENDAR_GOAL: &str = "이 혜택 마감일 캘린더에 넣어줘";

/// The programme on screen and what the user has set on it; `None` when
/// it could not be read (then both directions are offered).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProgramState {
    pub id: Uuid,
    pub alert: Option<bool>,
    pub saved: Option<bool>,
    /// What can run on it now ([`ProgramFacts::can`]).
    pub facts: ProgramFacts,
}

/// What the deployment and the programme's state allow (QA B9, B14): the
/// one source of the answer's buttons, built on the planner's own tool set
/// ([`super::ToolFlags::allows`]). A button is offered only for an action
/// that can run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProgramFacts {
    pub flags: super::ToolFlags,
    /// Open now: the availability rule every programme read and write uses.
    pub open: bool,
    /// It has a deadline: what a 마감 알림 and a calendar date are about.
    pub has_deadline: bool,
    /// It has a form template the helper opens (the policy's templates).
    pub has_template: bool,
    /// `add_to_calendar` would card its deadline now ([`super::calendar::offers`]):
    /// the calendar button's condition is the tool's own.
    pub calendar_date: bool,
}

impl ProgramFacts {
    /// Unknown facts: everything offered (the state could not be read).
    pub const ANY: Self = Self {
        flags: super::ToolFlags::ALL,
        open: true,
        has_deadline: true,
        has_template: true,
        calendar_date: true,
    };

    /// The facts of a programme row read now (`today`: the Korean date).
    pub fn of(
        flags: super::ToolFlags,
        program: Option<&mazimi_core::models::Program>,
        open: bool,
        has_template: bool,
        today: chrono::NaiveDate,
    ) -> Self {
        Self {
            flags,
            open,
            has_deadline: program.is_some_and(|p| p.effective_deadline().is_some()),
            has_template,
            calendar_date: program.is_some_and(|p| super::calendar::offers(p, today)),
        }
    }

    /// Whether `tool` can run on this programme now.
    pub fn can(&self, tool: ToolName) -> bool {
        self.flags.allows(tool)
            && match tool {
                ToolName::SetAlert => self.open && self.has_deadline,
                ToolName::AddToCalendar => self.calendar_date,
                ToolName::OpenFormFill => self.open && self.has_template,
                ToolName::Bookmark
                | ToolName::GetPreparation
                | ToolName::ToggleChecklistItem
                | ToolName::OpenOfficialApplication => self.open,
                _ => true,
            }
    }
}

/// What this run knows about the programme's official page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageRead {
    /// Read and open: the page and status routes too.
    Open,
    /// Read and closed or stale (guarded): no route into applying.
    Closed,
    /// Not read (a refusal, the fallback): no page or status route unless
    /// the goal names it; the checklist stays.
    NotRead,
}

/// The write buttons for the programme on screen (round 12): a strict card
/// gate can only miss, so every write type the goal could have meant has a
/// button here. Alert and save show the current state; "신청 상태 기록"
/// opens the status picker, which writes only on the user's own tap; the
/// form helper is a goal that plans its card.
pub fn program_buttons(
    state: &ProgramState,
    alert_declined: bool,
    page: PageRead,
) -> Vec<SuggestedAction> {
    let open = page == PageRead::Open;
    let goal = |label: &str, goal: &str, kind: &str| SuggestedAction {
        kind: "goal".into(),
        label: label.into(),
        payload: json!({"goal": goal, "goal_kind": kind}),
    };
    let can = |tool: ToolName| state.facts.can(tool);
    let mut buttons = Vec::new();
    // A declined alert hides only the button that would turn it on: the
    // opposite direction is what the user wants (batch 1: an alert-off
    // request read as a decline left no alert button).
    if can(ToolName::SetAlert) && !alert_declined && state.alert != Some(true) {
        buttons.push(goal(
            "마감 알림 켜기",
            super::answer::PROGRAM_ALERT_GOAL,
            "enable_alerts",
        ));
    }
    // Turning an alert off runs on any programme with one on (closed or
    // undated too): only the deployment's flag.
    if state.facts.flags.allows(ToolName::SetAlert) && state.alert != Some(false) {
        buttons.push(goal(
            "마감 알림 끄기",
            PROGRAM_ALERT_OFF_GOAL,
            "disable_alerts",
        ));
    }
    if can(ToolName::Bookmark) && state.saved != Some(true) {
        buttons.push(goal("저장하기", SAVE_GOAL, "bookmark"));
    }
    if can(ToolName::Bookmark) && state.saved != Some(false) {
        buttons.push(goal("저장 취소", UNSAVE_GOAL, "bookmark"));
    }
    if can(ToolName::OpenFormFill) {
        buttons.push(goal("신청서 작성 도우미", FORM_GOAL, "form_fill"));
    }
    if can(ToolName::AddToCalendar) {
        buttons.push(goal("마감일 캘린더에 넣기", CALENDAR_GOAL, "calendar"));
    }
    if open && can(ToolName::OpenOfficialApplication) {
        buttons.push(goal(
            "공식 신청 페이지",
            OFFICIAL_GOAL,
            "official_application",
        ));
    }
    // No way into the application screen for a page that is not open.
    if open && can(ToolName::GetPreparation) {
        buttons.push(status_route(state.id));
    }
    // "신청 완료 기록" on every programme answer (holdout v2: "mark as
    // applied" had no button off the journey). Its goal plans exactly the
    // applied status card; the user confirms it.
    if can(ToolName::RecordApplicationStatus) {
        buttons.push(goal("신청 완료 기록", APPLIED_GOAL, "record_status"));
    }
    // The checklist screen: a task is checked only on the user's own tap
    // there (understanding step 0: no checklist miss is a dead end). Not
    // into the application screen of a page this run read as closed.
    if page != PageRead::Closed && can(ToolName::ToggleChecklistItem) {
        buttons.push(checklist_route(state.id));
    }
    // A profile change goes to the profile screen (writes on the user's tap).
    buttons.push(SuggestedAction {
        kind: "edit_profile".into(),
        label: "프로필 수정".into(),
        payload: json!({}),
    });
    buttons
}

/// The route to the checklist screen ("체크리스트").
fn checklist_route(id: Uuid) -> SuggestedAction {
    SuggestedAction {
        kind: "prepare_application".into(),
        label: "체크리스트".into(),
        payload: json!({"program_id": id, "focus": "checklist"}),
    }
}

/// The route to the status picker ("신청 상태 기록").
fn status_route(id: Uuid) -> SuggestedAction {
    SuggestedAction {
        kind: "prepare_application".into(),
        label: "신청 상태 기록".into(),
        payload: json!({"program_id": id, "focus": "status"}),
    }
}

/// The saved-list alert turn for an off-screen goal that asks for an alert
/// (a request, or unclear below the card bar): "저장한 혜택 알림 다 켜기" or
/// "… 끄기" by the read's direction. A decline hides only the "on" turn.
fn saved_alert_turn(
    goal: &str,
    declined: bool,
    context: super::goals::GoalContext,
) -> Option<SuggestedAction> {
    // The run's context, read off screen (the saved-list turn).
    let off_screen = super::goals::GoalContext {
        has_program: false,
        ..context
    };
    let alert = super::intent::read(goal, off_screen, None)
        .into_iter()
        .find(|i| {
            i.write == super::intent::WriteType::Alert
                && matches!(
                    i.act,
                    super::intent::Act::Request | super::intent::Act::Unclear
                )
                && (!declined || i.on == Some(false))
        })?;
    let (label, turn, kind) = if alert.on == Some(false) {
        (
            "저장한 혜택 알림 다 끄기",
            SAVED_ALERTS_OFF_GOAL,
            "disable_alerts",
        )
    } else {
        (
            "저장한 혜택 알림 다 켜기",
            SAVED_ALERTS_ON_GOAL,
            "enable_alerts",
        )
    };
    Some(SuggestedAction {
        kind: "goal".into(),
        label: label.into(),
        payload: json!({"goal": turn, "goal_kind": kind}),
    })
}

/// The goal names an application status (합격, 불합격, 신청 완료, 포기 …).
fn names_status(goal: &str) -> bool {
    let plain: String = goal.split_whitespace().collect();
    [
        "합격",
        "불합격",
        "신청완료",
        "접수완료",
        "제출완료",
        "포기",
        "선정",
        "탈락",
        "당첨",
    ]
    .iter()
    .any(|w| plain.contains(w))
}

/// Add `extra` to `actions`, skipping a goal or route already offered and
/// a write type already carded in this run.
fn add_buttons(
    actions: &mut Vec<SuggestedAction>,
    extra: Vec<SuggestedAction>,
    carded: &HashSet<String>,
) {
    for button in extra {
        let written = match button.payload["goal"].as_str() {
            Some(g) if g == super::answer::PROGRAM_ALERT_GOAL || g == PROGRAM_ALERT_OFF_GOAL => {
                carded.contains("set_alert")
            }
            Some(g) if g == SAVE_GOAL || g == UNSAVE_GOAL => carded.contains("bookmark"),
            Some(g) if g == FORM_GOAL => carded.contains("open_form_fill"),
            Some(g) if g == CALENDAR_GOAL => carded.contains("add_to_calendar"),
            Some(g) if g == OFFICIAL_GOAL => carded.contains("open_official_application"),
            Some(g) if g == APPLIED_GOAL => carded.contains("record_application_status"),
            _ => false,
        };
        let shown = (button.kind == "edit_profile"
            && actions.iter().any(|a| a.kind == "edit_profile"))
            || actions.iter().any(|a| {
                a.kind == button.kind
                    && a.payload["goal"] == button.payload["goal"]
                    && a.payload["focus"] == button.payload["focus"]
                    && a.label == button.label
            })
            || button.payload["goal"]
                .as_str()
                .is_some_and(|g| actions.iter().any(|a| a.payload["goal"] == g));
        if !written && !shown {
            actions.push(button);
        }
    }
}

fn goal_chips(has_program: bool) -> Vec<SuggestedAction> {
    GOAL_CHIPS
        .iter()
        .filter(|(_, _, needs_program)| has_program || !needs_program)
        .map(|(kind, goal, _)| SuggestedAction {
            kind: "goal".into(),
            label: (*goal).into(),
            payload: json!({"goal": goal, "goal_kind": kind}),
        })
        .collect()
}

/// The display title of a result's programme ([`mazimi_core::title_display`]).
fn title_of(result: &ToolResult) -> Option<String> {
    result
        .data
        .get("title_display")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            result
                .data
                .get("title")
                .and_then(Value::as_str)
                .map(mazimi_core::title_display::title_display)
        })
}

pub use super::answer::fit_reasons;

fn count(result: &ToolResult, key: &str) -> usize {
    result
        .data
        .get(key)
        .and_then(Value::as_array)
        .map_or(0, Vec::len)
}

/// One sentence per result, in plan order.
fn sentence(result: &ToolResult) -> Option<String> {
    if let Some(error) = &result.error {
        return Some(error.message.clone());
    }
    let title = title_of(result).unwrap_or_else(|| "이 혜택".into());
    let title = title.as_str();
    Some(match result.tool {
        ToolName::SearchBenefits => {
            let required = count(result, "required_profile_fields");
            if required > 0 {
                "혜택을 찾으려면 프로필에 출생연도와 거주 지역이 필요해요.".into()
            } else if result.data["not_found"].as_bool().unwrap_or(false) {
                "조건에 맞는 후보를 찾지 못했어요. 조건을 바꿔 다시 찾아볼 수 있어요.".into()
            } else {
                format!("후보 {}개를 찾았어요.", count(result, "results"))
            }
        }
        ToolName::CheckEligibility => format!(
            "‘{title}’: {}",
            result.data["summary"].as_str().unwrap_or("조건을 확인했어요.")
        ),
        ToolName::AnswerFromNotice => {
            let answer = &result.data["answer"];
            let documents = &result.data["documents"];
            if documents.is_object() {
                let mut line = documents_sentence(documents, title);
                // The other topics the question asked keep their quotes.
                let asked_others = answer["topics"]
                    .as_array()
                    .is_some_and(|t| t.iter().any(|t| t != "documents"));
                let others = answer["passages"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|p| asked_others && p["section"] != "documents")
                    .count();
                if others > 0 {
                    line.push_str(&format!(
                        " 다른 질문은 공고 원문에서 관련 문장 {others}개를 그대로 옮겼어요."
                    ));
                }
                return Some(line);
            }
            if answer["unsupported"].as_bool().unwrap_or(false) {
                "그 질문은 공고 문장으로 답할 수 없어요.".into()
            } else if answer["not_found"].as_bool().unwrap_or(false) {
                format!("‘{title}’ 공고에서 관련 문장을 찾지 못했어요. 공식 공고를 확인해 주세요.")
            } else {
                format!(
                    "‘{title}’ 공고 원문에서 관련 문장 {}개를 그대로 옮겼어요.",
                    answer["passages"].as_array().map_or(0, Vec::len)
                )
            }
        }
        ToolName::GetPreparation => {
            let remaining = result.data["remaining"].as_u64().unwrap_or(0);
            let total = result.data["total"].as_u64().unwrap_or(0);
            match result.data["next_task"].as_str() {
                Some(next) if remaining > 0 => format!(
                    "‘{title}’ 준비 항목 {total}개 중 {remaining}개가 남았어요. 다음은 ‘{next}’이에요."
                ),
                _ => format!(
                    "‘{title}’ 준비 항목을 모두 확인했어요."
                ),
            }
        }
        ToolName::Compare => format!(
            "혜택 {}개를 조건·마감·준비 상황으로 나란히 놓았어요. 중복 수혜 가능 여부는 판단하지 않아요.",
            count(result, "programs")
        ),
        ToolName::Deadlines => {
            let items: Vec<&Value> = result.data["items"].as_array().into_iter().flatten().collect();
            if items.is_empty() {
                "마감이 가까운 저장 혜택이 없어요.".into()
            } else {
                // One line per programme, as the search answer (QA: #04 named
                // no programme in its text).
                let mut text = format!("저장·준비 중인 혜택에서 마감이 가까운 {}개예요.", items.len());
                for (i, item) in items.iter().enumerate() {
                    let when = item["deadline_at"]
                        .as_str()
                        .and_then(super::answer::month_day)
                        .map_or_else(|| "마감일 정보 없음".to_owned(), |d| format!("마감 {d}"));
                    text.push_str(&format!(
                        "\n{}. {} · {when}",
                        i + 1,
                        item["title"].as_str().map(mazimi_core::title_display::title_display).unwrap_or_default()
                    ));
                }
                text
            }
        }
        ToolName::OpenFormFill => {
            let forms = count(result, "forms");
            if let Some(message) = result.data["card_message"].as_str() {
                message.to_owned()
            } else if forms == 0 {
                format!("‘{title}’: 이 혜택은 아직 신청서 미리 채우기를 지원하지 않아요. 복사용 안내와 공식 신청처의 양식을 확인해 주세요.")
            } else {
                // What approval fills is the card's (its 승인하면 cell): the
                // answer says only what the card does not, once.
                format!("‘{title}’ 양식 {forms}개를 작성 도우미로 열 수 있어요. 입력값은 기기에만 남아요.")
            }
        }
        ToolName::CheckOfficialPage => official_page_sentence(result, title),
        ToolName::DocumentIssueLink => {
            let documents = result.data["documents"].as_array().cloned().unwrap_or_default();
            let unknown: Vec<&str> = documents
                .iter()
                .filter(|d| d["known"] != true)
                .filter_map(|d| d["name"].as_str())
                .collect();
            match (documents.len(), unknown.as_slice()) {
                (0, _) => format!("‘{title}’ 준비 목록에 발급받을 서류가 없어요."),
                (1, [name]) => format!(
                    "‘{name}’은(는) 발급처를 안내할 수 없어요. 공고문에서 발급 기관을 확인해 주세요."
                ),
                (n, []) => format!(
                    "서류 {n}개의 발급처를 정리했어요. 로그인과 발급은 공식 사이트에서 직접 해 주세요."
                ),
                (n, unknown) => format!(
                    "서류 {n}개의 발급처를 정리했어요. {}개는 발급처를 안내할 수 없어요. 공고문에서 발급 기관을 확인해 주세요.",
                    unknown.len()
                ),
            }
        }
        ToolName::MatchVaultDocuments => {
            if result.data["no_document_list"].as_bool().unwrap_or(false) {
                format!("‘{title}’은(는) 확인된 서류 목록이 아직 없어 준비 목록과 비교하지 못했어요. 공식 공고의 제출 서류를 확인해 주세요.")
            } else {
                let names = |key: &str| -> Vec<String> {
                    result.data[key]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect()
                };
                let (have, check, missing) = (names("have"), names("needs_check"), names("missing"));
                let total = have.len() + check.len() + missing.len();
                // Counts on the checklist only, worded so they cannot
                // contradict the card, which adds the vault on this device
                // (re-review H1).
                let mut line = format!(
                    "‘{title}’ 필요 서류 {total}개 중 준비 목록 기준으로 완료 표시 {}개, 확인 필요 {}개, 미완료 {}개예요.",
                    have.len(),
                    check.len(),
                    missing.len()
                );
                line.push_str(" 보관함 서류와 맞춘 결과는 아래 카드에서 이 기기 기준으로 보여 드려요.");
                line
            }
        }
        ToolName::RequestSignature
        | ToolName::OpenOfficialApplication
        | ToolName::Bookmark
        | ToolName::SetAlert
        | ToolName::ToggleChecklistItem
        | ToolName::RecordApplicationStatus
        | ToolName::ProfilePatch
        | ToolName::AddToCalendar => match result.status {
            // A handoff with nothing to open says only why.
            StepStatus::Skipped if result.data["skipped"].is_string() => result.data["already"]
                .as_str()
                .unwrap_or("이 단계는 건너뛰었어요.")
                .to_owned(),
            StepStatus::Skipped => format!(
                "‘{title}’: {}",
                result.data["already"]
                    .as_str()
                    .unwrap_or("이미 요청한 상태예요.")
            ),
            _ => return None,
        },
    })
}

/// The required documents of one programme, every item named, from the one
/// source the detail page and the checklist use (QA B1).
fn documents_sentence(documents: &Value, title: &str) -> String {
    if documents["none_required"].as_bool().unwrap_or(false) {
        return format!("‘{title}’ 공고상 별도 제출 서류는 없어요.");
    }
    let items: Vec<String> = documents["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|d| {
            let name = d["name"].as_str()?;
            Some(if d["required"] == false {
                format!("{name}(선택)")
            } else {
                name.to_owned()
            })
        })
        .collect();
    let list: String = items
        .iter()
        .enumerate()
        .map(|(i, name)| format!("\n{}. {name}", i + 1))
        .collect();
    let mut line = format!(
        "‘{title}’ 제출 서류는 공고 기준 {}개예요.{list}",
        items.len()
    );
    if documents["notes"].as_array().is_some_and(|n| !n.is_empty()) {
        // The notice's own notes, not an example of what they might say
        // (PR #20 review M3).
        line.push_str("\n공고에 유의사항이 있어요. 원문을 확인해 주세요.");
    }
    line
}

/// The live check in plain words. Never says an application is open for
/// sure: the page's own wording is quoted as evidence instead.
fn official_page_sentence(result: &ToolResult, title: &str) -> String {
    // No 확인일 or source text in the answer (release/v1-client removed
    // evidence and source UI): the page is "지금 확인한" one.
    let mut line = match result.data["live_status"].as_str() {
        Some("closed") => format!(
            "‘{title}’ 지금 확인한 공식 페이지에는 접수가 마감된 것으로 나와요. 마지미에 저장된 정보와 달라요. 신청하기 전에 공식 페이지를 꼭 확인해 주세요."
        ),
        Some("upcoming") => format!("‘{title}’ 지금 확인한 공식 페이지에는 아직 접수 전으로 나와요."),
        Some("open") => format!(
            "‘{title}’ 지금 확인한 공식 페이지에 접수 중으로 보이는 문구가 있어요. 자격과 세부 조건은 따로 확인해야 해요."
        ),
        Some("unknown") => format!(
            "‘{title}’ 지금 확인한 공식 페이지에서 접수 상태를 분명히 읽지 못했어요. 원문 인용을 확인해 주세요."
        ),
        _ => {
            return format!(
                "‘{title}’: 지금 공식 페이지를 확인하지 못했어요. 공식 페이지 링크에서 직접 확인해 주세요."
            )
        }
    };
    let mut differences: Vec<String> = Vec::new();
    for change in result.data["changes"].as_array().into_iter().flatten() {
        match change["field"].as_str() {
            Some("deadline") => differences.push(format!(
                "마감일(마지미 {} → 공식 {})",
                change["ours"].as_str().unwrap_or("정보 없음"),
                change["live"].as_str().unwrap_or("")
            )),
            Some("attachments_added") => differences.push(format!(
                "마지미에 없는 첨부 {}개",
                change["live"].as_array().map_or(0, Vec::len)
            )),
            Some("attachments_missing") => differences.push(format!(
                "페이지에 없는 첨부 {}개",
                change["ours"].as_array().map_or(0, Vec::len)
            )),
            _ => {}
        }
    }
    if !differences.is_empty() {
        line.push_str(&format!(
            " 마지미 정보와 다른 점: {}.",
            differences.join(", ")
        ));
    }
    line
}

/// Programmes whose official page was read and did not show the intake
/// open: no other sentence about them may read as "you can apply" from our
/// stored data. A page that could not be checked compared nothing.
fn live_not_open(results: &[ToolResult]) -> HashSet<Uuid> {
    results
        .iter()
        .filter(|r| {
            r.tool == ToolName::CheckOfficialPage
                && r.ok
                && matches!(
                    r.data["live_status"].as_str(),
                    Some("closed" | "upcoming" | "unknown")
                )
        })
        .filter_map(|r| r.program_id)
        .collect()
}

/// The sentence of one result, with the stale-page guard applied.
fn line_for(r: &ToolResult, guarded: &HashSet<Uuid>, stale_guard: bool) -> Option<String> {
    let about_guarded = r.program_id.is_none_or(|id| guarded.contains(&id));
    // A rule pass about a programme whose page is not open says so,
    // never as "you can apply" (code review M3).
    if stale_guard
        && about_guarded
        && r.ok
        && r.tool == ToolName::CheckEligibility
        && r.data["status"] == "pass"
    {
        let title = title_of(r).unwrap_or_else(|| "이 혜택".into());
        return Some(format!(
            "‘{title}’: 저장한 정보와 맞는 조건이 있어도, 공식 페이지 기준으로는 지금 접수 중이 아니에요."
        ));
    }
    let line = sentence(r)?;
    let claims =
        (line.contains("신청 가능") || line.contains("신청할 수 있")) && !line.contains("여부");
    (!(stale_guard && claims && about_guarded && r.tool != ToolName::CheckOfficialPage))
        .then_some(line)
}

/// Compose the message for a planned run.
pub fn compose(
    results: &[ToolResult],
    confirmations: &[ConfirmationView],
    has_program: bool,
) -> AgentMessage {
    let context = super::goals::GoalContext {
        has_program,
        ..Default::default()
    };
    compose_for(results, confirmations, context, "", None)
}

/// [`compose`] for a goal: the search answer names who the goal says the
/// user is ("대학생") in its condition line.
pub fn compose_for(
    results: &[ToolResult],
    confirmations: &[ConfirmationView],
    context: super::goals::GoalContext,
    goal: &str,
    state: Option<ProgramState>,
) -> AgentMessage {
    let guarded = live_not_open(results);
    let stale_guard = !guarded.is_empty();
    // Each search answers with its relaxed re-searches (`relaxes` = the seq
    // it retries), and the eligibility checks of the rows it names are
    // folded into those rows.
    let searches: Vec<&ToolResult> = results
        .iter()
        .filter(|r| r.tool == ToolName::SearchBenefits)
        .collect();
    let group = |root: &ToolResult| -> Vec<&ToolResult> {
        searches
            .iter()
            .copied()
            .filter(|r| r.seq == root.seq || r.data["relaxes"].as_i64() == Some(root.seq as i64))
            .collect()
    };
    let named: HashSet<Uuid> = searches
        .iter()
        .filter(|r| r.data["relaxes"].is_null())
        .flat_map(|root| super::answer::named_ids(&group(root)))
        .collect();
    let checks: HashMap<Uuid, &ToolResult> = results
        .iter()
        .filter(|r| r.tool == ToolName::CheckEligibility && r.ok)
        .filter_map(|r| Some((r.program_id?, r)))
        .filter(|(id, _)| named.contains(id))
        .collect();
    let mut lines: Vec<String> = results
        .iter()
        .flat_map(|r| {
            if r.tool == ToolName::SearchBenefits && r.error.is_none() {
                if !r.data["relaxes"].is_null() {
                    return Vec::new();
                }
                return super::answer::search_lines(&group(r), &checks, goal);
            }
            if r.tool == ToolName::CheckEligibility
                && r.program_id.is_some_and(|id| checks.contains_key(&id))
            {
                return Vec::new();
            }
            line_for(r, &guarded, stale_guard).into_iter().collect()
        })
        .collect();
    // Grounding: a line stating a name, date, D-day or amount that no
    // result of this run carries is not sent.
    let facts = super::answer::facts(results);
    let before = lines.len();
    lines.retain(|line| super::answer::ungrounded_with(line, &facts).is_empty());
    let dropped = before - lines.len();
    if dropped > 0 {
        tracing::warn!(dropped, "agent answer lines failed the grounding check");
    }
    let pending = confirmations
        .iter()
        .filter(|c| c.status == "pending")
        .count();
    if pending > 0 {
        // The one statement that nothing changes before 승인: the card
        // under it does not repeat it.
        lines.push(format!(
            "승인이 필요한 변경 {pending}건이 있어요. 승인하기 전에는 아무것도 바뀌지 않아요."
        ));
    }
    if lines.is_empty() {
        lines.push("요청을 처리했어요.".into());
    }
    // The rules decide; the eligibility tool's status is the verdict (of
    // one programme: a search answers about several, so none).
    let verdict = rule_verdict(results).to_owned();
    let citations = results
        .iter()
        .flat_map(|r| {
            r.evidence.iter().map(move |e| Citation {
                evidence_id: e.id.clone(),
                program_id: e.program_id,
                source_label: e.source_label.clone(),
                source_url: e.source_url.clone(),
                captured_at: e.captured_at,
                claim_ids: vec![claim(r.tool).into()],
            })
        })
        .take(12)
        .collect();
    let mut missing: Vec<String> = Vec::new();
    for r in results {
        for key in ["missing_fields", "required_profile_fields"] {
            for field in r.data[key].as_array().into_iter().flatten() {
                if let Some(f) = field.as_str() {
                    if !missing.iter().any(|m| m == f) {
                        missing.push(f.to_owned());
                    }
                }
            }
        }
    }
    missing.truncate(2);
    let mut actions: Vec<SuggestedAction> = confirmations
        .iter()
        .filter(|c| c.status == "pending")
        .map(|c| SuggestedAction {
            kind: "confirm".into(),
            label: c.preview.action.clone(),
            payload: json!({"confirmation_id": c.id}),
        })
        .collect();
    for r in results.iter().filter(|r| r.ok) {
        match r.tool {
            ToolName::CheckEligibility | ToolName::AnswerFromNotice => {
                // No way into preparing a programme whose page is not open.
                if let Some(id) = r.program_id.filter(|id| !guarded.contains(id)) {
                    actions.push(SuggestedAction {
                        kind: "prepare_application".into(),
                        label: "신청 준비 화면 열기".into(),
                        payload: json!({"program_id": id}),
                    });
                }
            }
            // A pending card is the way into the form helper; direct links
            // only when no card was made.
            ToolName::OpenFormFill if r.status == StepStatus::Ok => {
                for form in r.data["forms"].as_array().into_iter().flatten().take(2) {
                    actions.push(SuggestedAction {
                        kind: "open_form_fill".into(),
                        label: format!(
                            "‘{}’ 작성 도우미 열기",
                            form["label"].as_str().unwrap_or("양식")
                        ),
                        payload: json!({"program_id": r.program_id, "resource_id": form["resource_id"]}),
                    });
                }
            }
            // The next turns of the 신청 준비 journey, planned without a model.
            // Once per message, whatever the expansion. The handoffs are
            // buttons: the journey proposes no handoff card the goal did not
            // name (round 10), and a handoff already carded is no button.
            // Once: keyed on the button always pushed (re-review 10 L2).
            ToolName::GetPreparation
                if r.program_id.is_some()
                    && !actions.iter().any(|a| a.payload["goal"] == APPLIED_GOAL) =>
            {
                let carded = |tool: ToolName| results.iter().any(|x| x.tool == tool);
                // No form helper button without a template to open (QA B14).
                if !carded(ToolName::OpenFormFill) && r.data["has_template"] != false {
                    actions.push(SuggestedAction {
                        kind: "goal".into(),
                        label: "신청서 작성 도우미".into(),
                        payload: json!({"goal": FORM_GOAL, "goal_kind": "form_fill"}),
                    });
                }
                if !carded(ToolName::OpenOfficialApplication) {
                    actions.push(SuggestedAction {
                        kind: "goal".into(),
                        label: "공식 신청 페이지".into(),
                        payload: json!({"goal": OFFICIAL_GOAL, "goal_kind": "official_application"}),
                    });
                }
                actions.push(SuggestedAction {
                    kind: "goal".into(),
                    label: "신청 완료 기록".into(),
                    payload: json!({"goal": APPLIED_GOAL, "goal_kind": "record_status"}),
                });
            }
            _ => {}
        }
    }
    actions.extend(super::answer::next_actions(results));
    // Not after the user declined it ("알림은 필요 없어": re-review 4 L1).
    let declined = super::intent::declines(goal, super::intent::WriteType::Alert);
    // Every answer with a programme on screen carries its write buttons
    // (also after a search or a digest: a strict miss is never a dead end),
    // with the alert from this run's own read when there is one.
    if let Some(mut st) = state {
        if let Some(on) = results
            .iter()
            .filter(|r| r.ok && r.program_id == Some(st.id))
            .find_map(|r| r.data["alert_enabled"].as_bool())
        {
            st.alert = Some(on);
        }
        let carded: HashSet<String> = confirmations
            .iter()
            .filter(|c| c.status == "pending")
            .map(|c| c.tool.clone())
            .chain(
                results
                    .iter()
                    .filter(|r| r.status == StepStatus::AwaitingConfirmation)
                    .map(|r| r.tool.as_str().to_owned()),
            )
            .collect();
        add_buttons(
            &mut actions,
            program_buttons(
                &st,
                declined,
                if guarded.contains(&st.id) {
                    PageRead::Closed
                } else {
                    PageRead::Open
                },
            ),
            &carded,
        );
    } else if !declined {
        actions.extend(super::answer::alert_offer(results));
    }
    // R9: no programme on screen and the goal asks for an alert (the saved
    // list's, a search's): the saved-list alert turn as a button, so an
    // off-screen alert request the plan did not carry is never a dead end.
    if state.is_none() && !confirmations.iter().any(|c| c.tool == "set_alert") {
        add_buttons(
            &mut actions,
            saved_alert_turn(goal, declined, context)
                .into_iter()
                .collect(),
            &HashSet::new(),
        );
    }
    let asked = super::answer::ask_user(results);
    if let Some(ask) = &asked {
        actions.push(ask.clone());
    }
    if !missing.is_empty() && asked.is_none() {
        actions.push(SuggestedAction {
            kind: "edit_profile".into(),
            label: "프로필 정보 채우기".into(),
            payload: json!({"fields": missing}),
        });
    }
    // Conditional, informative warnings only: the not-final note is said
    // once, in the search answer itself (`answer::search_lines`).
    let mut warnings: Vec<String> = Vec::new();
    if stale_guard {
        warnings.push(
            "공식 페이지와 마지미 정보가 다를 수 있어요. 신청하기 전에 공식 페이지를 확인해 주세요."
                .to_owned(),
        );
    }
    // One line per sentence or row: a short, structured answer, not one
    // dense paragraph.
    let mut message = AgentMessage {
        answer: lines.join("\n"),
        verdict,
        citations,
        suggested_actions: actions,
        missing_profile_fields: missing,
        warnings,
        topic_line: searches
            .iter()
            .rev()
            .find(|r| r.ok)
            .and_then(|r| super::answer::topic_line(&r.data, goal)),
        question: asked.and_then(|a| a.payload["question"].as_str().map(str::to_owned)),
    };
    validate(&mut message, results, confirmations);
    if message.suggested_actions.is_empty() {
        message.suggested_actions = goal_chips(context.has_program);
    }
    message
}

fn claim(tool: ToolName) -> &'static str {
    match tool {
        ToolName::CheckEligibility => "eligibility",
        ToolName::AnswerFromNotice => "notice",
        ToolName::GetPreparation => "documents",
        ToolName::SearchBenefits => "eligibility",
        ToolName::CheckOfficialPage => "official_page",
        _ => "source",
    }
}

/// Response validator: citations must name evidence of this run; actions
/// must be registered types pointing at programmes or confirmations of this
/// run; the verdict is the rule result. No URL is added here.
pub fn validate(
    message: &mut AgentMessage,
    results: &[ToolResult],
    confirmations: &[ConfirmationView],
) {
    let evidence: HashSet<&str> = results
        .iter()
        .flat_map(|r| r.evidence.iter().map(|e| e.id.as_str()))
        .collect();
    message
        .citations
        .retain(|c| evidence.contains(c.evidence_id.as_str()));
    let programs: HashSet<Uuid> = results
        .iter()
        .flat_map(|r| {
            r.program_id.into_iter().chain(
                ["results", "programs", "items"]
                    .iter()
                    .flat_map(|k| r.data[*k].as_array().into_iter().flatten())
                    .filter_map(|v| v["program_id"].as_str()?.parse().ok()),
            )
        })
        .collect();
    let confirmation_ids: HashSet<Uuid> = confirmations.iter().map(|c| c.id).collect();
    message.suggested_actions.retain(|a| {
        ACTION_TYPES.contains(&a.kind.as_str())
            && a.payload["program_id"]
                .as_str()
                .and_then(|s| s.parse::<Uuid>().ok())
                .is_none_or(|id| programs.contains(&id))
            && a.payload["program_ids"].as_array().is_none_or(|ids| {
                ids.len() <= 5
                    && ids.iter().all(|v| {
                        v.as_str()
                            .and_then(|s| s.parse::<Uuid>().ok())
                            .is_some_and(|id| programs.contains(&id))
                    })
            })
            && a.payload["confirmation_id"]
                .as_str()
                .and_then(|s| s.parse::<Uuid>().ok())
                .is_none_or(|id| confirmation_ids.contains(&id))
            && !a.payload.to_string().contains("://")
    });
    message.verdict = rule_verdict(results).to_owned();
}

/// The rule verdict of the one programme a run checked; `not_evaluated`
/// when it checked none, several, or ones a search found (code review M5:
/// a search next to a check of the programme on screen keeps its verdict).
fn rule_verdict(results: &[ToolResult]) -> &str {
    let found: HashSet<Uuid> = results
        .iter()
        .filter(|r| r.tool == ToolName::SearchBenefits)
        .flat_map(|r| r.data["results"].as_array().into_iter().flatten())
        .filter_map(|row| row["program_id"].as_str()?.parse().ok())
        .collect();
    let checked: Vec<&ToolResult> = results
        .iter()
        .filter(|r| r.tool == ToolName::CheckEligibility && r.ok)
        .collect();
    match checked.as_slice() {
        [one] if one.program_id.is_none_or(|id| !found.contains(&id)) => {
            one.data["status"].as_str().unwrap_or("not_evaluated")
        }
        _ => "not_evaluated",
    }
}

/// A refusal's chips; with the programme on screen, a refused submission or
/// delegation still offers its write buttons (an injection offers none).
/// A refusal reads neither the programme nor its page, so it offers no
/// page or status route (re-review 12 L1), and no "alert on" after the goal
/// declined it.
pub fn refusal(
    refusal: Refusal,
    has_program: bool,
    state: Option<ProgramState>,
    goal: &str,
) -> AgentMessage {
    let suggested_actions = if refusal == Refusal::ProfileChange {
        vec![SuggestedAction {
            kind: "edit_profile".into(),
            label: "마이페이지에서 프로필 수정".into(),
            payload: json!({}),
        }]
    } else {
        let mut actions = match state {
            Some(st) if refusal != Refusal::Injection && refusal != Refusal::Credential => {
                program_buttons(
                    &st,
                    super::intent::declines(goal, super::intent::WriteType::Alert),
                    PageRead::NotRead,
                )
            }
            _ => Vec::new(),
        };
        add_buttons(&mut actions, goal_chips(has_program), &HashSet::new());
        actions
    };
    AgentMessage {
        answer: refusal.message().into(),
        verdict: "not_evaluated".into(),
        suggested_actions,
        ..AgentMessage::default()
    }
}

/// A recognized goal about one programme with none on screen: offer the
/// user's saved programmes to pick from.
pub fn needs_program(kind: GoalKind, goal: &str, saved: &[(Uuid, String)]) -> AgentMessage {
    let mut actions: Vec<SuggestedAction> = saved
        .iter()
        .map(|(id, title)| SuggestedAction {
            kind: "pick_program".into(),
            label: title.chars().take(30).collect(),
            payload: json!({"program_id": id, "goal": goal, "goal_kind": kind.as_str()}),
        })
        .collect();
    if actions.is_empty() {
        actions = goal_chips(false);
    }
    AgentMessage {
        answer: if saved.is_empty() {
            "어떤 혜택인지 알려 주세요. 혜택 상세 화면에서 ‘비서에게 맡기기’를 누르면 그 혜택으로 시작해요."
                .into()
        } else {
            "어떤 혜택으로 할까요? 저장한 혜택 중에서 골라 주세요.".into()
        },
        verdict: "not_evaluated".into(),
        suggested_actions: actions,
        ..AgentMessage::default()
    }
}

/// Unrecognized goal and no model planner: say what the agent can do.
/// With the programme on screen, its write buttons come first: an
/// unmatched or unplanned goal always leaves the writes to tap (round 12).
pub fn fallback(
    context: super::goals::GoalContext,
    state: Option<ProgramState>,
    goal: &str,
    reason: &str,
) -> AgentMessage {
    let mut actions = state
        .map(|st| {
            program_buttons(
                &st,
                super::intent::declines(goal, super::intent::WriteType::Alert),
                PageRead::NotRead,
            )
        })
        .unwrap_or_default();
    // The fallback read nothing, so the page and status buttons come only
    // when the goal names them: whenever the intent layer reads that type at
    // all (any act, any confidence), or the goal names a status (re-review
    // 13 L1). The official-page button is a goal that plans and guards its
    // own card; the status picker writes only on the user's tap.
    if let Some(st) = state {
        // The run's context, with the programme on screen.
        let on_screen = super::goals::GoalContext {
            has_program: true,
            ..context
        };
        let named = super::intent::read(goal, on_screen, None);
        let names = |w: super::intent::WriteType| named.iter().any(|i| i.write == w);
        if names(super::intent::WriteType::Page) {
            actions.push(SuggestedAction {
                kind: "goal".into(),
                label: "공식 신청 페이지".into(),
                payload: json!({"goal": OFFICIAL_GOAL, "goal_kind": "official_application"}),
            });
        }
        if names(super::intent::WriteType::Status) || names_status(goal) {
            actions.push(status_route(st.id));
        }
    }
    // Off screen, an alert request the fallback could not plan still gets
    // the saved-list alert turn (dev: the saved list's and the selection's
    // bulk alerts were dead ends here).
    if state.is_none() {
        let declined = super::intent::declines(goal, super::intent::WriteType::Alert);
        add_buttons(
            &mut actions,
            saved_alert_turn(goal, declined, context)
                .into_iter()
                .collect(),
            &HashSet::new(),
        );
    }
    add_buttons(
        &mut actions,
        goal_chips(context.has_program),
        &HashSet::new(),
    );
    // A profile change the rules did not read goes to the profile screen.
    add_buttons(
        &mut actions,
        vec![SuggestedAction {
            kind: "edit_profile".into(),
            label: "프로필 수정".into(),
            payload: json!({}),
        }],
        &HashSet::new(),
    );
    AgentMessage {
        answer: format!("{reason} 아래처럼 요청해 주시면 바로 처리할 수 있어요."),
        verdict: "not_evaluated".into(),
        suggested_actions: actions,
        ..AgentMessage::default()
    }
}

#[cfg(test)]
mod tests {
    /// 발급 → 보관함 → 체크리스트 (benchmark 4부 순위 3): an issue-link
    /// answer about the programme on screen carries the checklist route, the
    /// screen whose document tasks open the vault and show what it holds. No
    /// other UI.
    #[test]
    fn an_issue_link_answer_routes_to_the_checklist() {
        let id = uuid::Uuid::from_u128(7);
        let links = tool_result(
            ToolName::DocumentIssueLink,
            serde_json::json!({"program_id": id, "title": "청년 지원", "documents": [
                {"name": "주민등록표 등본", "known": true, "url": "https://www.gov.kr/"}]}),
        );
        let state = super::ProgramState {
            id,
            alert: Some(false),
            saved: Some(true),
            facts: super::ProgramFacts::ANY,
        };
        let message =
            super::compose_for(&[links], &[], ON_SCREEN_RUN, "등본 어디서 떼?", Some(state));
        assert!(
            message
                .suggested_actions
                .iter()
                .any(|a| a.kind == "prepare_application"
                    && a.payload["focus"] == "checklist"
                    && a.payload["program_id"] == serde_json::json!(id)),
            "{:?}",
            message.suggested_actions
        );
    }

    /// The calendar button offers exactly what `add_to_calendar` cards
    /// (review follow-up: button condition != tool condition). A benefit end
    /// date alone, a past deadline or an inactive row: the tool refuses, so no
    /// button.
    #[test]
    fn the_calendar_button_is_the_tools_condition() {
        use chrono::TimeZone;
        use mazimi_core::models::Program;
        let today = chrono::NaiveDate::from_ymd_opt(2026, 10, 16).unwrap();
        let at = |m, d| Some(chrono::Utc.with_ymd_and_hms(2026, m, d, 14, 59, 0).unwrap());
        let row = |end: Option<chrono::DateTime<chrono::Utc>>,
                   valid: Option<chrono::DateTime<chrono::Utc>>,
                   active: bool|
         -> Program {
            serde_json::from_value(serde_json::json!({
                "id": uuid::Uuid::nil(), "program_type": "policy", "source_type": "manual",
                "title": "청년 지원", "program_status": "active", "is_active": active,
                "created_at": chrono::Utc::now(), "updated_at": chrono::Utc::now(),
                "application_end_at": end, "benefit_valid_until": valid,
            }))
            .unwrap()
        };
        let all = crate::agent::ToolFlags::ALL;
        let button = |p: &Program| {
            super::ProgramFacts::of(all, Some(p), true, true, today)
                .can(mazimi_core::agent::ToolName::AddToCalendar)
        };
        let tool = |p: &Program| {
            p.is_active
                && crate::agent::calendar::propose(
                    p,
                    mazimi_core::agent::CalendarDate::Deadline,
                    today,
                )
                .is_ok()
        };
        for p in [
            row(at(11, 27), None, true),
            row(None, at(12, 31), true),
            row(at(9, 1), None, true),
            row(at(11, 27), None, false),
            row(None, None, true),
        ] {
            assert_eq!(
                button(&p),
                tool(&p),
                "{:?} {:?} {}",
                p.application_end_at,
                p.benefit_valid_until,
                p.is_active
            );
        }
        assert!(button(&row(at(11, 27), None, true)));
        assert!(!super::ProgramFacts::of(all, None, true, true, today)
            .can(mazimi_core::agent::ToolName::AddToCalendar));
    }

    const ON_SCREEN_RUN: crate::agent::goals::GoalContext = crate::agent::goals::GoalContext {
        has_program: true,
        selected: 0,
        saved: 0,
        suspect: false,
        handoffs: false,
    };
    const OFF_SCREEN_RUN: crate::agent::goals::GoalContext = crate::agent::goals::GoalContext {
        has_program: false,
        ..ON_SCREEN_RUN
    };

    fn tool_result(tool: ToolName, data: Value) -> ToolResult {
        ToolResult {
            seq: 0,
            tool,
            permission: tool.permission(),
            program_id: data["program_id"].as_str().and_then(|s| s.parse().ok()),
            ok: true,
            status: StepStatus::Ok,
            data,
            evidence: Vec::new(),
            error: None,
            executed_at: chrono::Utc::now(),
        }
    }

    /// User report (2026-10-31), the exact goal: "부산 사는 대학생인데 월세 지원
    /// 받을 수 있는 거 있을까?". Snapshot of the answer's shape: a natural
    /// lead, one block per programme in plain words, the shared "no deadline"
    /// said once, no breadcrumb, no label-speak, no repeated disclaimer.
    #[test]
    fn the_housing_answer_reads_as_a_reply() {
        let ids: Vec<Uuid> = (1..=3).map(Uuid::from_u128).collect();
        let titles = [
            "부산광역시 평생함께 청년모두가 주거비 지원",
            "부자가족 자립 주거지원 사업",
            "부산 청년 맞춤형 민간임대주택(희망더함주택)",
        ];
        let rows: Vec<Value> = ids
            .iter()
            .zip(titles)
            .map(|(id, title)| {
                json!({"program_id": id, "title": title, "deadline": null,
                "topic_match": "both", "fit_reasons": ["부산 거주"]})
            })
            .collect();
        let mut search = tool_result(
            ToolName::SearchBenefits,
            json!({"results": rows, "topic": {"labels": ["주거"], "on_topic": 66, "mentions": 0},
                "region_label": "부산", "intent": {"region": "busan"},
                "args": {"fields": ["housing"], "region": "busan"}, "total_candidates": 66}),
        );
        search.seq = 0;
        let bundle = json!({"label": "필수 조건", "status": "unknown", "bundle": true});
        let check = |i: usize, checks: Value| {
            let mut r = tool_result(
                ToolName::CheckEligibility,
                json!({"program_id": ids[i], "title": titles[i], "checks": checks}),
            );
            r.seq = i as i32 + 1;
            r
        };
        let results = vec![
            search,
            check(
                0,
                json!([{"label": "지역", "status": "pass"}, bundle.clone()]),
            ),
            check(
                1,
                json!([{"label": "지역", "status": "pass"}, bundle.clone()]),
            ),
            check(
                2,
                json!([{"label": "연령", "status": "pass"}, {"label": "지역", "status": "pass"},
                {"label": "시·군·구", "status": "unknown"}, bundle.clone(),
                {"label": "소득", "status": "unknown"}]),
            ),
        ];
        let message = compose_for(
            &results,
            &[],
            OFF_SCREEN_RUN,
            "부산 사는 대학생인데 월세 지원 받을 수 있는 거 있을까?",
            None,
        );
        assert_eq!(
            message.answer,
            "부산 · 대학생 조건으로 찾은 주거 지원이 66개예요.
1. 부산광역시 평생함께 청년모두가 주거비 지원
   부산 거주 조건은 맞아요.
2. 부자가족 자립 주거지원 사업
   부산 거주 조건은 맞아요.
3. 부산 청년 맞춤형 민간임대주택(희망더함주택)
   나이와 부산 거주 조건은 맞아요. 사는 구·군과 소득 조건은 확인이 필요해요.
3개 모두 마감일 정보가 없어요. 나머지 신청 자격은 공고에서 확인해 주세요."
        );
        assert!(message.warnings.is_empty(), "{:?}", message.warnings);
    }

    /// QA B9, B14: a button only for an action that can run: the
    /// deployment's tools (the planner's own set) and the programme's state.
    #[test]
    fn buttons_follow_what_can_run() {
        let labels = |facts: ProgramFacts| -> Vec<String> {
            let state = ProgramState {
                id: Uuid::new_v4(),
                alert: Some(false),
                saved: Some(false),
                facts,
            };
            program_buttons(&state, false, PageRead::Open)
                .into_iter()
                .map(|b| b.label)
                .collect()
        };
        let all = labels(ProgramFacts::ANY);
        for label in [
            "마감 알림 켜기",
            "저장하기",
            "신청서 작성 도우미",
            "마감일 캘린더에 넣기",
        ] {
            assert!(all.iter().any(|l| l == label), "{label}: {all:?}");
        }
        // Production's flags (QA 10-31: calendar, official page and
        // document links unset; the 신청 준비 journey on).
        const PRODUCTION: crate::agent::ToolFlags = crate::agent::ToolFlags {
            official_page: false,
            document_links: false,
            calendar: false,
            journey_off: false,
        };
        let deployed = labels(ProgramFacts {
            flags: PRODUCTION,
            ..ProgramFacts::ANY
        });
        assert!(
            !deployed.iter().any(|l| l == "마감일 캘린더에 넣기"),
            "{deployed:?}"
        );
        for label in [
            "마감 알림 켜기",
            "저장하기",
            "신청서 작성 도우미",
            "체크리스트",
        ] {
            assert!(deployed.iter().any(|l| l == label), "{label}: {deployed:?}");
        }
        // No template: no form helper; no deadline: no 마감 알림.
        let plain = labels(ProgramFacts {
            has_template: false,
            has_deadline: false,
            ..ProgramFacts::ANY
        });
        assert!(
            !plain
                .iter()
                .any(|l| l == "신청서 작성 도우미" || l == "마감 알림 켜기"),
            "{plain:?}"
        );
        // Closed: only what still runs on it (the applied record, the profile).
        let closed = labels(ProgramFacts {
            open: false,
            ..ProgramFacts::ANY
        });
        for label in [
            "마감 알림 켜기",
            "저장하기",
            "신청서 작성 도우미",
            "체크리스트",
        ] {
            assert!(!closed.iter().any(|l| l == label), "{label}: {closed:?}");
        }
        assert!(closed.iter().any(|l| l == "신청 완료 기록"), "{closed:?}");
        // An alert already on can always be turned off (closed or undated).
        let on = ProgramState {
            id: Uuid::new_v4(),
            alert: Some(true),
            saved: Some(false),
            facts: ProgramFacts {
                open: false,
                has_deadline: false,
                ..ProgramFacts::ANY
            },
        };
        let labels_on: Vec<String> = program_buttons(&on, false, PageRead::Open)
            .into_iter()
            .map(|b| b.label)
            .collect();
        assert!(
            labels_on.iter().any(|l| l == "마감 알림 끄기"),
            "{labels_on:?}"
        );
    }

    /// Holdout v2: "mark as applied" had no button off the journey. Every
    /// programme answer offers it, whatever this run read of the page.
    #[test]
    fn every_programme_answer_offers_the_applied_record() {
        let state = ProgramState {
            id: Uuid::new_v4(),
            alert: None,
            saved: None,
            facts: ProgramFacts::ANY,
        };
        for page in [PageRead::Open, PageRead::Closed, PageRead::NotRead] {
            assert!(
                program_buttons(&state, false, page)
                    .iter()
                    .any(|a| a.payload["goal"] == APPLIED_GOAL),
                "{page:?}"
            );
        }
        // Not when this run already carded the status.
        let carded: HashSet<String> = ["record_application_status".to_owned()].into();
        let mut actions = Vec::new();
        add_buttons(
            &mut actions,
            program_buttons(&state, false, PageRead::Open),
            &carded,
        );
        assert!(!actions.iter().any(|a| a.payload["goal"] == APPLIED_GOAL));
    }
    use super::*;
    use crate::agent::EvidenceRef;
    use chrono::Utc;
    use mazimi_core::agent::Permission;

    fn result(tool: ToolName, data: Value, evidence: Vec<EvidenceRef>) -> ToolResult {
        ToolResult {
            seq: 0,
            tool,
            permission: Permission::Read,
            program_id: data["program_id"].as_str().and_then(|s| s.parse().ok()),
            ok: true,
            status: StepStatus::Ok,
            data,
            evidence,
            error: None,
            executed_at: Utc::now(),
        }
    }

    /// The journey: a checklist offers the form turn as a goal chip; a form
    /// card is the only way into the form helper while it is pending.
    #[test]
    fn journey_chips_and_form_cards() {
        let program = Uuid::new_v4();
        let prep = result(
            ToolName::GetPreparation,
            json!({"program_id": program, "title":"t","remaining":1,"total":2,"next_task":"재학증명서"}),
            vec![],
        );
        let message = compose(std::slice::from_ref(&prep), &[], true);
        assert!(message
            .suggested_actions
            .iter()
            .any(|a| a.kind == "goal" && a.payload["goal"] == FORM_GOAL));
        let forms = json!({"program_id": program, "title":"t","forms":[{"resource_id": Uuid::new_v4(), "label":"신청서"}]});
        let mut card = result(ToolName::OpenFormFill, forms.clone(), vec![]);
        card.status = StepStatus::AwaitingConfirmation;
        let message = compose(std::slice::from_ref(&card), &[], true);
        // The card says what approval fills; the answer says the rest once
        // (capture 2026-11-04 #08 said it three times).
        assert!(!message.answer.contains(crate::agent::confirm::FORM_PREFILL));
        assert!(!message.answer.contains("미리 채워"));
        assert!(!message.answer.contains("직접 해 주세요"));
        assert_eq!(
            message.answer.matches("기기에만 남아요").count(),
            1,
            "{}",
            message.answer
        );
        assert!(!message
            .suggested_actions
            .iter()
            .any(|a| a.kind == "open_form_fill"));
        let read = result(ToolName::OpenFormFill, forms, vec![]);
        assert!(compose(std::slice::from_ref(&read), &[], true)
            .suggested_actions
            .iter()
            .any(|a| a.kind == "open_form_fill"));
    }

    /// Round 12: every answer about the programme on screen carries its
    /// write buttons, alert and save in the direction the state allows, and
    /// each button's goal plans exactly its own card on the fast path.
    /// R9: with no programme on screen, an alert request the plan did not
    /// carry gets the saved-list alert turn, which plans its own card.
    #[test]
    fn an_off_screen_alert_request_gets_the_saved_list_alert_button() {
        use super::super::goals::{decide, Decision, GoalContext};
        let deadlines = result(ToolName::Deadlines, json!({"items": []}), vec![]);
        let goals_of = |goal: &str| -> Vec<String> {
            compose_for(
                std::slice::from_ref(&deadlines),
                &[],
                OFF_SCREEN_RUN,
                goal,
                None,
            )
            .suggested_actions
            .iter()
            .filter_map(|a| a.payload["goal"].as_str().map(str::to_owned))
            .collect()
        };
        assert!(goals_of("저장한 장학금들 마감 알림 켜 주세요")
            .contains(&SAVED_ALERTS_ON_GOAL.to_owned()));
        let none = GoalContext {
            has_program: false,
            selected: 0,
            suspect: false,
            handoffs: false,
            saved: 0,
        };
        for (goal, on) in [(SAVED_ALERTS_ON_GOAL, true), (SAVED_ALERTS_OFF_GOAL, false)] {
            match decide(goal, none) {
                Decision::Plan { plan, .. } => assert!(
                    matches!(plan.steps.as_slice(), [mazimi_core::agent::ToolCall::SetAlert(a)] if a.enabled == on && a.program == mazimi_core::agent::ProgramRef::Saved),
                    "{goal}: {plan:?}"
                ),
                other => panic!("{goal}: {other:?}"),
            }
        }
    }

    #[test]
    fn program_answers_carry_state_aware_write_buttons() {
        use super::super::goals::{decide, Decision, GoalContext};
        let program = Uuid::new_v4();
        let check = result(
            ToolName::CheckEligibility,
            json!({"program_id": program, "title":"t","status":"unknown","summary":"아직 확인할 조건이 있어요.","alert_enabled": true}),
            vec![],
        );
        let state = ProgramState {
            id: program,
            alert: Some(false),
            saved: Some(true),
            facts: ProgramFacts::ANY,
        };
        let goals_of = |m: &AgentMessage| -> Vec<String> {
            m.suggested_actions
                .iter()
                .map(|a| {
                    a.payload["goal"]
                        .as_str()
                        .map_or_else(|| a.label.clone(), str::to_owned)
                })
                .collect()
        };
        // The run's own read wins over the state read before it.
        let message = compose_for(
            std::slice::from_ref(&check),
            &[],
            ON_SCREEN_RUN,
            "이거 어때",
            Some(state),
        );
        let offered = goals_of(&message);
        assert!(
            offered.contains(&PROGRAM_ALERT_OFF_GOAL.to_owned()),
            "{offered:?}"
        );
        assert!(
            !offered.contains(&super::super::answer::PROGRAM_ALERT_GOAL.to_owned()),
            "{offered:?}"
        );
        assert!(
            offered.contains(&UNSAVE_GOAL.to_owned()) && !offered.contains(&SAVE_GOAL.to_owned()),
            "{offered:?}"
        );
        assert!(offered.contains(&FORM_GOAL.to_owned()), "{offered:?}");
        assert!(message
            .suggested_actions
            .iter()
            .any(|a| a.kind == "prepare_application" && a.label == "신청 상태 기록"));
        // Understanding step 0: every programme answer offers the checklist
        // route (a task is checked only on the user's tap there).
        assert!(message
            .suggested_actions
            .iter()
            .any(|a| a.kind == "prepare_application"
                && a.payload["focus"] == "checklist"
                && a.payload["program_id"] == json!(program)));
        // Unknown state: both directions; a declined alert: no alert button.
        let unknown = ProgramState {
            id: program,
            alert: None,
            saved: None,
            facts: ProgramFacts::ANY,
        };
        let both = goals_of(&fallback(ON_SCREEN_RUN, Some(unknown), "음 뭔가 해줘", "x"));
        // The fallback read nothing: no page or status route, unless the
        // goal names a status (then the picker, which writes on a tap).
        assert!(
            !both
                .iter()
                .any(|g| g == OFFICIAL_GOAL || g == "신청 상태 기록"),
            "{both:?}"
        );
        let named = goals_of(&fallback(
            ON_SCREEN_RUN,
            Some(unknown),
            "합격으로 기록해줘 아니 진짜 붙었어",
            "x",
        ));
        assert!(named.iter().any(|g| g == "신청 상태 기록"), "{named:?}");
        let declined = goals_of(&fallback(
            ON_SCREEN_RUN,
            Some(unknown),
            "알림은 필요 없어 뭔가 해줘",
            "x",
        ));
        // A decline hides the "on" button only; "off" is what was asked.
        assert!(
            !declined
                .iter()
                .any(|g| g == super::super::answer::PROGRAM_ALERT_GOAL),
            "{declined:?}"
        );
        assert!(
            declined.iter().any(|g| g == PROGRAM_ALERT_OFF_GOAL),
            "{declined:?}"
        );
        // Batch 1: the official page had no button in fallbacks. A goal the
        // intent layer reads as the page gets it.
        let page = goals_of(&fallback(
            ON_SCREEN_RUN,
            Some(unknown),
            "이 혜택 공식 신청 페이지 열어줘",
            "x",
        ));
        assert!(page.iter().any(|g| g == OFFICIAL_GOAL), "{page:?}");
        // Off screen, an alert request the fallback could not plan gets the
        // saved-list alert turn in its direction.
        let off = goals_of(&fallback(
            OFF_SCREEN_RUN,
            None,
            "저장한 혜택 알림 다 꺼줘",
            "x",
        ));
        assert!(off.iter().any(|g| g == SAVED_ALERTS_OFF_GOAL), "{off:?}");
        let on = goals_of(&fallback(
            OFF_SCREEN_RUN,
            None,
            "저장한 혜택 알림 다 켜줘",
            "x",
        ));
        assert!(on.iter().any(|g| g == SAVED_ALERTS_ON_GOAL), "{on:?}");
        for g in [
            super::super::answer::PROGRAM_ALERT_GOAL,
            PROGRAM_ALERT_OFF_GOAL,
            SAVE_GOAL,
            UNSAVE_GOAL,
            FORM_GOAL,
        ] {
            assert!(both.contains(&g.to_owned()), "{g}: {both:?}");
        }
        let prep = result(
            ToolName::GetPreparation,
            json!({"program_id": program, "title":"t","remaining":1,"total":2,"next_task":"재학증명서"}),
            vec![],
        );
        let declined = goals_of(&compose_for(
            std::slice::from_ref(&prep),
            &[],
            ON_SCREEN_RUN,
            "이 혜택 신청 준비해줘 알림은 필요 없어",
            Some(unknown),
        ));
        assert!(
            !declined
                .iter()
                .any(|g| g == super::super::answer::PROGRAM_ALERT_GOAL),
            "{declined:?}"
        );
        assert!(
            declined.iter().any(|g| g == PROGRAM_ALERT_OFF_GOAL),
            "{declined:?}"
        );
        // An answer about other programmes still offers the on-screen
        // programme's goal buttons; its status route needs this run's read.
        let other = ProgramState {
            id: Uuid::new_v4(),
            ..unknown
        };
        let elsewhere = compose_for(
            std::slice::from_ref(&check),
            &[],
            ON_SCREEN_RUN,
            "",
            Some(other),
        );
        assert!(goals_of(&elsewhere).contains(&SAVE_GOAL.to_owned()));
        assert!(!elsewhere
            .suggested_actions
            .iter()
            .any(|a| a.label == "신청 상태 기록"));
        // Each button plans its own card, nothing else.
        let on_screen = GoalContext {
            has_program: true,
            selected: 0,
            suspect: false,
            handoffs: false,
            saved: 0,
        };
        for (goal, tool) in [
            (super::super::answer::PROGRAM_ALERT_GOAL, "set_alert"),
            (PROGRAM_ALERT_OFF_GOAL, "set_alert"),
            (SAVE_GOAL, "bookmark"),
            (UNSAVE_GOAL, "bookmark"),
            (FORM_GOAL, "open_form_fill"),
            (CALENDAR_GOAL, "add_to_calendar"),
            (OFFICIAL_GOAL, "open_official_application"),
        ] {
            match decide(goal, on_screen) {
                Decision::Plan { plan, .. } => {
                    let writes: Vec<&str> = plan.writes().map(|w| w.name().as_str()).collect();
                    assert_eq!(writes, [tool], "{goal}");
                }
                other => panic!("{goal}: {other:?}"),
            }
        }
    }

    /// "마감 알림 켜기" after a 신청 준비 run, unless the alert was declined
    /// (re-review 4 L1); declining another part keeps it.
    #[test]
    fn the_alert_button_is_not_offered_after_the_alert_was_declined() {
        let program = Uuid::new_v4();
        let prep = result(
            ToolName::GetPreparation,
            json!({"program_id": program, "title":"t","remaining":1,"total":2,"next_task":"재학증명서"}),
            vec![],
        );
        let offered = |goal: &str| {
            compose_for(std::slice::from_ref(&prep), &[], ON_SCREEN_RUN, goal, None)
                .suggested_actions
                .iter()
                .any(|a| a.payload["goal"] == super::super::answer::PROGRAM_ALERT_GOAL)
        };
        assert!(offered("이 혜택 신청 준비해줘"));
        assert!(!offered("이 혜택 신청 준비해줘 알림은 필요 없어"));
        assert!(!offered("신청 준비해줘 근데 알림은 켜지 마"));
        assert!(offered("이 혜택 신청 준비해줘 저장은 하지 마"));
    }

    #[test]
    fn verdict_is_the_rule_result_and_stray_references_are_dropped() {
        let program = Uuid::new_v4();
        let eligibility = result(
            ToolName::CheckEligibility,
            json!({"program_id": program, "title":"테스트 장학금","status":"fail","summary":"저장한 프로필과 맞지 않는 조건이 있어요."}),
            vec![EvidenceRef {
                id: "0.0".into(),
                program_id: Some(program),
                kind: "official_text".into(),
                quote: Some("부산 거주".into()),
                source_label: "공고".into(),
                source_url: None,
                captured_at: None,
            }],
        );
        let mut message = compose(std::slice::from_ref(&eligibility), &[], true);
        assert_eq!(message.verdict, "fail");
        assert!(message.answer.contains("테스트 장학금"));
        assert_eq!(message.citations.len(), 1);
        // Something tries to claim a pass, cite unknown evidence and link out.
        message.verdict = "pass".into();
        message.citations.push(Citation {
            evidence_id: "9.9".into(),
            program_id: None,
            source_label: "invented".into(),
            source_url: Some("https://evil.example".into()),
            captured_at: None,
            claim_ids: vec![],
        });
        message.suggested_actions.push(SuggestedAction {
            kind: "open_url".into(),
            label: "x".into(),
            payload: json!({"url":"https://evil.example"}),
        });
        message.suggested_actions.push(SuggestedAction {
            kind: "open_program".into(),
            label: "x".into(),
            payload: json!({"program_id": Uuid::new_v4()}),
        });
        validate(&mut message, &[eligibility], &[]);
        assert_eq!(message.verdict, "fail", "the model cannot override rules");
        assert_eq!(message.citations.len(), 1);
        assert!(message
            .suggested_actions
            .iter()
            .all(|a| a.kind != "open_url" && a.payload["program_id"] != json!(Uuid::nil())));
        assert!(!message
            .suggested_actions
            .iter()
            .any(|a| a.kind == "open_program"));
    }

    #[test]
    fn pending_confirmations_are_announced_never_reported_done() {
        let view = ConfirmationView {
            id: Uuid::new_v4(),
            tool: "set_alert".into(),
            program_id: Some(Uuid::new_v4()),
            preview: super::super::ConfirmationPreview {
                title: "테스트".into(),
                action: "이 혜택의 마감 알림 켜기".into(),
                before: "알림 꺼짐".into(),
                after: "알림 켜짐".into(),
                reversible: true,
                note: None,
            },
            payload_hash: "0".repeat(64),
            expires_at: Utc::now(),
            status: "pending".into(),
            result: None,
        };
        let message = compose(&[], std::slice::from_ref(&view), false);
        assert!(message.answer.contains("승인이 필요한 변경 1건"));
        assert_eq!(
            message.answer.matches("바뀌지 않아요").count(),
            1,
            "{}",
            message.answer
        );
        assert!(!message.answer.contains("켰어요"));
        assert_eq!(message.suggested_actions[0].kind, "confirm");
        assert_eq!(message.verdict, "not_evaluated");
    }

    /// A live page that is not open: the message says so plainly and no
    /// sentence built from stored data says the user can apply.
    #[test]
    fn a_closed_live_page_overrides_stale_wording() {
        let program = Uuid::new_v4();
        let live = result(
            ToolName::CheckOfficialPage,
            json!({"program_id": program, "title":"청년 면접 지원", "live_status":"closed",
                "checked_at":"2026-10-16T05:03:00Z",
                "changes":[{"field":"deadline","ours":"2026. 11. 27.","live":"2026. 10. 10."}]}),
            vec![],
        );
        let stale = result(
            ToolName::CheckEligibility,
            json!({"program_id": program, "title":"청년 면접 지원", "status":"pass",
                "summary":"지금 신청 가능해요."}),
            vec![],
        );
        let message = compose(&[stale, live.clone()], &[], true);
        assert!(
            message.answer.contains("접수가 마감된 것으로 나와요"),
            "{}",
            message.answer
        );
        assert!(
            !message.answer.contains("10월 16일") && !message.answer.contains("확인)"),
            "{}",
            message.answer
        );
        assert!(message
            .answer
            .contains("마감일(마지미 2026. 11. 27. → 공식 2026. 10. 10.)"));
        assert!(
            !message.answer.contains("신청 가능해요"),
            "{}",
            message.answer
        );
        assert!(message
            .warnings
            .iter()
            .any(|w| w.contains("공식 페이지와 마지미 정보가 다를 수")));
        let mut unchecked = live.clone();
        unchecked.data = json!({"program_id": program, "title":"청년 면접 지원", "live_status":"not_checked", "reason":"timeout"});
        let message = compose(&[unchecked], &[], true);
        assert!(message
            .answer
            .contains("지금 공식 페이지를 확인하지 못했어요"));
        assert!(message.warnings.is_empty(), "nothing was compared");
        // Another programme's sentence is not touched by this page.
        let other = result(
            ToolName::CheckEligibility,
            json!({"program_id": Uuid::new_v4(), "title":"다른 혜택", "status":"pass",
                "summary":"지금 신청 가능해요."}),
            vec![],
        );
        let message = compose(&[other, live.clone()], &[], true);
        assert!(
            message.answer.contains("다른 혜택’: 지금 신청 가능해요."),
            "{}",
            message.answer
        );
        let mut open = live;
        open.data["live_status"] = json!("open");
        open.data["changes"] = json!([]);
        let message = compose(&[open], &[], true);
        assert!(!message.answer.contains("신청 가능"));
        assert!(message.warnings.is_empty());
    }

    #[test]
    fn issue_links_say_what_is_unknown() {
        let one = result(
            ToolName::DocumentIssueLink,
            json!({"documents":[{"name":"임대차계약서","known":false}]}),
            vec![],
        );
        assert!(compose(&[one], &[], false)
            .answer
            .contains("‘임대차계약서’은(는) 발급처를 안내할 수 없어요"));
        let mixed = result(
            ToolName::DocumentIssueLink,
            json!({"documents":[{"name":"주민등록등본","known":true,"url":"https://www.gov.kr/x"},{"name":"임대차계약서","known":false}]}),
            vec![],
        );
        let message = compose(&[mixed], &[], false);
        assert!(message.answer.contains("서류 2개의 발급처"));
        assert!(message
            .suggested_actions
            .iter()
            .all(|a| !a.payload.to_string().contains("://")));
    }
}
