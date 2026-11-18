//! Parity between the app's form-context command vocabulary
//! (`apps/mobile/lib/formFill/chatCommands.json`) and the agent's goal rules.
//! In form context the app sends a typed message only when it is one of the
//! server's fixed goals or made only of that vocabulary; these checks keep the
//! two sides from drifting.

use api::agent::{
    answer, compose,
    goals::{self, Decision, GoalContext, GOAL_CHIPS},
};
use std::collections::BTreeSet;

fn vocabulary() -> serde_json::Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../apps/mobile/lib/formFill/chatCommands.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn strings(value: &serde_json::Value) -> Vec<String> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn server_goals_are_exactly_the_servers_fixed_goals() {
    let mut fixed: BTreeSet<String> = [
        compose::FORM_GOAL,
        compose::OFFICIAL_GOAL,
        compose::APPLIED_GOAL,
        compose::PROGRAM_ALERT_OFF_GOAL,
        compose::SAVE_GOAL,
        compose::UNSAVE_GOAL,
        compose::SAVED_ALERTS_ON_GOAL,
        compose::SAVED_ALERTS_OFF_GOAL,
        compose::CALENDAR_GOAL,
        answer::PROGRAM_ALERT_GOAL,
        answer::SELECTED_ALERT_GOAL,
        answer::PREPARE_GOAL,
    ]
    .iter()
    .map(|g| g.to_string())
    .collect();
    fixed.extend(GOAL_CHIPS.iter().map(|(_, goal, _)| goal.to_string()));
    let listed: BTreeSet<String> = strings(&vocabulary()["server_goals"]).into_iter().collect();
    assert_eq!(listed, fixed);
}

/// Every command the app sends in form context is one the agent takes: never
/// refused, and planned by the fast rules unless the fixture lists it under
/// `probes.classifier` (understood by the classifier/model path at run time).
#[test]
fn every_command_the_app_sends_is_a_goal_the_server_takes() {
    let context = GoalContext {
        has_program: true,
        ..GoalContext::default()
    };
    let vocabulary = vocabulary();
    let classifier: BTreeSet<String> = strings(&vocabulary["probes"]["classifier"])
        .into_iter()
        .collect();
    let mut lines = strings(&vocabulary["probes"]["server"]);
    lines.extend(strings(&vocabulary["server_goals"]));
    let mut wrong = Vec::new();
    for line in &lines {
        let decision = goals::decide_with_model(line, context);
        let ok = match &decision {
            Decision::Refuse(_) => false,
            Decision::Unrecognized => classifier.contains(line),
            _ => !classifier.contains(line),
        };
        if !ok {
            wrong.push(format!("{line}: {decision:?}"));
        }
    }
    assert!(classifier.iter().all(|c| lines.contains(c)));
    assert!(wrong.is_empty(), "{wrong:#?}");
}
