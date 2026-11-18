//! The agent loop around a plan: what the executor adds after the planner
//! and after it observes a search. Deterministic; no model.
//!
//! - [`expand`]: a lone search takes the goal's closing-soon window, and a
//!   lone search for "what can I get" also checks the
//!   eligibility of what it finds (`check_eligibility` on `found`), so the
//!   answer can say per programme which conditions are met, unmet or still
//!   unknown. Planner output is not changed for the planner evaluation;
//!   the run stores and shows the expanded plan.
//! - [`revise`]: a search with fewer than [`MIN_ON_TOPIC`] on-topic rows is
//!   run once more with one condition relaxed (the deadline window or the
//!   earliest-row superlative, then the programme type), at most
//!   [`MAX_REVISIONS`] times per run. Each re-search is its own tool call in
//!   the run store, tagged with what it relaxed.
//! - `found` steps act on what the answer names
//!   ([`super::answer::named_ids_of`]).
//!
//! Bounds: [`MAX_TOOL_CALLS`] tool calls per run (each expanded programme
//! counts), inside the runtime's run timeout and per-tool timeout.
use super::goals::{self, GoalKind};
use mazimi_core::agent::{Plan, ProgramArgs, ProgramRef, SearchArgs, ToolCall};
use serde_json::Value;

/// Tool calls one run may make; later steps are recorded as skipped.
pub const MAX_TOOL_CALLS: usize = 12;
/// Relaxed re-searches one run may make.
pub const MAX_REVISIONS: usize = 2;
/// Fewer on-topic rows than this and the search is relaxed once more.
pub const MIN_ON_TOPIC: usize = 2;

/// What the run may add for the 신청 준비 journey.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Journey {
    /// `ASSISTANT_AGENT_JOURNEY_ENABLED` (default on): the vault match and
    /// the draft card.
    pub enabled: bool,
    /// The client sent `X-Mazimi-Client-Caps: journey-v1`: it opens the
    /// official-page handoff. An older client gets no handoff card it cannot
    /// act on (code review M4).
    pub handoffs: bool,
    /// `check_official_page` is on: a client without the journey capability
    /// gets the read-only page check in place of the official-page handoff
    /// (re-review 10 L1); otherwise that step is dropped.
    pub page_check: bool,
    /// A programme is on screen: the writes the goal asks for about it are
    /// produced ([`super::cards::requested`], R0).
    pub on_screen: bool,
    /// Programmes the user selected: a write on the selection is a target
    /// only with one (code review H1).
    pub selected: usize,
    /// The classifier leans to refuse the goal (security review F1): no
    /// write is kept or produced ([`goals::GoalContext::suspect`]).
    pub suspect: bool,
    /// How many programmes the user has saved: the saved list is a write
    /// target only with some (integrated review H1).
    pub saved: usize,
}

impl Journey {
    pub const ALL: Self = Self {
        enabled: true,
        handoffs: true,
        page_check: true,
        on_screen: true,
        selected: 0,
        suspect: false,
        saved: 0,
    };

    /// The run's goal context, built once in `run::plan` and carried here:
    /// every reader of the journey (the producer, the gate, the empty-plan
    /// check) applies the policy to the same screen, selection and saved
    /// list as routing did.
    pub fn context(&self) -> goals::GoalContext {
        goals::GoalContext {
            has_program: self.on_screen,
            selected: self.selected,
            saved: self.saved,
            suspect: self.suspect,
            handoffs: self.handoffs,
        }
    }
}

/// The plan the run executes (module docs). `kind` is the route's reading of
/// the goal: the journey extends only the rules' 신청 준비 plan, never a model
/// or classifier plan that happens to have its shape (code review L2).
pub fn expand(plan: Plan, goal: &str, kind: GoalKind, journey: Journey) -> Plan {
    let original = plan.clone();
    let mut expanded = expand_unchecked(plan, goal, kind, journey);
    // R0: the writes the goal asks for are produced on every path (rules,
    // model, classifier), after the planner's reads; a route's own write of
    // that type keeps its arguments.
    super::cards::merge_requested(&mut expanded, goal, journey.context());
    // Security review L5: what runs is a valid plan. The card gate holds on
    // the fallback too: no path runs a card the goal did not ask for.
    match expanded.validate() {
        Ok(()) => expanded,
        Err(error) => {
            tracing::warn!(%error, "expanded agent plan is invalid; running the planned one");
            let mut original = original;
            gate_cards(&mut original, goal, journey);
            original
        }
    }
}

