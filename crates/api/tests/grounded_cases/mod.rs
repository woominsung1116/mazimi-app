//! Grounded explanations: evidence citations on eligibility checks, program
//! detail, assistant sections and recommendation items, plus the
//! recommendation filter summary. Everything is deterministic from stored
//! data; the AI extraction job runs with the mock provider only.
use super::*;
use mazimi_worker::ai_extraction::{
    job::{run_batch, JobConfig},
    provider::{char_offset, MockExtractionProvider},
};

const SUMMARY: &str = "부산광역시 거주 만 19세~34세 청년 월세 지원";
const METHOD: &str = "온라인 신청 (신청기간: 2026.10.01 ~ 2027.12.31)";
const NOTE: &str = "공식 공고 원문의 해당 문장과 대조해 확인했습니다.";

fn evidence(text: &str, quote: &str) -> Value {
    json!({"quote": quote, "start": char_offset(text, quote).unwrap_or(0)})
}

fn mock() -> MockExtractionProvider {
    MockExtractionProvider::new("mock-grounded-v1", |request| {
        let text = &request.source_text;
        Ok(json!({
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
                 "evidence": evidence(text, "주민등록 등본")}
            ],
            "application_window": {"kind": "dated", "start_date": "2026-10-01", "end_date": "2027-12-31",
                                   "confidence": "HIGH", "evidence": evidence(text, "신청기간: 2026.10.01 ~ 2027.12.31")}
        }))
    })
}

async fn pool() -> sqlx::PgPool {
    PgPoolOptions::new()
        .max_connections(3)
        .connect(&DATABASE_URL)
        .await
        .unwrap()
}

async fn cleanup(pool: &sqlx::PgPool, ids: &[Uuid]) {
    sqlx::query("DELETE FROM programs WHERE id = ANY($1)")
        .bind(ids)
        .execute(pool)
        .await
        .unwrap();
}

/// A collected 온통청년 notice whose age, window and income only appear in
/// free text, so the AI curator is the only path to a structured value.
async fn collected_program(pool: &sqlx::PgPool) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO programs (id, program_type, source_type, source_id, title, summary,
             application_method, submission_documents, program_status, is_active, regions,
             official_url, last_synced_at, raw_payload)
         VALUES ($1, 'policy', 'youth_center', $2, $3, $4, $5, '신분증, 주민등록 등본', 'active', true, '{}',
             'https://www.youthcenter.go.kr/youthPolicy/ythPlcyTotalSearch/ythPlcyDetail/G1', now(), $6)",
    )
    .bind(id)
    .bind(format!("grounded-{id}"))
    .bind(format!("근거 인용 검증 공고 {}", &id.simple().to_string()[..8]))
    .bind(SUMMARY)
    .bind(METHOD)
    .bind(json!({"sprtTrgtCn": "소득분위 8분위 이하, 미취업 청년"}))
    .execute(pool)
    .await
    .unwrap();
    id
}

async fn candidate(pool: &sqlx::PgPool, id: Uuid, key: &str) -> Uuid {
    sqlx::query_scalar(
        "SELECT id FROM program_ai_extraction_candidates
         WHERE program_id = $1 AND fact_key = $2 AND status <> 'superseded'",
    )
    .bind(id)
    .bind(key)
    .fetch_one(pool)
    .await
    .unwrap_or_else(|e| panic!("{key}: {e}"))
}

async fn get_json(base: &str, path: &str, token: Option<&str>) -> Value {
    let mut request = Client::new().get(format!("{base}{path}"));
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    let response = request.send().await.unwrap();
    let status = response.status();
    let body: Value = response.json().await.unwrap();
    assert_eq!(status, 200, "{path}: {body}");
    body
}

async fn preview(base: &str, body: Value) -> Value {
    let response = Client::new()
        .post(format!("{base}/api/v1/recommend/preview"))
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body: Value = response.json().await.unwrap();
    assert_eq!(status, 200, "{body}");
    body
}

fn quotes(evidence: &Value) -> Vec<String> {
    evidence
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|e| e["quote"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn topic<'a>(evidence: &'a Value, check: &str) -> Vec<&'a Value> {
    evidence
        .as_array()
        .into_iter()
        .flatten()
        .filter(|e| e["check"] == check)
        .collect()
}

