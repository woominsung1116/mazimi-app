//! Grounded Q&A: quoted passages and verdicts on POST /assistant, the
//! optional generation stream, and cross-programme search.
use super::*;
use api::assistant_provider::{AnswerProvider, AnswerRequest, AssistantRuntime, ProviderError};
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, Notify};

const DOCUMENTS: &str = "신분증, 주민등록 등본";

async fn collected(pool: &sqlx::PgPool) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO programs(id,program_type,source_type,source_id,title,summary,program_status,regions,
             official_url,application_method,submission_documents,raw_payload,normalized_payload,deadline_at,
             is_active,last_synced_at)
         VALUES($1,'policy','youth_center',$2,'근거 질의 테스트 공고','부산광역시 거주 청년에게 월세를 지원합니다.','active',
             ARRAY['busan'],'https://www.youthcenter.go.kr/x','청년포털 누리집에서 온라인으로 신청합니다.',$3,$4,$5,
             now()+interval '10 days',true,now())",
    )
    .bind(id)
    .bind(format!("grounded-qa-{id}"))
    .bind(DOCUMENTS)
    .bind(json!({"sprtTrgtCn": "소득분위 8분위 이하 무주택 청년"}))
    .bind(json!({"enrollment_statuses": ["enrolled"]}))
    .execute(pool)
    .await
    .unwrap();
    id
}

