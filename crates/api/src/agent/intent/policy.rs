//! The write policy over the understanding model's reading (architect
//! design): it only vetoes and resolves, never recognizes.
//!
//! - A type is a card when its write probability is at or above the
//!   artefact's card bar **and** the top act is a write act (a request; a
//!   report for status and checklist): a question, mention or retraction
//!   never cards, whatever the mass.
//! - Below the bar a write act on top, or "none" on top with a write
//!   probability at or above [`BUTTON_FLOOR`], is `Unclear`: no card, and
//!   the answer offers the type's state-aware button. Otherwise the top
//!   act is kept for the answer (a mention, a question, a retraction).
//! - The value (alert on/off, save/unsave, checklist done/undo, status) is
//!   the value head's top label. Every value head (alert, save, checklist,
//!   status) cards only when the margin between its two likeliest values
//!   reaches the artefact's value bar; below it, `Unclear` (the state-aware
//!   button). The status value "rejected" has no status to record: no
//!   card, the status picker.
//! - The target: on_screen only with a programme on screen; saved only when
//!   the model says saved and the user has saved programmes; selected only
//!   with a selection; otherwise no programme (a search's finds are the
//!   gate's, after the plan's search).
//! - Arguments come only from the existing extractors, run after the model
//!   has said the type: the checklist document, the calendar date, the
//!   profile field and value. A missing argument is no card (the button).
use super::{Act, WriteIntent, WriteType};
use crate::agent::classifier::understand::{Understanding, Understood, ACTS, TARGETS, TYPES};
use crate::agent::goals::{self, GoalContext};
use crate::assistant_provider::cue_words;
use mazimi_core::agent::{
    AlertArgs, ApplicationStatus, BookmarkArgs, CalendarArgs, ChecklistArgs, ProgramRef,
    StatusArgs, ToolCall,
};

fn write_type(name: &str) -> Option<WriteType> {
    Some(match name {
        "alert" => WriteType::Alert,
        "save" => WriteType::Save,
        "status" => WriteType::Status,
        "checklist" => WriteType::Checklist,
        "calendar" => WriteType::Calendar,
        "profile" => WriteType::Profile,
        "form" => WriteType::Form,
        "page" => WriteType::Page,
        _ => return None,
    })
}

fn act(name: &str) -> Act {
    match name {
        "request" => Act::Request,
        "mention" => Act::Mention,
        "question" => Act::Question,
        "report" => Act::Report,
        "retract" => Act::Retract,
        _ => Act::Unclear,
    }
}

fn top(values: &[(String, f64)]) -> Option<&str> {
    values
        .iter()
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(l, _)| l.as_str())
}

/// How the goal refers to programmes: a plural or a quantifier over items
/// ("…들", "다", "전부", "모두", "모든", "싹", "일괄", "전체"), and an
/// explicit list word ("목록", "리스트").
fn list_reference(words: &[String]) -> (bool, bool) {
    const QUANTIFIERS: &[&str] = &[
        "다",
        "전부",
        "모두",
        "모든",
        "싹",
        "일괄",
        "일괄로",
        "전체",
        "몽땅",
    ];
    const PLURAL_ENDINGS: &[&str] = &["들", "들은", "들을", "들도", "들이", "들의", "들만"];
    const LIST_WORDS: &[&str] = &["목록", "리스트"];
    let plural = words.iter().any(|w| {
        QUANTIFIERS.contains(&w.as_str())
            || (w.chars().count() > 1 && PLURAL_ENDINGS.iter().any(|e| w.ends_with(e)))
    });
    let list = words
        .iter()
        .any(|w| LIST_WORDS.iter().any(|l| w.starts_with(l) || w.contains(l)));
    (plural || list, list)
}

/// The programme a write is about, after the policy's vetoes. The model's
/// target head says which kind of reference it is; the screen and the
/// goal's number decide what it can mean (batch 2: on screen, "the one I
/// saved" is the programme on screen, not the saved list):
/// - saved: the saved list only when the user has saved programmes and
///   either names a list ("목록", "리스트") or, with nothing on screen,
///   refers to several ("…들", "다", "전부"); otherwise, on screen, the
///   programme on screen;
/// - selected: only with a selection;
/// - on screen: only with a programme on screen.
pub fn target(understood: &Understood, context: GoalContext, goal: &str) -> Option<ProgramRef> {
    let best = understood
        .target
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.total_cmp(b.1))
        .map(|(i, _)| TARGETS[i]);
    match best {
        Some("saved") => {
            let (several, list_word) = list_reference(&cue_words(goal));
            if context.saved > 0 && (list_word || (several && !context.has_program)) {
                Some(ProgramRef::Saved)
            } else if context.has_program {
                Some(ProgramRef::Context)
            } else {
                None
            }
        }
        Some("selected") if context.selected > 0 => Some(ProgramRef::Selected),
        Some("on_screen") if context.has_program => Some(ProgramRef::Context),
        _ => None,
    }
}

