//! Planner evaluation (spec §5.2 Phase 0): goals → expected tool sequence
//! and arguments. Used by the `agent-eval` CLI and by CI with a mock
//! provider. Only the planner is measured; tools are not executed and no
//! database is touched.
//!
//! Four splits:
//! - `dev` (`agent_goals.json`): the set the fast path and the prompt were
//!   tuned on. Its numbers are a regression floor, not a quality claim.
//! - `holdout` (`agent_goals_holdout.json`, holdout v1): written by a
//!   separate labeler that never read the dev set or the router, frozen
//!   before tuning. It is never edited; a new set gets a new file.
//! - `holdout_v2` (`agent_goals_holdout_v2.json`): a second independent set
//!   labelled from the tool catalog and product docs only, frozen the same
//!   way.
//! - `holdout_v3` (`agent_goals_holdout_v3.json`): written after the
//!   planner tuning on v2, without reading the planner, guard, prompts or
//!   any earlier set; label-reviewed by a second agent. Disjoint from the
//!   other splits by goal hash. This is the gate for the next planner.
//!
//! Leakage: holdout v1's per-goal results on the Oracle host were read to
//! diagnose the routing guard (`guard.rs`), and holdout v2's to build
//! planner v3 (`args.rs`, the fast-path precision rules, the model-step
//! alignment in `planner.rs`). Both are dev sets now: their numbers are
//! regression floors, not quality claims. The gate is a newer holdout
//! written without seeing either.
//!
//! The model path is [`planner::plan`], the same call production makes.
//! With an intent classifier ([`evaluate_with`]), pipeline mode routes as
//! production does with `ASSISTANT_INTENT_CLASSIFIER_ENABLED`: guard, then
//! the classifier, then (below its threshold) the rules' confident shapes
//! and the model, or the rules when there is no model.
use super::{
    classifier::{IntentClassifier, Routed},
    goals::{self, Decision, GoalContext},
    planner::{self, ModelFailure, ModelOutcome},
};
use crate::assistant_provider::{PlanProvider, PlanRequest};
use mazimi_core::agent::{Permission, Plan, PlanError, ToolCall};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

pub const FIXTURE: &str = include_str!("../../tests/fixtures/agent_goals.json");
pub const HOLDOUT_FIXTURE: &str = include_str!("../../tests/fixtures/agent_goals_holdout.json");
pub const HOLDOUT_V2_FIXTURE: &str =
    include_str!("../../tests/fixtures/agent_goals_holdout_v2.json");
pub const HOLDOUT_V3_FIXTURE: &str =
    include_str!("../../tests/fixtures/agent_goals_holdout_v3.json");

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Expect {
    Plan(Plan),
    /// Refused: fast-path refusal or a model `refuse`.
    Refuse,
    /// Recognized, but needs a programme on screen.
    NeedsProgram,
    /// Nothing to plan (chit-chat); a refusal also counts.
    NoPlan,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Case {
    pub id: String,
    pub goal: String,
    #[serde(default)]
    pub context: GoalContext,
    pub category: String,
    pub expect: Expect,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Fixture {
    pub labeler: String,
    #[serde(default)]
    pub split: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub frozen: bool,
    /// The date the set's deadline windows are labelled on (dev set);
    /// `None` for the frozen sets, which label day counts.
    #[serde(default)]
    pub today: Option<chrono::NaiveDate>,
    pub cases: Vec<Case>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Split {
    Dev,
    Holdout,
    HoldoutV2,
    HoldoutV3,
}

pub fn fixture() -> Fixture {
    split_fixture(Split::Dev)
}

pub fn split_fixture(split: Split) -> Fixture {
    serde_json::from_str(match split {
        Split::Dev => FIXTURE,
        Split::Holdout => HOLDOUT_FIXTURE,
        Split::HoldoutV2 => HOLDOUT_V2_FIXTURE,
        Split::HoldoutV3 => HOLDOUT_V3_FIXTURE,
    })
    .expect("checked-in agent goal fixture")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    /// Production order: refusal rules and fast paths, then the model for
    /// unrecognized goals.
    Pipeline,
    /// Every goal through the model (measures the model's tool calling).
    ModelOnly,
}

#[derive(Debug, Clone, PartialEq)]
enum Produced {
    Plan(Plan),
    Refuse,
    NeedsProgram,
    NoPlan,
    InvalidJson,
    InvalidPlan,
    ProviderError,
}

impl Produced {
    fn label(&self) -> &'static str {
        match self {
            Self::Plan(_) => "plan",
            Self::Refuse => "refuse",
            Self::NeedsProgram => "needs_program",
            Self::NoPlan => "no_plan",
            Self::InvalidJson => "invalid_json",
            Self::InvalidPlan => "invalid_plan",
            Self::ProviderError => "provider_error",
        }
    }

    /// Tool sequence for a plan, else the outcome label (confusion keys).
    fn signature(&self) -> String {
        match self {
            Self::Plan(plan) => signature(plan),
            other => other.label().to_owned(),
        }
    }
}

fn signature(plan: &Plan) -> String {
    plan.steps
        .iter()
        .map(|s| s.name().as_str())
        .collect::<Vec<_>>()
        .join("+")
}

fn expect_signature(expect: &Expect) -> String {
    match expect {
        Expect::Plan(plan) => signature(plan),
        Expect::Refuse => "refuse".into(),
        Expect::NeedsProgram => "needs_program".into(),
        Expect::NoPlan => "no_plan".into(),
    }
}

/// The plan as the fixtures label it. The goal sets label region, status,
/// types and keywords; the topic fields and the closing-soon window of a
/// search were added after they were frozen and are measured by the topic
/// evaluation instead (`tests/assistant_topic_eval.rs`), so they are not
/// compared here. The digest's window is compared on the set's own date
/// ([`plans_match`]).
fn canonical(plan: &Plan) -> Plan {
    let mut plan = plan.clone();
    for step in &mut plan.steps {
        if let mazimi_core::agent::ToolCall::SearchBenefits(args) = step {
            args.fields.clear();
            args.deadline_within_days = None;
            args.deadline_from_days = None;
        }
    }
    plan.canonicalize();
    plan
}

/// Whether `actual` is the labelled plan. A deadline period is a calendar
/// range of the day it is read on (`args::period_on`):
/// - a set with a date (`Fixture::today`, the dev set) has its digest
///   window read from the goal on that date and compared with the label;
/// - a frozen set states no date: a length ("일주일 내", "3일 안") is the
///   same on every date and is compared as labelled, a named period
///   ("이번 주") is skipped.
///
/// The produced window is re-read from the goal, not taken from the plan.
/// That is sound only while the server derives every digest window from
/// the goal (`planner.rs` strips the model's `within_days` / `from_days`
/// and writes the goal's); a planner that passes a window through would
/// be hidden here.
fn plans_match(
    expected: &Plan,
    actual: &Plan,
    goal: &str,
    today: Option<chrono::NaiveDate>,
) -> bool {
    // Any date reads a length the same; it only tells a length from a name.
    let named = chrono::NaiveDate::from_ymd_opt(2026, 1, 1)
        .and_then(|day| super::args::reading_on(goal, day))
        .is_some_and(|r| r.named);
    let on_date = |plan: &Plan, read: bool| {
        let mut plan = canonical(plan);
        for step in &mut plan.steps {
            if let mazimi_core::agent::ToolCall::Deadlines(args) = step {
                *args = match today {
                    Some(day) if read && args.within_days.is_some() => {
                        super::args::deadline_args(super::args::deadline_period_on(goal, day))
                    }
                    Some(_) => args.clone(),
                    None if named => Default::default(),
                    None => args.clone(),
                };
            }
        }
        plan
    };
    on_date(expected, false) == on_date(actual, true)
}

struct ModelResult {
    produced: Produced,
    calls: u8,
    first_error: Option<PlanError>,
    replies: Vec<String>,
}

async fn model_plan(
    provider: &dyn PlanProvider,
    goal: &str,
    context: GoalContext,
    timeout: Duration,
) -> ModelResult {
    match tokio::time::timeout(timeout, planner::plan(provider, goal, context)).await {
        Ok(run) => ModelResult {
            produced: match run.outcome {
                Ok(ModelOutcome::Plan(plan)) => Produced::Plan(plan),
                Ok(ModelOutcome::Refuse) => Produced::Refuse,
                // The guard's refusal of a writes-only plan, not the model's.
                Ok(ModelOutcome::WritesBarred) => Produced::Refuse,
                Ok(ModelOutcome::NeedsProgram) => Produced::NeedsProgram,
                Err(ModelFailure::Invalid(PlanError::InvalidJson)) => Produced::InvalidJson,
                Err(ModelFailure::Invalid(_)) => Produced::InvalidPlan,
                Err(ModelFailure::Provider(_)) => Produced::ProviderError,
            },
            calls: run.calls,
            first_error: run.first_error,
            replies: run.replies,
        },
        Err(_) => ModelResult {
            produced: Produced::ProviderError,
            calls: 1,
            first_error: None,
            replies: Vec::new(),
        },
    }
}

fn percentile(sorted: &[u64], p: usize) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    sorted[(sorted.len() * p)
        .div_ceil(100)
        .saturating_sub(1)
        .min(sorted.len() - 1)]
}

fn rate(n: usize, d: usize) -> Value {
    if d == 0 {
        Value::Null
    } else {
        json!(n as f64 / d as f64)
    }
}

#[derive(Default)]
struct Tally {
    cases: usize,
    plan_cases: usize,
    tool_ok: usize,
    write_cases: usize,
    card_ok: usize,
    args_ok: usize,
    refusal_cases: usize,
    refusal_ok: usize,
}

impl Tally {
    fn report(&self) -> Value {
        json!({
            "cases": self.cases,
            "plan_cases": self.plan_cases,
            "tool_name_accuracy": rate(self.tool_ok, self.plan_cases),
            "write_cases": self.write_cases,
            "card_accuracy": rate(self.card_ok, self.write_cases),
            "arg_exact_match": rate(self.args_ok, self.plan_cases),
            "refusal_cases": self.refusal_cases,
            "refusal_accuracy": rate(self.refusal_ok, self.refusal_cases),
        })
    }
}

/// Run the planner over the fixture and score it.
pub async fn evaluate(
    fixture: &Fixture,
    provider: Option<&dyn PlanProvider>,
    mode: Mode,
    timeout: Duration,
    limit: usize,
) -> Value {
    evaluate_with(fixture, provider, None, mode, timeout, limit).await
}

fn produced_of(outcome: ModelOutcome) -> Produced {
    match outcome {
        ModelOutcome::Plan(plan) => Produced::Plan(plan),
        ModelOutcome::Refuse => Produced::Refuse,
        ModelOutcome::WritesBarred => Produced::Refuse,
        ModelOutcome::NeedsProgram => Produced::NeedsProgram,
    }
}

/// The intent classifier in the eval pipeline.
#[derive(Clone, Copy)]
pub struct ClassifierRoute<'a> {
    pub classifier: &'a IntentClassifier,
    /// Below the threshold, try the rules' confident shapes
    /// (`goals::decide_with_model`) before the model, as production does.
    pub rules_before_model: bool,
    /// Comparison only: the classifier before the rules' confident shapes
    /// (the order before router tuning).
    pub classifier_first: bool,
    /// As `AgentConfig::planner_refusals`: answer a model refusal as one.
    /// Off (the default), the rules' answer stands in.
    pub model_refusals: bool,
}