async fn serve(pool: &sqlx::PgPool, runtime: Arc<AssistantRuntime>) -> String {
    let app = api::build_app_with_assistant_runtime(
        pool.clone(),
        JWT_SECRET.into(),
        "test".into(),
        "https://example.org".into(),
        runtime,
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    format!("http://{addr}")
}

async fn ask(base: &str, token: &str, id: Uuid, body: Value) -> (u16, Value) {
    let response = Client::new()
        .post(format!("{base}/api/v1/programs/{id}/assistant"))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    (status, response.json().await.unwrap_or(Value::Null))
}

async fn cleanup(pool: &sqlx::PgPool, users: &[Uuid], programs: &[Uuid]) {
    sqlx::query("DELETE FROM users WHERE id=ANY($1)")
        .bind(users)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM programs WHERE id=ANY($1)")
        .bind(programs)
        .execute(pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn grounded_answer_quotes_passages_verdicts_and_stays_additive() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let (base, pool, user, token) = setup().await;
    let id = collected(&pool).await;
    profile(&pool, user, None).await;

    let overview = guidance(&base, &token, id).await;
    assert!(overview.get("answer").is_none(), "overview stays v1-shaped");

    let (status, body) = ask(&base, &token, id, json!({"question":"필요한 서류는?"})).await;
    assert_eq!(status, 200, "{body}");
    // v1 fields are unchanged; `answer` is added.
    assert_eq!(body["mode"], "rules");
    assert_eq!(body["sections"][0]["topic"], "documents");
    assert!(body["eligibility"]["checks"].is_array());
    let answer = &body["answer"];
    assert_eq!(answer["not_found"], false);
    assert_eq!(answer["follow_up_topic"], "documents");
    let passage = &answer["passages"][0];
    assert_eq!(passage["id"], "documents.0");
    assert_eq!(passage["text"], DOCUMENTS);
    assert_eq!(passage["kind"], "official_text");
    assert_eq!(passage["source_label"], "온통청년 공고 원문 · 제출서류");
    assert!(passage["captured_at"].is_string());
    assert!(!passage["highlights"].as_array().unwrap().is_empty());

    let (_, body) = ask(
        &base,
        &token,
        id,
        json!({"question":"외국인도 신청할 수 있어?"}),
    )
    .await;
    assert_eq!(body["answer"]["not_found"], true);
    assert_eq!(body["answer"]["passages"], json!([]));
    assert_eq!(
        body["source"]["official_url"],
        "https://www.youthcenter.go.kr/x"
    );

    // The verdict is the structural check the eligibility card shows.
    let (_, body) = ask(&base, &token, id, json!({"question":"휴학생도 돼?"})).await;
    let verdict = &body["answer"]["verdict"];
    assert_eq!(verdict["check"], "enrollment");
    assert_eq!(verdict["asked_value"], "휴학");
    assert_eq!(verdict["asked_status"], "fail");
    let card = body["eligibility"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["label"] == "학적")
        .unwrap();
    assert_eq!(verdict["status"], card["status"]);
    assert_eq!(verdict["status"], "pass");

    let (_, body) = ask(&base, &token, id, json!({"question":"알림 켜줘"})).await;
    assert_eq!(body["answer"]["unsupported"], true);
    assert_eq!(body["alert_enabled"], false);

    // Follow-up without a topic of its own keeps the previous one.
    let (_, body) = ask(
        &base,
        &token,
        id,
        json!({"question":"그건 어때?","previous_topic":"documents"}),
    )
    .await;
    assert_eq!(body["answer"]["topics"], json!(["documents"]));
    assert_eq!(body["answer"]["passages"][0]["id"], "documents.0");
    let (status, _) = ask(
        &base,
        &token,
        id,
        json!({"question":"서류","previous_topic":"execute_sql"}),
    )
    .await;
    assert_eq!(status, 422);
    cleanup(&pool, &[user], &[id]).await;
}

#[tokio::test]
async fn answer_stream_requires_consent_and_enabled_ai() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let (base, pool, user, token) = setup().await;
    let id = collected(&pool).await;
    let url = format!("{base}/api/v1/programs/{id}/assistant/answer-stream");
    let client = Client::new();
    assert_eq!(
        client
            .post(&url)
            .json(&json!({"question":"서류","allow_ai_processing":true}))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let response = client
        .post(&url)
        .bearer_auth(&token)
        .json(&json!({"question":"서류"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    assert_eq!(
        response.json::<Value>().await.unwrap()["code"],
        "consent_required"
    );
    let response = client
        .post(&url)
        .bearer_auth(&token)
        .json(&json!({"question":"서류","allow_ai_processing":true}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 503, "AI is off by default");
    assert_eq!(
        response.json::<Value>().await.unwrap()["code"],
        "ai_unavailable"
    );
    cleanup(&pool, &[user], &[id]).await;
}

struct Scripted {
    reply: String,
    requests: Mutex<Vec<AnswerRequest>>,
    hold: Option<Arc<Notify>>,
}

#[async_trait::async_trait]
impl AnswerProvider for Scripted {
    async fn generate(
        &self,
        request: &AnswerRequest,
        deltas: mpsc::Sender<String>,
    ) -> Result<String, ProviderError> {
        self.requests.lock().unwrap().push(request.clone());
        if let Some(hold) = &self.hold {
            hold.notified().await;
        }
        let split = self
            .reply
            .char_indices()
            .nth(self.reply.chars().count() / 2)
            .map_or(0, |(i, _)| i);
        for part in [&self.reply[..split], &self.reply[split..]] {
            deltas
                .send(part.to_owned())
                .await
                .map_err(|_| ProviderError::Cancelled)?;
        }
        Ok(self.reply.clone())
    }
}

fn scripted(reply: &str, hold: Option<Arc<Notify>>) -> Arc<Scripted> {
    Arc::new(Scripted {
        reply: reply.into(),
        requests: Mutex::new(Vec::new()),
        hold,
    })
}

async fn stream(base: &str, token: &str, id: Uuid, question: &str) -> reqwest::Response {
    Client::new()
        .post(format!(
            "{base}/api/v1/programs/{id}/assistant/answer-stream"
        ))
        .bearer_auth(token)
        .json(&json!({"question":question,"allow_ai_processing":true}))
        .send()
        .await
        .unwrap()
}

async fn events(response: reqwest::Response) -> Vec<Value> {
    response
        .text()
        .await
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[tokio::test]
async fn answer_stream_sends_deltas_then_a_validated_final() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let (_, pool, user, token) = setup().await;
    let id = collected(&pool).await;
    // Round 3: only whole source sentences pass the guard; the passage
    // here is a list ("신분증, 주민등록 등본"), so the model's honest answer is
    // the not-found sentence. A paraphrase falls back (below).
    let provider = scripted("공고 문장에서 확인되지 않아요.", None);
    let runtime = Arc::new(AssistantRuntime::new(
        Some(provider.clone()),
        std::time::Duration::from_secs(5),
    ));
    let base = serve(&pool, runtime).await;

    let response = stream(&base, &token, id, "필요한 서류는? 010-1234-5678").await;
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.headers()["content-type"],
        "application/x-ndjson; charset=utf-8"
    );
    assert_eq!(response.headers()["cache-control"], "private, no-store");
    let events = events(response).await;
    assert_eq!(events[0]["type"], "start");
    let deltas: String = events
        .iter()
        .filter(|e| e["type"] == "delta")
        .map(|e| e["text"].as_str().unwrap())
        .collect();
    // Round 4 (security H3): deltas carry progress only; the model's text
    // arrives with `final`, after the guard.
    assert!(events.iter().any(|e| e["type"] == "delta"));
    assert_eq!(deltas, "");
    let last = events.last().unwrap();
    assert_eq!(last["type"], "final");
    assert_eq!(last["valid"], true, "{last}");
    assert_eq!(last["text"], "공고 문장에서 확인되지 않아요.");

    // Only the masked question and the server's own passages reach the model.
    let sent = provider.requests.lock().unwrap()[0].clone();
    assert_eq!(sent.question, "필요한 서류는? [개인정보 생략]");
    assert_eq!(sent.passages[0], DOCUMENTS);
    assert!(sent.verdict.is_none());

    let again = stream(&base, &token, id, "필요한 서류는?").await;
    assert_eq!(again.status(), 429);
    assert_eq!(again.json::<Value>().await.unwrap()["code"], "too_soon");

    let (other, other_token, _) = create_test_user(&base).await;
    let none = stream(&base, &other_token, id, "외국인도 신청할 수 있어?").await;
    assert_eq!(none.status(), 409, "nothing to summarize");
    cleanup(&pool, &[user, other], &[id]).await;
}

#[tokio::test]
async fn ungrounded_generation_is_replaced_by_the_fallback() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let (_, pool, user, token) = setup().await;
    let id = collected(&pool).await;
    let runtime = Arc::new(AssistantRuntime::new(
        Some(scripted("서류만 내면 월 50만원을 받을 수 있어요.", None)),
        std::time::Duration::from_secs(5),
    ));
    let base = serve(&pool, runtime).await;
    let events = events(stream(&base, &token, id, "필요한 서류는?").await).await;
    let last = events.last().unwrap();
    assert_eq!(last["valid"], false);
    assert_eq!(last["text"], "근거 문장을 확인해 주세요");
    assert!(
        last.get("violation").is_none(),
        "the failing check is not disclosed"
    );
    cleanup(&pool, &[user], &[id]).await;
}

#[tokio::test]
async fn one_generation_at_a_time_and_quotes_never_wait() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let (_, pool, user, token) = setup().await;
    let id = collected(&pool).await;
    let hold = Arc::new(Notify::new());
    let runtime = Arc::new(AssistantRuntime::new(
        Some(scripted(
            "공고 문장에서 확인되지 않아요.",
            Some(hold.clone()),
        )),
        std::time::Duration::from_secs(5),
    ));
    let base = serve(&pool, runtime).await;
    let first = stream(&base, &token, id, "필요한 서류는?").await;
    assert_eq!(first.status(), 200);
    let (other, other_token, _) = create_test_user(&base).await;
    let busy = stream(&base, &other_token, id, "필요한 서류는?").await;
    assert_eq!(busy.status(), 429);
    let busy: Value = busy.json().await.unwrap();
    assert_eq!(busy["code"], "busy", "{busy}");
    // The grounded answer does not wait for the generation slot.
    let (status, body) = ask(
        &base,
        &other_token,
        id,
        json!({"question":"필요한 서류는?"}),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["answer"]["passages"][0]["id"], "documents.0");
    hold.notify_one();
    let events = events(first).await;
    assert_eq!(events.last().unwrap()["valid"], true);
    cleanup(&pool, &[user, other], &[id]).await;
}

#[tokio::test]
async fn search_parses_the_question_and_narrows_the_shared_preview() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let (base, pool, user, token) = setup().await;
    let marker = format!("검증{}", &Uuid::new_v4().simple().to_string()[..6]);
    let scholarship = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO programs(id,program_type,source_type,source_id,title,summary,program_status,regions,
             normalized_payload,deadline_at,is_active)
         VALUES($1,'scholarship','manual',$2,$3,'휴학생 대상 장학금','active',ARRAY['busan'],$4,
             now()+interval '10 days',true)",
    )
    .bind(scholarship)
    .bind(scholarship.to_string())
    .bind(format!("부산 {marker} 장학금"))
    .bind(json!({"enrollment_statuses": ["on_leave"]}))
    .execute(&pool)
    .await
    .unwrap();
    let url = format!("{base}/api/v1/assistant/search");
    let client = Client::new();
    let search = |question: String| {
        let client = client.clone();
        let url = url.clone();
        let token = token.clone();
        async move {
            let response = client
                .post(&url)
                .bearer_auth(token)
                .json(&json!({ "question": question }))
                .send()
                .await
                .unwrap();
            assert_eq!(response.headers()["cache-control"], "private, no-store");
            (
                response.status().as_u16(),
                response.json::<Value>().await.unwrap(),
            )
        }
    };
    assert_eq!(
        client
            .post(&url)
            .json(&json!({"question":"장학금"}))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let (status, body) = search(format!("부산 사는 휴학생이 받을 수 있는 {marker} 장학금?")).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["intent"]["region"], "busan");
    assert_eq!(body["intent"]["enrollment_status"], "on_leave");
    assert_eq!(body["intent"]["program_types"], json!(["scholarship"]));
    assert_eq!(body["intent"]["keywords"], json!([marker]));
    assert_eq!(body["results"][0]["program_id"], scholarship.to_string());
    assert_eq!(body["results"][0]["matched_keywords"], json!([marker]));
    assert!(body["filter_summary"]["considered"].as_u64().unwrap() > 0);
    // The saved profile has no enrollment status; the question's status is
    // what admits the on-leave-only programme, without saving it.
    let saved: Option<String> =
        sqlx::query_scalar("SELECT enrollment_status FROM user_profiles WHERE user_id=$1")
            .bind(user)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(saved, None);
    let (_, body) = search(format!("재학생 {marker} 장학금")).await;
    assert_eq!(body["not_found"], true, "{body}");
    let (_, body) = search("알림 켜줘".into()).await;
    assert_eq!(body["unsupported"], true);
    for _ in 0..3 {
        assert_eq!(search("장학금".into()).await.0, 200);
    }
    assert_eq!(search("장학금".into()).await.0, 429, "6 per minute");
    cleanup(&pool, &[user], &[scholarship]).await;
}

/// A saved 시·군·구 belongs to the saved sido. A question about another sido
/// searches that sido without the district instead of failing with 400.
#[tokio::test]
async fn search_in_another_sido_ignores_the_saved_district() {
    let (base, pool, user, token) = setup().await;
    sqlx::query(
        "INSERT INTO user_profiles(user_id,birth_year,region_code,city_code) VALUES($1,2000,'daegu','27110')
         ON CONFLICT(user_id) DO UPDATE SET birth_year=2000,region_code='daegu',city_code='27110'",
    )
    .bind(user)
    .execute(&pool)
    .await
    .unwrap();
    let response = Client::new()
        .post(format!("{base}/api/v1/assistant/search"))
        .bearer_auth(&token)
        .json(&json!({"question": "부산 사는 휴학생 장학금"}))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body: Value = response.json().await.unwrap();
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["intent"]["region"], "busan");
    let city: Option<String> =
        sqlx::query_scalar("SELECT city_code FROM user_profiles WHERE user_id=$1")
            .bind(user)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        city.as_deref(),
        Some("27110"),
        "the saved district is untouched"
    );
    cleanup(&pool, &[user], &[]).await;
}

