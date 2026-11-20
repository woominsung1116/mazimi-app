//! What the user's goal asks the agent to **write**, decided once, from the
//! masked goal only.
//!
//! # Invariant
//! A write card is proposed only for a [`WriteIntent`] whose act counts for
//! its type ([`writes_for`]): a Request, or for a status or a checklist mark
//! a Report. The intent is a function of the masked goal and the run's
//! context ([`read`]); programme and page text never reach it, so tool
//! output cannot create an act. A goal the guard bars writes on
//! ([`writes_barred`]) or the classifier leans to refuse (`suspect`) has no
//! intents. Every planner's writes pass the gate (`cards::gate_plan`); the
//! produced writes are merged into every plan by `observe::expand`.
//!
//! # Who decides what
//! - **The understanding model** (`classifier::understand`, on by default)
//!   reads the goal once per run ([`prepare`]): act, value and target per
//!   write type. The **policy** ([`policy`]) only vetoes and resolves: card
//!   bars, value bars, the target against the screen and the saved list,
//!   the arguments from the existing extractors.
//! - **The lexical rules** (`cards.rs`) read writes only on the AI-off
//!   fallback: the model not loaded (yet), its artefact rejected, the flag
//!   off, or the kill switch `ASSISTANT_MODELS_ENABLED=false`.
//! - **Neither**: no card. The answer's state-aware buttons carry every
//!   write type for the programme on screen, so a miss costs a tap.
//!
//! [`ActHead`] is the rules path's optional act labeller (unused in
//! production; kept for the AI-off path's tests).
pub mod policy;

use super::cards::{self, Verdict};
use super::goals::{self, GoalContext, Handoff};
use crate::agent::classifier::understand::{Understanding, Understood};
use crate::assistant_provider::{cue_words, MASK};
use mazimi_core::agent::{
    AlertArgs, BookmarkArgs, CalendarArgs, ChecklistArgs, ProgramRef, StatusArgs, ToolCall,
    ToolName,
};

/// A write the agent can propose as a card.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WriteType {
    Alert,
    Save,
    Status,
    Checklist,
    Calendar,
    Profile,
    Form,
    Page,
}

/// What the goal does with a write it names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Act {
    /// Asks for it now.
    Request,
    /// Quotes it, reports saying it, or sets it aside.
    Mention,
    /// Asks about it.
    Question,
    /// Reports a state ("신청했어"): a request only for a status or a mark.
    Report,
    /// Takes an earlier request back.
    Retract,
    /// The rules cannot tell; the act head may.
    Unclear,
}

/// The act head (trained on the router-tuning branch): the act of one
/// candidate, from its clause window and its write type, with a score in
/// 0..=1. `None`: no opinion. It never sees programme text.
pub trait ActHead: Send + Sync {
    fn classify(&self, clause: &str, write: WriteType) -> Option<(Act, f32)>;
}

/// The act head's score below which an unclear candidate stays unclear.
pub const HEAD_THRESHOLD: f32 = 0.8;

/// `ASSISTANT_UNDERSTANDING_ENABLED` (default **on**; `false`, `0`, `no`
/// or `off` turns it off) and the AI kill switch `ASSISTANT_MODELS_ENABLED`:
/// writes come from the understanding model and the policy. While the model
/// is not loaded (startup, a failed artefact), or with either switch off,
/// the lexical rules read the writes: the AI-off fallback.
static UNDERSTANDING_ON: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| {
    understanding_wanted(
        std::env::var("ASSISTANT_UNDERSTANDING_ENABLED")
            .ok()
            .as_deref(),
        std::env::var("ASSISTANT_MODELS_ENABLED").ok().as_deref(),
    )
});

/// The understanding flag (default on) under the AI kill switch.
fn understanding_wanted(flag: Option<&str>, models: Option<&str>) -> bool {
    crate::assistant_provider::models_allowed_from(flag)
        && crate::assistant_provider::models_allowed_from(models)
}
static UNDERSTANDING: std::sync::OnceLock<
    std::sync::Arc<crate::agent::classifier::understand::Understanding>,
> = std::sync::OnceLock::new();

/// Install the loaded understanding model (the runtime's loader, once).
pub fn install_understanding(
    model: std::sync::Arc<crate::agent::classifier::understand::Understanding>,
) -> bool {
    UNDERSTANDING.set(model).is_ok()
}

/// The understanding model is wanted ([`UNDERSTANDING_ON`]): the runtime
/// loads it.
pub fn understanding_on() -> bool {
    *UNDERSTANDING_ON
}

/// The understanding model decides writes now: wanted and loaded.
/// Otherwise the lexical rules do (the AI-off fallback).
pub fn model_active() -> bool {
    understanding_on() && UNDERSTANDING.get().is_some()
}

