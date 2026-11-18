//! PLAN → EXECUTE → CONFIRM → RESULT for one goal.
//!
//! [`start`] stores the run and returns at once (202); [`drive`] runs in a
//! background task bounded by the runtime's run slots and the run timeout,
//! emitting progress events. Read tools execute; write tools only become
//! confirmations (`confirm::propose`). Nothing in this file applies a write.
use super::{
    classifier::{IntentClassifier, Routed},
    compose, confirm,
    goals::{self, Decision, GoalContext, GoalKind},
    guard,
    planner::ModelOutcome,
    runtime::{PlannerUnavailable, RunChannel},
    store::{self, error, Begun, Error},
    tools::{self, EvidenceIds, Scope},
    AgentMessage, ConfirmationView, StepStatus, StoredResult, ToolResult,
};
use crate::{assistant_provider::redact_question, AppState};
use axum::http::StatusCode;
use chrono::Utc;
use mazimi_core::agent::{Plan, ProgramRef, RunStatus, ToolCall, ToolName, MAX_GOAL_CHARS};
use serde::Serialize;
use serde_json::{json, Value};
use std::{sync::Arc, time::Instant};
use tokio::sync::OwnedSemaphorePermit;
use uuid::Uuid;

pub struct StartInput {
    pub goal: String,
    pub program_id: Option<Uuid>,
    pub program_ids: Vec<Uuid>,
    pub idempotency_key: Option<String>,
    /// The client sent `X-Mazimi-Client-Caps: journey-v1`.
    pub journey_client: bool,
}

#[derive(Debug, Serialize)]
pub struct Started {
    pub run_id: Uuid,
    pub status: String,
    /// The idempotency key matched an earlier request.
    pub existing: bool,
}

pub fn valid_goal(goal: &str) -> Result<&str, Error> {
    let goal = goal.trim();
    if goal.is_empty()
        || goal.chars().count() > MAX_GOAL_CHARS
        || goal
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "invalid_goal",
            "개인정보를 빼고 요청을 1~300자로 적어 주세요.",
        ));
    }
    Ok(goal)
}

pub fn valid_idempotency_key(key: Option<&str>) -> Result<Option<&str>, Error> {
    match key {
        None => Ok(None),
        Some(k)
            if (8..=128).contains(&k.len())
                && k.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') =>
        {
            Ok(Some(k))
        }
        Some(_) => Err(error(
            StatusCode::BAD_REQUEST,
            "invalid_idempotency_key",
            "Idempotency-Key 형식이 올바르지 않아요.",
        )),
    }
}

pub async fn start(
    app: &AppState,
    user: Uuid,
    conversation: Uuid,
    input: StartInput,
) -> Result<Started, Error> {
    let goal = valid_goal(&input.goal)?;
    let key = valid_idempotency_key(input.idempotency_key.as_deref())?;
    let selected = input.program_ids.clone();
    let distinct: std::collections::HashSet<&Uuid> = selected.iter().collect();
    if selected.len() > 4 || distinct.len() != selected.len() {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "invalid_selection",
            "비교할 혜택은 서로 다른 2~4개까지 고를 수 있어요.",
        ));
    }
    if !app.agent.admit_message(user) {
        return Err(error(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "요청이 너무 많아요. 1분 뒤에 다시 시도해 주세요.",
        ));
    }
    let slot = app.agent.try_run_slot().ok_or_else(|| {
        error(
            StatusCode::TOO_MANY_REQUESTS,
            "agent_busy",
            "지금은 비서가 붐벼요. 잠시 후 다시 시도해 주세요.",
        )
    })?;
    // The goal is masked once; nothing downstream sees the raw text.
    let masked: String = redact_question(goal).chars().take(MAX_GOAL_CHARS).collect();
    let begun = store::begin_run(
        &app.pool,
        store::NewRun {
            user,
            conversation,
            goal_masked: &masked,
            context_program_id: input.program_id,
            selected: &selected,
            idempotency_key: key,
        },
    )
    .await?;
    match begun {
        Begun::Existing { run } => {
            let mut connection = app.pool.acquire().await.map_err(store::db)?;
            let row = store::run_row(&mut connection, user, run).await?;
            Ok(Started {
                run_id: run,
                status: row.status,
                existing: true,
            })
        }
        Begun::Started { run, context } => {
            let channel = app.agent.register(run);
            let ctx = RunContext {
                user,
                conversation,
                run,
                goal: masked,
                scope: Scope {
                    user,
                    context,
                    selected,
                    found: Vec::new(),
                },
                journey_client: input.journey_client,
            };
            let app = app.clone();
            tokio::spawn(execute(app, ctx, channel, slot));
            Ok(Started {
                run_id: run,
                status: RunStatus::Received.as_str().into(),
                existing: false,
            })
        }
    }
}

/// Margin over the run timeout before a run is taken for abandoned.
pub const STALE_GRACE: std::time::Duration = std::time::Duration::from_secs(30);
/// How often the API looks for abandoned runs.
const STALE_SWEEP_EVERY: std::time::Duration = std::time::Duration::from_secs(60);

/// Mark runs a stopped process left in flight as failed (`abandoned`) and
/// expire their pending cards, so the client sees a clear failure instead
/// of `planning` for an hour (QA B8: a deploy during a run). A live run
/// takes its slot before its row is created and ends within its run
/// timeout (`execute`). The cutoff is the largest timeout any replica may
/// run with ([`super::runtime::MAX_RUN_TIMEOUT`]) plus [`STALE_GRACE`], so
/// a rolling deploy that lowers the timeout never fails another replica's
/// live run (review).
pub async fn abandon_stale(pool: &sqlx::PgPool) -> Result<u64, sqlx::Error> {
    let stale: Vec<Uuid> = sqlx::query_scalar(
        "UPDATE assistant_runs SET status='failed', error_code='abandoned', updated_at=now(), completed_at=now()
         WHERE status IN ('received','planning','tool_running','executing','validating_response')
           AND created_at < now() - make_interval(secs => $1)
         RETURNING id",
    )
    .bind((super::runtime::MAX_RUN_TIMEOUT + STALE_GRACE).as_secs_f64())
    .fetch_all(pool)
    .await?;
    let mut connection = pool.acquire().await?;
    for run in &stale {
        if store::expire_run_confirmations(&mut connection, *run)
            .await
            .is_err()
        {
            tracing::warn!("cards of an abandoned run were not expired");
        }
    }
    Ok(stale.len() as u64)
}

/// [`abandon_stale`] at startup and then every [`STALE_SWEEP_EVERY`].
pub fn spawn_stale_sweep(pool: sqlx::PgPool) {
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        return;
    };
    handle.spawn(async move {
        let mut tick = tokio::time::interval(STALE_SWEEP_EVERY);
        loop {
            tick.tick().await;
            match abandon_stale(&pool).await {
                Ok(0) => {}
                Ok(n) => tracing::info!(runs = n, "agent runs left in flight marked abandoned"),
                Err(error) => tracing::warn!(%error, "agent stale-run sweep failed"),
            }
        }
    });
}

pub struct RunContext {
    pub user: Uuid,
    pub conversation: Uuid,
    pub run: Uuid,
    pub goal: String,
    pub scope: Scope,
    /// The client opens the journey's handoff cards.
    pub journey_client: bool,
}

