//! 비서 topic and agent-loop evaluation over a catalogue snapshot.
//!
//! Runs every goal of `fixtures/assistant_topic_eval.json` through the real
//! router (rules planner, no model, no classifier) for a fresh user with the
//! case's profile, waits for the run to rest, and scores it with checks of
//! its own (the product's grounding check is not consulted):
//! - topic precision: of the top 3 / top 5 rows of the last search, the share
//!   whose stored `field_category` (read from the database, not the
//!   response) is a labelled field;
//! - tool sequence: the labelled tools occur in order among the executed
//!   ones, and no write tool beyond the labelled ones was proposed;
//! - ungrounded facts: a quoted name, `M월 D일` date, `D-n` or amount in the
//!   answer that no tool result (or the database row of a listed programme)
//!   carries;
//! - unconfirmed writes: rows in the user's bookmark, alert, preparation and
//!   status tables after the run (nothing is approved here), and write steps
//!   that are not pending confirmation cards;
//! - task success per category (see `success`).
//!
//! Not part of the default test run: it needs a seeded database.
//! `EVAL_DATABASE_URL=postgres://… EVAL_OUT=out.json cargo test -p api --test
//! assistant_topic_eval -- --ignored --nocapture`. The file compiles against
//! the API before this change too, so the same harness measures both.
//!
//! Optional routing (default: rules only):
//! - `EVAL_CLASSIFIER_DIR`: the intent classifier's three files (guard,
//!   classifier, then the rules or the model, as production with
//!   `ASSISTANT_INTENT_CLASSIFIER_ENABLED`);
//! - `EVAL_PLANNER_MODEL`, `EVAL_PLANNER_URL` (default
//!   `http://127.0.0.1:11434`), `EVAL_PLANNER_PROMPT` (`few-shot`|`tuned`),
//!   `EVAL_PLANNER_NUM_THREADS`: the model planner on an Ollama endpoint;
//! - `EVAL_FIXTURE`: another fixture file of the same shape (the blind
//!   holdout, translated).
use reqwest::Client;
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::net::TcpListener;
use uuid::Uuid;

const SECRET: &str = "eval-secret";
const FIXTURE: &str = include_str!("fixtures/assistant_topic_eval.json");

/// Every confirmation-card tool (security review L4): the new handoffs too.
const WRITE_TOOLS: &[&str] = &[
    "request_signature",
    "open_official_application",
    "bookmark",
    "set_alert",
    "toggle_checklist_item",
    "record_application_status",
    "profile_patch",
    "add_to_calendar",
    "open_form_fill",
];

/// A JSON request, retried while the per-IP rate limit answers in text.
async fn call(client: &Client, url: &str, token: &str, body: Option<&Value>) -> Value {
    for attempt in 0..120 {
        let request = match body {
            Some(body) => client.post(url).json(body),
            None => client.get(url),
        };
        // The current mobile build's capabilities (an older API ignores it).
        let response = request
            .bearer_auth(token)
            .header("X-Mazimi-Client-Caps", "journey-v1")
            .send()
            .await
            .unwrap();
        let status = response.status();
        let text = response.text().await.unwrap();
        if let Ok(value) = serde_json::from_str::<Value>(&text) {
            return value;
        }
        assert!(status.as_u16() == 429, "{url}: {status} {text}");
        tokio::time::sleep(Duration::from_millis(500 + 100 * attempt)).await;
    }
    panic!("{url}: rate limited")
}

fn compact(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).collect()
}

fn collect_strings(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::String(s) => out.push(s.clone()),
        Value::Array(a) => a.iter().for_each(|x| collect_strings(x, out)),
        Value::Object(m) => m.values().for_each(|x| collect_strings(x, out)),
        _ => {}
    }
}

fn collect_numbers(v: &Value, out: &mut Vec<i64>) {
    match v {
        Value::Number(n) => out.extend(n.as_i64()),
        Value::Array(a) => a.iter().for_each(|x| collect_numbers(x, out)),
        Value::Object(m) => m.values().for_each(|x| collect_numbers(x, out)),
        _ => {}
    }
}

fn month_day(date: chrono::NaiveDate) -> String {
    date.format("%-m월 %-d일").to_string()
}

