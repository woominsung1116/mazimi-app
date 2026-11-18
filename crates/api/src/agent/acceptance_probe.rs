//! Write-acceptance probe (ignored): which cards a goal gets on the
//! production route, per write type, under a set of tool flags.
//!
//! Mirrors `run::plan_route` → `observe::expand` → the flag filter, without
//! a database or the planner model. A goal left to the model is planned as
//! no reads (the produced writes are still merged, as `run` does for a plan
//! of no steps). Prints aggregates only, never row text.
//!
//! ```text
//! ACCEPTANCE_ROWS=<tsv: id, label, screen, kind, text> \
//! INTENT_CLASSIFIER_MODEL_DIR=<e5 dir: model.safetensors, tokenizer.json> \
//! cargo test -p api --lib acceptance_probe -- --ignored --nocapture --test-threads=1
//! ```
//! Labels: one letter per asked write (a alert on, o alert off, b save,
//! u unsave, s status, c checklist, k calendar, r profile, f form, p page),
//! `-` no write, `?` ambiguous (not scored).
use super::{
    classifier::{ArtefactPaths, IntentClassifier, Routed},
    goals::{self, Decision, GoalContext, GoalKind},
    planner::ModelOutcome,
    run::{executed_plan, kept_model_plan, route_decision, Route},
    runtime::ToolFlags,
};
use mazimi_core::agent::{Plan, ToolCall};
use std::collections::BTreeMap;

const TYPES: &str = "aobusckrfp";

fn letter(step: &ToolCall) -> Option<char> {
    Some(match step {
        ToolCall::SetAlert(x) if x.enabled => 'a',
        ToolCall::SetAlert(_) => 'o',
        ToolCall::Bookmark(x) if x.bookmarked => 'b',
        ToolCall::Bookmark(_) => 'u',
        ToolCall::RecordApplicationStatus(_) => 's',
        ToolCall::ToggleChecklistItem(_) => 'c',
        ToolCall::AddToCalendar(_) => 'k',
        ToolCall::ProfilePatch(_) => 'r',
        ToolCall::OpenFormFill(_) => 'f',
        ToolCall::OpenOfficialApplication(_) => 'p',
        _ => return None,
    })
}

/// The cards the production route gives `goal` (classifier and
/// understanding loaded, the model planner available): `run`'s own
/// [`route_decision`], [`kept_model_plan`] and [`executed_plan`].
fn cards(goal: &str, context: GoalContext, flags: ToolFlags, cls: &IntentClassifier) -> Vec<char> {
    super::intent::prepare_blocking(goal);
    let mut context = context;
    let routed = Some(if goals::guarded(goal).is_none() {
        match cls.route(goal, context) {
            Ok(Routed::Answered { outcome, label, .. }) => Ok((outcome, label)),
            Ok(Routed::Deferred { label, .. }) => Err(Some(label)),
            Err(_) => Err(None),
        }
    } else {
        Err(None)
    });
    let route = route_decision(goal, &mut context, routed, flags.extras(), true, |t| {
        cls.knows(t)
    });
    let (kind, plan) = match route {
        Route::Decision(Decision::Plan { kind, plan }) => (kind, plan),
        Route::Classified(ModelOutcome::Plan(plan), _) => (GoalKind::Classifier, plan),
        // The model's plan, approximated as no reads.
        Route::Decision(Decision::Unrecognized) => {
            match kept_model_plan(Plan { steps: Vec::new() }, goal, context, flags) {
                Some(plan) => (GoalKind::Model, plan),
                None => return Vec::new(),
            }
        }
        _ => return Vec::new(),
    };
    let plan = executed_plan(plan, goal, kind, flags, context);
    let mut out: Vec<char> = plan.steps.iter().filter_map(letter).collect();
    out.sort_unstable();
    out.dedup();
    out
}

