//! Shape and freeze checks for the independent guard-evaluation corpus
//! (`fixtures/agent_guard_corpus.json`). Provenance lives in
//! `fixtures/agent_guard_corpus.README.md`. Nothing here runs the guard; the
//! corpus is only frozen so a later scorer cannot be tuned by editing it.

use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::HashSet;

const CORPUS: &str = include_str!("fixtures/agent_guard_corpus.json");
const README: &str = include_str!("fixtures/agent_guard_corpus.README.md");

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    id: String,
    goal: String,
    expect: String,
}

fn cases() -> Vec<Case> {
    serde_json::from_str(CORPUS).expect("corpus is an array of {id, goal, expect}")
}

/// The corpus is frozen: if this hash changes, someone edited it.
/// Add a new versioned file instead.
#[test]
fn corpus_is_frozen() {
    assert_eq!(
        format!("{:x}", Sha256::digest(CORPUS.as_bytes())),
        "ddacc25dc35e8ae4d56f571ebeaea58d796a70cfae28a15e1e4788231b6e9b02"
    );
}

#[test]
fn ids_are_unique() {
    let cases = cases();
    let ids: HashSet<&str> = cases.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids.len(), cases.len());
}

#[test]
fn goals_are_unique_and_non_empty() {
    let cases = cases();
    assert!(cases.iter().all(|c| !c.goal.trim().is_empty()));
    let goals: HashSet<&str> = cases.iter().map(|c| c.goal.as_str()).collect();
    assert_eq!(goals.len(), cases.len());
}

#[test]
fn expect_is_allow_or_refuse_and_matches_id_prefix() {
    for c in cases() {
        assert!(
            matches!(c.expect.as_str(), "allow" | "refuse"),
            "{}: expect={}",
            c.id,
            c.expect
        );
        assert!(c.id.starts_with(&format!("{}-", c.expect)), "{}", c.id);
    }
}

#[test]
fn has_about_260_allow_and_90_refuse_cases() {
    let cases = cases();
    let allow = cases.iter().filter(|c| c.expect == "allow").count();
    let refuse = cases.len() - allow;
    assert!((250..=280).contains(&allow), "allow={allow}");
    assert!((85..=100).contains(&refuse), "refuse={refuse}");
}

#[test]
fn readme_labels_source_as_unreviewed_independent_agent() {
    assert!(README.contains(
        "independent-agent (opus, isolated from guard code and reviews), needs human review"
    ));
}
