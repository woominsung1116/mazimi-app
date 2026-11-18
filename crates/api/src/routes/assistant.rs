use crate::services::application_preparation::Error;
use crate::{
    assistant_provider::{redact_question, AnswerMode, AnswerRequest, Refusal, Topic},
    auth::AuthUser,
    services::{
        benefit_assistant::{self, Asked},
        grounded_qa::validate::{self, Grounding, FALLBACK_TEXT},
    },
    AppState,
};
use axum::{
    body::{Body, Bytes},
    extract::{Path, Query, State},
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use uuid::Uuid;

pub async fn private_response(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("private, no-store"),
    );
    response
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Question {
    question: String,
    /// Required (`true`) by the generation stream; ignored by `ask`, which
    /// never calls a model.
    #[serde(default)]
    allow_ai_processing: bool,
    /// The previous answer's `follow_up_topic`, for follow-ups without a
    /// topic of their own. The client keeps it; nothing is stored.
    #[serde(default)]
    previous_topic: Option<Topic>,
}

fn error(status: StatusCode, code: &str, message: &str) -> Error {
    (status, Json(json!({"error": message, "code": code})))
}

fn valid_question(question: &str) -> Result<&str, Error> {
    let question = question.trim();
    if question.is_empty()
        || question.chars().count() > 500
        || question
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"개인정보를 제외하고 질문을 1~500자로 입력해 주세요."})),
        ));
    }
    Ok(question)
}

fn admit(state: &AppState, user: Uuid) -> Result<(), Error> {
    if state.assistant.admit_question(user) {
        Ok(())
    } else {
        Err((
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({"error":"질문이 너무 많아요. 1분 뒤에 다시 시도해 주세요."})),
        ))
    }
}