/// The card gate on every plan, whoever planned it (rules, model,
/// classifier): round 11 lead decision, re-review 10 M1/M3.
/// - `open_form_fill` / `open_official_application` stay only when the whole
///   goal is that request ([`goals::strict_handoff`]; a form field value
///   keeps the form helper); `request_signature` is never planned.
/// - `set_alert` stays only when the goal asks for that state
///   ([`goals::alert_request`]).
/// - A client without the journey capability gets the read-only page check
///   in place of the page handoff (or nothing when that tool is off).
///
/// A plan left empty becomes the read-only journey for the programme on
/// screen (its answer offers the handoffs as buttons), or the saved digest.
fn gate_cards(plan: &mut Plan, goal: &str, journey: Journey) {
    // Every write tool, by one strict form per type (round 12): alert on/off,
    // bookmark/unsave, status, checklist, calendar, profile, form field
    // value and both handoffs ([`super::cards`]).
    let mut context = super::cards::gate_plan(plan, goal, journey.context());
    if !journey.handoffs {
        let page_check = journey.page_check;
        plan.steps.retain_mut(|step| match step {
            ToolCall::OpenOfficialApplication(args) if page_check => {
                *step = ToolCall::CheckOfficialPage(args.clone());
                true
            }
            ToolCall::OpenOfficialApplication(args) => {
                context |= args.program == ProgramRef::Context;
                false
            }
            _ => true,
        });
        plan.steps.dedup();
    }
    // A plan left empty is the read-only journey or the digest, unless the
    // goal asks for writes: then the produced writes are the plan (R3).
    let produced = !super::cards::requested(goal, journey.context()).is_empty();
    if plan.steps.is_empty() && !produced {
        let args = ProgramArgs {
            program: ProgramRef::Context,
        };
        plan.steps = if context {
            let mut steps = vec![
                ToolCall::CheckEligibility(args.clone()),
                ToolCall::GetPreparation(args.clone()),
            ];
            if journey.enabled {
                steps.push(ToolCall::MatchVaultDocuments(args));
            }
            steps
        } else {
            vec![ToolCall::Deadlines(Default::default())]
        };
    }
}

fn expand_unchecked(mut plan: Plan, goal: &str, kind: GoalKind, journey: Journey) -> Plan {
    // A lone search whose goal asks for closing-soon rows ("다음 달 마감인
    // 주거 지원 찾아줘") lists only those, deadline first. A plan with its own
    // deadlines step keeps the search whole ("찾아주고 2주 안에 마감인 것도").
    if let [ToolCall::SearchBenefits(search)] = plan.steps.as_mut_slice() {
        if search.deadline_within_days.is_none() {
            super::args::set_search_period(search, super::args::search_reading(goal));
        }
    }
    // Two topics in one goal are two searches (QA B6), whoever planned it.
    plan.steps = std::mem::take(&mut plan.steps)
        .into_iter()
        .flat_map(|step| match step {
            ToolCall::SearchBenefits(args) => super::args::split_topics(&args, goal)
                .into_iter()
                .map(ToolCall::SearchBenefits)
                .collect(),
            other => vec![other],
        })
        .collect();
    // The 신청 준비 journey: check, checklist, then the vault match, the
    // draft card (when a verified template exists), the signature handoff
    // and the official page handoff, before the alert. Every handoff is a
    // confirmation card; nothing is submitted.
    let prepare = kind == GoalKind::PrepareProgram && journey.enabled;
    // The rules' 신청 준비 plan, with its alert card or without it (a goal that
    // declined the alert).
    let shape = match plan.steps.as_slice() {
        [ToolCall::CheckEligibility(a), ToolCall::GetPreparation(b), ToolCall::SetAlert(c)] => {
            (a.program == b.program && b.program == c.program).then_some(a.program)
        }
        [ToolCall::CheckEligibility(a), ToolCall::GetPreparation(b)] => {
            (a.program == b.program).then_some(a.program)
        }
        _ => None,
    };
    if let Some(program) = shape.filter(|p| prepare && *p == ProgramRef::Context) {
        // No alert card the goal did not ask for (round 4 decision): the
        // answer offers "마감 알림 켜기" as a button instead, and tapping it
        // asks for the card. Asked means a request to turn it on (or off,
        // matching the step), not a question or a report ("알림 언제 와?",
        // "알림은 이미 켰어") (re-review 5 L1).
        let asked = match plan.steps.last() {
            Some(step @ ToolCall::SetAlert(_)) => super::intent::asks(goal, step),
            _ => false,
        };
        if plan.steps.len() == 3 && !asked {
            plan.steps.pop();
        }
        // The journey itself is read-only: check, checklist, vault match. Its
        // handoffs are always buttons ("신청서 작성 도우미", "공식 신청 페이지"),
        // whose tap is that request (round 10/11 lead decisions): a goal that
        // names 준비 is never wholly a handoff request, so the journey
        // proposes no handoff card.
        let args = ProgramArgs { program };
        let alert = (plan.steps.len() == 3).then(|| plan.steps.pop()).flatten();
        plan.steps.push(ToolCall::MatchVaultDocuments(args));
        // An alert the goal asked for comes last; one already on is no card
        // (`confirm::propose` sees no change).
        plan.steps.extend(alert);
    }
    // A search then a checklist of what it found also matches the vault.
    if let [ToolCall::SearchBenefits(_), ToolCall::GetPreparation(p)] = plan.steps.as_slice() {
        if p.program == ProgramRef::Found && journey.enabled {
            plan.steps.push(ToolCall::MatchVaultDocuments(ProgramArgs {
                program: ProgramRef::Found,
            }));
        }
    }
    gate_cards(&mut plan, goal, journey);
    if let [ToolCall::SearchBenefits(_)] = plan.steps.as_slice() {
        if goals::wants_eligibility(goal) {
            plan.steps.push(ToolCall::CheckEligibility(ProgramArgs {
                program: ProgramRef::Found,
            }));
        }
    }
    plan
}