/// [`evaluate`], optionally with the intent classifier in the pipeline.
pub async fn evaluate_with(
    fixture: &Fixture,
    provider: Option<&dyn PlanProvider>,
    route: Option<ClassifierRoute<'_>>,
    mode: Mode,
    timeout: Duration,
    limit: usize,
) -> Value {
    let classifier = route.map(|r| r.classifier);
    let mut rows = Vec::new();
    let mut by_source_tally: BTreeMap<&'static str, Tally> = BTreeMap::new();
    // Classifier time per routed goal (µs), routed and deferred counts, and
    // fixture goals that were training goals (leakage on a gate set).
    let mut classifier_us = Vec::new();
    let (mut classifier_answered, mut classifier_deferred, mut train_overlap) =
        (0usize, 0usize, 0usize);
    let mut latencies = Vec::new();
    let mut model_latencies = Vec::new();
    let mut total = Tally::default();
    let mut by_category: BTreeMap<String, Tally> = BTreeMap::new();
    let mut confusion: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    let (mut model_calls, mut model_goals, mut repairs, mut first_invalid) =
        (0usize, 0usize, 0usize, 0usize);
    let (mut invalid_json, mut invalid_plan, mut provider_errors) = (0usize, 0usize, 0usize);
    let (mut writes_proposed, mut writes_without_confirmation, mut injection_writes) =
        (0usize, 0usize, 0usize);
    // Injection-category writes on goals without skip-confirmation phrasing:
    // routing v4 plans "확인 없이 저장해" as a card (user decision), so those
    // count in `injection_writes` (labels that predate v4) but not here.
    let mut injection_writes_non_bypass = 0usize;
    let (mut fast_path, mut by_model, mut by_none) = (0usize, 0usize, 0usize);
    // Fast-path answers that were exact; benign goals refused.
    let (mut fast_path_exact, mut false_refusals) = (0usize, 0usize);
    // Model goals answered by the rules because the model could not plan.
    let mut rules_fallbacks = 0usize;
    // Model refusals not answered as refusals (`model_refusals` off).
    let mut model_refusals_overridden = 0usize;
    // Cards on goals whose label asks for no write.
    let mut unasked_cards = 0usize;
    for case in fixture.cases.iter().take(limit) {
        let started = Instant::now();
        let goal = crate::assistant_provider::redact_question(&case.goal);
        // As `run::plan`: the understanding model reads the goal once before
        // routing (a no-op unless the eval installed it).
        super::intent::prepare_blocking(&goal);
        // Production routing: with a model the fast path answers only its
        // confident shapes, and the rules' answer stands in when the model
        // cannot plan (`run::plan`).
        let rules = || goals::decide(&goal, case.context);
        let mut classified = None;
        let mut classifier_row = Value::Null;
        // The classifier leans to refuse, below its bar: no write is planned.
        let mut suspect = false;
        train_overlap += usize::from(classifier.is_some_and(|c| c.trained_on(&case.goal)));
        let fast = match (mode, route) {
            // As `run::plan`: the guard, the found-search follow-ups and the
            // agent-tools-v3 shapes (all offered here) answer before the
            // classifier.
            (Mode::Pipeline, Some(route)) => match goals::guarded(&goal) {
                Some(decision) => Some(decision),
                None => {
                    let t = Instant::now();
                    let routed = route.classifier.route(&goal, case.context);
                    let us = t.elapsed().as_micros() as u64;
                    classifier_us.push(us);
                    // As `run::plan`: a confident classifier refusal, then the
                    // found-search, declined-part and v3 shapes, then the
                    // rules' confident shapes, then the classifier's answer
                    // (`classifier_first` skips the rules' confident shapes:
                    // the order before tuning).
                    let refused = matches!(
                        routed,
                        Ok(Routed::Answered {
                            outcome: ModelOutcome::Refuse,
                            ..
                        })
                    );
                    // As `run::plan`: a refusal under the bar is routed as usual
                    // but gets no write step (dropped below).
                    suspect = matches!(&routed, Ok(Routed::Deferred { label, .. }) if super::classifier::is_refusal(label));
                    let rules_first = (!refused)
                        .then(|| {
                            goals::found_decision(&goal, case.context)
                                .or_else(|| {
                                    goals::declined_decision(
                                        &goal,
                                        case.context,
                                        goals::Extras::ALL,
                                    )
                                })
                                .or_else(|| {
                                    goals::v3_decision(&goal, case.context, goals::Extras::ALL)
                                })
                                .or_else(|| {
                                    (!route.classifier_first)
                                        .then(|| goals::decide_with_model(&goal, case.context))
                                        .filter(|d| !matches!(d, Decision::Unrecognized))
                                })
                        })
                        .flatten();
                    if let Some(decision) = rules_first {
                        classifier_row = json!({"us": us, "answered": false, "rules_first": true});
                        Some(decision)
                    } else {
                        match routed {
                            Ok(Routed::Answered {
                                outcome,
                                label,
                                confidence,
                            }) => {
                                classifier_answered += 1;
                                classifier_row = json!({"label": label, "confidence": confidence, "us": us, "answered": true});
                                classified = Some(produced_of(outcome));
                                None
                            }
                            other => {
                                classifier_deferred += 1;
                                classifier_row = match other {
                                    Ok(Routed::Deferred { label, confidence }) => {
                                        json!({"label": label, "confidence": confidence, "us": us, "answered": false})
                                    }
                                    _ => json!({"error": true, "us": us, "answered": false}),
                                };
                                // As production: below the threshold the rules'
                                // confident shapes, then the model; without a
                                // model the rules. `rules_before_model: false`
                                // sends every deferral to the model (comparison).
                                Some(match provider {
                                    Some(_) if route.rules_before_model => {
                                        goals::decide_with_model(&goal, case.context)
                                    }
                                    Some(_) => Decision::Unrecognized,
                                    None => rules(),
                                })
                            }
                        }
                    }
                }
            },
            (Mode::Pipeline, None) => Some(match provider {
                Some(_) => goals::decide_with_model(&goal, case.context),
                None => rules(),
            }),
            (Mode::ModelOnly, _) => None,
        };
        // The route's goal kind, for `observe::expand` as `run::drive` passes it.
        let kind = std::cell::Cell::new(goals::GoalKind::Model);
        let from_rules = |decision: Decision| match decision {
            Decision::Plan { plan, kind: k } => {
                kind.set(k);
                Some(Produced::Plan(plan))
            }
            Decision::Refuse(_) => Some(Produced::Refuse),
            Decision::NeedsProgram { .. } => Some(Produced::NeedsProgram),
            Decision::Unrecognized => None,
        };
        let mut calls = 0u8;
        let mut first_error = None;
        let mut replies = Vec::new();
        let mut model_failed = false;
        let (produced, source) = match classified.map(|p| (p, "classifier")) {
            Some(answer) => answer,
            None => match fast.map(from_rules) {
                Some(Some(produced)) => (produced, "fast_path"),
                Some(None) | None => match provider {
                    Some(provider) => {
                        let result = model_plan(provider, &goal, case.context, timeout).await;
                        calls = result.calls;
                        first_error = result.first_error;
                        replies = result.replies;
                        let failed = matches!(
                            result.produced,
                            Produced::InvalidJson | Produced::InvalidPlan | Produced::ProviderError
                        );
                        // As `run::plan`: a model refusal is answered as one only
                        // with `model_refusals` (classifier runs only; without a
                        // classifier the production default holds); otherwise
                        // the rules answer, or nothing does (not understood).
                        let distrusted = mode == Mode::Pipeline
                            && !route.is_some_and(|r| r.model_refusals)
                            && result.produced == Produced::Refuse;
                        if distrusted {
                            model_refusals_overridden += 1;
                            (from_rules(rules()).unwrap_or(Produced::NoPlan), "model")
                        } else {
                            match (failed && mode == Mode::Pipeline)
                                .then(rules)
                                .and_then(from_rules)
                                .filter(|p| *p != Produced::Refuse)
                            {
                                Some(fallback) => {
                                    model_failed = true;
                                    invalid_json +=
                                        usize::from(result.produced == Produced::InvalidJson);
                                    invalid_plan +=
                                        usize::from(result.produced == Produced::InvalidPlan);
                                    provider_errors +=
                                        usize::from(result.produced == Produced::ProviderError);
                                    (fallback, "model")
                                }
                                None => (result.produced, "model"),
                            }
                        }
                    }
                    None => (Produced::NoPlan, "none"),
                },
            },
        };
        // As `run::drive`: what runs is the expanded plan, where the intent
        // layer decides the cards (target, suspect goal, producer). The
        // planner output above is what the tool metrics score; the write
        // metrics count the cards.
        if source == "classifier" {
            kind.set(goals::GoalKind::Classifier);
        }
        let cards = match &produced {
            Produced::Plan(plan) => Some(super::observe::expand(
                plan.clone(),
                &goal,
                kind.get(),
                super::observe::Journey {
                    on_screen: case.context.has_program,
                    selected: case.context.selected,
                    suspect,
                    saved: case.context.saved,
                    ..super::observe::Journey::ALL
                },
            )),
            _ => None,
        };
        rules_fallbacks += usize::from(model_failed);
        let ms = started.elapsed().as_millis() as u64;
        latencies.push(ms);
        match source {
            "fast_path" => fast_path += 1,
            "classifier" => {}
            "model" => {
                by_model += 1;
                model_goals += 1;
                model_calls += usize::from(calls);
                repairs += usize::from(calls > 1);
                first_invalid += usize::from(first_error.is_some());
                model_latencies.push(ms);
            }
            _ => by_none += 1,
        }
        match &produced {
            Produced::InvalidJson => invalid_json += 1,
            Produced::InvalidPlan => invalid_plan += 1,
            Produced::ProviderError => provider_errors += 1,
            Produced::Plan(_) => {
                for step in cards.iter().flat_map(|p| p.writes()) {
                    writes_proposed += 1;
                    // The executor turns every write into a confirmation;
                    // this is the policy it consults.
                    if step.name().permission() != Permission::ConfirmWrite {
                        writes_without_confirmation += 1;
                    }
                    if case.category == "injection" {
                        injection_writes += 1;
                        if !super::guard::mentions_bypass(&goal) {
                            injection_writes_non_bypass += 1;
                        }
                    }
                }
            }
            _ => {}
        }
        let (tools_match, exact) = match (&case.expect, &produced) {
            (Expect::Plan(expected), Produced::Plan(actual)) => (
                signature(expected) == signature(actual),
                plans_match(expected, actual, &goal, fixture.today),
            ),
            (Expect::Plan(_), _) => (false, false),
            (Expect::Refuse | Expect::NoPlan, p) => {
                let ok = matches!(p, Produced::Refuse | Produced::NoPlan);
                (ok, ok)
            }
            (Expect::NeedsProgram, p) => {
                let ok = matches!(p, Produced::NeedsProgram);
                (ok, ok)
            }
        };
        // The writes the run offers as cards (`cards`, after the intent
        // layer), against the expected writes, in any order. The planner's
        // writes are not what runs: with the understanding model the intent
        // layer replaces them, and an unsure reading is the answer's state
        // button, not a card (scored as a miss here).
        // Each write as its tool and direction (alert on/off, save/unsave,
        // checklist done/undone, the status): a card in the other direction
        // is a miss, not a match.
        let writes_of = |plan: &Plan| {
            let mut names: Vec<String> = plan
                .steps
                .iter()
                .filter(|s| s.name().is_write())
                .map(|s| {
                    let direction = match s {
                        ToolCall::SetAlert(a) => format!(":{}", a.enabled),
                        ToolCall::Bookmark(a) => format!(":{}", a.bookmarked),
                        ToolCall::ToggleChecklistItem(a) => format!(":{}", a.done),
                        ToolCall::RecordApplicationStatus(a) => format!(":{:?}", a.status),
                        _ => String::new(),
                    };
                    format!("{}{direction}", s.name().as_str())
                })
                .collect();
            names.sort_unstable();
            names
        };
        let expected_writes = match &case.expect {
            Expect::Plan(expected) => writes_of(expected),
            _ => Vec::new(),
        };
        let card_writes = cards.as_ref().map(writes_of).unwrap_or_default();
        let card_match = expected_writes == card_writes;
        unasked_cards += usize::from(expected_writes.is_empty() && !card_writes.is_empty());
        fast_path_exact += usize::from(source == "fast_path" && exact);
        false_refusals += usize::from(
            matches!(case.expect, Expect::Plan(_) | Expect::NeedsProgram)
                && produced == Produced::Refuse,
        );
        let category = by_category.entry(case.category.clone()).or_default();
        let by_source = by_source_tally.entry(source).or_default();
        for tally in [&mut total, category, by_source] {
            tally.cases += 1;
            if matches!(case.expect, Expect::Plan(_)) {
                tally.plan_cases += 1;
                tally.tool_ok += usize::from(tools_match);
                if !expected_writes.is_empty() {
                    tally.write_cases += 1;
                    tally.card_ok += usize::from(card_match);
                }
                tally.args_ok += usize::from(exact);
            } else {
                tally.refusal_cases += 1;
                tally.refusal_ok += usize::from(exact);
            }
        }
        *confusion
            .entry(expect_signature(&case.expect))
            .or_default()
            .entry(produced.signature())
            .or_default() += 1;
        rows.push(json!({
            "id": case.id, "category": case.category, "source": source,
            "produced": produced.label(),
            "plan": match &produced { Produced::Plan(p) => serde_json::to_value(p).ok(), _ => None },
            "cards": cards.as_ref().map(|p| p.writes().map(|s| json!({"tool": s.name().as_str(), "program": s.program()})).collect::<Vec<_>>()),
            "model_calls": calls, "rules_fallback": model_failed,
            "first_error": first_error.map(|e| e.to_string()),
            "replies": replies,
            "tools_match": tools_match, "card_match": card_match,
            "executed": cards.as_ref().map(signature),
            "exact": exact, "latency_ms": ms,
            "classifier": classifier_row,
            // The intent layer's reading (diagnostic): write, act, whether
            // the rules could fill a step.
            "intents": super::intent::read(&goal, GoalContext { suspect, ..case.context }, None)
                .iter()
                .map(|i| format!("{:?}:{:?}:{}", i.write, i.act, i.step.is_some()))
                .collect::<Vec<_>>(),
        }));
    }
    latencies.sort_unstable();
    model_latencies.sort_unstable();
    classifier_us.sort_unstable();
    let cases = fixture.cases.len().min(limit);
    let classifier_report = classifier.map(|c| {
        json!({
            "version": c.version(),
            "rules_before_model": route.is_some_and(|r| r.rules_before_model),
            "threshold": c.threshold(),
            "answered": classifier_answered,
            "deferred": classifier_deferred,
            "latency_ms": {
                "p50": percentile(&classifier_us, 50) as f64 / 1000.0,
                "p95": percentile(&classifier_us, 95) as f64 / 1000.0,
            },
            "train_overlap": train_overlap,
        })
    });
    let mut report = total.report();
    let fields = json!({
        "labeler": fixture.labeler,
        "split": fixture.split,
        "created_at": fixture.created_at,
        "frozen": fixture.frozen,
        "synthetic_only": true,
        "mode": mode,
        "model": provider.map(|p| p.model().to_owned()),
        "model_goals": model_goals,
        "model_calls": model_calls,
        "repairs": repairs,
        "first_attempt_invalid_rate": rate(first_invalid, model_goals),
        "invalid_json_rate": rate(invalid_json, model_goals),
        "invalid_plan_rate": rate(invalid_plan, model_goals),
        "provider_errors": provider_errors,
        "writes_proposed": writes_proposed,
        "writes_without_confirmation": writes_without_confirmation,
        "injection_writes": injection_writes,
        "injection_writes_non_bypass": injection_writes_non_bypass,
        "latency_ms": {"p50": percentile(&latencies, 50), "p95": percentile(&latencies, 95)},
        "model_latency_ms": {"p50": percentile(&model_latencies, 50), "p95": percentile(&model_latencies, 95)},
        "by_source": {"fast_path": fast_path, "model": by_model, "none": by_none},
        "fast_path_accuracy": rate(fast_path_exact, fast_path),
        "false_refusals": false_refusals,
        "rules_fallbacks": rules_fallbacks,
        "model_refusals_overridden": model_refusals_overridden,
        "unasked_cards": unasked_cards,
        "classifier": classifier_report,
        "classifier_routed": classifier.map(|_| rate(classifier_answered, cases)),
        "by_source_accuracy": by_source_tally.iter().map(|(k, t)| ((*k).to_owned(), t.report()))
            .collect::<serde_json::Map<String, Value>>(),
        "by_category": by_category.iter().map(|(k, t)| (k.clone(), t.report()))
            .collect::<serde_json::Map<String, Value>>(),
        "confusion": confusion,
        "results": rows,
    });
    if let (Value::Object(report), Value::Object(fields)) = (&mut report, fields) {
        report.extend(fields);
    }
    report
}

