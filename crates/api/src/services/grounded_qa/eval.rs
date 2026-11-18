//! Q&A eval gate over `tests/fixtures/grounded_qa_draft.json`.
//!
//! The fixture is draft-by-agent and was run while the retriever was
//! written, so these numbers are a regression floor, never a quality claim.
//! A human-verified set decides release (contract §G-eval).
use super::passages::tests::program;
use super::verdict::Overall;
use super::{answer, Inputs};
use mazimi_core::rule_engine::RuleVerdict;
use mazimi_core::structural::StructuralProfile;
use serde::Deserialize;
use std::collections::HashMap;

const DRAFT: &str = include_str!("../../../tests/fixtures/grounded_qa_draft.json");

#[derive(Deserialize)]
struct Fixture {
    meta: Meta,
    programs: HashMap<String, FixtureProgram>,
    cases: Vec<Case>,
}
#[derive(Deserialize)]
struct Meta {
    labeler: String,
    created_at: String,
}
#[derive(Deserialize)]
struct FixtureProgram {
    source: String,
    fields: serde_json::Value,
}
#[derive(Deserialize)]
struct Case {
    program: String,
    question: String,
    #[serde(default)]
    expect: Vec<String>,
    #[serde(default)]
    not_found: bool,
}

pub(crate) struct Metrics {
    pub cases: usize,
    pub answerable: usize,
    pub hits: usize,
    pub predicted_not_found: usize,
    pub correct_not_found: usize,
    pub expected_not_found: usize,
}

pub(crate) fn run(raw: &str) -> (Metrics, Vec<String>) {
    let fixture: Fixture = serde_json::from_str(raw).expect("fixture parses");
    assert!(fixture.meta.labeler.contains("needs human review"));
    assert!(!fixture.meta.created_at.is_empty());
    let mut metrics = Metrics {
        cases: fixture.cases.len(),
        answerable: 0,
        hits: 0,
        predicted_not_found: 0,
        correct_not_found: 0,
        expected_not_found: 0,
    };
    let mut misses = Vec::new();
    for case in &fixture.cases {
        let source = &fixture.programs[&case.program];
        assert!(!source.source.is_empty());
        let p = program(source.fields.clone());
        let result = answer(&Inputs {
            question: &case.question,
            previous_topic: None,
            program: &p,
            program_university_id: None,
            facts: StructuralProfile::default(),
            overall: Overall {
                status: RuleVerdict::Unknown,
                summary: "아직 확인할 조건이 있어요.",
                income: None,
                overridden: &[],
            },
            evidence: &[],
        });
        let shown: Vec<&str> = result.passages.iter().map(|p| p.id.as_str()).collect();
        if result.not_found {
            metrics.predicted_not_found += 1;
        }
        if case.not_found {
            metrics.expected_not_found += 1;
            if result.not_found {
                metrics.correct_not_found += 1;
            } else {
                misses.push(format!(
                    "  [{}] {} => {shown:?} (expected not found)",
                    case.program, case.question
                ));
            }
        } else {
            metrics.answerable += 1;
            if case.expect.iter().any(|id| shown.contains(&id.as_str())) {
                metrics.hits += 1;
            } else {
                misses.push(format!(
                    "  [{}] {} => {shown:?} (expected one of {:?})",
                    case.program, case.question, case.expect
                ));
            }
        }
    }
    (metrics, misses)
}

/// Draft gate. Measured on the draft set on 2026-10-12; see the contract
/// for the numbers. Lower only with a recorded reason.
#[test]
fn draft_set_hit_at_3_and_not_found_precision() {
    let (m, misses) = run(DRAFT);
    let hit = m.hits as f64 / m.answerable as f64;
    let precision = if m.predicted_not_found == 0 {
        1.0
    } else {
        m.correct_not_found as f64 / m.predicted_not_found as f64
    };
    eprintln!(
        "grounded draft: hit@3 {}/{} = {:.1}%, not-found precision {}/{} = {:.1}%, not-found recall {}/{}\n{}",
        m.hits,
        m.answerable,
        hit * 100.0,
        m.correct_not_found,
        m.predicted_not_found,
        precision * 100.0,
        m.correct_not_found,
        m.expected_not_found,
        misses.join("\n")
    );
    assert!(m.cases >= 40, "at least 40 questions");
    assert!(hit >= 0.85, "hit@3 {hit:.3}");
    assert!(precision >= 0.60, "not-found precision {precision:.3}");
}