async fn execute(
    app: AppState,
    ctx: RunContext,
    channel: Arc<RunChannel>,
    _slot: OwnedSemaphorePermit,
) {
    let started = Instant::now();
    let outcome =
        tokio::time::timeout(app.agent.config.run_timeout, drive(&app, &ctx, &channel)).await;
    let failure = match outcome {
        Ok(Ok(())) => None,
        Ok(Err((status, _))) => {
            tracing::warn!(status = status.as_u16(), "agent run failed");
            Some((RunStatus::Failed, "internal_error"))
        }
        Err(_) => Some((RunStatus::TimedOut, "run_timeout")),
    };
    if let Some((status, code)) = failure {
        if let Ok(mut connection) = app.pool.acquire().await {
            let _ = store::expire_run_confirmations(&mut connection, ctx.run).await;
            if store::transition(&mut connection, ctx.run, status)
                .await
                .unwrap_or(false)
            {
                let _ = sqlx::query(
                    "UPDATE assistant_runs SET error_code=$2, latency_ms=$3 WHERE id=$1",
                )
                .bind(ctx.run)
                .bind(code)
                .bind(started.elapsed().as_millis().min(i32::MAX as u128) as i32)
                .execute(&mut *connection)
                .await;
            }
        }
    }
    let view = match app.pool.acquire().await {
        Ok(mut connection) => {
            let mut view = store::run_view(&mut connection, ctx.user, ctx.run)
                .await
                .ok();
            // A cancel can land while a write is being proposed: close any
            // confirmation created after the cancel expired the others.
            if view.as_ref().is_some_and(|v| {
                matches!(v.status.as_str(), "cancelled" | "failed" | "timed_out")
                    && v.confirmations.iter().any(|c| c.status == "pending")
            }) {
                let _ = store::expire_run_confirmations(&mut connection, ctx.run).await;
                view = store::run_view(&mut connection, ctx.user, ctx.run)
                    .await
                    .ok();
            }
            view
        }
        Err(_) => None,
    };
    let status = view.as_ref().map(|v| v.status.clone());
    channel.emit(json!({"type":"done","status":status,"run":view}));
    channel.finish();
    // The client stopped waiting: tell it when the run is ready.
    if started.elapsed() > app.agent.config.background_after {
        let body = match status.as_deref() {
            Some("awaiting_confirmation") => {
                Some("요청한 정리가 끝났어요. 승인이 필요한 변경이 있어요.")
            }
            Some("completed") => Some("요청한 정리가 끝났어요. 비서 탭에서 확인해 주세요."),
            _ => None,
        };
        if let Some(body) = body {
            if let Err((status, _)) = store::queue_push(&app.pool, ctx.user, ctx.run, body).await {
                tracing::warn!(status = status.as_u16(), "agent push not queued");
            }
        }
    }
}

/// Move the run; `false` means it was cancelled or finished meanwhile.
async fn advance(
    app: &AppState,
    run: Uuid,
    to: RunStatus,
    channel: &RunChannel,
) -> Result<bool, Error> {
    let mut connection = app.pool.acquire().await.map_err(store::db)?;
    let moved = store::transition(&mut connection, run, to).await?;
    if moved {
        channel.emit(json!({"type":"status","status":to.as_str()}));
    }
    Ok(moved)
}

/// Finish a run that planned no tools (refusal, clarification, fallback).
async fn finish_without_tools(
    app: &AppState,
    ctx: &RunContext,
    channel: &RunChannel,
    mut message: AgentMessage,
    started: Instant,
) -> Result<(), Error> {
    if !advance(app, ctx.run, RunStatus::ValidatingResponse, channel).await? {
        return Ok(());
    }
    with_bypass_notice(&ctx.goal, &mut message);
    let result = StoredResult {
        results: Vec::new(),
        message: Some(message.clone()),
    };
    store::save_result(&app.pool, ctx.run, &result, elapsed_ms(started), None).await?;
    let mut connection = app.pool.acquire().await.map_err(store::db)?;
    store::add_message(
        &mut connection,
        ctx.conversation,
        ctx.run,
        "assistant",
        &message.answer,
        &json!([]),
    )
    .await?;
    drop(connection);
    advance(app, ctx.run, RunStatus::Completed, channel).await?;
    Ok(())
}

/// A goal that asked to skip confirmation gets one fixed line saying that
/// changes always need it (routing v4: such goals are routed, not refused,
/// and their writes are confirmation cards like any other).
fn with_bypass_notice(goal: &str, message: &mut AgentMessage) {
    if guard::mentions_bypass(goal) && !message.answer.contains(guard::BYPASS_NOTICE) {
        if !message.answer.is_empty() {
            message.answer.push(' ');
        }
        message.answer.push_str(guard::BYPASS_NOTICE);
    }
}

fn elapsed_ms(started: Instant) -> i32 {
    started.elapsed().as_millis().min(i32::MAX as u128) as i32
}

/// The model that made a plan, stored on the run. For the intent
/// classifier, `prompt_version` holds its label (`label:<tool sequence>`).
struct PlanSource {
    provider: &'static str,
    model: String,
    prompt_version: Option<String>,
}

impl PlanSource {
    fn stored(&self) -> store::PlanSource<'_> {
        store::PlanSource {
            provider: self.provider,
            model: &self.model,
            prompt_version: self.prompt_version.as_deref(),
        }
    }

    fn classifier(version: &str, label: &str) -> Self {
        Self {
            provider: "intent-classifier",
            model: version.to_owned(),
            prompt_version: Some(format!("label:{label}")),
        }
    }
}

enum Planned {
    Steps {
        planner: &'static str,
        kind: GoalKind,
        plan: Plan,
        source: Option<PlanSource>,
    },
    Message {
        planner: &'static str,
        kind: Option<GoalKind>,
        message: AgentMessage,
        source: Option<PlanSource>,
    },
    /// Cancelled while the model planned; the cancel already closed the run.
    Cancelled,
}

/// The intent classifier's routing of a goal that passed the guard, off the
/// async runtime. `Err`: no confident answer (below a threshold, a reply
/// `interpret` rejected), with the deferred label, or `Err(None)` on an
/// error; the rules or the model decide.
async fn classify(
    classifier: Arc<IntentClassifier>,
    goal: &str,
    context: GoalContext,
) -> Result<(ModelOutcome, String), Option<String>> {
    let goal = goal.to_owned();
    let started = Instant::now();
    // A bounded wait (code review L): past it the rules or the model decide;
    // the blocking task finishes on its own.
    let Ok(routed) = tokio::time::timeout(
        CLASSIFIER_TIMEOUT,
        tokio::task::spawn_blocking(move || classifier.route(&goal, context)),
    )
    .await
    else {
        tracing::warn!("intent classifier timed out; routing falls back");
        return Err(None);
    };
    let ms = started.elapsed().as_millis() as u64;
    // Labels are tool names; the goal is never logged.
    match routed {
        Ok(Ok(Routed::Answered {
            outcome,
            label,
            confidence,
        })) => {
            tracing::debug!(%label, confidence, ms, "intent classifier answered");
            Ok((outcome, label))
        }
        Ok(Ok(Routed::Deferred { label, confidence })) => {
            tracing::debug!(%label, confidence, ms, "intent classifier deferred");
            Err(Some(label))
        }
        Ok(Err(error)) => {
            tracing::warn!(%error, "intent classifier failed; routing falls back");
            Err(None)
        }
        Err(_) => {
            tracing::warn!("intent classifier task failed; routing falls back");
            Err(None)
        }
    }
}