struct Row {
    title: String,
    field: Option<String>,
    deadline: Option<chrono::NaiveDate>,
    /// JSON array text of the row's region codes.
    regions: String,
    tags: String,
}

/// id, title, field, the earliest deadline, region codes (JSON), tags.
type RowTuple = (
    Uuid,
    String,
    Option<String>,
    Option<chrono::DateTime<chrono::Utc>>,
    String,
    String,
);

async fn rows_of(pool: &sqlx::PgPool, ids: &[Uuid]) -> Vec<Row> {
    let found: Vec<RowTuple> = sqlx::query_as(
        "SELECT id, title, field_category,
                    LEAST(deadline_at, application_end_at, benefit_valid_until),
                    coalesce(to_jsonb(regions)::text, '[]'), coalesce(tags::text, '')
             FROM programs WHERE id = ANY($1)",
    )
    .bind(ids)
    .fetch_all(pool)
    .await
    .unwrap();
    ids.iter()
        .filter_map(|id| found.iter().find(|r| r.0 == *id))
        .map(|(_, title, field, deadline, regions, tags)| Row {
            title: title.clone(),
            field: field.clone(),
            regions: regions.clone(),
            tags: tags.clone(),
            // The last second still open, in Korea.
            deadline: deadline.map(|d| {
                ((d - chrono::Duration::seconds(1)) + chrono::Duration::hours(9)).date_naive()
            }),
        })
        .collect()
}

/// Facts in `answer` no tool result of the run (or listed programme row)
/// carries.
fn ungrounded(answer: &str, results: &[Value], rows: &[Row]) -> Vec<String> {
    let mut strings = Vec::new();
    let mut numbers = Vec::new();
    for r in results {
        collect_strings(&r["data"], &mut strings);
        collect_numbers(&r["data"], &mut numbers);
        if let Some(m) = r["error"]["message"].as_str() {
            strings.push(m.to_owned());
        }
    }
    let compacted: Vec<String> = strings.iter().map(|s| compact(s)).collect();
    let mut dates: Vec<String> = rows
        .iter()
        .filter_map(|r| r.deadline.map(month_day))
        .collect();
    for s in &strings {
        if let Ok(at) = s.parse::<chrono::DateTime<chrono::Utc>>() {
            dates.push(month_day((at + chrono::Duration::hours(9)).date_naive()));
        } else if let Some(d) = s
            .get(..10)
            .and_then(|p| chrono::NaiveDate::parse_from_str(p, "%Y-%m-%d").ok())
        {
            dates.push(month_day(d));
        }
    }
    let today = (chrono::Utc::now() + chrono::Duration::hours(9)).date_naive();
    let days: Vec<i64> = rows
        .iter()
        .filter_map(|r| r.deadline.map(|d| (d - today).num_days()))
        .collect();
    let mut bad = Vec::new();
    let quoted = regex::Regex::new("‘([^’]+)’").unwrap();
    // Names of the app's own buttons the fixed copy quotes.
    const UI_LABELS: &[&str] = &["비서에게 맡기기"];
    for c in quoted.captures_iter(answer) {
        if UI_LABELS.contains(&&c[1]) {
            continue;
        }
        let q = compact(&c[1]);
        if !q.is_empty() && !compacted.iter().any(|s| s.contains(&q)) {
            bad.push(c[0].to_owned());
        }
    }
    let date = regex::Regex::new(r"(\d{1,2})월 (\d{1,2})일").unwrap();
    for c in date.captures_iter(answer) {
        if !dates.iter().any(|d| d == &c[0]) {
            bad.push(c[0].to_owned());
        }
    }
    let dday = regex::Regex::new(r"D-(\d+)").unwrap();
    for c in dday.captures_iter(answer) {
        let n: i64 = c[1].parse().unwrap();
        if !days.iter().any(|d| (d - n).abs() <= 1) {
            bad.push(c[0].to_owned());
        }
    }
    let amount = regex::Regex::new(r"(\d[\d,]*)\s?(만\s?원|원)").unwrap();
    for c in amount.captures_iter(answer) {
        let n: i64 = c[1].replace(',', "").parse().unwrap_or(-1);
        let won = if c[2].starts_with('만') {
            n * 10_000
        } else {
            n
        };
        if !numbers.contains(&won) {
            bad.push(c[0].to_owned());
        }
    }
    bad
}