/// One write the goal names, with the act and the rules' arguments (`None`
/// when the rules cannot fill them, e.g. a checklist mark naming no
/// document; then there is no card, only the button).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteIntent {
    pub write: WriteType,
    pub act: Act,
    /// The polarity a phrase says (alert on/off, save/unsave, check/undo).
    pub on: Option<bool>,
    pub step: Option<ToolCall>,
    /// The goal is this write and nothing else (a profile change, a form
    /// field value): no planner reads to wait for.
    pub whole: bool,
}

/// Whether an act counts as asking for a write of this type.
/// A route-owned handoff (the form, the official page): no policy step.
pub fn is_handoff(step: &ToolCall) -> bool {
    matches!(write_of(step), Some(WriteType::Form | WriteType::Page))
}

pub fn writes_for(write: WriteType, act: Act) -> bool {
    match act {
        Act::Request => true,
        Act::Report => matches!(write, WriteType::Status | WriteType::Checklist),
        _ => false,
    }
}

/// The one place that bars writes on a goal: any injection marker, a
/// lookalike script, or masked text.
pub fn writes_barred(goal: &str) -> bool {
    super::guard::bars_writes(goal)
}

/// The form-field-value card's own bar: masked text alone does not bar it
/// (the masked value changes nothing on the server); every other marker
/// does. Only that card has this exception.
pub fn field_value_barred(goal: &str) -> bool {
    super::guard::bars_writes(&goal.replace(MASK, " "))
}

/// The writes a declined part of the goal names ("알림은 필요 없어"): no
/// card and no button for them after the run.
pub fn declines(goal: &str, write: WriteType) -> bool {
    match write {
        WriteType::Alert => goals::declines_alert(goal),
        _ => false,
    }
}

/// The goal's write intents. Pure: the masked goal, the screen and the act
/// head only; with the understanding model, its reading of the goal
/// computed once per run ([`prepare`]), never here.
pub fn read(goal: &str, context: GoalContext, head: Option<&dyn ActHead>) -> Vec<WriteIntent> {
    read_uncached(goal, context, head)
}

/// How long the understanding model may take on one goal (design: hard
/// 300 ms). Past it the goal has no reading: no card, the buttons.
pub const UNDERSTAND_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(300);
/// Readings kept (per masked goal, first in first out): enough for every
/// run in flight (`ASSISTANT_AGENT_MAX_RUNS`) many times over.
const READINGS_KEPT: usize = 1024;

/// The model's reading per masked goal. It does not depend on the screen,
/// the selection or the saved list, so it is computed once per run by
/// [`prepare`] and every reader (routing, gate, producer, answer) applies
/// the policy with its own context to the same reading. Only readings are
/// kept: a read that timed out or failed is not, so the next run of that
/// goal reads it again (delta review HIGH).
type Readings = (
    std::collections::HashMap<String, std::sync::Arc<Understood>>,
    std::collections::VecDeque<String>,
);
static READINGS: std::sync::LazyLock<std::sync::Mutex<Readings>> =
    std::sync::LazyLock::new(Default::default);

/// Model reads in flight, queued or running: twice the model's pool
/// threads. A read holds its slot until it finishes, not until its run
/// stops waiting, so reads that timed out cannot pile up behind the pool;
/// a run that finds no free slot within its time limit gets no reading
/// (no card, the buttons) and leaves nothing queued.
pub const READS_IN_FLIGHT: usize = 4;
static READ_SLOTS: std::sync::LazyLock<std::sync::Arc<tokio::sync::Semaphore>> =
    std::sync::LazyLock::new(|| std::sync::Arc::new(tokio::sync::Semaphore::new(READS_IN_FLIGHT)));

/// Read slots free now (tests, metrics).
pub fn free_read_slots() -> usize {
    READ_SLOTS.available_permits()
}

fn remember(goal: &str, reading: std::sync::Arc<Understood>) {
    let Ok(mut store) = READINGS.lock() else {
        return;
    };
    let (map, order) = &mut *store;
    if map.insert(goal.to_owned(), reading).is_none() {
        order.push_back(goal.to_owned());
        while order.len() > READINGS_KEPT {
            if let Some(old) = order.pop_front() {
                map.remove(&old);
            }
        }
    }
}

fn reading(goal: &str) -> Option<std::sync::Arc<Understood>> {
    READINGS.lock().ok()?.0.get(goal).cloned()
}

fn prepared(goal: &str) -> bool {
    READINGS.lock().is_ok_and(|s| s.0.contains_key(goal))
}

/// Read the goal with the understanding model once, off the async runtime
/// (`spawn_blocking`) and within [`UNDERSTAND_TIMEOUT`] (the wait for a
/// read slot included); the run calls this before routing. Nothing when the
/// model is not active or the guard bars the goal's writes (the model
/// never reads a barred goal).
pub async fn prepare(goal: &str) {
    prepare_within(goal, UNDERSTAND_TIMEOUT).await;
}