/// A classifier answer as a planned run: a plan or a programme picker is a
/// fast-path answer (instant and local), a `refuse` is a refusal. Goal kind
/// `classifier`, provider `intent-classifier`, the artefact version as the
/// model and `label:<label>` as the prompt version.
async fn classified(
    app: &AppState,
    ctx: &RunContext,
    context: GoalContext,
    outcome: ModelOutcome,
    source: PlanSource,
    label: &str,
) -> Planned {
    let kind = GoalKind::Classifier;
    match outcome {
        ModelOutcome::Plan(plan) => Planned::Steps {
            planner: "fast_path",
            kind,
            plan,
            source: Some(source),
        },
        // A way to misreport gets the misreport reply.
        ModelOutcome::Refuse if label == "refuse_fraud" => Planned::Message {
            planner: "refused",
            kind: Some(kind),
            message: AgentMessage {
                answer: MISREPORT_REFUSAL.into(),
                verdict: "not_evaluated".into(),
                ..AgentMessage::default()
            },
            source: Some(source),
        },
        ModelOutcome::Refuse | ModelOutcome::WritesBarred => Planned::Message {
            planner: "refused",
            kind: Some(kind),
            message: compose::fallback(
                context,
                program_state(app, ctx).await,
                &ctx.goal,
                "이 요청은 비서가 할 수 있는 일이 아니에요.",
            ),
            source: Some(source),
        },
        ModelOutcome::NeedsProgram => {
            let saved = saved_titles(app, ctx).await;
            Planned::Message {
                planner: "fast_path",
                kind: Some(kind),
                message: compose::needs_program(kind, &ctx.goal, &saved),
                source: Some(source),
            }
        }
    }
}

/// The classifier's wait budget per goal (p95 about 15 ms on 4 ARM cores).
const CLASSIFIER_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(300);

/// The reply to a way to misreport income, assets, work or residence
/// (the classifier's `refuse_fraud`): no tool, no card, and the honest route.
const MISREPORT_REFUSAL: &str = "사실과 다르게 신고해 혜택을 받는 방법은 안내하지 않아요. \
부정수급은 지원금 환수나 처벌로 이어질 수 있어요. 실제 상황 그대로 받을 수 있는 혜택은 찾아 드릴게요.";

/// [`plan_route`], then the topic head (when the classifier is loaded) on a
/// search whose goal names no field in the lexicon (`classifier::with_topic`).
/// Builds the run's goal context once (screen, selection, the real saved
/// count; `suspect` when the classifier leans to refuse the goal below its
/// bar, security review F1) and reads the goal with the understanding model
/// once (`intent::prepare`, off the runtime, 300 ms): every reader after
/// this (routing, `observe::expand`, the gate, the answer) applies the
/// policy to that context and that reading (integrated review H1, M2).
async fn plan(
    app: &AppState,
    ctx: &RunContext,
    channel: &RunChannel,
) -> Result<(Planned, GoalContext), Error> {
    let mut context = GoalContext {
        has_program: ctx.scope.context.is_some(),
        selected: ctx.scope.selected.len(),
        saved: saved_count(app, ctx).await,
        suspect: false,
        handoffs: ctx.journey_client
            && app
                .agent
                .config
                .tools
                .allows(ToolName::OpenOfficialApplication),
    };
    super::intent::prepare(&ctx.goal).await;
    let planned = plan_route(app, ctx, channel, &mut context).await?;
    let Planned::Steps {
        planner,
        kind,
        plan,
        source,
    } = planned
    else {
        return Ok((planned, context));
    };
    let open_search = plan.steps.iter().any(
        |s| matches!(s, mazimi_core::agent::ToolCall::SearchBenefits(a) if a.fields.is_empty()),
    );
    let plan = match app
        .agent
        .classifier()
        .filter(|c| open_search && c.has_topic_head())
    {
        Some(classifier) => {
            let goal = ctx.goal.clone();
            match tokio::task::spawn_blocking(move || classifier.topic(&goal)).await {
                Ok(Ok(topic)) => super::classifier::with_topic(plan, topic.as_deref()),
                _ => plan,
            }
        }
        None => plan,
    };
    Ok((
        Planned::Steps {
            planner,
            kind,
            plan,
            source,
        },
        context,
    ))
}

/// The rules' confident answer (`goals::decide_with_model_and`), if any.
fn confident_rules(goal: &str, context: GoalContext, extras: goals::Extras) -> Option<Decision> {
    match goals::decide_with_model_and(goal, context, extras) {
        Decision::Unrecognized => None,
        decision => Some(decision),
    }
}

/// A model plan the run keeps: the steps this deployment offers, or no
/// steps when the goal asks for an offered write (R3: expand adds it; code
/// review L: a produced calendar card with calendar off would leave an
/// empty plan). `None`: the fallback message.
pub(crate) fn kept_model_plan(
    mut plan: Plan,
    goal: &str,
    context: GoalContext,
    flags: super::ToolFlags,
) -> Option<Plan> {
    plan.steps.retain(|s| flags.allows(s.name()));
    let writes = super::cards::requested(goal, context)
        .iter()
        .any(|s| flags.allows(s.name()));
    (!plan.steps.is_empty() || writes).then_some(plan)
}

/// The plan a run executes: the agent's own steps on what a search finds
/// and the asked writes ([`super::observe::expand`]), then only the tools
/// this deployment offers.
pub(crate) fn executed_plan(
    plan: Plan,
    goal: &str,
    kind: GoalKind,
    flags: super::ToolFlags,
    context: GoalContext,
) -> Plan {
    let mut plan = super::observe::expand(
        plan,
        goal,
        kind,
        super::observe::Journey {
            enabled: flags.allows(ToolName::MatchVaultDocuments),
            handoffs: context.handoffs,
            page_check: flags.allows(ToolName::CheckOfficialPage),
            on_screen: context.has_program,
            selected: context.selected,
            suspect: context.suspect,
            saved: context.saved,
        },
    );
    plan.steps.retain(|s| flags.allows(s.name()));
    plan
}

/// The classifier's reading of a goal the guard let through ([`classify`]):
/// `None` without a classifier.
pub(crate) type ClassifierRead = Option<Result<(ModelOutcome, String), Option<String>>>;

/// Where a goal goes: a routing decision, or the classifier's own answer.
#[derive(Debug)]
pub(crate) enum Route {
    Decision(Decision),
    Classified(ModelOutcome, String),
}

