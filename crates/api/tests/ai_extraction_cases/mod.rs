//! A-09 AI extraction: worker job with the deterministic mock provider,
//! admin review over HTTP, atomic application with provenance, supersede on
//! source change, access control and RLS. No real model is called.
use super::*;
use mazimi_worker::ai_extraction::{
    job::{run_batch, JobConfig, Mode},
    provider::{char_offset, MockExtractionProvider, ProviderFailure},
};

mod auto_verify;

/// run_batch resolves and raises global operational incidents; tests that
/// run it are serialized so the incident assertions see only their own runs.
static AI_JOB_LOCK: LazyLock<tokio::sync::Mutex<()>> =
    LazyLock::new(|| tokio::sync::Mutex::new(()));

const SUMMARY: &str = "부산광역시 거주 만 19세~34세 청년 월세 지원";

fn evidence(text: &str, quote: &str) -> Value {
    json!({"quote": quote, "start": char_offset(text, quote).unwrap_or(0)})
}

/// What a well-behaved model would return for the fixture text, plus one
/// hallucinated document whose quote does not name it.
fn mock_answer(text: &str) -> Value {
    json!({
        "age": {"min_age": 19, "max_age": 34, "basis": "man_age", "confidence": "HIGH",
                "evidence": evidence(text, "만 19세~34세")},
        "region": {"codes": ["busan"], "confidence": "HIGH", "evidence": evidence(text, "부산광역시 거주")},
        "enrollment": null,
        "employment": {"statuses": ["unemployed"], "confidence": "MEDIUM", "evidence": evidence(text, "미취업 청년")},
        "income": {"criterion": "income_decile", "comparator": "lte", "value": 8, "confidence": "HIGH",
                   "evidence": evidence(text, "소득분위 8분위 이하")},
        "documents": [
            {"name": "신분증", "required": true, "notes": null, "confidence": "HIGH",
             "evidence": evidence(text, "신분증,")},
            {"name": "주민등록등본", "required": true, "notes": null, "confidence": "HIGH",
             "evidence": evidence(text, "주민등록 등본")},
            {"name": "가족관계증명서", "required": true, "notes": null, "confidence": "HIGH",
             "evidence": evidence(text, "신분증")}
        ],
        "application_window": {"kind": "dated", "start_date": "2099-03-02", "end_date": "2099-04-30",
                               "confidence": "HIGH", "evidence": evidence(text, "신청기간: 2099.03.02 ~ 2099.04.30")}
    })
}

fn mock() -> MockExtractionProvider {
    MockExtractionProvider::new("mock-extractor-v1", |request| {
        Ok(mock_answer(&request.source_text))
    })
}

fn config(ids: &[Uuid]) -> JobConfig {
    JobConfig {
        only_programs: Some(ids.to_vec()),
        ..JobConfig::default()
    }
}

async fn pool() -> sqlx::PgPool {
    PgPoolOptions::new()
        .max_connections(3)
        .connect(&DATABASE_URL)
        .await
        .unwrap()
}

async fn program(pool: &sqlx::PgPool) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO programs (id, program_type, source_type, source_id, title, summary,
             application_method, submission_documents, program_status, is_active, regions, raw_payload)
         VALUES ($1, 'policy', 'manual', $2, $3, $4, $5, '신분증, 주민등록 등본', 'active', true, '{}', $6)",
    )
    .bind(id)
    .bind(format!("ai-ext-{id}"))
    .bind(format!("AI 추출 검증 공고 {}", &id.simple().to_string()[..8]))
    .bind(SUMMARY)
    .bind("온라인 신청 (신청기간: 2099.03.02 ~ 2099.04.30)")
    .bind(json!({"sprtTrgtCn": "소득분위 8분위 이하, 미취업 청년"}))
    .execute(pool)
    .await
    .unwrap();
    id
}

async fn cleanup(pool: &sqlx::PgPool, ids: &[Uuid]) {
    sqlx::query("DELETE FROM programs WHERE id = ANY($1)")
        .bind(ids)
        .execute(pool)
        .await
        .unwrap();
}