/// [`prepare`] with its time limit (tests).
pub async fn prepare_within(goal: &str, limit: std::time::Duration) {
    let Some(model) = UNDERSTANDING.get().filter(|_| understanding_on()).cloned() else {
        return;
    };
    if writes_barred(goal)
        || prepared(goal)
        || button_intent(goal, GoalContext::default()).is_some()
    {
        return;
    }
    let deadline = tokio::time::Instant::now() + limit;
    let Ok(Ok(slot)) = tokio::time::timeout_at(deadline, READ_SLOTS.clone().acquire_owned()).await
    else {
        tracing::warn!("understanding reads are saturated; no write card for this run");
        return;
    };
    let text = goal.to_owned();
    let task = tokio::task::spawn_blocking(move || {
        let _slot = slot;
        model.read(&text)
    });
    match tokio::time::timeout_at(deadline, task).await {
        Ok(Ok(Ok(understood))) => remember(goal, std::sync::Arc::new(understood)),
        Ok(_) => tracing::warn!("understanding model failed on a goal; no write card"),
        Err(_) => tracing::warn!("understanding model timed out on a goal; no write card"),
    }
}

/// [`prepare`] on the calling thread, without the timeout (evaluation,
/// probes, tests).
pub fn prepare_blocking(goal: &str) {
    let Some(model) = UNDERSTANDING.get().filter(|_| understanding_on()) else {
        return;
    };
    if writes_barred(goal)
        || prepared(goal)
        || button_intent(goal, GoalContext::default()).is_some()
    {
        return;
    }
    if let Ok(understood) = model.read(goal) {
        remember(goal, std::sync::Arc::new(understood));
    }
}

/// The model path: a barred goal (injection marker, lookalike script,
/// masked text) asks for no write; otherwise the goal's prepared reading
/// through the policy. No reading (not prepared, timed out, failed): no
/// card, only the buttons.
fn read_understood(
    goal: &str,
    context: GoalContext,
    model: &Understanding,
    understood: Option<&Understood>,
) -> Vec<WriteIntent> {
    if writes_barred(goal) {
        return Vec::new();
    }
    understood
        .map(|understood| policy::intents(goal, context, model, understood))
        .unwrap_or_default()
}

/// The answer's own buttons: each sends back a goal string the server
/// wrote (`compose.rs`, `answer.rs`), so it is one typed action, not user
/// language. It becomes its card directly, through the same gate, producer
/// and confirmation as any write, without the model: a fixed button can
/// never miss its card and loop (delta review 2). `None`: not a button.
pub fn button_intent(goal: &str, context: GoalContext) -> Option<WriteIntent> {
    use super::answer::{PROGRAM_ALERT_GOAL, SELECTED_ALERT_GOAL};
    use super::compose::{
        APPLIED_GOAL, CALENDAR_GOAL, FORM_GOAL, OFFICIAL_GOAL, PROGRAM_ALERT_OFF_GOAL,
        SAVED_ALERTS_OFF_GOAL, SAVED_ALERTS_ON_GOAL, SAVE_GOAL, UNSAVE_GOAL,
    };
    let screen = context.has_program.then_some(ProgramRef::Context);
    let selected = (context.selected > 0).then_some(ProgramRef::Selected);
    let alert = |program: Option<ProgramRef>, enabled: bool| {
        (
            WriteType::Alert,
            Some(enabled),
            program.map(|program| ToolCall::SetAlert(AlertArgs { program, enabled })),
        )
    };
    let save = |bookmarked: bool| {
        (
            WriteType::Save,
            Some(bookmarked),
            screen.map(|program| {
                ToolCall::Bookmark(BookmarkArgs {
                    program,
                    bookmarked,
                })
            }),
        )
    };
    let (write, on, step) = match goal {
        g if g == PROGRAM_ALERT_GOAL => alert(screen, true),
        g if g == PROGRAM_ALERT_OFF_GOAL => alert(screen, false),
        // The saved list whatever its size: with nothing saved the tool
        // answers "저장하거나 준비 중인 혜택이 아직 없어요", as on the rules
        // path, instead of the button coming back (final delta review).
        g if g == SAVED_ALERTS_ON_GOAL => alert(Some(ProgramRef::Saved), true),
        g if g == SAVED_ALERTS_OFF_GOAL => alert(Some(ProgramRef::Saved), false),
        g if g == SELECTED_ALERT_GOAL => alert(selected, true),
        g if g == SAVE_GOAL => save(true),
        g if g == UNSAVE_GOAL => save(false),
        g if g == APPLIED_GOAL => (
            WriteType::Status,
            None,
            screen.map(|program| {
                ToolCall::RecordApplicationStatus(StatusArgs {
                    program,
                    status: mazimi_core::agent::ApplicationStatus::Applied,
                })
            }),
        ),
        g if g == CALENDAR_GOAL => (
            WriteType::Calendar,
            None,
            screen.map(|program| {
                // The calendar button's own defaults, as the rules read
                // its goal (no date named: the deadline).
                let words = cue_words(goal);
                ToolCall::AddToCalendar(CalendarArgs {
                    program,
                    date: goals::calendar_date(&words),
                    days_before: goals::calendar_days_before(&words),
                })
            }),
        ),
        // The handoffs stay with their routes (the client's capability).
        g if g == FORM_GOAL => (WriteType::Form, None, None),
        g if g == OFFICIAL_GOAL => (WriteType::Page, None, None),
        _ => return None,
    };
    Some(WriteIntent {
        write,
        act: Act::Request,
        on,
        step,
        whole: false,
    })
}