/// The routing decision, pure: production ([`plan_route`]) and the
/// acceptance probe call this one function.
///
/// Order with a classifier: the guard; a confident classifier refusal (the
/// misreporting class is the model's: "위장전입으로 … 찾아서 저장해줘" must not
/// reach the found-search write route); the found-search, declined-part and
/// v3 shapes; the rules' confident shapes (the 신청 준비 journey, catalogue
/// closing-soon searches); then the classifier's answer; below its
/// threshold the model (`model_on`) or the rules. A refusal lean under the
/// classifier's bar sets `context.suspect` before the rules run (integrated
/// review L3), weighed against the label space (`knows`,
/// [`super::classifier::suspect_lean`]): no write is planned or produced
/// for a suspect goal, an honest question with the same words keeps its read.
pub(crate) fn route_decision(
    goal: &str,
    context: &mut GoalContext,
    routed: ClassifierRead,
    extras: goals::Extras,
    model_on: bool,
    knows: impl Fn(ToolName) -> bool,
) -> Route {
    let Some(routed) = routed else {
        return Route::Decision(if model_on {
            goals::decide_with_model_and(goal, *context, extras)
        } else {
            goals::decide_with(goal, *context, extras)
        });
    };
    if let Some(decision) = goals::guarded(goal) {
        return Route::Decision(decision);
    }
    if let Ok((ModelOutcome::Refuse, label)) = &routed {
        return Route::Classified(ModelOutcome::Refuse, label.clone());
    }
    context.suspect = matches!(&routed, Err(Some(label)) if
    super::classifier::suspect_lean(
        label,
        &knows,
        &super::intent::asked_tools(goal, *context),
        super::guard::names_other_person(goal),
    ));
    let context = *context;
    let rules = goals::found_decision(goal, context)
        .or_else(|| goals::declined_decision(goal, context, extras))
        .or_else(|| goals::v3_decision(goal, context, extras))
        .or_else(|| confident_rules(goal, context, extras));
    match rules.map(Ok).unwrap_or(Err(routed)) {
        Ok(decision) => Route::Decision(decision),
        Err(Ok((outcome, label))) => Route::Classified(outcome, label),
        Err(Err(_)) if model_on => Route::Decision(Decision::Unrecognized),
        Err(Err(_)) => Route::Decision(goals::decide_with(goal, context, extras)),
    }
}

async fn plan_route(
    app: &AppState,
    ctx: &RunContext,
    channel: &RunChannel,
    context: &mut GoalContext,
) -> Result<Planned, Error> {
    // With the model on, the fast path answers only its confident shapes;
    // the rules' own answer is the fallback when the model cannot plan.
    let model_on = app.agent.model_planner_available();
    // agent-tools-v3 flags: a tool that is off is never routed, planned or
    // offered as a fallback.
    let flags = app.agent.config.tools;
    let extras = flags.extras();
    let classifier = app.agent.classifier();
    // The classifier reads only a goal the guard lets through.
    let routed = match &classifier {
        Some(c) if goals::guarded(&ctx.goal).is_none() => {
            Some(classify(c.clone(), &ctx.goal, *context).await)
        }
        Some(_) => Some(Err(None)),
        None => None,
    };
    let knows = |tool: ToolName| classifier.as_ref().is_some_and(|c| c.knows(tool));
    let decision = match route_decision(&ctx.goal, context, routed, extras, model_on, knows) {
        Route::Decision(decision) => decision,
        Route::Classified(outcome, label) => {
            let version = classifier.as_ref().map_or("", |c| c.version());
            let source = PlanSource::classifier(version, &label);
            return Ok(classified(app, ctx, *context, outcome, source, &label).await);
        }
    };
    let context = *context;
    Ok(match decision {
        Decision::Plan { kind, plan } => Planned::Steps {
            planner: "fast_path",
            kind,
            plan,
            source: None,
        },
        Decision::Refuse(refusal) => Planned::Message {
            planner: "refused",
            kind: None,
            message: compose::refusal(
                refusal,
                context.has_program,
                program_state(app, ctx).await,
                &ctx.goal,
            ),
            source: None,
        },
        Decision::NeedsProgram { kind } => {
            let saved = saved_titles(app, ctx).await;
            Planned::Message {
                planner: "fast_path",
                kind: Some(kind),
                message: compose::needs_program(kind, &ctx.goal, &saved),
                source: None,
            }
        }
        Decision::Unrecognized => {
            if !model_on {
                return Ok(Planned::Message {
                    planner: "fallback",
                    kind: None,
                    message: compose::fallback(
                        context,
                        program_state(app, ctx).await,
                        &ctx.goal,
                        "이 요청은 아직 바로 처리할 수 없어요.",
                    ),
                    source: None,
                });
            }
            channel.emit(json!({"type":"planning","planner":"model"}));
            // A cancel drops the model call, which frees the shared model
            // slot at once instead of after the plan timeout.
            let planned = tokio::select! {
                planned = app.agent.plan_with_model(ctx.user, &ctx.goal, context) => planned,
                () = channel.cancelled() => return Ok(Planned::Cancelled),
            };
            // The model could not plan (busy, budget, outage, invalid
            // output): a goal the rules can answer gets their answer, recorded
            // as `fast_path_fallback` so run metrics tell it from a fast path.
            // A model refusal is answered as one only when the deployment opts
            // in (`planner_refusals`, default off): refusing is the guard's,
            // the rules' and the classifier bar's job, and the model refuses
            // honest goals that name fraud terms. Otherwise it is handled like
            // a model that could not plan. A plan of writes only on a goal
            // that bars writes is the guard's refusal (`WritesBarred`), not
            // the model's, and stays one.
            let distrusted = !app.agent.config.planner_refusals
                && matches!(planned, Ok(Ok(ModelOutcome::Refuse)));
            if distrusted || matches!(planned, Ok(Err(_)) | Err(_)) {
                match goals::decide_with(&ctx.goal, context, extras) {
                    Decision::Plan { kind, plan } => {
                        return Ok(Planned::Steps {
                            planner: "fast_path_fallback",
                            kind,
                            plan,
                            source: None,
                        })
                    }
                    Decision::NeedsProgram { kind } => {
                        let saved = saved_titles(app, ctx).await;
                        return Ok(Planned::Message {
                            planner: "fast_path_fallback",
                            kind: Some(kind),
                            message: compose::needs_program(kind, &ctx.goal, &saved),
                            source: None,
                        });
                    }
                    Decision::Refuse(refusal) if distrusted => {
                        return Ok(Planned::Message {
                            planner: "refused",
                            kind: None,
                            message: compose::refusal(
                                refusal,
                                context.has_program,
                                program_state(app, ctx).await,
                                &ctx.goal,
                            ),
                            source: None,
                        })
                    }
                    Decision::Refuse(_) | Decision::Unrecognized => {}
                }
            }
            match planned {
                Ok(outcome) => match outcome {
                    // Not the model's call to refuse, and no rule answers it.
                    Ok(ModelOutcome::Refuse) if distrusted => Planned::Message {
                        planner: "fallback",
                        kind: None,
                        message: compose::fallback(
                            context,
                            program_state(app, ctx).await,
                            &ctx.goal,
                            "요청을 도구 계획으로 바꾸지 못했어요.",
                        ),
                        source: None,
                    },
                    Ok(ModelOutcome::Refuse | ModelOutcome::WritesBarred) => Planned::Message {
                        planner: "model",
                        kind: Some(GoalKind::Model),
                        message: compose::fallback(
                            context,
                            program_state(app, ctx).await,
                            &ctx.goal,
                            "이 요청은 비서가 할 수 있는 일이 아니에요.",
                        ),
                        source: None,
                    },
                    Ok(ModelOutcome::NeedsProgram) => {
                        let saved = saved_titles(app, ctx).await;
                        Planned::Message {
                            planner: "model",
                            kind: Some(GoalKind::Model),
                            message: compose::needs_program(GoalKind::Model, &ctx.goal, &saved),
                            source: None,
                        }
                    }
                    // Steps of a tool this deployment does not offer are
                    // dropped; nothing left is the model's refusal.
                    Ok(ModelOutcome::Plan(plan)) => {
                        let kept = kept_model_plan(plan, &ctx.goal, context, flags);
                        if let Some(plan) = kept {
                            Planned::Steps {
                                planner: "model",
                                kind: GoalKind::Model,
                                plan,
                                source: app.agent.planner_model().map(|model| PlanSource {
                                    provider: "ollama",
                                    model,
                                    prompt_version: Some(
                                        app.agent.planner_prompt_version(&flags.v3_tools()),
                                    ),
                                }),
                            }
                        } else {
                            Planned::Message {
                                planner: "model",
                                kind: Some(GoalKind::Model),
                                message: compose::fallback(
                                    context,
                                    program_state(app, ctx).await,
                                    &ctx.goal,
                                    "이 요청은 지금 비서가 할 수 없어요.",
                                ),
                                source: None,
                            }
                        }
                    }
                    Err(_) => Planned::Message {
                        planner: "fallback",
                        kind: None,
                        message: compose::fallback(
                            context,
                            program_state(app, ctx).await,
                            &ctx.goal,
                            "요청을 도구 계획으로 바꾸지 못했어요.",
                        ),
                        source: None,
                    },
                },
                Err(reason) => Planned::Message {
                    planner: "fallback",
                    kind: None,
                    message: compose::fallback(
                        context,
                        program_state(app, ctx).await,
                        &ctx.goal,
                        match reason {
                            PlannerUnavailable::Busy => "지금은 AI가 다른 요청을 처리하고 있어요.",
                            PlannerUnavailable::UserLimit => {
                                "AI 계획은 한 시간에 6번까지 쓸 수 있어요."
                            }
                            _ => "지금은 AI 계획을 쓸 수 없어요.",
                        },
                    ),
                    source: None,
                },
            }
        }
    })
}