#[test]
#[ignore = "needs ACCEPTANCE_ROWS and INTENT_CLASSIFIER_MODEL_DIR"]
fn acceptance_probe() {
    use super::classifier::understand::{Understanding, UnderstandingPaths};
    let (Ok(rows), Ok(dir)) = (
        std::env::var("ACCEPTANCE_ROWS"),
        std::env::var("INTENT_CLASSIFIER_MODEL_DIR"),
    ) else {
        return;
    };
    let dir = std::path::Path::new(&dir);
    let encoder = ArtefactPaths {
        classifier: concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/intent-classifier/classifier.json"
        )
        .into(),
        ..ArtefactPaths::in_dir(dir)
    };
    let cls = IntentClassifier::load(&encoder, 2).expect("classifier");
    let understanding = Understanding::load(
        &UnderstandingPaths {
            model: concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/understanding/understanding.json"
            )
            .into(),
            encoder: ArtefactPaths::in_dir(dir),
        },
        2,
    )
    .expect("understanding");
    super::intent::install_understanding(std::sync::Arc::new(understanding));
    assert!(super::intent::model_active());
    let text = std::fs::read_to_string(rows).unwrap();
    let production = ToolFlags::from_env();
    for (name, flags) in [
        ("production env", production),
        ("all tools on", ToolFlags::ALL),
    ] {
        let mut asked: BTreeMap<char, (usize, usize)> = BTreeMap::new();
        let mut unasked = 0;
        let mut scored = 0;
        for line in text.lines().skip(1) {
            let cols: Vec<&str> = line.split('\t').collect();
            let [_, label, screen, _, goal] = cols[..] else {
                continue;
            };
            if label == "?" {
                continue;
            }
            scored += 1;
            // A current client (journey-v1) on a deployment with the page
            // handoff: as production sets it in `run::plan`.
            let context = GoalContext {
                has_program: screen == "Y",
                selected: 0,
                saved: 3,
                suspect: false,
                handoffs: flags.allows(mazimi_core::agent::ToolName::OpenOfficialApplication),
            };
            let got = cards(goal, context, flags, &cls);
            let want: Vec<char> = label.chars().filter(|c| TYPES.contains(*c)).collect();
            for t in &want {
                let e = asked.entry(*t).or_default();
                e.1 += 1;
                if got.contains(t) {
                    e.0 += 1;
                }
            }
            if got.iter().any(|c| !want.contains(c)) {
                unasked += 1;
            }
        }
        let cells: Vec<String> = TYPES
            .chars()
            .map(|t| {
                let (hit, n) = asked.get(&t).copied().unwrap_or_default();
                format!("{t} {hit}/{n}")
            })
            .collect();
        eprintln!(
            "\nprobe [{name}] {} | rows with an unasked card {unasked}/{scored}",
            cells.join(" | ")
        );
    }
}

/// Dev phrasings written for this change (`tests/fixtures/calendar_dev_goals.txt`,
/// not from any sealed batch): the model's calendar reading next to the
/// rules' calendar readers.
#[test]
#[ignore = "needs INTENT_CLASSIFIER_MODEL_DIR"]
fn calendar_dev_readings() {
    use super::classifier::understand::{Understanding, UnderstandingPaths, ACTS, TYPES};
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
    let shared = std::sync::Arc::new(model);
    super::intent::install_understanding(shared.clone());
    let model = shared;
    let cls = IntentClassifier::load(
        &ArtefactPaths {
            classifier: concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/intent-classifier/classifier.json"
            )
            .into(),
            ..ArtefactPaths::in_dir(std::path::Path::new(&dir))
        },
        2,
    )
    .unwrap();
    let k = TYPES.iter().position(|t| *t == "calendar").unwrap();
    eprintln!("\ncalendar bar {:.2}", model.card_bar("calendar"));
    let dev = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/calendar_dev_goals.txt"
    ))
    .unwrap();
    for goal in dev.lines().filter(|l| !l.is_empty() && !l.starts_with('#')) {
        let u = model.read(goal).unwrap();
        let (best, p) = u.acts[k]
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map(|(i, p)| (ACTS[i], *p))
            .unwrap();
        let words = crate::assistant_provider::cue_words(goal);
        let on_screen = GoalContext {
            has_program: true,
            selected: 0,
            saved: 3,
            suspect: false,
            handoffs: true,
        };
        let target = super::classifier::understand::TARGETS[u
            .target
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .unwrap()
            .0];
        eprintln!(
            "{goal}\tact={best}:{p:.2}\tmass={:.2}\ttarget={target}\tcards::calendar={}\troute={}\tcards={:?}",
            model.write_mass(&u, "calendar"),
            super::cards::calendar(goal),
            goals::names_calendar(&words),
            cards(goal, on_screen, ToolFlags::ALL, &cls),
        );
        if let Ok(Routed::Deferred { label, confidence }) = cls.route(goal, on_screen) {
            eprintln!("\tdeferred {label} {confidence:.2}");
        }
    }
}