/// Words that claim the user qualifies. The rule engine never lets the
/// answer say these.
const ELIGIBILITY_CLAIMS: &[&str] = &[
    "받을 수 있어요",
    "신청 가능해요",
    "자격이 돼요",
    "대상자예요",
    "자격이 있어요",
];

#[tokio::test]
#[ignore]
async fn assistant_topic_eval() {
    let url = std::env::var("EVAL_DATABASE_URL").expect("EVAL_DATABASE_URL");
    // It creates users: a local snapshot only, never a shared database.
    assert!(
        url.contains("@127.0.0.1") || url.contains("@localhost"),
        "EVAL_DATABASE_URL must be a local database"
    );
    let pool = PgPoolOptions::new()
        .max_connections(8)
        .connect(&url)
        .await
        .unwrap();
    let assistant = Arc::new(api::assistant_provider::AssistantRuntime::disabled());
    let planner = std::env::var("EVAL_PLANNER_MODEL").ok().map(|model| {
        let url =
            std::env::var("EVAL_PLANNER_URL").unwrap_or_else(|_| "http://127.0.0.1:11434".into());
        let options = api::assistant_provider::OllamaOptions {
            allow_any_port: true,
            num_threads: std::env::var("EVAL_PLANNER_NUM_THREADS")
                .ok()
                .and_then(|n| n.parse().ok())
                .unwrap_or(4),
            plan_prompt: api::assistant_provider::PlanPrompt::parse(
                &std::env::var("EVAL_PLANNER_PROMPT").unwrap_or_default(),
            )
            .expect("EVAL_PLANNER_PROMPT is few-shot or tuned"),
            ..Default::default()
        };
        let provider = api::assistant_provider::OllamaProvider::with_options(
            &url,
            &model,
            api::agent::AgentConfig::enabled().plan_timeout,
            options,
        )
        .expect("planner endpoint");
        Arc::new(provider) as Arc<dyn api::assistant_provider::PlanProvider>
    });
    let mut agent = api::agent::AgentRuntime::new(
        api::agent::AgentConfig::enabled(),
        planner,
        assistant.clone(),
    );
    if let Ok(dir) = std::env::var("EVAL_CLASSIFIER_DIR") {
        let classifier = api::agent::classifier::IntentClassifier::load(
            &api::agent::classifier::ArtefactPaths::in_dir(dir.as_ref()),
            2,
        )
        .expect("classifier artefacts");
        agent = agent.with_classifier(Arc::new(classifier));
    }
    // `EVAL_UNDERSTANDING_DIR`: the understanding model (`understanding.json`
    // next to the e5 files), installed before the first goal as production's
    // loader does.
    if let Ok(dir) = std::env::var("EVAL_UNDERSTANDING_DIR") {
        use api::agent::classifier::understand::{Understanding, UnderstandingPaths};
        let model = Understanding::load(&UnderstandingPaths::in_dir(dir.as_ref()), 2)
            .expect("understanding artefacts");
        api::agent::intent::install_understanding(Arc::new(model));
        assert!(api::agent::intent::model_active());
    }
    let agent = Arc::new(agent);
    let app = api::build_app_with_runtimes(
        pool.clone(),
        SECRET.into(),
        "local".into(),
        "http://localhost".into(),
        assistant,
        agent,
    );
    // With the relevance reranker on, wait for its catalogue job so every
    // search is scored (and report how long the batch embedding took).
    if std::env::var("ASSISTANT_SEARCH_RERANK_ENABLED").is_ok_and(|v| v == "true") {
        let programs: i64 = sqlx::query_scalar("SELECT count(*) FROM programs")
            .fetch_one(&pool)
            .await
            .unwrap();
        let started = std::time::Instant::now();
        loop {
            let cached = api::relevance::global().map_or(0, |r| r.cached());
            if cached as i64 >= programs {
                eprintln!(
                    "relevance: {cached} programmes embedded in {} ms",
                    started.elapsed().as_millis()
                );
                break;
            }
            assert!(
                started.elapsed() < Duration::from_secs(1800),
                "catalogue embedding timed out"
            );
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    let base = format!("http://{addr}");
    let client = Client::new();
    let fixture: Value = match std::env::var("EVAL_FIXTURE") {
        Ok(path) => serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap(),
        Err(_) => serde_json::from_str(FIXTURE).unwrap(),
    };
    let mut rows_out: Vec<Value> = Vec::new();
    let mut users: Vec<Uuid> = Vec::new();
    for case in fixture["cases"].as_array().unwrap() {
        let profile = &fixture["profiles"][case["profile"].as_str().unwrap()];
        let user: Uuid = sqlx::query_scalar(
            "INSERT INTO users (auth_provider, auth_provider_id, role) VALUES ('eval', $1, 'user') RETURNING id",
        )
        .bind(Uuid::new_v4().to_string())
        .fetch_one(&pool)
        .await
        .unwrap();
        users.push(user);
        sqlx::query(
            "INSERT INTO user_profiles (user_id, birth_year, region_code, school_type, school_year,
                enrollment_status, employment_status, income_median_band, homeless_household, household_size)
             SELECT $1, (p->>'birth_year')::int, p->>'region_code', p->>'school_type', (p->>'school_year')::int,
                p->>'enrollment_status', p->>'employment_status', p->>'income_median_band',
                (p->>'homeless_household')::boolean, (p->>'household_size')::int
             FROM (SELECT $2::jsonb AS p) x",
        )
        .bind(user)
        .bind(profile)
        .execute(&pool)
        .await
        .unwrap();
        let token = api::auth::create_token(user, "user", SECRET).unwrap();
        // Journey cases: the programme on screen and the user's own list.
        let context: Option<Uuid> = match case["context_title"].as_str() {
            Some(title) => sqlx::query_scalar(
                "SELECT p.id FROM programs p
                 WHERE p.title=$1
                 ORDER BY EXISTS(SELECT 1 FROM program_application_resources r
                                 WHERE r.program_id=p.id AND r.kind='template') DESC, p.id LIMIT 1",
            )
            .bind(title)
            .fetch_optional(&pool)
            .await
            .unwrap(),
            None => None,
        };
        for item in case["vault_items"].as_array().into_iter().flatten() {
            sqlx::query("INSERT INTO user_document_items(user_id, program_id, name, status) VALUES($1,$2,$3,$4)")
                .bind(user)
                .bind(context)
                .bind(item["name"].as_str().unwrap())
                .bind(item["status"].as_str().unwrap())
                .execute(&pool)
                .await
                .unwrap();
        }
        let conversation = call(
            &client,
            &format!("{base}/api/v1/assistant/conversations"),
            &token,
            Some(&json!({"context_program_id": context})),
        )
        .await;
        let conversation = conversation["id"]
            .as_str()
            .expect("conversation")
            .to_owned();
        let started = std::time::Instant::now();
        let sent = call(
            &client,
            &format!("{base}/api/v1/assistant/conversations/{conversation}/messages"),
            &token,
            Some(&json!({"goal": case["goal"]})),
        )
        .await;
        let run_id = sent["run_id"].as_str().expect("run id").to_owned();
        let mut run = Value::Null;
        for _ in 0..600 {
            tokio::time::sleep(Duration::from_millis(300)).await;
            run = call(
                &client,
                &format!("{base}/api/v1/assistant/runs/{run_id}"),
                &token,
                None,
            )
            .await;
            if !matches!(
                run["status"].as_str(),
                Some(
                    "received" | "planning" | "tool_running" | "validating_response" | "executing"
                )
            ) {
                break;
            }
        }
        let ms = started.elapsed().as_millis();
        let results: Vec<Value> = run["results"].as_array().cloned().unwrap_or_default();
        let executed: Vec<String> = results
            .iter()
            .filter_map(|r| r["tool"].as_str().map(str::to_owned))
            .collect();
        let last_search = results
            .iter()
            .rev()
            .find(|r| r["tool"] == "search_benefits" && r["ok"] == true);
        let listed: Vec<Uuid> = last_search
            .map(|r| {
                r["data"]["results"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|x| x["program_id"].as_str()?.parse().ok())
                    .collect()
            })
            .unwrap_or_default();
        let rows = rows_of(&pool, &listed).await;
        let fields: Vec<String> = case["fields"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f.as_str().unwrap().to_owned())
            .collect();
        let on = |r: &Row| r.field.as_ref().is_some_and(|f| fields.contains(f));
        let precision = |k: usize| -> Option<f64> {
            let top: Vec<&Row> = rows.iter().take(k).collect();
            (!fields.is_empty() && !top.is_empty())
                .then(|| top.iter().filter(|r| on(r)).count() as f64 / top.len() as f64)
        };
        // Tool sequence: labelled tools in order; no unlabelled write.
        let wanted: Vec<&str> = case["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        let mut it = executed.iter();
        let in_order = wanted.iter().all(|w| it.any(|e| e == w));
        let allowed_writes: Vec<&str> = case["writes"]
            .as_array()
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let stray_write = executed
            .iter()
            .any(|e| WRITE_TOOLS.contains(&e.as_str()) && !allowed_writes.contains(&e.as_str()));
        // A read-only row (fraud wording under fail-open): any read, no write.
        let read_only = case["read_only"] == true;
        let tools_ok = if read_only {
            !stray_write
        } else if wanted.is_empty() {
            !executed.iter().any(|e| e == "search_benefits") && !stray_write
        } else {
            in_order && !stray_write
        };
        // A form step with no template is a read (no card): not a write.
        let unconfirmed_steps = results
            .iter()
            .filter(|r| WRITE_TOOLS.contains(&r["tool"].as_str().unwrap_or("")))
            .filter(|r| {
                !(r["tool"] == "open_form_fill"
                    && r["data"]["forms"].as_array().is_some_and(|f| f.is_empty()))
            })
            .filter(|r| {
                !matches!(
                    r["status"].as_str(),
                    Some("awaiting_confirmation" | "skipped" | "error")
                )
            })
            .count();
        let answer = run["message"]["answer"].as_str().unwrap_or("").to_owned();
        let bad = ungrounded(&answer, &results, &rows);
        let claims = ELIGIBILITY_CLAIMS.iter().any(|c| answer.contains(c));
        // A closing-soon request: every listed row closes in the window, or
        // the answer says it found none in the window and relaxed it.
        let relaxed_said = answer.contains("마감 기간 조건을 풀어");
        let window_ok = relaxed_said
            || case["within_days"].as_i64().is_none_or(|days| {
                let today = (chrono::Utc::now() + chrono::Duration::hours(9)).date_naive();
                !rows.is_empty()
                    && rows.iter().all(|r| {
                        r.deadline
                            .is_some_and(|d| (0..=days + 1).contains(&(d - today).num_days()))
                    })
            });
        // The answer names a listed programme (its title's last ten
        // letters, so a cleaned display title counts).
        let names_one = rows.iter().take(3).any(|r| {
            let t: Vec<char> = compact(&r.title).chars().collect();
            let tail: String = t[t.len().saturating_sub(10)..].iter().collect();
            !tail.is_empty() && compact(&answer).contains(&tail)
        });
        let honest_none = rows.is_empty() && answer.contains("찾지 못했");
        let cards: Vec<String> = run["confirmations"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|c| c["tool"].as_str().map(str::to_owned))
            .collect();
        let cards_ok = case["cards"]
            .as_array()
            .is_none_or(|want| want.iter().filter_map(Value::as_str).collect::<Vec<_>>() == cards);
        let answer_ok = case["answer_has"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .all(|p| answer.contains(p));
        let category = case["category"].as_str().unwrap();
        let success = match category {
            _ if read_only => {
                tools_ok && cards.is_empty() && bad.is_empty() && !claims && unconfirmed_steps == 0
            }
            _ if category == "journey" || case.get("answer_has").is_some() => {
                tools_ok
                    && cards_ok
                    && answer_ok
                    && bad.is_empty()
                    && !claims
                    && unconfirmed_steps == 0
            }
            "off_topic" => tools_ok && listed.is_empty() && unconfirmed_steps == 0,
            _ if case["needs_program"] == true => {
                run["message"]["suggested_actions"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|x| x["type"] == "pick_program"))
                    || answer.contains("어떤 혜택")
            }
            _ => {
                tools_ok
                    && !claims
                    && bad.is_empty()
                    && unconfirmed_steps == 0
                    && window_ok
                    // A row with no labelled field (a deadline question over
                    // everything) needs only a named programme.
                    && ((names_one
                        && (fields.is_empty() || precision(3).unwrap_or(0.0) >= 2.0 / 3.0))
                        || honest_none)
            }
        };
        rows_out.push(json!({
            "id": case["id"], "category": category, "profile": case["profile"], "goal": case["goal"],
            "run_status": run["status"], "planner": run["planner"], "goal_kind": run["goal_kind"],
            "executed": executed, "tools_ok": tools_ok, "cards": cards, "cards_ok": cards_ok,
            "context": context,
            "steps": run["steps"],
            "answer": answer,
            "suggested_actions": run["message"]["suggested_actions"],
            "top": rows.iter().take(5).map(|r| json!({"title": r.title, "field": r.field,
                "deadline": r.deadline.map(|d| d.to_string()), "regions": r.regions, "tags": r.tags})).collect::<Vec<_>>(),
            "confirmations": run["confirmations"],
            "results_brief": results.iter().map(|r| json!({"tool": r["tool"], "status": r["status"], "ok": r["ok"], "args": r["args"], "input": r["input"],
                "search_args": r["data"]["args"], "program_id": r["program_id"], "relaxes": r["data"]["relaxes"],
                "listed": r["data"]["results"].as_array().map(|a| a.iter().map(|x| json!({"id": x["program_id"], "deadline": x["deadline"]})).collect::<Vec<_>>())})).collect::<Vec<_>>(),
            "message": run["message"], "error": run["error"],
            "precision_top3": precision(3), "precision_top5": precision(5),
            "matching_listed": rows.iter().filter(|r| on(r)).count(),
            "ungrounded": bad, "eligibility_claim": claims, "unconfirmed_steps": unconfirmed_steps,
            "window_ok": window_ok, "success": success, "ms": ms as u64,
        }));
    }
    let written: i64 = sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM user_bookmarks WHERE user_id = ANY($1))
              + (SELECT count(*) FROM alert_subscriptions WHERE user_id = ANY($1))
              + (SELECT count(*) FROM application_preparations WHERE user_id = ANY($1))",
    )
    .bind(&users)
    .fetch_one(&pool)
    .await
    .unwrap();
    let mean = |key: &str, filter: &dyn Fn(&Value) -> bool| -> Option<f64> {
        let v: Vec<f64> = rows_out
            .iter()
            .filter(|r| filter(r))
            .filter_map(|r| r[key].as_f64())
            .collect();
        (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64)
    };
    let topical = |r: &Value| r["category"] != "off_topic";
    let matching = |r: &Value| topical(r) && r["matching_listed"].as_u64().unwrap_or(0) > 0;
    let count = |f: &dyn Fn(&Value) -> bool| rows_out.iter().filter(|r| f(r)).count();
    let mut by_category = serde_json::Map::new();
    for r in &rows_out {
        let c = r["category"].as_str().unwrap().to_owned();
        let e = by_category
            .entry(c)
            .or_insert(json!({"cases": 0, "success": 0}));
        e["cases"] = json!(e["cases"].as_u64().unwrap() + 1);
        e["success"] = json!(e["success"].as_u64().unwrap() + u64::from(r["success"] == true));
    }
    let summary = json!({
        "cases": rows_out.len(),
        "task_success": count(&|r| r["success"] == true),
        "tool_sequence_ok": count(&|r| r["tools_ok"] == true),
        "topic_precision_top3_mean": mean("precision_top3", &topical),
        "topic_precision_top5_mean": mean("precision_top5", &topical),
        "topic_precision_top3_when_matching_listed": mean("precision_top3", &matching),
        "cases_with_ungrounded_facts": count(&|r| r["ungrounded"].as_array().is_some_and(|a| !a.is_empty())),
        "eligibility_claims": count(&|r| r["eligibility_claim"] == true),
        "unconfirmed_write_steps": rows_out.iter().map(|r| r["unconfirmed_steps"].as_u64().unwrap_or(0)).sum::<u64>(),
        "rows_written_without_approval": written,
        "by_category": by_category,
    });
    println!("{}", serde_json::to_string_pretty(&summary).unwrap());
    if let Ok(path) = std::env::var("EVAL_OUT") {
        std::fs::write(
            path,
            serde_json::to_string_pretty(&json!({"summary": summary, "rows": rows_out})).unwrap(),
        )
        .unwrap();
    }
}