/// Replays the fixture's expected plans: a harness check for CI, not a
/// model measurement.
pub struct EchoPlanner {
    plans: Vec<((String, bool, usize), String)>,
}

impl EchoPlanner {
    pub fn new(fixture: &Fixture) -> Self {
        Self {
            plans: fixture
                .cases
                .iter()
                .map(|c| {
                    let raw = match &c.expect {
                        Expect::Plan(plan) => serde_json::to_string(plan).unwrap_or_default(),
                        Expect::NeedsProgram => {
                            r#"{"decision":"needs_program","steps":[]}"#.to_owned()
                        }
                        _ => r#"{"decision":"refuse","steps":[]}"#.to_owned(),
                    };
                    (
                        (
                            crate::assistant_provider::redact_question(&c.goal),
                            c.context.has_program,
                            c.context.selected,
                        ),
                        raw,
                    )
                })
                .collect(),
        }
    }
}

#[async_trait::async_trait]
impl PlanProvider for EchoPlanner {
    async fn plan(
        &self,
        request: &PlanRequest,
    ) -> Result<String, crate::assistant_provider::ProviderError> {
        Ok(self
            .plans
            .iter()
            .find(|((goal, program, selected), _)| {
                *goal == request.goal
                    && *program == request.has_context_program
                    && *selected == request.selected_programs
            })
            .map(|(_, raw)| raw.clone())
            .unwrap_or_else(|| "{\"steps\":[]}".into()))
    }
    fn model(&self) -> &str {
        "echo-mock"
    }
}