fn read_uncached(goal: &str, context: GoalContext, head: Option<&dyn ActHead>) -> Vec<WriteIntent> {
    // A goal the classifier leans to refuse (below its bar) asks for no
    // write on any path (security review F1): no planned write passes the
    // gate and nothing is produced.
    if context.suspect {
        return Vec::new();
    }
    if let Some(button) = button_intent(goal, context) {
        return vec![button];
    }
    if let Some(model) = UNDERSTANDING.get().filter(|_| understanding_on()) {
        let mut intents = read_understood(goal, context, model, reading(goal).as_deref());
        agree_with_rules(&mut intents, &read_lexical(goal, context, head));
        return intents;
    }
    read_lexical(goal, context, head)
}

/// Where the model reads a write but cannot settle it (unclear: below the
/// card or value bar; or a request whose step it could not fill), the rules'
/// card of the same type stands when the rules settle it and agree with
/// what the model did fill. The model's other reads (a question, a mention,
/// a retraction, no write at all) keep their veto, and nothing is added for
/// a type the model did not read.
fn agree_with_rules(intents: &mut [WriteIntent], rules: &[WriteIntent]) {
    for intent in intents.iter_mut() {
        let unsettled = intent.act == Act::Unclear
            || (writes_for(intent.write, intent.act) && intent.step.is_none());
        if !unsettled {
            continue;
        }
        let Some(rule) = rules.iter().find(|r| {
            r.write == intent.write
                && writes_for(r.write, r.act)
                && r.step.is_some()
                && intent.on.is_none_or(|on| r.on == Some(on))
                && intent
                    .step
                    .as_ref()
                    .is_none_or(|step| r.step.as_ref() == Some(step))
        }) else {
            continue;
        };
        intent.act = rule.act;
        intent.step = rule.step.clone();
    }
}

/// The lexical rules' write intents: the AI-off fallback, and the second
/// reader [`agree_with_rules`] consults.
fn read_lexical(goal: &str, context: GoalContext, head: Option<&dyn ActHead>) -> Vec<WriteIntent> {
    // A barred goal keeps only a form-field-value card its own bar allows.
    if writes_barred(goal) {
        let field = cards::field_value(goal) || goals::names_field_value(&cue_words(goal));
        return if field && !field_value_barred(goal) {
            vec![WriteIntent {
                write: WriteType::Form,
                act: Act::Request,
                on: None,
                step: None,
                whole: true,
            }]
        } else {
            Vec::new()
        };
    }

    // Only the programme on screen: a write on the saved list or on what a
    // search found is its route's (which knows the list and the search);
    // off screen the intent still names the write, for the buttons.
    // The intent's target (code review H1): a saved-list goal's writes are
    // its route's, so only the programme on screen gets a produced step.
    let program =
        (target(goal, context) == Some(ProgramRef::Context)).then_some(ProgramRef::Context);
    let mut out = Vec::new();
    let mut push = |write, act, on: Option<bool>, step: Option<ToolCall>, whole| {
        out.push(WriteIntent {
            write,
            act,
            on,
            step,
            whole,
        });
    };
    // Alert and save: a made request settles the type; otherwise the last
    // candidate's verdict is the act, and the head may judge an unclear one.
    let settled = [
        (WriteType::Alert, cards::alert(goal)),
        (WriteType::Save, cards::bookmark(goal)),
    ];
    let candidates = cards::candidates(goal);
    for (write, made) in settled {
        let step = |on: bool| {
            program.map(|program| match write {
                WriteType::Alert => ToolCall::SetAlert(AlertArgs {
                    program,
                    enabled: on,
                }),
                _ => ToolCall::Bookmark(BookmarkArgs {
                    program,
                    bookmarked: on,
                }),
            })
        };
        if let Some(on) = made {
            push(write, Act::Request, Some(on), step(on), false);
            continue;
        }
        let Some((_, on, verdict, clause)) = candidates.iter().rev().find(|c| c.0 == write) else {
            continue;
        };
        let act = match verdict {
            Verdict::Mentioned => Act::Mention,
            Verdict::Unclear => head
                .and_then(|h| h.classify(clause, write))
                .filter(|(_, score)| *score >= HEAD_THRESHOLD)
                .map_or(Act::Unclear, |(act, _)| act),
            // Made, yet no card: a question or a retraction took it.
            Verdict::Made if goal.contains('?') => Act::Question,
            Verdict::Made => Act::Retract,
        };
        push(write, act, Some(*on), step(*on), false);
    }
    let on_screen = program.is_some();
    if let Some(status) = cards::status(goal) {
        let act = if cards::status_reported(goal) {
            Act::Report
        } else {
            Act::Request
        };
        let step = on_screen.then_some({
            ToolCall::RecordApplicationStatus(StatusArgs {
                program: ProgramRef::Context,
                status,
            })
        });
        push(WriteType::Status, act, None, step, false);
    }
    let words = cue_words(goal);
    if let Some(done) = cards::checklist(goal) {
        // The task is the routes' own extractor; no document, no card.
        let step = goals::named_document(&words)
            .filter(|_| on_screen)
            .map(|task| {
                ToolCall::ToggleChecklistItem(ChecklistArgs {
                    program: ProgramRef::Context,
                    task,
                    done,
                })
            });
        push(WriteType::Checklist, Act::Request, Some(done), step, true);
    }
    if cards::calendar(goal) {
        let step = on_screen.then(|| {
            ToolCall::AddToCalendar(CalendarArgs {
                program: ProgramRef::Context,
                date: goals::calendar_date(&words),
                days_before: goals::calendar_days_before(&words),
            })
        });
        push(WriteType::Calendar, Act::Request, None, step, false);
    }
    if cards::profile(goal) {
        let step = goals::own_profile_patch(goal).map(ToolCall::ProfilePatch);
        push(WriteType::Profile, Act::Request, None, step, true);
    }
    let handoff = goals::strict_handoff(goal);
    if handoff == Some(Handoff::Form) || cards::field_value(goal) {
        push(WriteType::Form, Act::Request, None, None, true);
    }
    if handoff == Some(Handoff::OfficialPage) {
        push(
            WriteType::Page,
            Act::Request,
            None,
            page_step(on_screen, context),
            true,
        );
    }
    out
}