/// Round 3 (code M-6): an exclusion filters the rows' regions, not the
/// profile. Saved 부산, "부산 말고": the search runs under 부산, rows local to
/// 부산 leave, nationwide rows stay; nothing is asked of the profile.
#[tokio::test]
async fn an_excluded_region_drops_its_local_rows() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let (base, pool, user, token) = setup().await;
    sqlx::query(
        "INSERT INTO user_profiles(user_id,birth_year,region_code) VALUES($1,2000,'busan')
         ON CONFLICT(user_id) DO UPDATE SET birth_year=2000,region_code='busan'",
    )
    .bind(user)
    .execute(&pool)
    .await
    .unwrap();
    let marker = format!("검증{}", &Uuid::new_v4().simple().to_string()[..6]);
    let (local, nationwide) = (Uuid::new_v4(), Uuid::new_v4());
    for (id, regions) in [(local, vec!["busan"]), (nationwide, vec![])] {
        sqlx::query(
            "INSERT INTO programs(id,program_type,source_type,source_id,title,program_status,regions,deadline_at,is_active)
             VALUES($1,'scholarship','manual',$2,$3,'active',$4,now()+interval '10 days',true)",
        )
        .bind(id)
        .bind(id.to_string())
        .bind(format!("{marker} 장학금"))
        .bind(regions)
        .execute(&pool)
        .await
        .unwrap();
    }
    let response = Client::new()
        .post(format!("{base}/api/v1/assistant/search"))
        .bearer_auth(&token)
        .json(&json!({ "question": format!("부산 말고 {marker} 장학금") }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["required_profile_fields"], json!([]), "{body}");
    let ids: Vec<&str> = body["results"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["program_id"].as_str())
        .collect();
    assert!(ids.contains(&nationwide.to_string().as_str()), "{body}");
    assert!(!ids.contains(&local.to_string().as_str()), "{body}");
    cleanup(&pool, &[user], &[local, nationwide]).await;
}