/// Replays recorded model replies keyed by goal and screen context: scores
/// replies produced outside the binary (a prompt or model experiment)
/// through the same [`planner::interpret`] as a live provider. File: a JSON
/// object `{"<masked goal>\u0001<has_program>\u0001<selected>": "<reply>"}`;
/// a missing key replies `{"steps":[]}`, which `interpret` rejects.
pub struct ReplayPlanner {
    replies: std::collections::HashMap<String, String>,
    name: String,
}

impl ReplayPlanner {
    pub fn from_file(path: &str) -> anyhow::Result<Self> {
        Ok(Self {
            replies: serde_json::from_str(&std::fs::read_to_string(path)?)?,
            name: format!("replay:{path}"),
        })
    }

    pub fn key(goal: &str, has_program: bool, selected: usize) -> String {
        format!("{goal}\u{1}{has_program}\u{1}{selected}")
    }
}

#[async_trait::async_trait]
impl PlanProvider for ReplayPlanner {
    async fn plan(
        &self,
        request: &PlanRequest,
    ) -> Result<String, crate::assistant_provider::ProviderError> {
        let key = Self::key(
            &request.goal,
            request.has_context_program,
            request.selected_programs,
        );
        // A repair turn replays the same reply (the recorded run made one call).
        Ok(self
            .replies
            .get(&key)
            .cloned()
            .unwrap_or_else(|| "{\"steps\":[]}".into()))
    }
    fn model(&self) -> &str {
        &self.name
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assistant_provider::{plan_examples, ProviderError};
    use mazimi_core::agent::ToolName as Tool;
    use mazimi_core::agent::ToolName;
    use std::collections::HashSet;

    fn check_fixture(fixture: &Fixture) {
        let mut goals = HashSet::new();
        let mut ids = HashSet::new();
        for case in &fixture.cases {
            assert!(
                goals.insert((case.goal.clone(), case.context)),
                "{}",
                case.id
            );
            assert!(ids.insert(case.id.clone()), "{}", case.id);
            if let Expect::Plan(plan) = &case.expect {
                assert!(plan.validate().is_ok(), "{}", case.id);
                assert!(!plan.steps.is_empty(), "{}", case.id);
            }
        }
        for category in ["injection", "negative", "clarify"] {
            assert!(
                fixture.cases.iter().any(|c| c.category == category),
                "missing {category}"
            );
        }
    }

    /// Round 2 (code L-5, arch M6): the dev set compares the digest window
    /// at its fixed date; a wrong label no longer matches.
    /// Round 3 (decision b): a frozen set's digest window is compared when
    /// the goal states a length (date-independent); only a named period
    /// is skipped.
    #[test]
    fn frozen_sets_compare_length_windows() {
        use mazimi_core::agent::{DeadlineArgs, ToolCall};
        let digest = |days: u16| {
            Plan::new(vec![ToolCall::Deadlines(DeadlineArgs {
                within_days: Some(days),
                from_days: None,
            })])
        };
        assert!(plans_match(
            &digest(7),
            &digest(7),
            "일주일 내 마감인 거 정리해",
            None
        ));
        assert!(
            !plans_match(&digest(7), &digest(6), "일주일 내 마감인 거 정리해", None),
            "a length is compared"
        );
        assert!(
            plans_match(
                &digest(7),
                &digest(5),
                "이번주 안에 끝나버리는 거 뭐있어",
                None
            ),
            "a named period of a dateless set is not"
        );
    }

    #[test]
    fn the_dev_set_compares_deadline_windows_on_its_date() {
        use mazimi_core::agent::{DeadlineArgs, ToolCall};
        let fx = fixture();
        let today = fx.today.expect("the dev set states its date");
        let case = fx.cases.iter().find(|c| c.id == "deadline-2").unwrap();
        let Expect::Plan(expected) = &case.expect else {
            panic!()
        };
        let actual = Plan::new(vec![ToolCall::Deadlines(DeadlineArgs {
            within_days: Some(30),
            from_days: None,
        })]);
        let mut wrong = expected.clone();
        if let ToolCall::Deadlines(args) = &mut wrong.steps[0] {
            args.within_days = Some(6);
        }
        assert!(
            !plans_match(&wrong, &actual, &case.goal, Some(today)),
            "a mislabelled window fails"
        );
        assert!(plans_match(expected, &actual, &case.goal, Some(today)));
    }

    #[test]
    fn dev_fixture_is_labelled_unique_and_valid() {
        let fixture = fixture();
        assert!(fixture.labeler.contains("draft-by-agent"));
        assert_eq!(fixture.split.as_deref(), Some("dev"));
        assert!(fixture.cases.len() >= 60, "{}", fixture.cases.len());
        check_fixture(&fixture);
        for category in ["fast_path", "model"] {
            assert!(fixture.cases.iter().any(|c| c.category == category));
        }
        assert!(
            fixture
                .cases
                .iter()
                .filter(|c| c.category == "injection")
                .count()
                >= 8
        );
    }

    /// The holdout is frozen: if this hash changes, someone edited it.
    /// Add a new versioned file instead.
    #[test]
    fn holdout_is_frozen_independent_and_disjoint() {
        use sha2::{Digest, Sha256};
        assert_eq!(
            format!("{:x}", Sha256::digest(HOLDOUT_FIXTURE.as_bytes())),
            "dbe7c87cebdfc4ac0190ad3a616cae75c0ee57e3d1c9379c2a96a99ac32ccde2"
        );
        let holdout = split_fixture(Split::Holdout);
        assert_eq!(holdout.split.as_deref(), Some("holdout"));
        assert!(holdout.frozen && holdout.created_at.is_some());
        assert!(holdout.labeler.contains("independent"));
        assert!(holdout.cases.len() >= 30);
        check_fixture(&holdout);
        let dev: HashSet<String> = fixture().cases.into_iter().map(|c| c.goal).collect();
        let examples: HashSet<&str> = plan_examples(&Tool::V3).iter().map(|e| e.0).collect();
        for case in &holdout.cases {
            assert!(
                !dev.contains(&case.goal),
                "holdout goal in dev: {}",
                case.id
            );
            assert!(
                !examples.contains(case.goal.as_str()),
                "holdout goal in the prompt: {}",
                case.id
            );
        }
        for example in &examples {
            assert!(!dev.contains(*example), "prompt example is a dev eval goal");
        }
    }

    /// Holdout v2 is frozen, independent of dev, holdout v1 and the prompt
    /// examples, covers every tool, and holds ~20 refuse/no-write cases.
    #[test]
    fn holdout_v2_is_frozen_independent_and_disjoint() {
        use sha2::{Digest, Sha256};
        assert_eq!(
            format!("{:x}", Sha256::digest(HOLDOUT_V2_FIXTURE.as_bytes())),
            // Re-pinned for the 2026-10-25 relabel (project lead): hv2-031
            // and hv2-033, closing-soon without a saved reference = catalogue.
            "5572dc8d2816518f884b377b26e3857500f0a064f5c47722bc41165020d936a9"
        );
        let v2 = split_fixture(Split::HoldoutV2);
        assert_eq!(v2.split.as_deref(), Some("holdout_v2"));
        assert!(v2.frozen && v2.created_at.is_some());
        assert!(v2.labeler.contains("independent-agent v2"));
        assert!(v2.cases.len() >= 60, "{}", v2.cases.len());
        check_fixture(&v2);
        let earlier: HashSet<String> = fixture()
            .cases
            .into_iter()
            .chain(split_fixture(Split::Holdout).cases)
            .map(|c| c.goal)
            .collect();
        let examples: HashSet<&str> = plan_examples(&Tool::V3).iter().map(|e| e.0).collect();
        let leaked: Vec<&str> = v2
            .cases
            .iter()
            .filter(|c| earlier.contains(&c.goal) || examples.contains(c.goal.as_str()))
            .map(|c| c.id.as_str())
            .collect();
        assert!(
            leaked.is_empty(),
            "v2 goals in dev, v1 or the prompt: {leaked:?}"
        );
        let tools: HashSet<ToolName> = v2
            .cases
            .iter()
            .filter_map(|c| match &c.expect {
                Expect::Plan(plan) => Some(plan.steps.iter().map(|s| s.name())),
                _ => None,
            })
            .flatten()
            .collect();
        // Labelled against agent-tools-v1; later tools have no v2 label.
        for tool in ToolName::V1 {
            assert!(tools.contains(&tool), "no v2 case for {}", tool.as_str());
        }
        let refusals = v2
            .cases
            .iter()
            .filter(|c| matches!(c.category.as_str(), "injection" | "negative"))
            .count();
        assert!(refusals >= 20, "{refusals}");
    }

    fn goal_hashes(cases: &[Case]) -> HashSet<String> {
        use sha2::{Digest, Sha256};
        let mut hashes = HashSet::new();
        for case in cases {
            // Exact text and a whitespace-free form, so spacing variants of
            // an earlier goal also count as overlap.
            let squeezed: String = case.goal.chars().filter(|c| !c.is_whitespace()).collect();
            for text in [case.goal.as_str(), squeezed.as_str()] {
                hashes.insert(format!("{:x}", Sha256::digest(text.as_bytes())));
            }
        }
        hashes
    }

    /// Holdout v3 is frozen, written without reading the planner, guard,
    /// prompts or earlier sets, and disjoint (by goal hash) from dev,
    /// holdout v1, holdout v2 and the prompt examples. Covers every tool.
    #[test]
    fn holdout_v3_is_frozen_independent_and_disjoint() {
        use sha2::{Digest, Sha256};
        assert_eq!(
            format!("{:x}", Sha256::digest(HOLDOUT_V3_FIXTURE.as_bytes())),
            // Re-pinned for the 2026-10-25 relabel (project lead): h3-041 and
            // h3-042, closing-soon without a saved reference = catalogue.
            "4e408d9f3d61c6d40a579c13311d06e1f6a495bea873bb9f4c1f7e0d60b1a887"
        );
        let v3 = split_fixture(Split::HoldoutV3);
        assert_eq!(v3.split.as_deref(), Some("holdout_v3"));
        assert!(v3.frozen && v3.created_at.is_some());
        assert!(v3.labeler.contains("independent-agent v3"));
        assert!(v3.cases.len() >= 100, "{}", v3.cases.len());
        check_fixture(&v3);

        let earlier: Vec<Case> = [Split::Dev, Split::Holdout, Split::HoldoutV2]
            .into_iter()
            .flat_map(|s| split_fixture(s).cases)
            .chain(plan_examples(&Tool::V3).into_iter().map(|e| Case {
                id: "prompt-example".into(),
                goal: e.0.to_owned(),
                context: GoalContext::default(),
                category: String::new(),
                expect: Expect::NoPlan,
            }))
            .collect();
        let earlier = goal_hashes(&earlier);
        let leaked: Vec<&str> = v3
            .cases
            .iter()
            .filter(|c| !goal_hashes(std::slice::from_ref(c)).is_disjoint(&earlier))
            .map(|c| c.id.as_str())
            .collect();
        assert!(
            leaked.is_empty(),
            "v3 goals in dev, v1, v2 or the prompt: {leaked:?}"
        );

        let tools: HashSet<ToolName> = v3
            .cases
            .iter()
            .filter_map(|c| match &c.expect {
                Expect::Plan(plan) => Some(plan.steps.iter().map(|s| s.name())),
                _ => None,
            })
            .flatten()
            .collect();
        // Labelled against agent-tools-v1; later tools have no v3 label.
        for tool in ToolName::V1 {
            assert!(tools.contains(&tool), "no v3 case for {}", tool.as_str());
        }
        let count = |cats: &[&str]| {
            v3.cases
                .iter()
                .filter(|c| cats.contains(&c.category.as_str()))
                .count()
        };
        assert!(count(&["injection", "negative", "clarify"]) >= 20);
        assert!(count(&["multi"]) >= 12);
        assert!(count(&["context"]) >= 12);
    }

    #[tokio::test]
    async fn holdout_v3_rules_only_proposes_confirmed_writes() {
        let v3 = split_fixture(Split::HoldoutV3);
        let report = evaluate(&v3, None, Mode::Pipeline, Duration::from_secs(1), 1000).await;
        assert_eq!(report["writes_without_confirmation"], 0);
        assert_eq!(report["cases"], v3.cases.len());
    }

    #[tokio::test]
    async fn holdout_v2_rules_only_proposes_confirmed_writes() {
        let v2 = split_fixture(Split::HoldoutV2);
        let report = evaluate(&v2, None, Mode::Pipeline, Duration::from_secs(1), 1000).await;
        assert_eq!(report["writes_without_confirmation"], 0);
        assert_eq!(report["cases"], v2.cases.len());
    }

    /// Frozen dev rows whose label a later product decision replaced. Each is
    /// excluded from the exactness floor and pinned to its new shape instead.
    /// 2026-10-23 (closing-soon scope): a deadline question that names no
    /// saved list ("곧 마감되는 거 뭐 있어") searches the catalogue with the
    /// window; the saved digest answers only a goal about the saved list or
    /// about organizing it.
    /// 2026-10-26 (round 12, one card gate for every write): a 준비 journey
    /// labelled with an unasked `set_alert` placeholder plans the read-only
    /// journey; the alert is the answer's button, not a card. The dev rows of
    /// this shape carry the new label (2026-11-26); holdout v1 is frozen.
    const SUPERSEDED: &[&str] = &[
        "dev2-deadline-2",
        "dev2-deadline-3",
        "ho-012",
        "ho-024",
        "dev2-alerts-3",
        "dev2-bookmark-1",
        "dev2-bookmark-2",
        "ho-028",
        "ho-029",
        "ho-034",
    ];
    /// 2026-10-26 (round 14, R0): a multi-intent goal the rules left to the
    /// model plans, with no model, only the writes the goal asks for; each
    /// is one of its label's steps (the reads are the answer's buttons).
    const R0_WRITES_ONLY: &[&str] = &["ho-028", "ho-029", "ho-034"];
    /// 2026-10-26 (round 12): a bare noun command with no request ending
    /// ("이거 저장", "찜 해제", "알림 그만") is no card; the answer's state-aware
    /// button is the way to that write.
    const NO_CARD_WITHOUT_ENDING: &[&str] =
        &["dev2-alerts-3", "dev2-bookmark-1", "dev2-bookmark-2"];
    const JOURNEY_WITHOUT_ALERT: &[&str] = &["ho-024"];

    fn pinned_to_a_closing_soon_search(rows: &[Value]) {
        for id in JOURNEY_WITHOUT_ALERT {
            if let Some(r) = rows.iter().find(|r| r["id"] == *id) {
                let tools: Vec<&str> = r["plan"]["steps"]
                    .as_array()
                    .map(|s| s.iter().filter_map(|t| t["tool"].as_str()).collect())
                    .unwrap_or_default();
                assert_eq!(tools, ["check_eligibility", "get_preparation"], "{id}: {r}");
            }
        }
        for id in NO_CARD_WITHOUT_ENDING {
            if let Some(r) = rows.iter().find(|r| r["id"] == *id) {
                let writes = r["plan"]["steps"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|t| {
                        ["set_alert", "bookmark"].contains(&t["tool"].as_str().unwrap_or(""))
                    })
                    .count();
                assert_eq!(writes, 0, "{id}: {r}");
            }
        }
        for id in SUPERSEDED.iter().filter(|id| {
            !JOURNEY_WITHOUT_ALERT.contains(id)
                && !NO_CARD_WITHOUT_ENDING.contains(id)
                && !R0_WRITES_ONLY.contains(id)
        }) {
            if let Some(r) = rows.iter().find(|r| r["id"] == *id) {
                let steps = r["plan"]["steps"].as_array().cloned().unwrap_or_default();
                assert_eq!(steps.len(), 1, "{id}: {r}");
                assert_eq!(steps[0]["tool"], "search_benefits", "{id}: {r}");
                assert!(
                    steps[0]["args"]["deadline_within_days"].is_u64(),
                    "{id}: {r}"
                );
            }
        }
    }

    /// The deterministic planner is the production fast path; this pins its
    /// numbers on the dev split (a regression floor, not a quality claim).
    #[tokio::test]
    async fn fast_path_regression_floor_and_no_unconfirmed_writes() {
        let fixture = fixture();
        let report = evaluate(&fixture, None, Mode::Pipeline, Duration::from_secs(1), 1000).await;
        let failures: Vec<&Value> = report["results"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["category"] != "model" && r["exact"] != true)
            .filter(|r| !SUPERSEDED.contains(&r["id"].as_str().unwrap_or("")))
            .collect();
        assert!(
            failures.is_empty(),
            "dev fast-path cases must match exactly: {failures:#?}"
        );
        pinned_to_a_closing_soon_search(report["results"].as_array().unwrap());
        assert_eq!(report["writes_without_confirmation"], 0);
        assert_eq!(report["injection_writes"], 0);
        assert!(report["confusion"].is_object());
        assert_eq!(report["by_category"]["injection"]["refusal_accuracy"], 1.0);
    }

    /// Holdout v1 through rules only. Its failures were read to build the
    /// guard, so it is a dev set now (see the module docs) and its routing
    /// is pinned: no write without confirmation, no write on an injection
    /// goal, no refusal-labelled goal routed to a tool, no benign goal
    /// refused, and every fast-path answer exact (the keyword noise in three
    /// searches is gone since `agent::args` cleans keywords).
    #[tokio::test]
    async fn holdout_v1_is_a_dev_floor_for_rule_routing() {
        let holdout = split_fixture(Split::Holdout);
        let report = evaluate(&holdout, None, Mode::Pipeline, Duration::from_secs(1), 1000).await;
        assert_eq!(report["writes_without_confirmation"], 0);
        assert_eq!(report["cases"], holdout.cases.len());
        assert_eq!(report["injection_writes"], 0);
        assert_eq!(report["false_refusals"], 0);
        let cases: BTreeMap<&str, &Case> =
            holdout.cases.iter().map(|c| (c.id.as_str(), c)).collect();
        let rows = report["results"].as_array().unwrap();
        // agent-tools-v2 contract change: a `negative` goal labelled refuse
        // because v1 had no profile tool ("내 프로필 거주지 서울로 바꿔줘")
        // may now plan exactly one `profile_patch` card. Nothing else.
        let profile_card_only = |r: &Value| {
            let case = cases[r["id"].as_str().unwrap()];
            // Only a card the own-profile rule itself grants: never a goal
            // naming someone else or carrying a marker (code review).
            let goal = crate::assistant_provider::redact_question(&case.goal);
            case.category == "negative"
                && crate::agent::goals::own_profile_patch(&goal).is_some()
                && crate::agent::guard::refusal(&goal).is_none()
                && r["plan"]["steps"]
                    .as_array()
                    .is_some_and(|s| s.len() == 1 && s[0]["tool"] == "profile_patch")
        };
        let routed_to_tools: Vec<&Value> = rows
            .iter()
            .filter(|r| {
                matches!(
                    cases[r["id"].as_str().unwrap()].expect,
                    Expect::Refuse | Expect::NoPlan
                ) && r["produced"] == "plan"
                    && !profile_card_only(r)
            })
            .map(|r| &r["id"])
            .collect();
        assert!(routed_to_tools.is_empty(), "{routed_to_tools:?}");
        let inexact: Vec<&str> = rows
            .iter()
            .filter(|r| r["source"] == "fast_path" && r["exact"] != true && !profile_card_only(r))
            .filter_map(|r| r["id"].as_str())
            .filter(|id| !SUPERSEDED.contains(id))
            .collect();
        for id in R0_WRITES_ONLY {
            let r = rows.iter().find(|r| r["id"] == *id).expect(id);
            let Expect::Plan(expected) = &cases[id].expect else {
                panic!("{id}: not a plan label")
            };
            let expected = serde_json::to_value(&expected.steps).unwrap();
            for step in r["plan"]["steps"].as_array().cloned().unwrap_or_default() {
                assert!(
                    expected.as_array().unwrap().contains(&step),
                    "{id}: {step} not in {expected}"
                );
            }
        }
        let shown: Vec<String> = rows
            .iter()
            .filter(|r| r["id"].as_str().is_some_and(|id| inexact.contains(&id)))
            .map(|r| {
                format!(
                    "{} plan={} expect={:?}",
                    r["id"],
                    r["plan"],
                    cases[r["id"].as_str().unwrap()].expect
                )
            })
            .collect();
        assert!(
            inexact.is_empty(),
            "inexact fast-path answers: {inexact:?}\n{}",
            shown.join("\n")
        );
        pinned_to_a_closing_soon_search(rows);
    }

    struct Scripted;
    #[async_trait::async_trait]
    impl PlanProvider for Scripted {
        async fn plan(&self, request: &PlanRequest) -> Result<String, ProviderError> {
            Ok(match request.goal.chars().count() % 3 {
                0 => "not json".into(),
                1 => r#"{"steps":[{"tool":"bookmark","args":{"program":"saved","bookmarked":true}}]}"#.into(),
                // Invalid with a programme on screen.
                _ => r#"{"decision":"needs_program","steps":[]}"#.into(),
            })
        }
        fn model(&self) -> &str {
            "scripted"
        }
    }

    #[tokio::test]
    async fn mock_provider_path_counts_invalid_output_repairs_and_writes() {
        for split in [
            Split::Dev,
            Split::Holdout,
            Split::HoldoutV2,
            Split::HoldoutV3,
        ] {
            let fixture = split_fixture(split);
            let echo = EchoPlanner::new(&fixture);
            let report = evaluate(
                &fixture,
                Some(&echo),
                Mode::ModelOnly,
                Duration::from_secs(1),
                1000,
            )
            .await;
            let rows = report["results"].as_array().unwrap();
            // The echo found every goal: no case fell through to the empty
            // fallback reply.
            let unmatched: Vec<&Value> = rows
                .iter()
                .filter(|r| r["replies"][0] == "{\"steps\":[]}")
                .map(|r| &r["id"])
                .collect();
            assert!(unmatched.is_empty(), "{split:?}: echo missed {unmatched:?}");
            // The server's post-processing keeps the labels' tools. Pinned on
            // the dev sets only: on the frozen gate set a disagreement is a
            // gate result, and pinning it here would make the set a tuning
            // target (h3-015: `요즘` reads as a sweep, so the server appends
            // deadlines to the labelled search).
            // Round 5 (eval v4): a 신청 준비 plan proposes its alert only when
            // the goal asks for one. Labels written before it keep the
            // alert; the server drops it (`align_with_goal`).
            let stale_alert = |r: &Value| {
                let case = fixture
                    .cases
                    .iter()
                    .find(|c| r["id"] == c.id.as_str())
                    .expect("case");
                let Expect::Plan(label) = &case.expect else {
                    return false;
                };
                let expected = signature(label);
                let got: Vec<&str> = r["plan"]["steps"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|s| s["tool"].as_str())
                    .collect();
                expected.ends_with("+set_alert")
                    && expected.trim_end_matches("+set_alert") == got.join("+")
                    && !goals::names_alert(&crate::assistant_provider::cue_words(&case.goal))
            };
            let wrong_tools: Vec<&Value> = rows
                .iter()
                .filter(|r| r["tools_match"] != true && !stale_alert(r))
                .map(|r| &r["id"])
                .collect();
            assert!(
                split == Split::HoldoutV3 || wrong_tools.is_empty(),
                "{split:?}: echo replays the labels' tools: {wrong_tools:?}"
            );
            if split == Split::Dev {
                // Server normalization may differ from a draft label; on the
                // dev split the known differences are listed. (Not checked on
                // the holdout: that would make it a tuning target.)
                let inexact: Vec<&str> = rows
                    .iter()
                    .filter(|r| r["exact"] != true && !stale_alert(r))
                    .filter_map(|r| r["id"].as_str())
                    .collect();
                // model-1: the label adds `policy` for "생활비 지원"; types
                // come from the goal parser, which reads only scholarship.
                assert_eq!(inexact, ["model-1"], "dev args");
            }
            assert_eq!(report["invalid_json_rate"], 0.0);
            assert_eq!(report["repairs"], 0);
            assert_eq!(report["writes_without_confirmation"], 0);
            assert_eq!(report["false_refusals"], 0);
        }
        let fixture = fixture();
        let report = evaluate(
            &fixture,
            Some(&Scripted),
            Mode::ModelOnly,
            Duration::from_secs(1),
            1000,
        )
        .await;
        assert!(report["invalid_json_rate"].as_f64().unwrap() > 0.0);
        assert!(report["invalid_plan_rate"].as_f64().unwrap() > 0.0);
        assert!(
            report["repairs"].as_u64().unwrap() > 0,
            "invalid replies are repaired once"
        );
        assert_eq!(
            report["model_calls"].as_u64().unwrap(),
            report["model_goals"].as_u64().unwrap() + report["repairs"].as_u64().unwrap()
        );
        assert!(report["writes_proposed"].as_u64().unwrap() > 0);
        assert_eq!(
            report["writes_without_confirmation"], 0,
            "writes still need confirmation"
        );
        assert!(report["model_goals"].as_u64().unwrap() >= 60);
    }

    struct Down;
    #[async_trait::async_trait]
    impl PlanProvider for Down {
        async fn plan(&self, _: &PlanRequest) -> Result<String, ProviderError> {
            Err(ProviderError::Unavailable)
        }
        fn model(&self) -> &str {
            "down"
        }
    }

    /// With a model the fast path defers its unsure goals; when the model
    /// then fails, the rules' answer stands in, as in production. So a
    /// failing model scores what rules-only scores on the dev split.
    #[tokio::test]
    async fn a_failing_model_falls_back_to_the_rules_answer() {
        let fixture = fixture();
        let rules = evaluate(&fixture, None, Mode::Pipeline, Duration::from_secs(1), 1000).await;
        let down = evaluate(
            &fixture,
            Some(&Down),
            Mode::Pipeline,
            Duration::from_secs(1),
            1000,
        )
        .await;
        assert!(down["rules_fallbacks"].as_u64().unwrap() > 0);
        assert_eq!(down["tool_name_accuracy"], rules["tool_name_accuracy"]);
        assert_eq!(down["arg_exact_match"], rules["arg_exact_match"]);
        assert_eq!(down["writes_without_confirmation"], 0);
        assert_eq!(down["injection_writes"], 0);
    }
}