/// The official-page handoff's step, as the calendar's: the programme on
/// screen, for a client that opens handoffs ([`GoalContext::handoffs`]).
/// Otherwise none: the answer's "공식 신청 페이지" button.
pub(crate) fn page_step(on_screen: bool, context: GoalContext) -> Option<ToolCall> {
    (on_screen && context.handoffs).then_some(ToolCall::OpenOfficialApplication(
        mazimi_core::agent::ProgramArgs {
            program: ProgramRef::Context,
        },
    ))
}

impl WriteType {
    /// The tool that writes this type.
    pub fn tool(self) -> ToolName {
        match self {
            Self::Alert => ToolName::SetAlert,
            Self::Save => ToolName::Bookmark,
            Self::Status => ToolName::RecordApplicationStatus,
            Self::Checklist => ToolName::ToggleChecklistItem,
            Self::Calendar => ToolName::AddToCalendar,
            Self::Profile => ToolName::ProfilePatch,
            Self::Form => ToolName::OpenFormFill,
            Self::Page => ToolName::OpenOfficialApplication,
        }
    }
}

/// The tools of the writes the goal asks for, read as if it were not
/// suspect: what a refusal lean is weighed against
/// ([`crate::agent::classifier::suspect_lean`]).
pub fn asked_tools(goal: &str, context: GoalContext) -> Vec<ToolName> {
    let context = GoalContext {
        suspect: false,
        ..context
    };
    let mut tools: Vec<ToolName> = read(goal, context, None)
        .into_iter()
        .filter(|i| writes_for(i.write, i.act))
        .map(|i| i.write.tool())
        .collect();
    tools.dedup();
    tools
}

/// The write type of a write step.
pub fn write_of(step: &ToolCall) -> Option<WriteType> {
    Some(match step.name() {
        ToolName::SetAlert => WriteType::Alert,
        ToolName::Bookmark => WriteType::Save,
        ToolName::RecordApplicationStatus => WriteType::Status,
        ToolName::ToggleChecklistItem => WriteType::Checklist,
        ToolName::AddToCalendar => WriteType::Calendar,
        ToolName::ProfilePatch => WriteType::Profile,
        ToolName::OpenFormFill => WriteType::Form,
        ToolName::OpenOfficialApplication => WriteType::Page,
        _ => return None,
    })
}

/// The programme a write of this goal is about. The intent owns it (code
/// review H1), not the planner: the saved list when the goal's structure
/// names it ([`cards::saved_list`]), else the selection when there is one
/// (it wins over the screen, review M2), else the programme on screen.
pub fn target(goal: &str, context: GoalContext) -> Option<ProgramRef> {
    if cards::saved_list(goal) {
        Some(ProgramRef::Saved)
    } else if context.selected > 0 {
        Some(ProgramRef::Selected)
    } else if context.has_program {
        Some(ProgramRef::Context)
    } else {
        None
    }
}