/// The alert and save state of the programme on screen and what can run on
/// it now ([`compose::ProgramFacts`]), for its write buttons; unknown (both
/// directions, every button) when it cannot be read.
async fn program_state(app: &AppState, ctx: &RunContext) -> Option<compose::ProgramState> {
    let id = ctx.scope.context?;
    let flags = app.agent.config.tools;
    let unknown = compose::ProgramState {
        id,
        alert: None,
        saved: None,
        facts: compose::ProgramFacts {
            flags,
            ..compose::ProgramFacts::ANY
        },
    };
    let read = async {
        let mut connection = app.pool.acquire().await?;
        let (alert, saved) = sqlx::query_as::<_, (Option<bool>, bool)>(
            "SELECT (SELECT enabled FROM alert_subscriptions WHERE user_id=$1 AND program_id=$2),
                    EXISTS(SELECT 1 FROM user_bookmarks WHERE user_id=$1 AND program_id=$2)",
        )
        .bind(ctx.user)
        .bind(id)
        .fetch_one(&mut *connection)
        .await?;
        let program =
            sqlx::query_as::<_, mazimi_core::models::Program>("SELECT * FROM programs WHERE id=$1")
                .bind(id)
                .fetch_optional(&mut *connection)
                .await?;
        let open: bool = sqlx::query_scalar(&format!(
            "SELECT EXISTS(SELECT 1 FROM current_programs WHERE id=$1 AND {})",
            mazimi_core::currently_available_sql("", "now()")
        ))
        .bind(id)
        .fetch_one(&mut *connection)
        .await?;
        // The form helper's own source: the policy's templates
        // (`tools::open_form_fill`).
        let has_template = match &program {
            Some(p) => {
                let resources =
                    crate::services::application_resources::load(&mut connection, p).await?;
                // Only an open programme's (the helper's own rule).
                open && !crate::services::application_policy::with_resources(p, resources)
                    .resources
                    .templates
                    .is_empty()
            }
            None => false,
        };
        Ok::<_, sqlx::Error>(compose::ProgramState {
            id,
            alert: Some(alert.unwrap_or(false)),
            saved: Some(saved),
            facts: compose::ProgramFacts::of(
                flags,
                program.as_ref(),
                open,
                has_template,
                super::calendar::kst_today(),
            ),
        })
    };
    Some(match read.await {
        Ok(state) => state,
        Err(error) => {
            tracing::warn!(%error, "programme state for the answer buttons failed");
            unknown
        }
    })
}

/// How many programmes the user has saved (0 when it cannot be read).
async fn saved_count(app: &AppState, ctx: &RunContext) -> usize {
    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM user_bookmarks WHERE user_id = $1")
        .bind(ctx.user)
        .fetch_one(&app.pool)
        .await
        .map_or(0, |n| usize::try_from(n).unwrap_or(0))
}

async fn saved_titles(app: &AppState, ctx: &RunContext) -> Vec<(Uuid, String)> {
    let Ok(ids) = tools::resolve(&app.pool, &ctx.scope, ProgramRef::Saved, 3, true).await else {
        return Vec::new();
    };
    sqlx::query_as::<_, (Uuid, String)>("SELECT id, title FROM programs WHERE id = ANY($1)")
        .bind(&ids)
        .fetch_all(&app.pool)
        .await
        .unwrap_or_default()
}