fn on_topic(data: &Value) -> usize {
    match data["topic"]["on_topic"].as_u64() {
        Some(n) => n as usize,
        None => data["results"].as_array().map_or(0, Vec::len),
    }
}

/// The relaxed re-search for a thin result, and what it relaxed.
pub fn revise(args: &SearchArgs, data: &Value) -> Option<(SearchArgs, &'static str)> {
    let blocked = data["required_profile_fields"]
        .as_array()
        .is_some_and(|f| !f.is_empty());
    if blocked || on_topic(data) >= MIN_ON_TOPIC {
        return None;
    }
    let mut next = args.clone();
    let found = !data["results"].as_array().is_some_and(|r| r.is_empty());
    // The period the goal states is kept while a row closes in it (audit
    // M5): only an empty period is relaxed, as the earliest row below.
    if next.deadline_within_days.is_some() && !found {
        super::args::set_search_period(&mut next, None);
        return Some((next, "deadline"));
    }
    // The first to close among rows that have no deadline at all: the
    // search's order without it (its answer names no "earliest" row).
    if next.earliest {
        if found {
            return None;
        }
        next.earliest = false;
        return Some((next, "deadline"));
    }
    // No keyword relaxation (QA B3): keywords never filter a structured
    // search, and a search on words alone that matches none of them says
    // so instead of listing unrelated rows.
    if !next.program_types.is_empty() && !next.fields.is_empty() {
        next.program_types.clear();
        return Some((next, "program_type"));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn search(fields: &[&str], keywords: &[&str]) -> SearchArgs {
        SearchArgs {
            fields: fields.iter().map(|f| (*f).to_owned()).collect(),
            keywords: keywords.iter().map(|k| (*k).to_owned()).collect(),
            ..SearchArgs::default()
        }
    }

    #[test]
    fn a_can_get_search_also_checks_what_it_finds() {
        let plan = Plan::new(vec![ToolCall::SearchBenefits(search(&["housing"], &[]))]);
        let expanded = expand(
            plan.clone(),
            "부산 사는 대학생인데 월세 지원 받을 수 있는 거 있을까?",
            GoalKind::Search,
            Journey::ALL,
        );
        assert_eq!(expanded.steps.len(), 2);
        assert_eq!(
            expanded.steps[1],
            ToolCall::CheckEligibility(ProgramArgs {
                program: ProgramRef::Found
            })
        );
        assert!(expanded.validate().is_ok());
        assert_eq!(
            expand(
                plan.clone(),
                "월세 지원 찾아줘",
                GoalKind::Search,
                Journey::ALL
            )
            .steps
            .len(),
            1
        );
        let ToolCall::SearchBenefits(args) = &expand(
            plan.clone(),
            "다음 달 마감인 주거 지원 찾아줘",
            GoalKind::Search,
            Journey::ALL,
        )
        .steps[0] else {
            panic!()
        };
        let next_month = super::super::args::deadline_window("다음 달 마감");
        assert!(next_month.is_some_and(|(from, _)| from > 0));
        assert_eq!(
            (args.deadline_from_days, args.deadline_within_days),
            (next_month.map(|p| p.0), next_month.map(|p| p.1))
        );
    }

    fn prepare_plan() -> Plan {
        let context = ProgramArgs {
            program: ProgramRef::Context,
        };
        Plan::new(vec![
            ToolCall::CheckEligibility(context.clone()),
            ToolCall::GetPreparation(context),
            ToolCall::SetAlert(mazimi_core::agent::AlertArgs {
                program: ProgramRef::Context,
                enabled: true,
            }),
        ])
    }

    fn names(plan: &Plan) -> Vec<&'static str> {
        plan.steps.iter().map(|s| s.name().as_str()).collect()
    }

    /// Re-review 8 H1/H2: a goal that mentions 준비 without asking for it never
    /// gets a form or official-page handoff, on the rules' plan, the rules in
    /// front of the model, or a model's 3-step plan; a real 준비 request keeps
    /// them (a question gets the vault match only).
    /// Round 11: no plan carries a card the goal did not ask for, on any
    /// path (the rules, the rules in front of the model, and the model's or
    /// classifier's plan shapes, each through `expand`). Hand labels in
    /// `agent_card_requests.json`, written before any run; a miss is allowed.
    #[test]
    fn no_path_carries_a_card_the_goal_did_not_ask_for() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/agent_card_requests.json"
        ))
        .unwrap();
        let rows = fixture["rows"].as_array().unwrap();
        assert!(rows.len() >= 880);
        let on_screen = goals::GoalContext {
            has_program: true,
            selected: 0,
            saved: 0,
            suspect: false,
            handoffs: false,
        };
        let a = ProgramArgs {
            program: ProgramRef::Context,
        };
        let alert = |on: bool| {
            ToolCall::SetAlert(mazimi_core::agent::AlertArgs {
                program: ProgramRef::Context,
                enabled: on,
            })
        };
        let shapes: Vec<(GoalKind, Vec<ToolCall>)> = vec![
            (GoalKind::Model, prepare_plan().steps),
            (GoalKind::PrepareProgram, prepare_plan().steps),
            (
                GoalKind::Model,
                vec![
                    ToolCall::CheckEligibility(a.clone()),
                    ToolCall::GetPreparation(a.clone()),
                    ToolCall::OpenFormFill(a.clone()),
                    ToolCall::OpenOfficialApplication(a.clone()),
                ],
            ),
            (
                GoalKind::Model,
                vec![
                    ToolCall::CheckEligibility(a.clone()),
                    ToolCall::OpenFormFill(a.clone()),
                ],
            ),
            (GoalKind::Model, vec![ToolCall::OpenFormFill(a.clone())]),
            (
                GoalKind::Classifier,
                vec![ToolCall::OpenFormFill(a.clone())],
            ),
            (GoalKind::Model, vec![alert(true)]),
            (GoalKind::Classifier, vec![alert(false)]),
            (GoalKind::Model, vec![alert(false)]),
            (GoalKind::Classifier, vec![alert(true)]),
        ];
        // Round 12: every write type, on the model's and the classifier's
        // plan shapes, alone and after a read.
        use mazimi_core::agent::{
            ApplicationStatus, BookmarkArgs, CalendarArgs, ChecklistArgs, ProfileField,
            ProfilePatchArgs, StatusArgs,
        };
        let mut writes: Vec<ToolCall> = vec![
            ToolCall::Bookmark(BookmarkArgs {
                program: ProgramRef::Context,
                bookmarked: true,
            }),
            ToolCall::Bookmark(BookmarkArgs {
                program: ProgramRef::Context,
                bookmarked: false,
            }),
            ToolCall::ToggleChecklistItem(ChecklistArgs {
                program: ProgramRef::Context,
                task: "등본".into(),
                done: true,
            }),
            ToolCall::ToggleChecklistItem(ChecklistArgs {
                program: ProgramRef::Context,
                task: "등본".into(),
                done: false,
            }),
            ToolCall::AddToCalendar(CalendarArgs {
                program: ProgramRef::Context,
                date: Default::default(),
                days_before: 0,
            }),
            ToolCall::ProfilePatch(ProfilePatchArgs {
                field: ProfileField::RegionCode,
                value: "26".into(),
            }),
            ToolCall::OpenOfficialApplication(a.clone()),
        ];
        for status in [
            ApplicationStatus::Applied,
            ApplicationStatus::Waiting,
            ApplicationStatus::Received,
            ApplicationStatus::Abandoned,
            ApplicationStatus::Planning,
        ] {
            writes.push(ToolCall::RecordApplicationStatus(StatusArgs {
                program: ProgramRef::Context,
                status,
            }));
        }
        let mut shapes = shapes;
        for write in writes {
            for kind in [GoalKind::Model, GoalKind::Classifier] {
                shapes.push((kind, vec![write.clone()]));
                shapes.push((
                    kind,
                    vec![ToolCall::CheckEligibility(a.clone()), write.clone()],
                ));
            }
        }
        let cards = |plan: &Plan| -> Vec<char> {
            plan.steps
                .iter()
                .filter_map(|s| match s {
                    ToolCall::SetAlert(x) if x.enabled => Some('a'),
                    ToolCall::SetAlert(_) => Some('o'),
                    ToolCall::OpenFormFill(_) => Some('f'),
                    ToolCall::OpenOfficialApplication(_) => Some('p'),
                    ToolCall::Bookmark(x) if x.bookmarked => Some('b'),
                    ToolCall::Bookmark(_) => Some('u'),
                    ToolCall::RecordApplicationStatus(_) => Some('s'),
                    ToolCall::ToggleChecklistItem(_) => Some('c'),
                    ToolCall::AddToCalendar(_) => Some('k'),
                    ToolCall::ProfilePatch(_) => Some('r'),
                    s if s.name().is_write() => panic!("write without a label letter: {s:?}"),
                    _ => None,
                })
                .collect()
        };
        let mut unasked: Vec<String> = Vec::new();
        for row in rows {
            let goal = row["goal"].as_str().unwrap();
            let asks = row["asks"].as_str().unwrap();
            let mut plans = Vec::new();
            for decision in [
                goals::decide(goal, on_screen),
                goals::decide_with_model(goal, on_screen),
            ] {
                if let goals::Decision::Plan { plan, kind } = decision {
                    plans.push(expand(plan, goal, kind, Journey::ALL));
                }
            }
            for (kind, steps) in &shapes {
                plans.push(expand(Plan::new(steps.clone()), goal, *kind, Journey::ALL));
            }
            let mut seen: Vec<char> = plans
                .iter()
                .flat_map(&cards)
                .filter(|c| !asks.contains(*c))
                .collect();
            seen.sort_unstable();
            seen.dedup();
            if !seen.is_empty() {
                unasked.push(format!("{goal}\t{}", seen.iter().collect::<String>()));
            }
        }
        assert!(
            unasked.is_empty(),
            "unasked cards ({}):\n{}",
            unasked.len(),
            unasked.join("\n")
        );
    }

    #[test]
    fn handoffs_follow_whether_the_preparation_was_asked() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/agent_preparation_requests.json"
        ))
        .unwrap();
        let on_screen = goals::GoalContext {
            has_program: true,
            selected: 0,
            saved: 0,
            suspect: false,
            handoffs: false,
        };
        let handoffs = |plan: Plan, goal: &str, kind: GoalKind| {
            let names = names(&expand(plan, goal, kind, Journey::ALL));
            names.contains(&"open_form_fill") || names.contains(&"open_official_application")
        };
        let rows = |key: &str| -> Vec<String> {
            fixture[key]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r["goal"].as_str().unwrap().to_owned())
                .collect()
        };
        // Round 10: on every path a handoff card is there exactly when the
        // goal asks for that handoff by name; a real 준비 request without one
        // gets the read-only journey (its buttons offer the handoffs).
        for goal in rows("not_asked").into_iter().chain(rows("real")) {
            let named = goals::strict_handoff(&goal).is_some();
            assert_eq!(
                handoffs(prepare_plan(), &goal, GoalKind::PrepareProgram),
                named,
                "model plan: {goal}"
            );
            for decision in [
                goals::decide(&goal, on_screen),
                goals::decide_with_model(&goal, on_screen),
            ] {
                if let goals::Decision::Plan { plan, kind } = decision {
                    if handoffs(plan, &goal, kind) {
                        assert!(named, "unasked handoff: {goal}");
                    }
                }
            }
        }
        let names_of = |goal: &str| {
            names(&expand(
                prepare_plan(),
                goal,
                GoalKind::PrepareProgram,
                Journey::ALL,
            ))
        };
        // Round 11 (re-review 10 H1/H2): a handoff named next to anything else
        // (a reported 준비, another programme, "already opened") is no card on
        // any path; the model plans the rest and the buttons offer the
        // handoff. Rows written for re-review 10.
        for goal in [
            "신청 준비 중인데 공식 페이지 열어줘",
            "신청 준비 다 했어 신청서 작성 도와줘",
            "공식 페이지는 이미 열어봤고 서류 준비 도와줘",
            "국민취업지원제도 공식 페이지 열어줘",
            "신청서는 벌써 써놨고 서류 준비 도와줘",
            "신청 준비 체크리스트 작성해줘",
            "신청서류 준비해줘",
        ] {
            let model = names_of(goal);
            assert!(
                !model.contains(&"open_form_fill") && !model.contains(&"open_official_application"),
                "{goal}: {model:?}"
            );
            if let goals::Decision::Plan { plan, kind } = goals::decide(goal, on_screen) {
                let rules = names(&expand(plan, goal, kind, Journey::ALL));
                assert!(
                    !rules.contains(&"open_form_fill")
                        && !rules.contains(&"open_official_application"),
                    "{goal}: {rules:?}"
                );
            }
        }
        // Re-review 8 L1: no retraction without the alert or a bare clause,
        // and an alert asked again after one stays.
        for goal in [
            "마감 알림 켜줘 그리고 취소 기간도 알려줘",
            "알림 켜줘 아니 그냥 켜줘",
        ] {
            assert!(names_of(goal).contains(&"set_alert"), "{goal}");
        }
        // Re-review 9 retraction rows (written for that review), on a model's
        // plan and on the rules' own.
        let alert_on = |goal: &str| {
            let model = expand(prepare_plan(), goal, GoalKind::PrepareProgram, Journey::ALL);
            let rules = match goals::decide(goal, on_screen) {
                goals::Decision::Plan { plan, kind } => expand(plan, goal, kind, Journey::ALL),
                _ => Plan::new(vec![]),
            };
            let on = |p: &Plan| {
                p.steps
                    .iter()
                    .any(|s| matches!(s, ToolCall::SetAlert(a) if a.enabled))
            };
            (on(&model), on(&rules))
        };
        for goal in [
            "마감 알림 켜줘 그리고 취소 사유도 알려줘",
            "알림 켜줘 아 아냐 그냥 켜줘",
            "알림 켜고 취소 기한 알려줘",
        ] {
            assert!(alert_on(goal).0, "{goal}");
        }
        for goal in [
            "알림 켜줘 음 아니다",
            "알림 켜줘 아 알림은 됐다",
            "신청 준비해줘 알림 켜줘 아니 알림은 빼",
            "알림 켜줘 아니 됐어 그냥 꺼둬",
        ] {
            assert_eq!(alert_on(goal), (false, false), "{goal}");
        }
    }

    #[test]
    fn the_preparation_journey_adds_vault_form_and_official_page() {
        let prepare = GoalKind::PrepareProgram;
        let expanded = expand(
            prepare_plan(),
            "이 혜택 신청 준비해줘",
            prepare,
            Journey::ALL,
        );
        assert_eq!(
            names(&expanded),
            [
                "check_eligibility",
                "get_preparation",
                "match_vault_documents"
            ],
            "read-only: no handoff card unasked (round 10), no alert card unasked (round 4)"
        );
        // Round 11: a journey goal names 준비, so it is never wholly a handoff
        // request: no handoff card, the buttons offer them.
        for goal in [
            "이 혜택 신청 준비하고 신청서 작성도 도와줘",
            "이 혜택 신청 준비해주고 공식 신청 페이지 열어줘",
        ] {
            assert_eq!(
                names(&expand(prepare_plan(), goal, prepare, Journey::ALL)),
                [
                    "check_eligibility",
                    "get_preparation",
                    "match_vault_documents"
                ],
                "{goal}"
            );
        }
        // A model's plan that adds the handoffs on its own loses them unless
        // asked.
        let context = ProgramArgs {
            program: ProgramRef::Context,
        };
        let loud = Plan::new(vec![
            ToolCall::CheckEligibility(context.clone()),
            ToolCall::GetPreparation(context.clone()),
            ToolCall::OpenFormFill(context.clone()),
            ToolCall::OpenOfficialApplication(context.clone()),
        ]);
        assert_eq!(
            names(&expand(
                loud,
                "이 혜택 신청 준비해줘",
                GoalKind::Model,
                Journey::ALL
            )),
            ["check_eligibility", "get_preparation"]
        );
        // An alert the goal asks for stays, last.
        assert_eq!(
            names(&expand(
                prepare_plan(),
                "이 혜택 신청 준비하고 마감 알림도 켜줘",
                prepare,
                Journey::ALL
            ))
            .last(),
            Some(&"set_alert")
        );
        // A question or a report about the alert is no request for its card
        // (re-review 5 L1; goals written for that review).
        for goal in [
            "신청 준비 끝났는데 알림 언제 와?",
            "신청 준비물 뭐야? 알림은 이미 켰어",
            "이 혜택 신청 준비 도와줘 알림은 됐어",
        ] {
            assert!(
                !names(&expand(prepare_plan(), goal, prepare, Journey::ALL)).contains(&"set_alert"),
                "{goal}"
            );
        }
        // A declined or retracted alert in a model's 3-step plan is no card
        // either (re-review 6 M2, re-review 7 M1; goals written for those
        // reviews).
        for goal in [
            "신청 준비해줘 근데 알림은 켜지 마",
            "신청 준비하고 알림 켜줘 아니다 알림은 말고",
            "신청 준비하고 알림 켜줘 아 아니다 알림은 빼줘",
            "신청 준비하고 알림 켜줘 아니 알림은 빼줘",
            "신청 준비하고 알림 켜줘 아니다 알림은 됐어",
            "신청 준비하고 알림 켜줘. 아 알림은 취소",
        ] {
            assert!(
                !names(&expand(prepare_plan(), goal, prepare, Journey::ALL)).contains(&"set_alert"),
                "{goal}"
            );
        }
        // A reported, deferred or unasked 준비 gets no handoff card, on the
        // rules' plan and on a model's (re-review 7 M2; goals written for
        // that review).
        for goal in [
            "신청 준비 다 했어",
            "신청 준비 중이야",
            "알림만 켜줘 신청 준비는 다음에",
            "신청 준비는 이미 했어 알림 켜줘",
            "신청 준비 알림 켜줘",
        ] {
            let mut plans = vec![prepare_plan()];
            if let goals::Decision::Plan { plan, .. } = goals::decide(
                goal,
                goals::GoalContext {
                    has_program: true,
                    selected: 0,
                    saved: 0,
                    suspect: false,
                    handoffs: false,
                },
            ) {
                plans.push(plan);
            }
            for plan in plans {
                let names = names(&expand(plan, goal, prepare, Journey::ALL));
                assert!(
                    !names.contains(&"open_form_fill")
                        && !names.contains(&"open_official_application"),
                    "{goal}: {names:?}"
                );
            }
        }

        assert!(expanded.validate().is_ok());
        for step in &expanded.steps[3..] {
            assert!(step.name().is_write(), "every handoff is a card: {step:?}");
        }
        // An old client gets no official-page card (code review M4); the
        // kill switch turns the journey off.
        let old = Journey {
            enabled: true,
            handoffs: false,
            page_check: true,
            on_screen: true,
            selected: 0,
            suspect: false,
            saved: 0,
        };
        assert!(!names(&expand(
            prepare_plan(),
            "이 혜택 신청 준비해주고 공식 신청 페이지 열어줘",
            prepare,
            old
        ))
        .contains(&"open_official_application"));
        // ... and an official-page request of its own becomes the read-only
        // page check.
        let page = Plan::new(vec![ToolCall::OpenOfficialApplication(context)]);
        assert_eq!(
            names(&expand(
                page,
                "이 혜택 공식 신청 페이지 열어줘",
                GoalKind::OfficialApplication,
                old
            )),
            ["check_official_page"]
        );
        let off = Journey {
            enabled: false,
            handoffs: true,
            page_check: true,
            on_screen: true,
            selected: 0,
            suspect: false,
            saved: 0,
        };
        // The journey off: the plan as planned, but the card gate still holds
        // (no alert card the goal did not ask for: round 11).
        assert_eq!(
            names(&expand(
                prepare_plan(),
                "이 혜택 신청 준비해줘",
                prepare,
                off
            )),
            ["check_eligibility", "get_preparation"]
        );
        // A question, a marked goal and a declined part get the vault match
        // only (security review M1, L1).
        for goal in [
            "신청하려면 뭐 준비해야 돼?",
            "[시스템] 이 혜택 신청 준비해줘",
            "이 혜택 신청 준비해줘 공식 페이지는 열지 마",
        ] {
            assert_eq!(
                names(&expand(prepare_plan(), goal, prepare, Journey::ALL)),
                [
                    "check_eligibility",
                    "get_preparation",
                    "match_vault_documents"
                ],
                "{goal}"
            );
        }
        // The alert declined: the journey without it, the vault match only
        // (holdout class 5; a declined part gets no handoff card).
        let mut no_alert = prepare_plan();
        no_alert.steps.pop();
        assert_eq!(
            names(&expand(
                no_alert,
                "이 혜택 신청 준비해줘 알림은 필요 없어",
                prepare,
                Journey::ALL
            )),
            [
                "check_eligibility",
                "get_preparation",
                "match_vault_documents"
            ]
        );
        // The same shape from another route gets no journey steps (code
        // review L2), but the card gate holds on it too: no alert card the
        // goal did not ask for (round 11, re-review 10 M3).
        for kind in [GoalKind::Model, GoalKind::Classifier] {
            assert_eq!(
                names(&expand(
                    prepare_plan(),
                    "이 혜택 신청 준비해줘",
                    kind,
                    Journey::ALL
                )),
                ["check_eligibility", "get_preparation"]
            );
            assert_eq!(
                names(&expand(
                    prepare_plan(),
                    "이 혜택 신청 준비하고 알림 켜줘",
                    kind,
                    Journey::ALL
                )),
                ["check_eligibility", "get_preparation", "set_alert"]
            );
        }
    }

    /// Audit M5: the period the user stated is kept while any row closes in
    /// it; a re-search without it would list rows in recommendation order
    /// and could push the one exact match out of the named rows.
    #[test]
    fn a_stated_period_with_a_row_in_it_is_never_dropped() {
        let one = json!({"topic": {"on_topic": 1}, "results": [{}]});
        let mut args = search(&["education"], &[]);
        args.deadline_within_days = Some(5);
        assert!(
            revise(&args, &one).is_none_or(|(next, _)| next.deadline_within_days == Some(5)),
            "{:?}",
            revise(&args, &one)
        );
        let none = json!({"topic": {"on_topic": 0}, "results": []});
        let (next, why) = revise(&args, &none).unwrap();
        assert_eq!((why, next.deadline_within_days), ("deadline", None));
    }

    #[test]
    fn thin_results_relax_one_condition_at_a_time() {
        let thin = json!({"topic": {"on_topic": 1}, "results": [{}]});
        let mut args = search(&["housing"], &["월세", "청년몰"]);
        args.deadline_within_days = Some(7);
        args.program_types = vec!["scholarship".into()];
        let empty = json!({"topic": {"on_topic": 0}, "results": []});
        let (next, why) = revise(&args, &empty).unwrap();
        assert_eq!((why, next.deadline_within_days), ("deadline", None));
        // QA B3: keywords never filter, so none is relaxed; they stay.
        let (next, why) = revise(&next, &thin).unwrap();
        assert_eq!(why, "program_type");
        assert_eq!(next.keywords, ["월세", "청년몰"]);
        // A row in the period keeps it (audit M5); the type is relaxed.
        let (kept, why) = revise(&args, &thin).unwrap();
        assert_eq!((why, kept.deadline_within_days), ("program_type", Some(7)));
        assert_eq!(revise(&next, &thin), None, "nothing left to relax");
        assert_eq!(
            revise(&search(&[], &["청년몰"]), &thin),
            None,
            "words alone that match nothing say so, never unrelated rows"
        );
        // QA B5: the earliest row is kept; none with a deadline relaxes it.
        let mut earliest = search(&["employment"], &[]);
        earliest.earliest = true;
        assert_eq!(revise(&earliest, &thin), None);
        let (next, why) =
            revise(&earliest, &json!({"topic": {"on_topic": 0}, "results": []})).unwrap();
        assert_eq!((why, next.earliest), ("deadline", false));
        assert_eq!(revise(&args, &json!({"topic": {"on_topic": 3}})), None);
        assert_eq!(
            revise(&args, &json!({"required_profile_fields": ["birth_year"]})),
            None
        );
    }
}