/// The step's polarity or value is the intent's.
fn same_value(i: &WriteIntent, step: &ToolCall) -> bool {
    match step {
        ToolCall::SetAlert(a) => i.on == Some(a.enabled),
        ToolCall::Bookmark(b) => i.on == Some(b.bookmarked),
        ToolCall::ToggleChecklistItem(c) => i.on == Some(c.done),
        ToolCall::RecordApplicationStatus(s) => matches!(
            &i.step,
            Some(ToolCall::RecordApplicationStatus(t)) if t.status == s.status
        ),
        _ => true,
    }
}

/// Whether the goal asks for this planned write step, on this screen: an
/// intent of its type whose act counts, whose polarity or value is the
/// step's, and whose target is the step's ([`target`]; what a search found
/// only when the plan searched before it, `searched`).
pub fn asks_in(goal: &str, context: GoalContext, step: &ToolCall, searched: bool) -> bool {
    let Some(write) = write_of(step) else {
        // A read, or a write no intent can ask for (the signature handoff).
        return !step.name().is_write();
    };
    if model_active() {
        // The policy resolved the target: the step must be the intent's,
        // or what the plan's search found.
        return read(goal, context, None).iter().any(|i| {
            i.write == write
                && writes_for(i.write, i.act)
                && same_value(i, step)
                && match step.program() {
                    None => true,
                    Some(ProgramRef::Found) => searched,
                    // A handoff has no policy step: the route's programme
                    // is the one on screen.
                    Some(ProgramRef::Context) if i.step.is_none() && is_handoff(step) => {
                        context.has_program
                    }
                    program => i.step.as_ref().and_then(ToolCall::program) == program,
                }
        });
    }
    let targeted = match step.program() {
        None => true,
        Some(ProgramRef::Found) => searched,
        Some(program) => target(goal, context) == Some(program),
    };
    let on_screen = GoalContext {
        has_program: true,
        ..context
    };
    targeted
        && read(goal, on_screen, None)
            .iter()
            .any(|i| i.write == write && writes_for(i.write, i.act) && same_value(i, step))
}

/// [`asks_in`] for a programme on screen, after a search (tests and the
/// journey's own alert).
pub fn asks(goal: &str, step: &ToolCall) -> bool {
    let on_screen = GoalContext {
        has_program: true,
        selected: 0,
        saved: 0,
        suspect: false,
        handoffs: false,
    };
    asks_in(goal, on_screen, step, true)
}

/// The gate on one planned write step: the step when the goal asks for it;
/// the intent's own step when the goal asks for that write on another
/// target (retargeted); otherwise nothing.
pub fn gate(goal: &str, context: GoalContext, step: &ToolCall, searched: bool) -> Option<ToolCall> {
    if asks_in(goal, context, step, searched) {
        return Some(step.clone());
    }
    let write = write_of(step)?;
    read(goal, context, None)
        .into_iter()
        .find(|i| i.write == write && writes_for(i.write, i.act) && same_value(i, step))
        .and_then(|i| i.step)
}

/// The write steps the goal asks for that the rules can fill: merged into
/// every plan (R0). The form handoff stays with its route (it needs the
/// programme's template); the page handoff has a step only for a client
/// that opens it ([`page_step`]).
pub fn produced(goal: &str, context: GoalContext, head: Option<&dyn ActHead>) -> Vec<WriteIntent> {
    read(goal, context, head)
        .into_iter()
        .filter(|i| writes_for(i.write, i.act) && i.step.is_some())
        .filter(|i| i.write != WriteType::Form)
        .collect()
}

#[cfg(test)]
mod tests {

    /// The official-page handoff is produced like the calendar card: the
    /// programme on screen, for a client that opens handoffs (PR #44 review:
    /// page 0/12 was a missing step, not a misreading). An older client and
    /// an off-screen goal get no step (the button).
    #[test]
    fn the_page_handoff_is_produced_for_a_capable_client_on_screen() {
        let goal = "공식 신청 페이지 열어줘";
        let ctx = |has_program, handoffs| GoalContext {
            has_program,
            selected: 0,
            saved: 0,
            suspect: false,
            handoffs,
        };
        let page = |c| {
            produced(goal, c, None)
                .into_iter()
                .filter_map(|i| i.step)
                .filter(|s| matches!(s, ToolCall::OpenOfficialApplication(_)))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            page(ctx(true, true)),
            vec![ToolCall::OpenOfficialApplication(
                mazimi_core::agent::ProgramArgs {
                    program: ProgramRef::Context
                }
            )]
        );
        assert!(
            page(ctx(true, false)).is_empty(),
            "an older client: the button"
        );
        assert!(
            page(ctx(false, true)).is_empty(),
            "off screen: no programme"
        );
        // The one gate: a goal that is not that request gets no page step.
        assert!(produced(
            "공식 신청 페이지 열어줘 했는데 안 열려",
            ctx(true, true),
            None
        )
        .iter()
        .all(|i| i.write != WriteType::Page));
    }