async fn drive(app: &AppState, ctx: &RunContext, channel: &RunChannel) -> Result<(), Error> {
    let started = Instant::now();
    if !advance(app, ctx.run, RunStatus::Planning, channel).await? {
        return Ok(());
    }
    let (planned, context) = plan(app, ctx, channel).await?;
    if channel.is_cancelled() {
        return Ok(());
    }
    let (planner, kind, plan, source) = match planned {
        Planned::Cancelled => return Ok(()),
        Planned::Message {
            planner,
            kind,
            message,
            source,
        } => {
            store::set_plan(
                &app.pool,
                ctx.run,
                planner,
                kind.map(GoalKind::as_str),
                None,
                source.as_ref().map(PlanSource::stored),
            )
            .await?;
            return finish_without_tools(app, ctx, channel, message, started).await;
        }
        Planned::Steps {
            planner,
            kind,
            plan,
            source,
        } => (planner, kind, plan, source),
    };
    // The agent's own steps on what a search finds, after any planner.
    let flags = app.agent.config.tools;
    let plan = executed_plan(plan, &ctx.goal, kind, flags, context);
    let plan_json = stored_plan(&plan)?;
    store::set_plan(
        &app.pool,
        ctx.run,
        planner,
        Some(kind.as_str()),
        Some(&plan_json),
        source.as_ref().map(PlanSource::stored),
    )
    .await?;
    channel.emit(json!({
        "type":"plan","planner":planner,"goal_kind":kind.as_str(),
        "steps": plan.steps.iter().enumerate().map(|(i, s)| json!({
            "index": i, "tool": s.name().as_str(), "label": super::steps::label(s.name(), 1, 1),
            "permission": super::permission_str(s.name().permission()),
        })).collect::<Vec<_>>(),
    }));
    if !advance(app, ctx.run, RunStatus::ToolRunning, channel).await? {
        return Ok(());
    }
    let mut results: Vec<ToolResult> = Vec::new();
    // Programmes whose official page, read live in this run, did not show
    // the intake open: no new calendar or form card for them (code review M3).
    let mut not_open: std::collections::HashSet<Uuid> = std::collections::HashSet::new();
    let mut confirmations: Vec<ConfirmationView> = Vec::new();
    let mut seq: i32 = 0;
    // Steps run in order; observing a search may insert a relaxed re-search
    // right after it (bounded by `observe::MAX_REVISIONS`), and `Found`
    // follow-ups read what every search so far found.
    let mut steps: Vec<ToolCall> = plan.steps.clone();
    // Inserted re-search steps: what they relax and the seq of the search
    // they retry. Planned steps keep their plan index for the timeline;
    // inserted ones have none (the client appends a row).
    let mut relaxed: std::collections::HashMap<usize, (&'static str, i32)> =
        std::collections::HashMap::new();
    let mut plan_index: Vec<Option<usize>> = (0..steps.len()).map(Some).collect();
    let mut scope = ctx.scope.clone();
    let mut found_by_root: Vec<(i32, Vec<Uuid>)> = Vec::new();
    // The candidate list a search scored, for its relaxed re-searches
    // (code review M6).
    let mut search_cache: crate::services::benefit_assistant::search::SearchCache = None;
    let mut revisions = 0usize;
    let mut next = 0usize;
    loop {
        if next >= steps.len() {
            break;
        }
        let index = next;
        next += 1;
        let step = &steps[index].clone();
        let total = plan.steps.len();
        let shown = plan_index[index];
        if channel.is_cancelled() {
            return Ok(());
        }
        let tool = step.name();
        // Tool-call budget: the remaining steps are recorded as skipped.
        if seq as usize >= super::observe::MAX_TOOL_CALLS {
            let id =
                store::insert_tool_call(&app.pool, ctx.run, seq, tool, None, &redacted_args(step))
                    .await?;
            store::finish_tool_call(&app.pool, id, "error", &json!({}), Some("step_budget"), 0)
                .await?;
            results.push(err_result(
                seq,
                tool,
                None,
                tools::tool_error(
                    "step_budget",
                    "한 번에 처리할 수 있는 단계를 넘어 이 단계는 건너뛰었어요.",
                ),
            ));
            seq += 1;
            continue;
        }
        // Compare runs once over every resolved programme; other tools run
        // once per resolved programme (or once with none).
        let mut compare_ids: Option<Vec<Uuid>> = None;
        let programs: Vec<Option<Uuid>> = match step.program() {
            None => vec![None],
            Some(reference) => {
                match tools::resolve(
                    &app.pool,
                    &scope,
                    reference,
                    tools::expansion_limit(step),
                    // Status records, calendar dates and turning an alert off
                    // may name a programme whose intake is closed or not open
                    // yet.
                    !matches!(
                        tool,
                        ToolName::RecordApplicationStatus | ToolName::AddToCalendar
                    ) && !matches!(step, ToolCall::SetAlert(a) if !a.enabled),
                )
                .await
                {
                    Ok(ids) if matches!(step, ToolCall::Compare(_)) => {
                        compare_ids = Some(ids);
                        vec![None]
                    }
                    Ok(ids) => ids.into_iter().map(Some).collect(),
                    Err(err) => {
                        let id = store::insert_tool_call(
                            &app.pool,
                            ctx.run,
                            seq,
                            tool,
                            None,
                            &redacted_args(step),
                        )
                        .await?;
                        store::finish_tool_call(
                            &app.pool,
                            id,
                            "error",
                            &json!({}),
                            Some(&err.code),
                            0,
                        )
                        .await?;
                        channel.emit(json!({"type":"tool","seq":seq,"index":shown,"total":total,
                            "tool":tool.as_str(),"label":super::steps::label(tool, 1, 1),"status":"error"}));
                        results.push(ToolResult {
                            seq,
                            tool,
                            permission: tool.permission(),
                            program_id: None,
                            ok: false,
                            status: StepStatus::Error,
                            data: json!({}),
                            evidence: Vec::new(),
                            error: Some(err),
                            executed_at: Utc::now(),
                        });
                        seq += 1;
                        continue;
                    }
                }
            }
        };
        // One timeline row for the step, whatever it expands to: the first
        // call's seq and one label ([`super::steps`]).
        let row = seq;
        let row_label = super::steps::label(tool, programs.len(), programs.len());
        // The row's status so far, by the saved view's own rule
        // (`steps::status`): live and saved timelines agree (verifier).
        let mut row_statuses: Vec<&'static str> = Vec::new();
        for program in programs {
            if channel.is_cancelled() {
                return Ok(());
            }
            // The budget holds inside an expanded step too.
            if seq as usize >= super::observe::MAX_TOOL_CALLS {
                let id = store::insert_tool_call(
                    &app.pool,
                    ctx.run,
                    seq,
                    tool,
                    program,
                    &redacted_args(step),
                )
                .await?;
                store::finish_tool_call(&app.pool, id, "error", &json!({}), Some("step_budget"), 0)
                    .await?;
                results.push(err_result(
                    seq,
                    tool,
                    program,
                    tools::tool_error(
                        "step_budget",
                        "한 번에 처리할 수 있는 단계를 넘어 이 단계는 건너뛰었어요.",
                    ),
                ));
                seq += 1;
                break;
            }
            let call_started = Instant::now();
            let call_id = store::insert_tool_call(
                &app.pool,
                ctx.run,
                seq,
                tool,
                program,
                &redacted_args(step),
            )
            .await?;
            channel.emit(json!({"type":"tool","seq":row,"index":shown,"total":total,
                "tool":tool.as_str(),"label":row_label,"status":"running","program_id":program}));
            let mut ids = EvidenceIds::new(seq);
            // `open_form_fill` reads first and proposes its card below.
            let held = matches!(
                tool,
                ToolName::AddToCalendar
                    | ToolName::OpenFormFill
                    | ToolName::RequestSignature
                    | ToolName::OpenOfficialApplication
            ) && program.is_some_and(|p| not_open.contains(&p));
            // No draft to sign: the form step found no template.
            let no_draft = results.iter().find(|r| {
                r.tool == ToolName::OpenFormFill
                    && r.program_id == program
                    && r.data["forms"].as_array().is_some_and(|f| f.is_empty())
            });
            let result = if let (ToolName::RequestSignature, Some(form)) = (tool, no_draft) {
                ok_result(
                    seq,
                    tool,
                    program,
                    StepStatus::Skipped,
                    json!({"program_id": program, "title": form.data["title"],
                        "already": "서명할 신청서 초안이 없어 서명 단계는 건너뛰었어요. 서명은 공식 신청처에서 직접 해 주세요."}),
                    Vec::new(),
                )
            } else if held {
                err_result(
                    seq,
                    tool,
                    program,
                    tools::tool_error(
                        "official_page_not_open",
                        "공식 페이지에서 접수 중으로 확인되지 않아 이 혜택의 일정·서식 카드는 만들지 않았어요.",
                    ),
                )
            } else if tool.is_write() && tool != ToolName::OpenFormFill {
                // A profile write names no programme: it is bound to none.
                let program = match (tool, program) {
                    (ToolName::ProfilePatch, _) => confirm::NO_PROGRAM,
                    (_, Some(program)) => program,
                    (_, None) => {
                        return Err(crate::errors::internal_error("write without programme"))
                    }
                };
                match confirm::propose(
                    &app.pool,
                    ctx.user,
                    ctx.run,
                    call_id,
                    tool,
                    program,
                    step,
                    app.agent.config.confirmation_ttl,
                )
                .await
                {
                    Ok(proposed) => {
                        let status = if proposed.confirmation.is_some() {
                            StepStatus::AwaitingConfirmation
                        } else {
                            StepStatus::Skipped
                        };
                        let data = json!({
                            "program_id": (program != confirm::NO_PROGRAM).then_some(program),
                            "title": proposed.preview.title,
                            "preview": proposed.preview,
                            "confirmation_id": proposed.confirmation.as_ref().map(|c| c.id),
                            "already": if status == StepStatus::Skipped { Some(format!("이미 ‘{}’ 상태예요. 바꿀 것이 없어요.", proposed.preview.before)) } else { None },
                        });
                        if let Some(view) = proposed.confirmation {
                            confirmations.push(view);
                        }
                        let program_id = (program != confirm::NO_PROGRAM).then_some(program);
                        ok_result(seq, tool, program_id, status, data, Vec::new())
                    }
                    // Nothing to hand off: the step is skipped and says why.
                    Err(err)
                        if matches!(
                            err.code.as_str(),
                            "no_official_destination" | "no_form_template" | "no_deadline"
                        ) =>
                    {
                        ok_result(
                            seq,
                            tool,
                            Some(program),
                            StepStatus::Skipped,
                            json!({"program_id": program, "already": err.message, "skipped": err.code}),
                            Vec::new(),
                        )
                    }
                    Err(err) => err_result(
                        seq,
                        tool,
                        (program != confirm::NO_PROGRAM).then_some(program),
                        err,
                    ),
                }
            } else {
                let programs_for_call: Vec<Uuid> = match &compare_ids {
                    Some(all) => all.clone(),
                    None => program.into_iter().collect(),
                };
                let read = async {
                    match (step, program) {
                        // The live page goes through the runtime's fetch client.
                        (ToolCall::CheckOfficialPage(_), Some(id)) => {
                            super::official_page::check(
                                &app.pool,
                                &app.agent.official_pages,
                                ctx.user,
                                id,
                                &mut ids,
                            )
                            .await
                        }
                        (ToolCall::SearchBenefits(args), _) => {
                            tools::search_benefits_cached(
                                &app.pool,
                                ctx.user,
                                args,
                                &mut ids,
                                &mut search_cache,
                            )
                            .await
                        }
                        _ => {
                            tools::run_read(&app.pool, ctx.user, step, &programs_for_call, &mut ids)
                                .await
                        }
                    }
                };
                match tokio::time::timeout(app.agent.config.tool_timeout, read).await {
                    Ok(Ok(mut outcome)) => {
                        let mut status = StepStatus::Ok;
                        // The form helper with a device-built draft is the
                        // user's call: a card when there is a form to open.
                        // No form keeps the read result (guided copy sheet).
                        let has_forms = outcome.data["forms"]
                            .as_array()
                            .is_some_and(|f| !f.is_empty());
                        if let (ToolName::OpenFormFill, Some(id), true) = (tool, program, has_forms)
                        {
                            match confirm::propose(
                                &app.pool,
                                ctx.user,
                                ctx.run,
                                call_id,
                                tool,
                                id,
                                step,
                                app.agent.config.confirmation_ttl,
                            )
                            .await
                            {
                                Ok(proposed) => {
                                    if let Some(view) = proposed.confirmation {
                                        outcome.data["confirmation_id"] = json!(view.id);
                                        outcome.data["preview"] = json!(proposed.preview);
                                        confirmations.push(view);
                                        status = StepStatus::AwaitingConfirmation;
                                    }
                                }
                                // No card, no direct links: a failed proposal
                                // must not open the helper without consent
                                // (code review). The step reports the error.
                                Err(err) => {
                                    outcome.data["forms"] = json!([]);
                                    outcome.data["card_error"] = json!(err.code);
                                    outcome.data["card_message"] = json!(err.message);
                                }
                            }
                        }
                        if let (ToolName::CheckOfficialPage, Some(id)) = (tool, program) {
                            if matches!(
                                outcome.data["live_status"].as_str(),
                                Some("closed" | "upcoming" | "unknown")
                            ) {
                                not_open.insert(id);
                            }
                        }
                        if let Some((reason, root)) = relaxed.get(&index) {
                            outcome.data["relaxed"] = json!(reason);
                            outcome.data["relaxes"] = json!(root);
                        }
                        if let ToolCall::SearchBenefits(args) = step {
                            if let Some(line) = super::answer::topic_line(&outcome.data, &ctx.goal)
                            {
                                outcome.data["topic_line"] = json!(line);
                            }
                            // `found` is what the answer names: a relaxed
                            // re-search replaces its root's rows (code
                            // review M1).
                            let root = relaxed.get(&index).map_or(seq, |(_, r)| *r);
                            let named = super::answer::named_ids_of(&outcome.data);
                            match found_by_root.iter_mut().find(|(r, _)| *r == root) {
                                Some((_, ids)) => *ids = named,
                                None => found_by_root.push((root, named)),
                            }
                            scope.found.clear();
                            for id in found_by_root.iter().flat_map(|(_, ids)| ids) {
                                if !scope.found.contains(id) {
                                    scope.found.push(*id);
                                }
                            }
                            if revisions < super::observe::MAX_REVISIONS {
                                if let Some((retry, reason)) =
                                    super::observe::revise(args, &outcome.data)
                                {
                                    revisions += 1;
                                    let retry = ToolCall::SearchBenefits(retry);
                                    if !steps.contains(&retry) {
                                        let root = relaxed.get(&index).map_or(seq, |(_, r)| *r);
                                        steps.insert(index + 1, retry);
                                        plan_index.insert(index + 1, None);
                                        relaxed.insert(index + 1, (reason, root));
                                    }
                                }
                            }
                        }
                        let r =
                            ok_result(seq, tool, program, status, outcome.data, outcome.evidence);
                        store::finish_tool_call(
                            &app.pool,
                            call_id,
                            status.as_str(),
                            &outcome.summary,
                            None,
                            elapsed_ms(call_started),
                        )
                        .await?;
                        row_statuses.push(status.as_str());
                        channel.emit(json!({"type":"tool","seq":row,"index":shown,"total":total,
                            "tool":tool.as_str(),"label":row_label,
                            "status":super::steps::status(row_statuses.iter().copied()),"program_id":program}));
                        results.push(r);
                        seq += 1;
                        continue;
                    }
                    Ok(Err(err)) => err_result(seq, tool, program, err),
                    Err(_) => err_result(
                        seq,
                        tool,
                        program,
                        tools::tool_error(
                            "tool_timeout",
                            "정보를 불러오는 데 시간이 너무 걸렸어요.",
                        ),
                    ),
                }
            };
            store::finish_tool_call(
                &app.pool,
                call_id,
                result.status.as_str(),
                &json!({"ok": result.ok}),
                result.error.as_ref().map(|e| e.code.as_str()),
                elapsed_ms(call_started),
            )
            .await?;
            row_statuses.push(result.status.as_str());
            channel.emit(json!({"type":"tool","seq":row,"index":shown,"total":total,
                "tool":tool.as_str(),"label":row_label,
                "status":super::steps::status(row_statuses.iter().copied()),"program_id":program}));
            results.push(result);
            seq += 1;
        }
    }
    if !advance(app, ctx.run, RunStatus::ValidatingResponse, channel).await? {
        return Ok(());
    }
    let state = program_state(app, ctx).await;
    let mut message = compose::compose_for(&results, &confirmations, context, &ctx.goal, state);
    with_bypass_notice(&ctx.goal, &mut message);
    let citations = serde_json::to_value(&message.citations).unwrap_or(Value::Array(vec![]));
    store::save_result(
        &app.pool,
        ctx.run,
        &StoredResult {
            results,
            message: Some(message.clone()),
        },
        elapsed_ms(started),
        None,
    )
    .await?;
    let mut connection = app.pool.acquire().await.map_err(store::db)?;
    store::add_message(
        &mut connection,
        ctx.conversation,
        ctx.run,
        "assistant",
        &message.answer,
        &citations,
    )
    .await?;
    drop(connection);
    if confirmations.is_empty() {
        advance(app, ctx.run, RunStatus::Completed, channel).await?;
    } else if advance(app, ctx.run, RunStatus::AwaitingConfirmation, channel).await? {
        channel.emit(json!({"type":"awaiting_confirmation","count":confirmations.len()}));
    }
    Ok(())
}

/// The plan as stored on the run row: a profile value is removed, as on the
/// tool-call row; the confirmation row is the one place that keeps it.
fn stored_plan(plan: &Plan) -> Result<Value, Error> {
    let mut value = serde_json::to_value(plan).map_err(crate::errors::internal_error)?;
    for step in value["steps"].as_array_mut().into_iter().flatten() {
        if step["tool"] == ToolName::ProfilePatch.as_str() {
            if let Some(args) = step["args"].as_object_mut() {
                args.remove("value");
            }
        }
    }
    Ok(value)
}

/// Arguments as stored on the tool call row. The only free text a plan may
/// carry (`question`, `task`) is already masked; keep it bounded.
fn redacted_args(step: &ToolCall) -> Value {
    let mut value = serde_json::to_value(step)
        .ok()
        .and_then(|v| v.get("args").cloned())
        .unwrap_or(Value::Null);
    if let Some(question) = value.get_mut("question") {
        *question = json!(question.as_str().map(|q| q.chars().count()).unwrap_or(0));
    }
    // A profile value is the user's data: the call row keeps the field
    // only. The confirmation row keeps the value, which its hash binds.
    if let (ToolCall::ProfilePatch(_), Some(args)) = (step, value.as_object_mut()) {
        args.remove("value");
    }
    value
}

fn ok_result(
    seq: i32,
    tool: ToolName,
    program_id: Option<Uuid>,
    status: StepStatus,
    data: Value,
    evidence: Vec<super::EvidenceRef>,
) -> ToolResult {
    ToolResult {
        seq,
        tool,
        permission: tool.permission(),
        program_id,
        ok: true,
        status,
        data,
        evidence,
        error: None,
        executed_at: Utc::now(),
    }
}

fn err_result(
    seq: i32,
    tool: ToolName,
    program_id: Option<Uuid>,
    error: super::ToolError,
) -> ToolResult {
    ToolResult {
        seq,
        tool,
        permission: tool.permission(),
        program_id,
        ok: false,
        status: StepStatus::Error,
        data: json!({}),
        evidence: Vec::new(),
        error: Some(error),
        executed_at: Utc::now(),
    }
}

pub async fn cancel(app: &AppState, user: Uuid, run: Uuid) -> Result<Value, Error> {
    let mut tx = app.pool.begin().await.map_err(store::db)?;
    let row = store::run_row(&mut tx, user, run).await?;
    let status = RunStatus::parse(&row.status).unwrap_or(RunStatus::Failed);
    if status.is_terminal() {
        return Err(error(
            StatusCode::CONFLICT,
            "run_finished",
            "이미 끝난 요청이에요.",
        ));
    }
    store::expire_run_confirmations(&mut tx, run).await?;
    if !store::transition(&mut tx, run, RunStatus::Cancelled).await? {
        return Err(error(
            StatusCode::CONFLICT,
            "run_finished",
            "이미 끝난 요청이에요.",
        ));
    }
    tx.commit().await.map_err(store::db)?;
    if let Some(channel) = app.agent.channel(run) {
        channel.cancel();
        channel.emit(json!({"type":"status","status":"cancelled"}));
    }
    Ok(json!({"run_id": run, "status": "cancelled"}))
}

#[cfg(test)]
mod route_tests {
    use super::*;
    use crate::agent::classifier::label_space_knows;

    /// The shipped routing classifier's labels (`assets/intent-classifier`).
    fn shipped_labels() -> Vec<String> {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/intent-classifier/classifier.json"
        );
        let artefact: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        serde_json::from_value(artefact["labels"].clone()).unwrap()
    }

    /// PR #44 review: the label-space rule is pinned to the shipped labels.
    /// No label plans the device calendar or the page handoff, so a generic
    /// `refuse` lean on those asks is not suspicion; if the classifier ever
    /// learns them, this fails and the rule must be re-checked.
    #[test]
    fn the_shipped_labels_do_not_plan_calendar_or_the_page_handoff() {
        let labels = shipped_labels();
        let knows = |tool| label_space_knows(&labels, tool);
        assert!(knows(ToolName::SetAlert) && knows(ToolName::Bookmark));
        assert!(!knows(ToolName::AddToCalendar));
        assert!(!knows(ToolName::OpenOfficialApplication));
        assert!(labels.iter().any(|l| l == "refuse") && labels.iter().any(|l| l == "refuse_fraud"));
    }

    /// PR #44 review: another person's calendar or page request gets no
    /// card through the production route, with the shipped labels and the
    /// classifier's generic `refuse` lean (its label for those rows).
    #[test]
    fn another_persons_calendar_or_page_ask_gets_no_card() {
        let labels = shipped_labels();
        let flags = super::super::ToolFlags::ALL;
        // Path-independent (the model may read the calendar ask as a
        // request): another person in the goal keeps the refusal lean.
        let suspect = |goal: &str| {
            let mut context = GoalContext {
                has_program: true,
                selected: 0,
                saved: 3,
                suspect: false,
                handoffs: true,
            };
            route_decision(
                goal,
                &mut context,
                Some(Err(Some("refuse".into()))),
                flags.extras(),
                true,
                |tool| label_space_knows(&labels, tool),
            );
            context.suspect
        };
        assert!(!suspect("마감일 캘린더에 넣어줘"));
        assert!(!suspect("공식 신청 페이지 열어줘"));
        assert!(suspect("동생 마감일 캘린더에 넣어줘"));
        // "동생 계정 …" names another account: the guard refuses it first.
        assert!(goals::guarded("동생 계정 공식 신청 페이지 열어줘").is_some());
        assert!(goals::guarded("동생 마감일 캘린더에 넣어줘").is_none());
        let cards = |goal: &str, model_on: bool| -> Vec<ToolName> {
            let mut context = GoalContext {
                has_program: true,
                selected: 0,
                saved: 3,
                suspect: false,
                handoffs: true,
            };
            let route = route_decision(
                goal,
                &mut context,
                Some(Err(Some("refuse".into()))),
                flags.extras(),
                model_on,
                |tool| label_space_knows(&labels, tool),
            );
            let plan = match route {
                Route::Decision(Decision::Plan { kind, plan }) => {
                    executed_plan(plan, goal, kind, flags, context)
                }
                Route::Decision(Decision::Unrecognized) => {
                    match kept_model_plan(Plan { steps: Vec::new() }, goal, context, flags) {
                        Some(plan) => executed_plan(plan, goal, GoalKind::Model, flags, context),
                        None => Plan { steps: Vec::new() },
                    }
                }
                _ => Plan { steps: Vec::new() },
            };
            plan.steps
                .iter()
                .map(ToolCall::name)
                .filter(|t| t.is_write())
                .collect()
        };
        // Control: the same harness sees the user's own calendar ask.
        for model_on in [true, false] {
            assert_eq!(
                cards("마감일 캘린더에 넣어줘", model_on),
                vec![ToolName::AddToCalendar],
                "model {model_on}"
            );
        }
        for goal in [
            "동생 마감일 캘린더에 넣어줘",
            "동생 계정 공식 신청 페이지 열어줘",
        ] {
            for model_on in [true, false] {
                assert!(
                    cards(goal, model_on).is_empty(),
                    "{goal} (model {model_on})"
                );
            }
        }
    }
}