async fn candidates(pool: &sqlx::PgPool, id: Uuid) -> Vec<(String, String, Uuid)> {
    sqlx::query_as(
        "SELECT fact_key, status, id FROM program_ai_extraction_candidates
         WHERE program_id = $1 ORDER BY fact_key, created_at",
    )
    .bind(id)
    .fetch_all(pool)
    .await
    .unwrap()
}

fn candidate_id(rows: &[(String, String, Uuid)], key: &str) -> Uuid {
    rows.iter()
        .find(|(k, s, _)| k == key && s != "superseded")
        .unwrap_or_else(|| panic!("{key} in {rows:?}"))
        .2
}

async fn public_state(pool: &sqlx::PgPool, id: Uuid) -> (Option<i32>, Option<i32>, i64, i64) {
    sqlx::query_as(
        "SELECT min_age, max_age,
                (SELECT count(*) FROM eligibility_rules WHERE program_id = $1),
                (SELECT count(*) FROM program_documents WHERE program_id = $1)
         FROM programs WHERE id = $1",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn post_review(base: &str, token: &str, id: Uuid, body: Value) -> (u16, Value) {
    let response = Client::new()
        .post(format!(
            "{base}/api/v1/admin/programs/{id}/ai-extraction/review"
        ))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    (status, response.json().await.unwrap_or(Value::Null))
}

const NOTE: &str = "공식 공고 원문의 해당 문장과 대조해 확인했습니다.";

#[tokio::test]
async fn ai_job_stores_validated_candidates_without_touching_public_data() {
    let _ai_guard = AI_JOB_LOCK.lock().await;
    let _base = start_test_server().await;
    let pool = pool().await;
    let id = program(&pool).await;
    let provider = mock();

    let first = run_batch(&pool, &provider, &config(&[id])).await.unwrap();
    assert_eq!((first.calls, first.failures), (1, 0), "{first:?}");
    assert_eq!(first.candidates_written, 8);
    assert_eq!(first.rejected_by_guard, 1, "hallucinated document");
    assert_eq!(first.status, "succeeded");
    let rows = candidates(&pool, id).await;
    let status = |key: &str| rows.iter().find(|r| r.0 == key).unwrap().1.clone();
    assert_eq!(rows.len(), 8, "{rows:?}");
    assert_eq!(status("age"), "pending");
    assert_eq!(status("income"), "pending");
    assert_eq!(
        status("employment"),
        "flagged",
        "MEDIUM is not in the default queue"
    );
    assert_eq!(status("document:가족관계증명서"), "invalid");
    assert_eq!(status("application_window"), "pending");
    assert_eq!(
        public_state(&pool, id).await,
        (None, None, 0, 0),
        "extraction alone must not change public data"
    );

    // Evidence is an exact span of the stored source text.
    let (source_text, evidence): (String, Value) = sqlx::query_as(
        "SELECT s.source_text, c.evidence FROM program_ai_extraction_candidates c
         JOIN program_ai_extraction_cache s ON s.id = c.cache_id
         WHERE c.program_id = $1 AND c.fact_key = 'age'",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    let start = evidence["start"].as_u64().unwrap() as usize;
    let end = evidence["end"].as_u64().unwrap() as usize;
    let span: String = source_text.chars().skip(start).take(end - start).collect();
    assert_eq!(span, "만 19세~34세");
    assert!(!source_text.contains("010-"), "contact data never stored");

    // Same source, model, prompt and schema: served from cache, no new rows.
    let second = run_batch(&pool, &provider, &config(&[id])).await.unwrap();
    assert_eq!(
        provider.calls(),
        1,
        "unchanged source must not call the model"
    );
    assert_eq!(second.calls, 0);
    assert_eq!(second.cache_hits + second.unchanged_skips, 1);
    assert_eq!(candidates(&pool, id).await.len(), 8);

    // NORMALIZE_ONLY replays cached answers without any model call.
    let replay = run_batch(
        &pool,
        &provider,
        &JobConfig {
            mode: Mode::NormalizeOnly,
            ..config(&[id])
        },
    )
    .await
    .unwrap();
    assert_eq!(provider.calls(), 1);
    assert_eq!(replay.calls, 0);

    let persisted: (String, i32, i32, i32) = sqlx::query_as(
        "SELECT status, calls, rejected_by_guard, candidates_written
         FROM program_ai_extraction_runs WHERE id = $1",
    )
    .bind(first.run_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(persisted, ("succeeded".into(), 1, 1, 8));
    cleanup(&pool, &[id]).await;
}

#[tokio::test]
async fn provider_failures_cool_down_open_the_breaker_and_keep_rule_values() {
    let _ai_guard = AI_JOB_LOCK.lock().await;
    let _base = start_test_server().await;
    let pool = pool().await;
    let ids = [
        program(&pool).await,
        program(&pool).await,
        program(&pool).await,
        program(&pool).await,
    ];
    sqlx::query("UPDATE programs SET min_age = 20 WHERE id = $1")
        .bind(ids[0])
        .execute(&pool)
        .await
        .unwrap();
    let down = MockExtractionProvider::new("mock-down-v1", |_| Err(ProviderFailure::Unavailable));

    let first = run_batch(&pool, &down, &config(&ids)).await.unwrap();
    assert_eq!(first.failures, 3, "{first:?}");
    assert_eq!(first.breaker_skips, 1, "fourth program is not attempted");
    assert!(first.breaker_opened);
    assert_eq!(first.status, "failed");
    assert_eq!(down.calls(), 3);
    let min_age: Option<i32> = sqlx::query_scalar("SELECT min_age FROM programs WHERE id = $1")
        .bind(ids[0])
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(min_age, Some(20), "rule value kept on failure");
    let failed: Vec<Uuid> = sqlx::query_scalar(
        "SELECT program_id FROM program_ai_extraction_cache
         WHERE program_id = ANY($1) AND model = 'mock-down-v1'
           AND status = 'failed' AND failure_count = 1",
    )
    .bind(&ids[..])
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(failed.len(), 3);
    let open_events: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM operational_events
         WHERE fingerprint IN ('ai_extraction:provider_failures', 'ai_extraction:circuit_open')
           AND status = 'open'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(open_events, 2);

    // Inside the cooldown the failed programs are skipped without a call.
    let second = run_batch(&pool, &down, &config(&failed)).await.unwrap();
    assert_eq!(second.cooldown_skips, 3, "{second:?}");
    assert_eq!(down.calls(), 3);

    // After the cooldown a working provider resolves both incidents.
    sqlx::query(
        "UPDATE program_ai_extraction_cache SET last_failed_at = now() - interval '7 hours'
         WHERE program_id = ANY($1)",
    )
    .bind(&ids[..])
    .execute(&pool)
    .await
    .unwrap();
    let ok = mock();
    let recovered = run_batch(&pool, &ok, &config(&failed[..1])).await.unwrap();
    assert_eq!((recovered.calls, recovered.failures), (1, 0));
    let still_open: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM operational_events
         WHERE fingerprint IN ('ai_extraction:provider_failures', 'ai_extraction:circuit_open')
           AND status = 'open'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(still_open, 0);
    cleanup(&pool, &ids).await;
}

#[tokio::test]
async fn approval_applies_facts_atomically_with_provenance_and_reject_changes_nothing() {
    let _ai_guard = AI_JOB_LOCK.lock().await;
    let base = start_test_server().await;
    let pool = pool().await;
    let id = program(&pool).await;
    run_batch(&pool, &mock(), &config(&[id])).await.unwrap();
    let (admin, token) = create_test_admin(&base).await;

    let review: Value = Client::new()
        .get(format!("{base}/api/v1/admin/programs/{id}/ai-extraction"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(review["candidates"].as_array().unwrap().len(), 8);
    assert_eq!(review["sources"].as_array().unwrap().len(), 1);
    assert_eq!(review["sources"][0]["stale"], false);
    let age = review["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["fact_key"] == "age")
        .unwrap();
    assert_eq!(age["plan"]["changes"][0]["target"], "programs.min_age");
    assert_eq!(age["plan"]["changes"][0]["current"], Value::Null);

    let rows = candidates(&pool, id).await;
    let approve =
        |key: &str| json!({"candidate_id": candidate_id(&rows, key), "decision": "approve"});
    // Invalid facts can never be approved.
    let (status, body) = post_review(
        &base,
        &token,
        id,
        json!({"decisions": [approve("document:가족관계증명서")], "note": NOTE}),
    )
    .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (409, Some("candidate_not_open"))
    );

    let (status, body) = post_review(
        &base,
        &token,
        id,
        json!({"decisions": [
            approve("age"), approve("income"), approve("employment"),
            approve("document:주민등록등본"), approve("application_window"),
            {"candidate_id": candidate_id(&rows, "region"), "decision": "reject"}
        ], "note": NOTE}),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["eligibility_rule_version"], 1);

    let (min_age, max_age, start, end): (Option<i32>, Option<i32>, chrono::DateTime<Utc>, chrono::DateTime<Utc>) =
        sqlx::query_as(
            "SELECT min_age, max_age, application_start_at, application_end_at FROM programs WHERE id = $1",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!((min_age, max_age), (Some(19), Some(34)));
    assert_eq!(start.to_rfc3339(), "2099-03-01T15:00:00+00:00", "00:00 KST");
    assert_eq!(
        end.to_rfc3339(),
        "2099-04-30T14:59:59+00:00",
        "23:59:59 KST"
    );

    let (hard, soft, explain, reviewed_by, created_by): (
        Value,
        Value,
        Option<Value>,
        String,
        String,
    ) = sqlx::query_as(
        "SELECT hard_filter_json, rule_json, explain_json, reviewed_by, created_by
         FROM eligibility_rules WHERE program_id = $1",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    let fields: Vec<&str> = hard["all"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["field"].as_str().unwrap())
        .collect();
    assert_eq!(fields, vec!["income_bracket", "employment_status"]);
    assert_eq!(hard, soft);
    assert!(mazimi_core::rule_engine::parse_rule_json(&hard).is_ok());
    assert!(
        explain.is_none(),
        "public explain_json carries no reviewer data"
    );
    let applied: Value = sqlx::query_scalar(
        "SELECT applied_changes FROM program_ai_extraction_candidates WHERE id = $1",
    )
    .bind(candidate_id(&rows, "income"))
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(applied["eligibility_rule_version"], 1);
    assert_eq!(reviewed_by, admin.to_string());
    assert_eq!(created_by, "ai_extraction");

    let (name, source, quote, candidate): (String, String, String, Uuid) = sqlx::query_as(
        "SELECT document_name, source, evidence->>'quote', ai_candidate_id FROM program_documents WHERE program_id = $1",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        (name.as_str(), source.as_str(), quote.as_str()),
        ("주민등록등본", "ai_extraction_approved", "주민등록 등본")
    );
    assert_eq!(candidate, candidate_id(&rows, "document:주민등록등본"));

    let overrides: i64 =
        sqlx::query_scalar("SELECT count(*) FROM program_ai_field_overrides WHERE program_id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(overrides, 4);

    let decided: Vec<(String, String, Option<Uuid>, Option<String>)> = sqlx::query_as(
        "SELECT fact_key, status, reviewer_id, review_note FROM program_ai_extraction_candidates
         WHERE program_id = $1 AND status IN ('approved', 'rejected') ORDER BY fact_key",
    )
    .bind(id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(decided.len(), 6);
    assert!(decided
        .iter()
        .all(|(_, _, reviewer, note)| *reviewer == Some(admin) && note.as_deref() == Some(NOTE)));
    assert!(decided
        .iter()
        .any(|(k, s, ..)| k == "region" && s == "rejected"));
    assert!(
        !hard.to_string().contains("region_code"),
        "a rejected fact leaves the rules untouched"
    );

    // A decided candidate cannot be decided again.
    let (status, _) = post_review(
        &base,
        &token,
        id,
        json!({"decisions": [approve("age")], "note": NOTE}),
    )
    .await;
    assert_eq!(status, 409);
    cleanup(&pool, &[id]).await;
}

#[tokio::test]
async fn overwrites_need_confirmation_and_a_refused_request_applies_nothing() {
    let _ai_guard = AI_JOB_LOCK.lock().await;
    let base = start_test_server().await;
    let pool = pool().await;
    let id = program(&pool).await;
    run_batch(&pool, &mock(), &config(&[id])).await.unwrap();
    // Values that exist before review: an authoritative age and an income rule.
    sqlx::query("UPDATE programs SET min_age = 20 WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let existing = json!({"all": [{"field": "income_bracket", "op": "lte", "value": 5}]});
    sqlx::query(
        "INSERT INTO eligibility_rules (program_id, rule_json, hard_filter_json, version, compiled_at)
         VALUES ($1, $2, $2, 1, now())",
    )
    .bind(id)
    .bind(&existing)
    .execute(&pool)
    .await
    .unwrap();
    let (_, token) = create_test_admin(&base).await;
    let rows = candidates(&pool, id).await;
    let decisions = |confirm: bool| {
        json!({"decisions": [
            {"candidate_id": candidate_id(&rows, "age"), "decision": "approve", "confirm_overwrite": confirm},
            {"candidate_id": candidate_id(&rows, "income"), "decision": "approve", "confirm_overwrite": confirm},
            {"candidate_id": candidate_id(&rows, "document:주민등록등본"), "decision": "approve"}
        ], "note": NOTE})
    };

    let (status, body) = post_review(&base, &token, id, decisions(false)).await;
    assert_eq!(status, 409, "{body}");
    assert_eq!(body["code"], "overwrite_requires_confirmation");
    let conflicts = body["details"]["conflicts"].as_array().unwrap();
    assert!(conflicts
        .iter()
        .any(|c| c["target"] == "programs.min_age" && c["current"] == 20 && c["proposed"] == 19));
    assert!(conflicts
        .iter()
        .any(|c| c["target"] == "eligibility_rules.hard_filter_json"));
    assert_eq!(
        public_state(&pool, id).await,
        (Some(20), None, 1, 0),
        "the document approval in the refused request was rolled back too"
    );
    assert!(candidates(&pool, id)
        .await
        .iter()
        .all(|(_, s, _)| s != "approved"));

    let (status, body) = post_review(&base, &token, id, decisions(true)).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(public_state(&pool, id).await, (Some(19), Some(34), 2, 1));
    let hard: Value = sqlx::query_scalar(
        "SELECT hard_filter_json FROM eligibility_rules WHERE program_id = $1 AND version = 2",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        hard,
        json!({"all": [{"field": "income_bracket", "op": "lte", "value": 8, "weight": 1.0}]}),
        "the confirmed condition replaces the old one"
    );
    cleanup(&pool, &[id]).await;
}

#[tokio::test]
async fn source_change_blocks_approval_and_supersedes_open_candidates() {
    let _ai_guard = AI_JOB_LOCK.lock().await;
    let base = start_test_server().await;
    let pool = pool().await;
    let id = program(&pool).await;
    let provider = mock();
    run_batch(&pool, &provider, &config(&[id])).await.unwrap();
    let (_, token) = create_test_admin(&base).await;
    let rows = candidates(&pool, id).await;
    sqlx::query(
        "UPDATE programs SET summary = '부산광역시 거주 만 19세~39세 청년 월세 지원' WHERE id = $1",
    )
    .bind(id)
    .execute(&pool)
    .await
    .unwrap();

    let (status, body) = post_review(
        &base,
        &token,
        id,
        json!({"decisions": [{"candidate_id": candidate_id(&rows, "age"), "decision": "approve"}], "note": NOTE}),
    )
    .await;
    assert_eq!(
        (status, body["code"].as_str()),
        (409, Some("source_changed"))
    );
    assert_eq!(public_state(&pool, id).await, (None, None, 0, 0));

    let rerun = run_batch(&pool, &provider, &config(&[id])).await.unwrap();
    assert_eq!(rerun.superseded, 8, "{rerun:?}");
    assert_eq!(provider.calls(), 2, "a new source hash is a new request");
    let after = candidates(&pool, id).await;
    let old: Vec<_> = after
        .iter()
        .filter(|r| rows.iter().any(|o| o.2 == r.2))
        .collect();
    assert!(old.iter().all(|(_, s, _)| s == "superseded"));
    // The mock still quotes 34세, which the new text no longer contains.
    let new_age = after
        .iter()
        .find(|(k, s, _)| k == "age" && s != "superseded")
        .unwrap();
    assert_eq!(new_age.1, "invalid");
    cleanup(&pool, &[id]).await;
}

#[tokio::test]
async fn ai_extraction_routes_require_an_admin() {
    let _ai_guard = AI_JOB_LOCK.lock().await;
    let base = start_test_server().await;
    let (_, user_token, _) = create_test_user(&base).await;
    let (_, admin_token) = create_test_admin(&base).await;
    let client = Client::new();
    let id = Uuid::new_v4();
    let gets = [
        format!("{base}/api/v1/admin/ai-extraction/candidates"),
        format!("{base}/api/v1/admin/programs/{id}/ai-extraction"),
    ];
    let review = format!("{base}/api/v1/admin/programs/{id}/ai-extraction/review");
    let body = json!({"decisions": [{"candidate_id": Uuid::new_v4(), "decision": "approve"}], "note": NOTE});
    for url in &gets {
        assert_eq!(client.get(url).send().await.unwrap().status(), 401, "{url}");
        assert_eq!(
            client
                .get(url)
                .bearer_auth(&user_token)
                .send()
                .await
                .unwrap()
                .status(),
            403,
            "{url}"
        );
    }
    assert_eq!(
        client
            .post(&review)
            .json(&body)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        client
            .post(&review)
            .bearer_auth(&user_token)
            .json(&body)
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    let queue = client
        .get(&gets[0])
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(queue.status(), 200);
    assert!(queue.json::<Value>().await.unwrap()["items"].is_array());
    assert_eq!(
        client
            .get(&gets[1])
            .bearer_auth(&admin_token)
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    assert_eq!(
        client
            .get(format!("{}?status=bogus", gets[0]))
            .bearer_auth(&admin_token)
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    // A reviewer id or any other unknown field in the body is refused.
    let smuggled = json!({"decisions": [], "note": NOTE, "reviewer_id": Uuid::new_v4()});
    assert_eq!(
        client
            .post(&review)
            .bearer_auth(&admin_token)
            .json(&smuggled)
            .send()
            .await
            .unwrap()
            .status(),
        422
    );
}

#[tokio::test]
async fn ai_extraction_tables_force_service_only_rls() {
    let _ai_guard = AI_JOB_LOCK.lock().await;
    let _base = start_test_server().await;
    let pool = pool().await;
    for table in [
        "program_ai_extraction_runs",
        "program_ai_extraction_cache",
        "program_ai_extraction_candidates",
        "program_ai_field_overrides",
    ] {
        let (enabled, forced): (bool, bool) = sqlx::query_as(
            "SELECT relrowsecurity, relforcerowsecurity FROM pg_class WHERE oid = $1::regclass",
        )
        .bind(table)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(enabled && forced, "{table}");
        let policies: Vec<String> =
            sqlx::query_scalar("SELECT qual FROM pg_policies WHERE tablename = $1")
                .bind(table)
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(policies, vec!["is_service_role()".to_string()], "{table}");
    }
}

#[tokio::test]
async fn ingestion_keeps_an_approved_value_only_for_the_same_upstream_payload() {
    let _ai_guard = AI_JOB_LOCK.lock().await;
    let _catalog_guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let _base = start_test_server().await;
    let pool = pool().await;
    let suffix = Uuid::new_v4();
    let source_name = format!("ai_override_test_{suffix}");
    let source_id = format!("record_{suffix}");
    let payload = json!({"title": "AI 승인 유지 확인"});
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO programs (program_type, source_type, source_id, title, program_status,
             is_active, normalizer_version, raw_payload, min_age)
         VALUES ('policy', $1, $2, 'AI 승인 유지 확인', 'active', true, 0, $3, 19) RETURNING id",
    )
    .bind(&source_name)
    .bind(&source_id)
    .bind(&payload)
    .fetch_one(&pool)
    .await
    .unwrap();
    // A candidate row is the override's provenance.
    let cache_id: Uuid = sqlx::query_scalar(
        "INSERT INTO program_ai_extraction_cache (program_id, source_type, source_hash, provider, model,
             prompt_version, schema_version, request_hash, source_text, status, response_json)
         VALUES ($1, 'test', repeat('a', 64), 'mock', 'm', 'p', 1, repeat('b', 64), 'x', 'succeeded', '{}')
         RETURNING id",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    let candidate: Uuid = sqlx::query_scalar(
        "INSERT INTO program_ai_extraction_candidates (cache_id, program_id, source_hash, provider, model,
             prompt_version, schema_version, fact_key, fact_type, value, evidence, confidence, status,
             reviewed_at, review_note)
         VALUES ($1, $2, repeat('a', 64), 'mock', 'm', 'p', 1, 'age', 'age', '{}', '{}', 'HIGH',
             'approved', now(), 'test approval note')
         RETURNING id",
    )
    .bind(cache_id)
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO program_ai_field_overrides (program_id, field, int_value, raw_payload_md5, candidate_id)
         VALUES ($1, 'min_age', 19, (SELECT md5(raw_payload::text) FROM programs WHERE id = $1), $2)",
    )
    .bind(id)
    .bind(candidate)
    .execute(&pool)
    .await
    .unwrap();
    let snapshot = |payload: Value| {
        let pool = pool.clone();
        let (source_name, source_id) = (source_name.clone(), source_id.clone());
        async move {
            sqlx::query(
                "INSERT INTO source_snapshots (source_name, source_id, content_hash, raw_payload, normalizer_version)
                 VALUES ($1, $2, md5($3::text), $3, 0)
                 ON CONFLICT (source_name, source_id) DO UPDATE SET raw_payload = EXCLUDED.raw_payload,
                     content_hash = EXCLUDED.content_hash, normalizer_version = 0",
            )
            .bind(&source_name)
            .bind(&source_id)
            .bind(&payload)
            .execute(&pool)
            .await
            .unwrap();
            mazimi_worker::pipeline::renormalize_stored_snapshots(&pool, Some(&source_name), 10)
                .await
                .unwrap();
            sqlx::query_scalar::<_, Option<i32>>("SELECT min_age FROM programs WHERE id = $1")
                .bind(id_of(&pool, &source_name).await)
                .fetch_one(&pool)
                .await
                .unwrap()
        }
    };
    assert_eq!(
        snapshot(payload.clone()).await,
        Some(19),
        "same payload keeps the approval"
    );
    assert_eq!(
        snapshot(json!({"title": "AI 승인 유지 확인", "changed": "upstream edit"})).await,
        None,
        "a changed payload falls back to the normalizer"
    );
    sqlx::query("DELETE FROM programs WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    for table in ["source_snapshots", "ingestion_runs"] {
        sqlx::query(&format!("DELETE FROM {table} WHERE source_name = $1"))
            .bind(&source_name)
            .execute(&pool)
            .await
            .unwrap();
    }
}

async fn id_of(pool: &sqlx::PgPool, source_name: &str) -> Uuid {
    sqlx::query_scalar("SELECT id FROM programs WHERE source_type = $1")
        .bind(source_name)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn approving_one_document_keeps_the_other_document_candidates_open() {
    let _ai_guard = AI_JOB_LOCK.lock().await;
    let base = start_test_server().await;
    let pool = pool().await;
    let id = program(&pool).await;
    let provider = mock();
    run_batch(&pool, &provider, &config(&[id])).await.unwrap();
    let (_, token) = create_test_admin(&base).await;
    let rows = candidates(&pool, id).await;
    let (status, body) = post_review(
        &base,
        &token,
        id,
        json!({"decisions": [{"candidate_id": candidate_id(&rows, "document:주민등록등본"), "decision": "approve"}], "note": NOTE}),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    // Documents are now rule-covered: the next request omits them, which must
    // not supersede the still-open document candidate.
    let rerun = run_batch(&pool, &provider, &config(&[id])).await.unwrap();
    assert_eq!(provider.calls(), 2, "coverage change is a new request");
    assert!(rerun.excluded_by_rules >= 1, "{rerun:?}");
    let after = candidates(&pool, id).await;
    let status_of = |key: &str| after.iter().find(|r| r.0 == key).unwrap().1.clone();
    assert_eq!(status_of("document:신분증"), "pending");
    assert_eq!(status_of("document:주민등록등본"), "approved");
    cleanup(&pool, &[id]).await;
}