pub async fn overview(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<benefit_assistant::Guidance>, Error> {
    benefit_assistant::guidance(
        &state.pool,
        user.id,
        id,
        &[Topic::Summary, Topic::Eligibility, Topic::Steps],
        AnswerMode::Rules,
        state.assistant.ai_available(),
        None,
    )
    .await
    .map(Json)
}

/// Grounded answer: quoted passages, verdict or not-found, from stored data
/// only. Deterministic and immediate; no model is called here.
pub async fn ask(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(input): Json<Question>,
) -> Result<Json<benefit_assistant::Guidance>, Error> {
    let question = valid_question(&input.question)?;
    admit(&state, user.id)?;
    let (topics, _) = crate::services::grounded_qa::topics_for(question, input.previous_topic);
    benefit_assistant::guidance(
        &state.pool,
        user.id,
        id,
        &topics,
        AnswerMode::Rules,
        state.assistant.ai_available(),
        Some(Asked {
            question,
            previous_topic: input.previous_topic,
        }),
    )
    .await
    .map(Json)
}

fn line(event: Value) -> Bytes {
    let mut text = event.to_string();
    text.push('\n');
    Bytes::from(text)
}

/// Optional generated summary of the grounded answer, streamed as JSON lines
/// (`application/x-ndjson`): `start`, then `delta {text: ""}` while the model
/// writes (progress only: no model text leaves before validation), then one
/// `final {valid, text}`. The final text is the model's only when it passes
/// the grounding check; otherwise it is [`FALLBACK_TEXT`] and the client
/// keeps the quotes.
///
/// FEATURE OFF (`ASSISTANT_AI_ENABLED`, default off). The guard
/// (`grounded_qa::validate`) is a frozen design with known open classes:
/// fragment passages are quotable, noun-final lines and list items read as
/// statements, and a true but misleading quote passes. Do not enable this
/// until the passage builder marks whole sentences with their heading, or
/// the summary becomes a selection of passage ids the server renders.
///
/// Refused before streaming: no consent 400, AI off or breaker open 503
/// `ai_unavailable` (the body is sanitized outside local; clients key on
/// the status), slot busy 429 `busy`, per-user limits 429 (`too_soon` for
/// spacing), nothing to summarize 409 `no_passages`.
pub async fn answer_stream(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(input): Json<Question>,
) -> Result<Response, Error> {
    let question = valid_question(&input.question)?;
    if !input.allow_ai_processing {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "consent_required",
            "AI 요약은 질문 처리에 동의한 경우에만 만들어요.",
        ));
    }
    let unavailable = || {
        error(
            StatusCode::SERVICE_UNAVAILABLE,
            "ai_unavailable",
            "지금은 AI 요약을 쓸 수 없어요. 근거 문장을 확인해 주세요.",
        )
    };
    if !state.assistant.ai_available() {
        return Err(unavailable());
    }
    admit(&state, user.id)?;
    let (topics, _) = crate::services::grounded_qa::topics_for(question, input.previous_topic);
    // Re-retrieve on the server: the model only sees what this request
    // retrieves, never passages sent by the client.
    let guidance = benefit_assistant::guidance(
        &state.pool,
        user.id,
        id,
        &topics,
        AnswerMode::Rules,
        true,
        Some(Asked {
            question,
            previous_topic: input.previous_topic,
        }),
    )
    .await?;
    let Some(answer) = guidance
        .answer
        .filter(|a| !a.unsupported && !a.passages.is_empty())
    else {
        return Err(error(
            StatusCode::CONFLICT,
            "no_passages",
            "요약할 근거 문장이 없어요.",
        ));
    };
    let generation =
        state
            .assistant
            .start_generation(user.id)
            .map_err(|refusal| match refusal {
                // 4xx: the 5xx body sanitizer would erase the code outside local.
                Refusal::Busy => error(
                    StatusCode::TOO_MANY_REQUESTS,
                    "busy",
                    "지금은 요약이 붐벼요. 근거 문장을 먼저 확인해 주세요.",
                ),
                Refusal::TooSoon => error(
                    StatusCode::TOO_MANY_REQUESTS,
                    "too_soon",
                    "AI 요약은 10초에 한 번 만들 수 있어요.",
                ),
                Refusal::Disabled | Refusal::Unavailable => unavailable(),
            })?;
    let passages: Vec<String> = answer.passages.iter().map(|p| p.text.clone()).collect();
    // The verdict summary is the eligibility sentence: the answer must
    // carry it unchanged, and every other sentence must be a passage's
    // (`validate`, default-deny).
    let verdict_text = answer.verdict.as_ref().map(|v| v.summary.clone());
    let request = AnswerRequest {
        question: redact_question(question),
        verdict: verdict_text.clone(),
        passages: passages.clone(),
    };
    let (out, mut lines) = mpsc::channel::<Bytes>(32);
    tokio::spawn(async move {
        let (deltas, mut incoming) = mpsc::channel::<String>(32);
        if out.send(line(json!({"type":"start"}))).await.is_err() {
            return;
        }
        let forward = {
            let out = out.clone();
            async move {
                // Progress only: the model's text reaches the client with
                // `final`, after the guard (round-4 review, security H3).
                while incoming.recv().await.is_some() {
                    if out
                        .send(line(json!({"type":"delta","text":""})))
                        .await
                        .is_err()
                    {
                        // Client left: dropping `incoming` cancels the model.
                        return;
                    }
                }
            }
        };
        // The programme title is a heading, never an answer (round-4 review).
        let check = |text: &str| {
            let sources: Vec<&str> = passages.iter().map(String::as_str).collect();
            validate::validate(
                text,
                &Grounding {
                    sources,
                    verdict: verdict_text.as_deref(),
                },
            )
        };
        let work = async { tokio::join!(generation.run(&request, deltas, check), forward) };
        // A closed client cancels at once, not only at the next token:
        // dropping `work` drops the model request and the generation slot.
        let (result, ()) = tokio::select! {
            done = work => done,
            () = out.closed() => return,
        };
        let final_event = match result {
            Ok(Ok(text)) => json!({"type":"final","valid":true,"text":text.trim()}),
            Ok(Err(violation)) => {
                // Logged, not sent: the client needs no hint which check to evade.
                tracing::info!(?violation, "Generated answer failed the grounding check");
                json!({"type":"final","valid":false,"text":FALLBACK_TEXT})
            }
            Err(crate::assistant_provider::ProviderError::Cancelled) => return,
            Err(_) => {
                json!({"type":"final","valid":false,"text":FALLBACK_TEXT,"error":"generation_failed"})
            }
        };
        let _ = out.send(line(final_event)).await;
    });
    let body = Body::from_stream(futures_util::stream::poll_fn(move |cx| {
        lines
            .poll_recv(cx)
            .map(|next| next.map(Ok::<_, std::convert::Infallible>))
    }));
    let mut response = body.into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/x-ndjson; charset=utf-8"),
    );
    response
        .headers_mut()
        .insert("x-accel-buffering", HeaderValue::from_static("no"));
    Ok(response)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchQuestion {
    question: String,
}

/// Cross-programme search from one question. Rules only.
pub async fn search(
    user: AuthUser,
    State(state): State<AppState>,
    Json(input): Json<SearchQuestion>,
) -> Result<Json<benefit_assistant::search::Search>, Error> {
    let question = valid_question(&input.question)?;
    if !state.assistant.admit_search(user.id) {
        return Err((
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({"error":"검색이 너무 많아요. 1분 뒤에 다시 시도해 주세요."})),
        ));
    }
    benefit_assistant::search::search(&state.pool, user.id, question)
        .await
        .map(Json)
}

pub async fn briefing(
    user: AuthUser,
    State(state): State<AppState>,
) -> Result<Json<benefit_assistant::Briefing>, Error> {
    benefit_assistant::briefing(&state.pool, user.id)
        .await
        .map(Json)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundleQuery {
    scenario: benefit_assistant::bundles::Scenario,
}
pub async fn bundle(
    user: AuthUser,
    State(state): State<AppState>,
    Query(input): Query<BundleQuery>,
) -> Result<Json<benefit_assistant::bundles::Bundle>, Error> {
    benefit_assistant::bundles::build(&state.pool, user.id, input.scenario)
        .await
        .map(Json)
}

pub async fn compare(
    user: AuthUser,
    State(state): State<AppState>,
    Json(input): Json<super::stack_check::StackCheckRequest>,
) -> Result<Json<crate::services::benefit_comparison::Comparison>, Error> {
    crate::services::benefit_comparison::compare(&state.pool, user.id, &input.program_ids)
        .await
        .map(Json)
}