fn status(label: &str) -> Option<ApplicationStatus> {
    Some(match label {
        "planning" => ApplicationStatus::Planning,
        "applied" => ApplicationStatus::Applied,
        "waiting" => ApplicationStatus::Waiting,
        "received" => ApplicationStatus::Received,
        "abandoned" => ApplicationStatus::Abandoned,
        // "rejected" (core records no rejection) and anything else: no
        // status card; the answer offers the status picker.
        _ => return None,
    })
}

/// The write probability from which a type whose top act is "none" still
/// gets its button (`Unclear`): a miss costs the user, a button costs a tap.
pub const BUTTON_FLOOR: f64 = 0.25;

/// The act the policy keeps for a type, from the top act, the write
/// probability and whether it reaches the card bar.
fn act_for(write: WriteType, at_bar: bool, best: &str) -> Act {
    match act(best) {
        read if super::writes_for(write, read) => {
            if at_bar {
                read
            } else {
                Act::Unclear
            }
        }
        // A question, mention, retraction or report (of an alert, a save)
        // is kept for the answer and never cards; "none" on top (kept only
        // from BUTTON_FLOOR, see `intents`) is unclear.
        read => read,
    }
}

/// The goal's write intents from the model's reading.
pub fn intents(
    goal: &str,
    context: GoalContext,
    model: &Understanding,
    understood: &Understood,
) -> Vec<WriteIntent> {
    let program = target(understood, context, goal);
    let words = cue_words(goal);
    let mut out = Vec::new();
    for (i, name) in TYPES.iter().enumerate() {
        let Some(write) = write_type(name) else {
            continue;
        };
        let acts = &understood.acts[i];
        let best = acts
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map_or("none", |(k, _)| ACTS[k]);
        let mass = model.write_mass(understood, name);
        if best == "none" && mass < BUTTON_FLOOR {
            continue;
        }
        let mut act = act_for(write, model.writes(understood, name), best);
        // One rule for every value head (alert, save, checklist, status): a
        // card only when its value is clear; below the value bar the
        // state-aware button, which offers what the current state allows
        // (batch 2: an unsave read as save; review M3: a rejection read as
        // applied).
        if super::writes_for(write, act)
            && Understanding::value_margin(understood, name) < model.value_bar(name)
        {
            act = Act::Unclear;
        }
        let value = understood.values.get(*name).and_then(|v| top(v));
        let on = match (write, value) {
            (WriteType::Alert, Some(v)) => Some(v == "on"),
            (WriteType::Save, Some(v)) => Some(v == "save"),
            (WriteType::Checklist, Some(v)) => Some(v == "done"),
            _ => None,
        };
        // A checklist mark and a status exist only for the programme on
        // screen (their tools take no other): with one on screen it is their
        // target unless the goal points at another (found, selected, saved:
        // PR #20 review M5).
        // QA B4: "통장사본 준비했어" on screen read target "none".
        let on_screen = program == Some(ProgramRef::Context);
        let screen_only =
            context.has_program && matches!(program, None | Some(ProgramRef::Context));
        let step = match write {
            WriteType::Alert => program
                .zip(on)
                .map(|(program, enabled)| ToolCall::SetAlert(AlertArgs { program, enabled })),
            WriteType::Save => program.zip(on).map(|(program, bookmarked)| {
                ToolCall::Bookmark(BookmarkArgs {
                    program,
                    bookmarked,
                })
            }),
            WriteType::Status => value
                .and_then(status)
                .filter(|_| screen_only)
                .map(|status| {
                    ToolCall::RecordApplicationStatus(StatusArgs {
                        program: ProgramRef::Context,
                        status,
                    })
                }),
            WriteType::Checklist => goals::named_document(&words)
                .filter(|_| screen_only)
                .zip(on)
                .map(|(task, done)| {
                    ToolCall::ToggleChecklistItem(ChecklistArgs {
                        program: ProgramRef::Context,
                        task,
                        done,
                    })
                }),
            WriteType::Calendar => on_screen.then(|| {
                ToolCall::AddToCalendar(CalendarArgs {
                    program: ProgramRef::Context,
                    date: goals::calendar_date(&words),
                    days_before: goals::calendar_days_before(&words),
                })
            }),
            WriteType::Profile => goals::own_profile_patch(goal).map(ToolCall::ProfilePatch),
            // The form stays with its route (its template); the page is the
            // programme on screen for a client that opens handoffs.
            WriteType::Page => super::page_step(on_screen, context),
            WriteType::Form => None,
        };
        let whole = matches!(write, WriteType::Profile);
        out.push(WriteIntent {
            write,
            act,
            on,
            step,
            whole,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::intent::writes_for;

    #[test]
    fn a_write_act_below_the_card_bar_is_never_a_card() {
        for write in [WriteType::Alert, WriteType::Save, WriteType::Calendar] {
            let a = act_for(write, false, "request");
            assert_eq!(a, Act::Unclear);
            assert!(!writes_for(write, a));
        }
        assert_eq!(act_for(WriteType::Status, false, "report"), Act::Unclear);
        assert_eq!(
            act_for(WriteType::Checklist, false, "request"),
            Act::Unclear
        );
    }

    /// Batch 1: a question about what the form helper fills got a form
    /// card. A question, mention or retraction on top never cards, even
    /// with the write mass at the bar (form and page bars are low).
    #[test]
    fn a_question_never_cards_whatever_the_mass() {
        for best in ["question", "mention", "retract"] {
            for write in [
                WriteType::Form,
                WriteType::Page,
                WriteType::Alert,
                WriteType::Status,
            ] {
                let a = act_for(write, true, best);
                assert!(!writes_for(write, a), "{best} {write:?} -> {a:?}");
                assert_ne!(a, Act::Unclear);
            }
        }
        // A report is a card only for status and checklist.
        assert!(!writes_for(
            WriteType::Alert,
            act_for(WriteType::Alert, true, "report")
        ));
        assert_eq!(act_for(WriteType::Status, true, "report"), Act::Report);
    }

    #[test]
    fn none_on_top_is_unclear_and_never_a_card() {
        assert_eq!(act_for(WriteType::Page, false, "none"), Act::Unclear);
        assert_eq!(act_for(WriteType::Page, true, "none"), Act::Unclear);
    }

    #[test]
    fn at_the_bar_a_card_is_a_request_or_a_status_report() {
        assert_eq!(act_for(WriteType::Alert, true, "request"), Act::Request);
        assert_eq!(act_for(WriteType::Status, true, "report"), Act::Report);
        assert_eq!(act_for(WriteType::Checklist, true, "report"), Act::Report);
    }

    fn understood_with_target(best: &str) -> Understood {
        let mut target = vec![0.0; TARGETS.len()];
        target[TARGETS.iter().position(|t| *t == best).unwrap()] = 1.0;
        Understood {
            acts: Vec::new(),
            values: Default::default(),
            target,
        }
    }

    /// Batch 2: on screen, "the one I saved" is the programme on screen.
    /// The saved list needs a list word, or a plural with nothing on
    /// screen, and saved programmes.
    #[test]
    fn the_saved_list_needs_a_list_reference() {
        let saved = understood_with_target("saved");
        let ctx = |has_program, saved| GoalContext {
            has_program,
            selected: 0,
            saved,
            suspect: false,
            handoffs: false,
        };
        let t = |goal: &str, c| target(&saved, c, goal);
        // On screen, a singular reference to a saved programme: the screen's.
        assert_eq!(
            t("내가 저장한 거 알림 켜줘", ctx(true, 3)),
            Some(ProgramRef::Context)
        );
        assert_eq!(
            t("찜한 거 알림 꺼 줘", ctx(true, 3)),
            Some(ProgramRef::Context)
        );
        // An explicit list word: the list, on screen or not.
        assert_eq!(
            t("관심 목록 알림 다 꺼줘", ctx(true, 3)),
            Some(ProgramRef::Saved)
        );
        assert_eq!(
            t("저장 리스트 알림 켜줘", ctx(false, 3)),
            Some(ProgramRef::Saved)
        );
        // Off screen, a plural or a quantifier: the list.
        assert_eq!(
            t("저장한 장학금들 알림 켜줘", ctx(false, 3)),
            Some(ProgramRef::Saved)
        );
        assert_eq!(
            t("찜한 거 알림 전부 꺼줘", ctx(false, 3)),
            Some(ProgramRef::Saved)
        );
        // Off screen, singular: no programme (the button).
        assert_eq!(t("저장한 거 알림 켜줘", ctx(false, 3)), None);
        // Nothing saved: never the list.
        assert_eq!(t("관심 목록 알림 다 꺼줘", ctx(false, 0)), None);
        assert_eq!(
            t("관심 목록 알림 다 꺼줘", ctx(true, 0)),
            Some(ProgramRef::Context)
        );
        // The other targets keep their vetoes.
        let on = understood_with_target("on_screen");
        assert_eq!(target(&on, ctx(false, 3), "이거 알림 켜줘"), None);
        assert_eq!(
            target(&on, ctx(true, 0), "이거 알림 켜줘"),
            Some(ProgramRef::Context)
        );
    }

    #[test]
    fn list_references_are_plurals_quantifiers_or_list_words() {
        let r = |g: &str| list_reference(&cue_words(g));
        assert_eq!(r("저장한 공고들 알림 꺼줘"), (true, false));
        assert_eq!(r("알림 다 켜줘"), (true, false));
        assert_eq!(r("관심 목록에 있는 거 알림"), (true, true));
        assert_eq!(r("저장한 거 알림 켜줘"), (false, false));
        // "들어가" is a verb, not a plural.
        assert_eq!(r("저장한 데 들어가서 알림 켜줘"), (false, false));
    }

    /// Review M3: a rejection has no status to record (core has none).
    #[test]
    fn a_rejection_records_no_status() {
        assert_eq!(status("rejected"), None);
        assert_eq!(status("applied"), Some(ApplicationStatus::Applied));
        assert_eq!(status("received"), Some(ApplicationStatus::Received));
    }
}