#[tokio::test]
async fn approved_facts_are_cited_with_their_source_span_until_the_source_changes() {
    let _catalog_guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let base = start_test_server().await;
    let pool = pool().await;
    let id = collected_program(&pool).await;
    run_batch(
        &pool,
        &mock(),
        &JobConfig {
            only_programs: Some(vec![id]),
            ..JobConfig::default()
        },
    )
    .await
    .unwrap();

    // Before any approval nothing AI-extracted is cited anywhere, even when
    // an operator already entered the same age band the pending fact proposes.
    sqlx::query("UPDATE programs SET min_age = 19, max_age = 34 WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let detail = get_json(&base, &format!("/api/v1/programs/{id}"), None).await;
    assert!(
        detail["evidence"]
            .as_array()
            .is_none_or(|items| items.iter().all(|e| e["kind"] != "ai_extracted_approved")),
        "pending facts are never cited: {}",
        detail["evidence"]
    );

    let (_, admin_token) = create_test_admin(&base).await;
    let approve = |c: Uuid| json!({"candidate_id": c, "decision": "approve"});
    let decisions = json!([
        approve(candidate(&pool, id, "age").await),
        approve(candidate(&pool, id, "income").await),
        approve(candidate(&pool, id, "document:신분증").await),
        approve(candidate(&pool, id, "application_window").await),
        {"candidate_id": candidate(&pool, id, "region").await, "decision": "reject"},
    ]);
    let response = Client::new()
        .post(format!(
            "{base}/api/v1/admin/programs/{id}/ai-extraction/review"
        ))
        .bearer_auth(&admin_token)
        .json(&json!({"decisions": decisions, "note": NOTE}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());

    // A second approved row for the same fact (another model over the same
    // text) must not cite the span twice.
    sqlx::query(
        "INSERT INTO program_ai_extraction_candidates (
             run_id, cache_id, program_id, source_hash, provider, model, prompt_version,
             schema_version, fact_key, fact_type, value, evidence, confidence, status,
             validation, reviewer_id, reviewed_at, review_note, applied_changes)
         SELECT run_id, cache_id, program_id, source_hash, provider, 'mock-grounded-v2',
             prompt_version, schema_version, fact_key, fact_type, value, evidence, confidence,
             status, validation, reviewer_id, reviewed_at - interval '1 day', review_note,
             applied_changes
         FROM program_ai_extraction_candidates
         WHERE program_id = $1 AND fact_key = 'age' AND status = 'approved'",
    )
    .bind(id)
    .execute(&pool)
    .await
    .unwrap();

    // Program detail: one evidence list, tagged by the check it supports.
    let detail = get_json(&base, &format!("/api/v1/programs/{id}"), None).await;
    let evidence = &detail["evidence"];
    let age = topic(evidence, "age");
    assert_eq!(age.len(), 1, "{evidence}");
    assert_eq!(age[0]["kind"], "ai_extracted_approved");
    assert_eq!(age[0]["quote"], "만 19세~34세");
    assert_eq!(
        age[0]["source_label"], "온통청년 공고 원문 · 요약",
        "an approved quote is the notice's own text, never an AI judgement"
    );
    assert!(age[0]["approved_at"].is_string());
    assert!(age[0]["captured_at"].is_string());
    assert_eq!(
        age[0]["source_url"],
        "https://www.youthcenter.go.kr/youthPolicy/ythPlcyTotalSearch/ythPlcyDetail/G1"
    );
    assert_eq!(
        quotes(&json!(topic(evidence, "hard_rule"))),
        ["소득분위 8분위 이하"]
    );
    let documents = topic(evidence, "documents");
    assert_eq!(documents.len(), 1, "{evidence}");
    assert_eq!(documents[0]["subject"], "신분증");
    assert_eq!(documents[0]["quote"], "신분증,");
    assert_eq!(
        quotes(&json!(topic(evidence, "application_window"))),
        ["신청기간: 2026.10.01 ~ 2027.12.31"]
    );
    let all = quotes(evidence);
    assert!(
        !all.iter().any(|q| q == "부산광역시 거주"),
        "rejected fact is not cited"
    );
    assert!(
        !all.iter()
            .any(|q| q == "미취업 청년" || q == "주민등록 등본"),
        "flagged/pending facts are not cited"
    );
    for item in evidence.as_array().unwrap() {
        assert!(item.get("reviewer_id").is_none() && item.get("review_note").is_none());
    }

    // Assistant: the same citations on the checks and the topic sections.
    let (user, token, _) = create_test_user(&base).await;
    sqlx::query(
        "INSERT INTO user_profiles(user_id,birth_year,region_code,income_bracket) VALUES($1,2000,'busan',3)
         ON CONFLICT(user_id) DO UPDATE SET birth_year=2000,region_code='busan',income_bracket=3",
    )
    .bind(user)
    .execute(&pool)
    .await
    .unwrap();
    let guidance = get_json(
        &base,
        &format!("/api/v1/programs/{id}/assistant"),
        Some(&token),
    )
    .await;
    let checks = guidance["eligibility"]["checks"].as_array().unwrap();
    let age_check = checks.iter().find(|c| c["label"] == "연령").unwrap();
    assert_eq!(quotes(&age_check["evidence"]), ["만 19세~34세"]);
    let rule_check = checks.iter().find(|c| c["label"] == "필수 조건").unwrap();
    assert_eq!(quotes(&rule_check["evidence"]), ["소득분위 8분위 이하"]);
    let eligibility_section = guidance["sections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["topic"] == "eligibility")
        .unwrap();
    assert!(quotes(&eligibility_section["evidence"]).contains(&"만 19세~34세".to_string()));
    assert!(eligibility_section["source_checked_at"].is_string());

    let asked: Value = Client::new()
        .post(format!("{base}/api/v1/programs/{id}/assistant"))
        .bearer_auth(&token)
        .json(&json!({"question": "필요한 서류랑 마감일 알려줘"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let section = |topic: &str| {
        asked["sections"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["topic"] == topic)
            .unwrap_or_else(|| panic!("{topic} in {asked}"))
            .clone()
    };
    let documents = section("documents");
    assert_eq!(quotes(&documents["evidence"]), ["신분증,"]);
    assert_eq!(documents["evidence"][0]["subject"], "신분증");
    assert_eq!(
        quotes(&section("deadline")["evidence"]),
        ["신청기간: 2026.10.01 ~ 2027.12.31"]
    );

    // Recommendation items carry the eligibility citations, not documents.
    let listed = preview(
        &base,
        json!({"birth_year": 2000, "region_code": "busan", "income_bracket": 3}),
    )
    .await;
    let item = listed["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["program_id"] == id.to_string())
        .expect("approved programme listed");
    let item_quotes = quotes(&item["evidence"]);
    assert!(item_quotes.contains(&"만 19세~34세".to_string()), "{item}");
    assert!(item_quotes.contains(&"소득분위 8분위 이하".to_string()));
    assert!(!item_quotes.contains(&"신분증,".to_string()));

    // A changed source no longer contains the verified span at the same
    // place: the approved value stays, its quote is no longer shown.
    sqlx::query(
        "UPDATE programs SET summary = '부산광역시 거주 만 19세~39세 청년 월세 지원' WHERE id = $1",
    )
    .bind(id)
    .execute(&pool)
    .await
    .unwrap();
    let detail = get_json(&base, &format!("/api/v1/programs/{id}"), None).await;
    assert_eq!(detail["min_age"], 19, "approved value is kept");
    assert!(
        detail["evidence"]
            .as_array()
            .is_none_or(|items| items.iter().all(|e| e["kind"] != "ai_extracted_approved")),
        "superseded source: {}",
        detail["evidence"]
    );
    let guidance = get_json(
        &base,
        &format!("/api/v1/programs/{id}/assistant"),
        Some(&token),
    )
    .await;
    let age_check = guidance["eligibility"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["label"] == "연령")
        .unwrap()
        .clone();
    assert!(age_check.get("evidence").is_none(), "{age_check}");
    cleanup(&pool, &[id]).await;
}

#[tokio::test]
async fn structured_and_official_fields_are_cited_only_while_they_match_the_source() {
    let _catalog_guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let base = start_test_server().await;
    let pool = pool().await;
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO programs (id, program_type, source_type, source_id, title, program_status,
             is_active, min_age, max_age, regions, submission_documents, official_url,
             last_synced_at, raw_payload)
         VALUES ($1, 'policy', 'youth_center', $2, '구조화 출처 검증 공고', 'active', true, 19, 34,
             ARRAY['busan'], '신분증 사본, 주민등록등본', 'http://insecure.example.org/notice', now(), $3)",
    )
    .bind(id)
    .bind(format!("structured-{id}"))
    .bind(json!({
        "sprtTrgtMinAge": "19", "sprtTrgtMaxAge": "34", "zipCd": "26110",
        "sbmsnDcmntCn": "신분증 사본, 주민등록등본"
    }))
    .execute(&pool)
    .await
    .unwrap();

    let detail = get_json(&base, &format!("/api/v1/programs/{id}"), None).await;
    let evidence = &detail["evidence"];
    let age = topic(evidence, "age");
    assert_eq!(age.len(), 1, "{evidence}");
    assert_eq!(age[0]["kind"], "structured_source");
    assert!(
        age[0].get("quote").is_none(),
        "no text to quote: never fabricated"
    );
    assert!(age[0]["source_label"]
        .as_str()
        .unwrap()
        .starts_with("온통청년 공고 데이터"));
    assert!(
        age[0].get("source_url").is_none(),
        "only safe https links are cited"
    );
    assert_eq!(topic(evidence, "region")[0]["kind"], "structured_source");
    let documents = topic(evidence, "documents");
    assert_eq!(documents[0]["kind"], "official_text");
    assert_eq!(documents[0]["quote"], "신분증 사본, 주민등록등본");

    // An operator edit that no longer matches the upstream field drops the
    // citation instead of attributing the edited value to the source.
    sqlx::query("UPDATE programs SET min_age = 20, regions = ARRAY['seoul'] WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let detail = get_json(&base, &format!("/api/v1/programs/{id}"), None).await;
    assert!(topic(&detail["evidence"], "age").is_empty());
    assert!(topic(&detail["evidence"], "region").is_empty());
    cleanup(&pool, &[id]).await;
}

const REASONS: &[&str] = &[
    "not_open",
    "age",
    "region",
    "school",
    "enrollment",
    "scope_unconfirmed",
    "hard_rule",
    "rule_unavailable",
    "not_individual",
    "target_group",
    "outside_youth_scope",
];

fn reason_count(summary: &Value, reason: &str) -> u64 {
    summary["excluded_by_reason"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["reason"] == reason)
        .map_or(0, |r| r["count"].as_u64().unwrap())
}

fn assert_reconciles_summary(summary: &Value) {
    let n = |key: &str| {
        summary[key]
            .as_u64()
            .unwrap_or_else(|| panic!("{key} in {summary}"))
    };
    assert_eq!(n("included") + n("excluded"), n("considered"), "{summary}");
    let by_reason: u64 = REASONS.iter().map(|r| reason_count(summary, r)).sum();
    assert_eq!(by_reason, n("excluded"), "{summary}");
}

fn assert_reconciles(body: &Value) {
    let summary = &body["filter_summary"];
    assert_reconciles_summary(summary);
    let n = |key: &str| summary[key].as_u64().unwrap();
    assert_eq!(
        n("included") as usize,
        body["items"].as_array().unwrap().len()
    );
    assert_eq!(body["total_available"].as_u64(), Some(n("included")));
    let unknown = body["items"]
        .as_array()
        .unwrap()
        .iter()
        // Confirmed = "조건 확인됨" only (audit M1): a `pass` on codes alone
        // or a notice rule still has something to confirm.
        .filter(|i| i["verified"] != true)
        .count();
    assert_eq!(n("included_with_unknowns") as usize, unknown);
    for entry in summary["excluded_by_reason"].as_array().unwrap() {
        let mut keys: Vec<&str> = entry
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(keys, ["count", "label", "reason"], "counts only: {entry}");
        assert!(entry["count"].as_u64().unwrap() > 0);
    }
}

#[tokio::test]
async fn filter_summary_counts_every_considered_programme_once() {
    let _catalog_guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let base = start_test_server().await;
    let pool = pool().await;
    let profile = json!({
        "birth_year": 2000, "region_code": "busan", "enrollment_status": "enrolled", "income_bracket": 9
    });
    let before = preview(&base, profile.clone()).await;
    assert_reconciles(&before);

    let tag = Uuid::new_v4().simple().to_string();
    let mut ids = Vec::new();
    // (title suffix, extra columns) — one programme per exclusion reason
    // plus two listed ones (one pass-capable, one with unknowns). "청년" in
    // the title keeps them inside the default youth scope.
    let rows: &[(&str, &str)] = &[
        (
            "not-open",
            "application_start_at = now() + interval '30 days'",
        ),
        ("age", "max_age = 20"),
        ("region", "regions = ARRAY['seoul']"),
        ("school", "program_type = 'campus_benefit'"),
        (
            "enrollment",
            "normalized_payload = '{\"enrollment_statuses\":[\"graduated\"]}'::jsonb",
        ),
        ("scope", "regions = ARRAY['unknown']"),
        ("hard", "summary = 'hard'"),
        (
            "listed",
            "summary = 'listed', application_method = '온라인 신청', \
             submission_documents = '신분증', screening_method = '서류 심사'",
        ),
        ("broken", "summary = 'broken rule'"),
        // Audience gates (appended so the rule indices above stay put).
        (
            "not-individual",
            "source_type = 'youth_center', \
             raw_payload = '{\"addAplyQlfcCndCn\":\"지원대학 대상 (개인 신청 불가)\"}'::jsonb",
        ),
        (
            "target-group",
            "title = '100세 어르신 장수축하 ' || source_id",
        ),
        ("outside-youth", "title = '주민 생활 지원 ' || source_id"),
    ];
    for (suffix, set) in rows {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO programs (id, program_type, source_type, source_id, title, program_status, is_active)
             VALUES ($1, 'policy', 'manual', $2, $3, 'active', true)",
        )
        .bind(id)
        .bind(format!("filter-{tag}-{suffix}"))
        .bind(format!("청년 필터 요약 제외 확인 {tag} {suffix}"))
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(&format!("UPDATE programs SET {set} WHERE id = $1"))
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        ids.push(id);
    }
    sqlx::query(
        "INSERT INTO eligibility_rules(program_id, rule_json, hard_filter_json, version)
         VALUES ($1, $2, $2, 1)",
    )
    .bind(ids[6])
    .bind(json!({"field": "income_bracket", "op": "lte", "value": 3}))
    .execute(&pool)
    .await
    .unwrap();
    // A rule the engine cannot parse is a data error, not a failed condition.
    sqlx::query(
        "INSERT INTO eligibility_rules(program_id, rule_json, hard_filter_json, version)
         VALUES ($1, '{}'::jsonb, $2, 1)",
    )
    .bind(ids[8])
    .bind(json!({"field": "income_bracket", "op": "bogus_operator", "value": 3}))
    .execute(&pool)
    .await
    .unwrap();

    let after = preview(&base, profile.clone()).await;
    assert_reconciles(&after);
    let (b, a) = (&before["filter_summary"], &after["filter_summary"]);
    assert_eq!(
        a["considered"].as_u64().unwrap() - b["considered"].as_u64().unwrap(),
        12
    );
    assert_eq!(
        a["included"].as_u64().unwrap() - b["included"].as_u64().unwrap(),
        1
    );
    assert_eq!(
        a["included_with_unknowns"].as_u64().unwrap()
            - b["included_with_unknowns"].as_u64().unwrap(),
        1,
        "a manual programme without rules is listed as unknown"
    );
    for reason in REASONS {
        assert_eq!(
            reason_count(a, reason) - reason_count(b, reason),
            1,
            "{reason}: {a}"
        );
    }
    // Privacy: the summary is counts and fixed labels, never excluded titles.
    let text = after["filter_summary"].to_string();
    assert!(!text.contains(&tag), "{text}");
    let listed: Vec<&str> = after["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|i| i["title"].as_str())
        .filter(|t| t.contains(&tag))
        .collect();
    assert_eq!(listed, [format!("청년 필터 요약 제외 확인 {tag} listed")]);

    // Backward compatibility: every pre-existing field keeps its shape.
    for key in [
        "total_available",
        "estimated_monthly",
        "estimated_semester",
        "items",
    ] {
        assert!(after.get(key).is_some(), "{key}");
    }
    let item = after["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["title"].as_str().is_some_and(|t| t.contains(&tag)))
        .unwrap();
    for key in [
        "eligibility_status",
        "program_id",
        "title",
        "program_type",
        "match_score",
        "deadline",
        "reasons",
        "missing_checks",
        "official_url",
    ] {
        assert!(item.get(key).is_some(), "{key} in {item}");
    }
    assert!(
        item.get("evidence").is_none(),
        "no evidence → field omitted"
    );
    assert_eq!(
        (
            &item["application_method"],
            &item["submission_documents"],
            &item["screening_method"]
        ),
        (&json!("온라인 신청"), &json!("신분증"), &json!("서류 심사")),
        "guide text is loaded for listed rows"
    );

    // The scenario bundles report the same single filter.
    let (user, token, _) = create_test_user(&base).await;
    sqlx::query("UPDATE user_profiles SET birth_year = NULL WHERE user_id = $1")
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
    let bundles = get_json(
        &base,
        "/api/v1/assistant/bundles?scenario=job_search",
        Some(&token),
    )
    .await;
    assert!(bundles["filter_summary"].is_null(), "no saved profile yet");
    sqlx::query(
        "UPDATE user_profiles SET birth_year = 2000, region_code = 'busan',
             enrollment_status = 'enrolled', income_bracket = 9 WHERE user_id = $1",
    )
    .bind(user)
    .execute(&pool)
    .await
    .unwrap();
    let bundles = get_json(
        &base,
        "/api/v1/assistant/bundles?scenario=job_search",
        Some(&token),
    )
    .await;
    assert_reconciles_summary(&bundles["filter_summary"]);
    assert_eq!(
        bundles["filter_summary"]["considered"], after["filter_summary"]["considered"],
        "same catalogue, same single filter"
    );
    cleanup(&pool, &ids).await;
}