    /// The understanding flag defaults on; it or the AI kill switch turns
    /// it off (the AI-off fallback reads writes with the rules).
    #[test]
    fn understanding_is_on_unless_switched_off() {
        assert!(understanding_wanted(None, None));
        assert!(understanding_wanted(Some("true"), None));
        assert!(!understanding_wanted(Some("false"), None));
        assert!(!understanding_wanted(Some("off"), None));
        assert!(!understanding_wanted(None, Some("false")));
        assert!(!understanding_wanted(Some("true"), Some("0")));
    }

    /// Model path, fail-first: the model alone cards the injected alert
    /// request; the barred check in front of it leaves no write.
    #[test]
    #[ignore = "needs INTENT_CLASSIFIER_MODEL_DIR with the e5 files"]
    fn the_model_path_gives_a_barred_goal_no_write() {
        use crate::agent::classifier::understand::UnderstandingPaths;
        use crate::agent::classifier::ArtefactPaths;
        let Ok(dir) = std::env::var("INTENT_CLASSIFIER_MODEL_DIR") else {
            return;
        };
        let model = Understanding::load(
            &UnderstandingPaths {
                model: concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/assets/understanding/understanding.json"
                )
                .into(),
                encoder: ArtefactPaths::in_dir(std::path::Path::new(&dir)),
            },
            2,
        )
        .unwrap();
        let ctx = GoalContext {
            has_program: true,
            selected: 0,
            saved: 0,
            suspect: false,
            handoffs: false,
        };
        // Masked text (a pasted identifier, a hidden span) bars writes.
        let masked = format!("이거 알림 켜줘 {MASK}");
        let raw = policy::intents(&masked, ctx, &model, &model.read(&masked).unwrap());
        assert!(
            raw.iter().any(|i| writes_for(i.write, i.act)),
            "the model alone cards it"
        );
        assert!(writes_barred(&masked));
        let masked_reading = model.read(&masked).unwrap();
        assert!(read_understood(&masked, ctx, &model, Some(&masked_reading)).is_empty());
        // A plain request still cards; no reading (timed out), no card.
        let plain_reading = model.read("이거 알림 켜줘").unwrap();
        let plain = read_understood("이거 알림 켜줘", ctx, &model, Some(&plain_reading));
        assert!(plain
            .iter()
            .any(|i| i.write == WriteType::Alert && i.act == Act::Request && i.on == Some(true)));
        assert!(read_understood("이거 알림 켜줘", ctx, &model, None).is_empty());
    }
    use super::*;

    const ON: GoalContext = GoalContext {
        has_program: true,
        selected: 0,
        saved: 0,
        suspect: false,
        handoffs: false,
    };
    const OFF: GoalContext = GoalContext {
        has_program: false,
        selected: 0,
        saved: 0,
        suspect: false,
        handoffs: false,
    };

    /// The intent owns the target (code review H1): the programme on
    /// screen, the saved list only when the goal's structure names it, the
    /// selection only when there is one, what a search found only after a
    /// search. A planned write on another target is retargeted or dropped.
    #[test]
    fn the_target_is_the_intents() {
        let alert = |program, enabled| ToolCall::SetAlert(AlertArgs { program, enabled });
        let save = |program, bookmarked| {
            ToolCall::Bookmark(BookmarkArgs {
                program,
                bookmarked,
            })
        };
        let selected = GoalContext {
            has_program: false,
            selected: 2,
            saved: 0,
            suspect: false,
            handoffs: false,
        };
        // A model or classifier alert on the saved list for "알림 켜줘".
        assert!(!asks_in(
            "알림 켜줘",
            ON,
            &alert(ProgramRef::Saved, true),
            false
        ));
        assert!(asks_in(
            "알림 켜줘",
            ON,
            &alert(ProgramRef::Context, true),
            false
        ));
        assert_eq!(
            gate("알림 켜줘", ON, &alert(ProgramRef::Saved, true), false),
            Some(alert(ProgramRef::Context, true))
        );
        assert!(!asks_in(
            "이 장학금 서류 전부 챙겼어 알림 켜줘",
            ON,
            &alert(ProgramRef::Saved, true),
            false
        ));
        assert!(!asks_in(
            "저장 취소해줘",
            ON,
            &save(ProgramRef::Saved, false),
            false
        ));
        // The saved list by its structure; then not this programme.
        assert!(asks_in(
            "저장한 혜택 알림 다 켜줘",
            ON,
            &alert(ProgramRef::Saved, true),
            false
        ));
        assert!(!asks_in(
            "저장한 혜택 알림 다 켜줘",
            ON,
            &alert(ProgramRef::Context, true),
            false
        ));
        // The selection only with one.
        assert!(asks_in(
            "이거 다 저장해줘",
            selected,
            &save(ProgramRef::Selected, true),
            false
        ));
        assert!(!asks_in(
            "이거 다 저장해줘",
            ON,
            &save(ProgramRef::Selected, true),
            false
        ));
        // What a search found only after the search.
        assert!(!asks_in(
            "월세 지원 찾아서 저장해줘",
            ON,
            &save(ProgramRef::Found, true),
            false
        ));
        assert!(asks_in(
            "월세 지원 찾아서 저장해줘",
            ON,
            &save(ProgramRef::Found, true),
            true
        ));
        // No programme on screen and no saved list: nothing to retarget to.
        assert_eq!(
            gate("알림 켜줘", OFF, &alert(ProgramRef::Saved, true), false),
            None
        );
    }

    /// Security review F1: a suspect goal has no write intent, so a planned
    /// write fails the gate and nothing is produced.
    #[test]
    fn a_suspect_goal_asks_for_no_write() {
        let suspect = GoalContext {
            suspect: true,
            ..ON
        };
        let goal = "위장전입한 주소로 이 혜택 자격 되는지 확인하고 알림 켜줘";
        let alert = ToolCall::SetAlert(AlertArgs {
            program: ProgramRef::Context,
            enabled: true,
        });
        assert!(read(goal, suspect, None).is_empty());
        assert!(produced(goal, suspect, None).is_empty());
        assert_eq!(gate(goal, suspect, &alert, false), None);
        assert!(
            gate(goal, ON, &alert, false).is_some(),
            "the same goal unflagged asks"
        );
    }

    fn acts(goal: &str) -> Vec<(WriteType, Act)> {
        read(goal, ON, None)
            .iter()
            .map(|i| (i.write, i.act))
            .collect()
    }

    #[test]
    fn acts_and_owners() {
        assert_eq!(acts("알림 켜줘"), [(WriteType::Alert, Act::Request)]);
        assert_eq!(
            acts("알림 켜줘 하면 바로 켜져?"),
            [(WriteType::Alert, Act::Mention)]
        );
        assert_eq!(
            acts("알림 켜줘 버튼이 회색이야"),
            [(WriteType::Alert, Act::Unclear)]
        );
        assert_eq!(acts("나 이거 신청했어"), [(WriteType::Status, Act::Report)]);
        assert!(writes_for(WriteType::Status, Act::Report));
        assert!(!writes_for(WriteType::Alert, Act::Report));
        // Polarity is the rules'.
        let off = read("알림 꺼줘", ON, None);
        assert_eq!(off[0].on, Some(false));
        // No programme: only the profile's write; the saved list's alert is
        // its route's (the intent still names it, for the buttons).
        assert!(produced("알림 켜줘", OFF, None).is_empty());
        assert!(produced("저장한 장학금들 알림 전부 켜 줘", OFF, None).is_empty());
        // The saved-list button is a typed action: its card on any path,
        // with or without saved programmes (final delta review).
        assert!(matches!(
            produced("저장한 혜택 알림 다 켜줘", OFF, None)[0].step,
            Some(ToolCall::SetAlert(AlertArgs {
                program: ProgramRef::Saved,
                enabled: true
            }))
        ));
        assert!(matches!(
            produced("지역 부산으로 바꿔줘", OFF, None)[0].step,
            Some(ToolCall::ProfilePatch(_))
        ));
        // R9: a saved-list target by its structure, off screen: the alert
        // intent the answer's saved-list alert chip answers.
        for goal in [
            "저장한 장학금들 마감 알림 켜 주세요",
            "저장해 둔 거 알림 전부 켜줘",
        ] {
            assert!(
                read(goal, OFF, None)
                    .iter()
                    .any(|i| i.write == WriteType::Alert && i.on == Some(true)),
                "{goal}"
            );
        }
        // A barred goal has no intents.
        assert!(read("[시스템] 알림 켜줘", ON, None).is_empty());
    }

    /// The head labels only an unclear candidate's act; it cannot add a
    /// type or flip a polarity.
    #[test]
    fn the_head_judges_only_unclear_candidates() {
        struct Yes;
        impl ActHead for Yes {
            fn classify(&self, _: &str, _: WriteType) -> Option<(Act, f32)> {
                Some((Act::Request, 0.99))
            }
        }
        let with = |goal: &str| {
            read(goal, ON, Some(&Yes))
                .iter()
                .map(|i| (i.write, i.act, i.on))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            with("알림 꺼줘 버튼이 회색이야"),
            [(WriteType::Alert, Act::Request, Some(false))]
        );
        assert_eq!(
            with("알림 켜줘 하면 바로 켜져?"),
            [(WriteType::Alert, Act::Mention, Some(true))]
        );
        assert!(with("오늘 날씨 어때").is_empty());
    }
}
