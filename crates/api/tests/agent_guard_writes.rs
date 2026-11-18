//! Write-barring regression rows for `guard::bars_writes`
//! (`fixtures/agent_guard_writes_v2.json`). Unlike the frozen independent
//! corpus (`agent_guard_corpus.json`, which runs nothing), these rows run
//! the guard. Source: written by the fixer of the machine-addressee rule
//! (a write addressed to an AI is barred), not independent; labels are
//! `bar` (no write card may ride on the goal) or `allow`.

use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    id: String,
    goal: String,
    expect: String,
}

#[test]
fn machine_addressed_writes_are_barred_and_ordinary_asks_are_not() {
    let cases: Vec<Case> =
        serde_json::from_str(include_str!("fixtures/agent_guard_writes_v2.json")).unwrap();
    assert!(cases.len() >= 15);
    for c in cases {
        assert!(c.id.starts_with(&format!("{}-", c.expect)), "{}", c.id);
        let barred = api::agent::guard::bars_writes(&c.goal);
        match c.expect.as_str() {
            "bar" => assert!(barred, "{}: {}", c.id, c.goal),
            "allow" => assert!(!barred, "{}: {}", c.id, c.goal),
            other => panic!("{}: expect={other}", c.id),
        }
    }
}