/// Review H4: the assistant's search must reach 산모·신생아 programmes even
/// though no profile flag says the user is pregnant.
#[tokio::test]
async fn search_finds_programmes_for_groups_without_a_profile_flag() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let (base, pool, user, token) = setup().await;
    profile(&pool, user, None).await;
    let marker = format!("검증{}", &Uuid::new_v4().simple().to_string()[..6]);
    let birth = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO programs(id,program_type,source_type,source_id,title,summary,program_status,regions,
             raw_payload,deadline_at,is_active)
         VALUES($1,'policy','gov_benefits',$2,$3,'출산 가정 산모 건강관리 서비스','active',ARRAY['busan'],$4,
             now()+interval '10 days',true)",
    )
    .bind(birth)
    .bind(birth.to_string())
    .bind(format!("산모·신생아 건강관리 지원 {marker}"))
    .bind(json!({"사용자구분": "개인", "지원대상": "○ 출산가정"}))
    .execute(&pool)
    .await
    .unwrap();
    let response = Client::new()
        .post(format!("{base}/api/v1/assistant/search"))
        .bearer_auth(&token)
        .json(&json!({ "question": format!("{marker} 출산 지원 뭐 있어?") }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["not_found"], false, "{body}");
    assert!(
        body["results"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["program_id"] == birth.to_string()),
        "{body}"
    );
    cleanup(&pool, &[user], &[birth]).await;
}

