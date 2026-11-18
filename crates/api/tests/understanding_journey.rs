//! The understanding model installed in its own process (it is a global):
//! the run's journey keeps the writes the model and the policy produce.
//! Needs the pinned e5 files:
//! `INTENT_CLASSIFIER_MODEL_DIR=… cargo test -p api --test understanding_journey -- --ignored`.

use api::agent::classifier::understand::{Understanding, UnderstandingPaths};
use api::agent::classifier::ArtefactPaths;
use api::agent::goals::{self, Decision, GoalContext};
use api::agent::intent;
use api::agent::observe::{expand, Journey};
use mazimi_core::agent::{ProgramRef, ToolCall};
use std::sync::Arc;

fn install() -> bool {
    let Ok(dir) = std::env::var("INTENT_CLASSIFIER_MODEL_DIR") else {
        return false;
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
    intent::install_understanding(Arc::new(model));
    assert!(intent::model_active());
    true
}

/// Integrated review H1: the home chip and the saved-list alert button
/// ("저장한 혜택 알림 다 켜줘/꺼줘", nothing on screen, three saved) keep
/// their saved-list alert card through the run's journey.
#[test]
#[ignore = "needs INTENT_CLASSIFIER_MODEL_DIR with the e5 files"]
fn the_saved_list_alert_survives_the_journey() {
    if !install() {
        return;
    }
    let context = GoalContext {
        has_program: false,
        selected: 0,
        saved: 3,
        suspect: false,
        handoffs: false,
    };
    for (goal, enabled) in [
        ("저장한 혜택 알림 다 켜줘", true),
        ("저장한 혜택 알림 다 꺼줘", false),
    ] {
        // As `run::plan`: the goal is read once, before routing.
        intent::prepare_blocking(goal);
        let Decision::Plan { plan, kind } =
            goals::decide_with_model_and(goal, context, goals::Extras::ALL)
        else {
            panic!("{goal}: not planned");
        };
        // As `run::drive`: the run's context, with the real saved count.
        let journey = Journey {
            on_screen: false,
            saved: context.saved,
            ..Journey::ALL
        };
        assert_eq!(journey.context(), context);
        let ran = expand(plan, goal, kind, journey);
        assert!(
            ran.steps.iter().any(|s| matches!(s,
                ToolCall::SetAlert(a) if a.program == ProgramRef::Saved && a.enabled == enabled)),
            "{goal}: {:?}",
            ran.steps
        );
    }
}

/// Integrated review M2: the model reads a goal once, in the run's
/// `intent::prepare`; no reader calls it. A goal that was not read (or
/// timed out) has no card on any path.
#[test]
#[ignore = "needs INTENT_CLASSIFIER_MODEL_DIR with the e5 files"]
fn readers_use_the_prepared_reading_only() {
    if !install() {
        return;
    }
    let on_screen = GoalContext {
        has_program: true,
        selected: 0,
        saved: 0,
        suspect: false,
        handoffs: false,
    };
    let goal = "이 장학금 마감 알림 좀 켜 주세요";
    assert!(
        intent::read(goal, on_screen, None).is_empty(),
        "no reading, no card"
    );
    intent::prepare_blocking(goal);
    assert!(intent::read(goal, on_screen, None)
        .iter()
        .any(|i| intent::writes_for(i.write, i.act) && i.step.is_some()));
    // The same reading under another context: the policy, not the model,
    // applies the screen.
    let off_screen = GoalContext {
        has_program: false,
        ..on_screen
    };
    assert!(intent::read(goal, off_screen, None)
        .iter()
        .all(|i| i.step.is_none()));
}

/// Delta review HIGH: a read that times out is not remembered. The next
/// run of the same goal, with the model idle, reads it and gets the card.
#[tokio::test]
#[ignore = "needs INTENT_CLASSIFIER_MODEL_DIR with the e5 files"]
async fn a_timed_out_read_is_read_again_next_run() {
    if !install() {
        return;
    }
    let on_screen = GoalContext {
        has_program: true,
        selected: 0,
        saved: 0,
        suspect: false,
        handoffs: false,
    };
    let goal = "이 지원금 마감 알림 꼭 켜 주세요";
    // No time at all: the read cannot finish.
    intent::prepare_within(goal, std::time::Duration::ZERO).await;
    assert!(intent::read(goal, on_screen, None).is_empty());
    // The next run, the model idle.
    intent::prepare_within(goal, std::time::Duration::from_secs(10)).await;
    assert!(
        intent::read(goal, on_screen, None)
            .iter()
            .any(|i| intent::writes_for(i.write, i.act) && i.step.is_some()),
        "the timeout was cached"
    );
}

/// Delta review HIGH: timed-out reads cannot starve the model's pool. At
/// most `READS_IN_FLIGHT` reads are queued or running; a run that finds no
/// slot leaves nothing queued, and every slot comes back when its read ends.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs INTENT_CLASSIFIER_MODEL_DIR with the e5 files"]
async fn timed_out_reads_cannot_pile_up() {
    if !install() {
        return;
    }
    let runs: Vec<_> = (0..24)
        .map(|i| {
            tokio::spawn(async move {
                let goal = format!("이 장학금 {i}번째 알림 켜 줘");
                intent::prepare_within(&goal, std::time::Duration::from_millis(1)).await;
            })
        })
        .collect();
    for run in runs {
        run.await.unwrap();
    }
    assert!(intent::free_read_slots() <= intent::READS_IN_FLIGHT);
    // Every read still running finishes and returns its slot.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while intent::free_read_slots() < intent::READS_IN_FLIGHT
        && std::time::Instant::now() < deadline
    {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(intent::free_read_slots(), intent::READS_IN_FLIGHT);
}

/// Whether a plan step is the card a button asks for.
type CardCheck = Box<dyn Fn(&ToolCall) -> bool>;

/// Delta review 2: every fixed button the answer offers produces its card
/// under the model, through the run's route, gate and producer, even with
/// no model reading of its goal (a timeout cannot make it loop).
#[test]
#[ignore = "needs INTENT_CLASSIFIER_MODEL_DIR with the e5 files"]
fn every_fixed_button_cards_under_the_model() {
    use api::agent::answer::{PROGRAM_ALERT_GOAL, SELECTED_ALERT_GOAL};
    use api::agent::compose::{
        APPLIED_GOAL, CALENDAR_GOAL, PROGRAM_ALERT_OFF_GOAL, SAVED_ALERTS_OFF_GOAL,
        SAVED_ALERTS_ON_GOAL, SAVE_GOAL, UNSAVE_GOAL,
    };
    use mazimi_core::agent::ApplicationStatus;
    if !install() {
        return;
    }
    let on_screen = GoalContext {
        has_program: true,
        selected: 0,
        saved: 3,
        suspect: false,
        handoffs: false,
    };
    let off_screen = GoalContext {
        has_program: false,
        ..on_screen
    };
    let selection = GoalContext {
        has_program: false,
        selected: 2,
        saved: 0,
        suspect: false,
        handoffs: false,
    };
    // Nothing saved: the saved-list buttons still card the saved list (the
    // tool then says nothing is saved), never the button again.
    let nothing_saved = GoalContext {
        has_program: false,
        selected: 0,
        saved: 0,
        suspect: false,
        handoffs: false,
    };
    let cases: Vec<(&str, GoalContext, CardCheck)> = vec![
        (
            PROGRAM_ALERT_GOAL,
            on_screen,
            Box::new(
                |s| matches!(s, ToolCall::SetAlert(a) if a.program == ProgramRef::Context && a.enabled),
            ),
        ),
        (
            PROGRAM_ALERT_OFF_GOAL,
            on_screen,
            Box::new(
                |s| matches!(s, ToolCall::SetAlert(a) if a.program == ProgramRef::Context && !a.enabled),
            ),
        ),
        (
            SAVED_ALERTS_ON_GOAL,
            off_screen,
            Box::new(
                |s| matches!(s, ToolCall::SetAlert(a) if a.program == ProgramRef::Saved && a.enabled),
            ),
        ),
        (
            SAVED_ALERTS_OFF_GOAL,
            off_screen,
            Box::new(
                |s| matches!(s, ToolCall::SetAlert(a) if a.program == ProgramRef::Saved && !a.enabled),
            ),
        ),
        (
            SAVED_ALERTS_ON_GOAL,
            nothing_saved,
            Box::new(
                |s| matches!(s, ToolCall::SetAlert(a) if a.program == ProgramRef::Saved && a.enabled),
            ),
        ),
        (
            SAVED_ALERTS_OFF_GOAL,
            nothing_saved,
            Box::new(
                |s| matches!(s, ToolCall::SetAlert(a) if a.program == ProgramRef::Saved && !a.enabled),
            ),
        ),
        (
            SELECTED_ALERT_GOAL,
            selection,
            Box::new(
                |s| matches!(s, ToolCall::SetAlert(a) if a.program == ProgramRef::Selected && a.enabled),
            ),
        ),
        (
            SAVE_GOAL,
            on_screen,
            Box::new(|s| matches!(s, ToolCall::Bookmark(b) if b.bookmarked)),
        ),
        (
            UNSAVE_GOAL,
            on_screen,
            Box::new(|s| matches!(s, ToolCall::Bookmark(b) if !b.bookmarked)),
        ),
        (
            APPLIED_GOAL,
            on_screen,
            Box::new(
                |s| matches!(s, ToolCall::RecordApplicationStatus(a) if a.status == ApplicationStatus::Applied),
            ),
        ),
        (
            CALENDAR_GOAL,
            on_screen,
            Box::new(|s| matches!(s, ToolCall::AddToCalendar(_))),
        ),
    ];
    for (goal, context, card) in cases {
        // No model reading at all (as after a timeout): a button needs none.
        let ran = match goals::decide_with_model_and(goal, context, goals::Extras::ALL) {
            Decision::Plan { plan, kind } => expand(
                plan,
                goal,
                kind,
                Journey {
                    on_screen: context.has_program,
                    selected: context.selected,
                    saved: context.saved,
                    ..Journey::ALL
                },
            ),
            other => panic!("{goal}: {other:?}"),
        };
        assert!(ran.steps.iter().any(&card), "{goal}: {:?}", ran.steps);
    }
}