/// Audit H3 end to end (arch M3): a status the question names and the user
/// has not saved is the search's basis. The row the list would pass for an
/// enrolled user is unknown, and says nothing about the enrollment
/// condition.
#[tokio::test]
async fn an_assumed_status_is_never_a_met_condition() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let (base, pool, user, token) = setup().await;
    sqlx::query(
        "INSERT INTO user_profiles(user_id,birth_year,region_code,enrollment_status) VALUES($1,2000,'busan','on_leave')
         ON CONFLICT(user_id) DO UPDATE SET birth_year=2000,region_code='busan',enrollment_status='on_leave'",
    )
    .bind(user)
    .execute(&pool)
    .await
    .unwrap();
    let marker = format!("검증{}", &Uuid::new_v4().simple().to_string()[..6]);
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO programs(id,program_type,source_type,source_id,title,program_status,regions,
             normalized_payload,deadline_at,is_active)
         VALUES($1,'scholarship','manual',$2,$3,'active',ARRAY['busan'],$4,now()+interval '10 days',true)",
    )
    .bind(id)
    .bind(id.to_string())
    .bind(format!("{marker} 재학생 장학금"))
    .bind(json!({"enrollment_statuses": ["enrolled"]}))
    .execute(&pool)
    .await
    .unwrap();
    let rule = json!({"all": [{"field": "region_code", "op": "eq", "value": "busan"},
                              {"field": "enrollment_status", "op": "in", "value": ["enrolled"]}]});
    sqlx::query("INSERT INTO eligibility_rules(program_id,rule_json,hard_filter_json,version) VALUES($1,$2,$2,1)")
        .bind(id)
        .bind(&rule)
        .execute(&pool)
        .await
        .unwrap();
    let response = Client::new()
        .post(format!("{base}/api/v1/assistant/search"))
        .bearer_auth(&token)
        .json(&json!({ "question": format!("재학생 {marker} 장학금") }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.unwrap();
    let row = body["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["program_id"] == id.to_string())
        .cloned()
        .unwrap_or_else(|| panic!("{body}"));
    assert_eq!(row["eligibility_status"], "unknown", "{row}");
    assert!(!row["reasons"].to_string().contains("재학 상태"), "{row}");
    cleanup(&pool, &[user], &[id]).await;
}
