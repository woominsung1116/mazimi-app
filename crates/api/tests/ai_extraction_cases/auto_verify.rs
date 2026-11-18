//! Auto-verify: a fact the rule parser reads identically from the same quote
//! is applied through the shared apply path with provenance; everything else
//! stays with a human. Kill switch, run cap, audit sample, revert and the
//! ingestion queue. Mock provider only.
use super::*;
use mazimi_worker::ai_extraction::{
    auto::AutoApplyConfig,
    crosscheck::PARSER_VERSION,
    facts::FactType,
    job::{RunSummary, Trigger},
};

fn auto(enabled: bool, types: &[FactType], audit_percent: u8, max_per_run: usize) -> JobConfig {
    JobConfig {
        auto_apply: AutoApplyConfig {
            enabled,
            types: types.to_vec(),
            max_per_run,
            audit_percent,
            audit_seed: "test-seed".into(),
        },
        ..JobConfig::default()
    }
}

const DEFAULT_TYPES: &[FactType] = &[
    FactType::Age,
    FactType::ApplicationWindow,
    FactType::Document,
];

async fn run(
    pool: &sqlx::PgPool,
    provider: &MockExtractionProvider,
    ids: &[Uuid],
    config: JobConfig,
) -> RunSummary {
    run_batch(
        pool,
        provider,
        &JobConfig {
            only_programs: Some(ids.to_vec()),
            ..config
        },
    )
    .await
    .unwrap()
}

#[derive(Debug, sqlx::FromRow)]
struct Row {
    fact_key: String,
    status: String,
    applied_by: Option<String>,
    reviewer_id: Option<Uuid>,
    auto_rule: Option<String>,
    auto_parser_version: Option<String>,
    auto_run_id: Option<Uuid>,
    audit_sample: bool,
    auto_check: Option<Value>,
}

async fn rows(pool: &sqlx::PgPool, id: Uuid) -> Vec<Row> {
    sqlx::query_as(
        "SELECT fact_key, status, applied_by, reviewer_id, auto_rule, auto_parser_version,
                auto_run_id, audit_sample, auto_check
         FROM program_ai_extraction_candidates
         WHERE program_id = $1 AND status <> 'superseded' ORDER BY fact_key",
    )
    .bind(id)
    .fetch_all(pool)
    .await
    .unwrap()
}

fn row<'a>(rows: &'a [Row], key: &str) -> &'a Row {
    rows.iter()
        .find(|r| r.fact_key == key)
        .unwrap_or_else(|| panic!("{key} in {rows:?}"))
}

type Columns = (
    Option<i32>,
    Option<i32>,
    Option<chrono::DateTime<Utc>>,
    Option<chrono::DateTime<Utc>>,
);

async fn columns(pool: &sqlx::PgPool, id: Uuid) -> Columns {
    sqlx::query_as(
        "SELECT min_age, max_age, application_start_at, application_end_at FROM programs WHERE id = $1",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn documents(pool: &sqlx::PgPool, id: Uuid) -> Vec<(String, Option<String>)> {
    sqlx::query_as(
        "SELECT document_name, source FROM program_documents WHERE program_id = $1 ORDER BY document_name",
    )
    .bind(id)
    .fetch_all(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn ai_equal_to_rule_is_applied_through_the_shared_path_with_provenance() {
    let _ai_guard = AI_JOB_LOCK.lock().await;
    let base = start_test_server().await;
    let pool = pool().await;
    let id = program(&pool).await;

    let summary = run(&pool, &mock(), &[id], auto(true, DEFAULT_TYPES, 100, 25)).await;
    assert_eq!(summary.auto.applied, 4, "{summary:?}");
    assert_eq!(summary.auto.audit_sampled, 4, "100% audit sample");
    assert!(summary.auto_apply_enabled);

    let (min, max, start, end) = columns(&pool, id).await;
    assert_eq!((min, max), (Some(19), Some(34)));
    assert_eq!(start.unwrap().to_rfc3339(), "2099-03-01T15:00:00+00:00");
    assert_eq!(end.unwrap().to_rfc3339(), "2099-04-30T14:59:59+00:00");
    assert_eq!(
        documents(&pool, id).await,
        vec![
            ("신분증".into(), Some("ai_extraction_auto".into())),
            ("주민등록등본".into(), Some("ai_extraction_auto".into())),
        ]
    );
    let overrides: Vec<Option<Uuid>> = sqlx::query_scalar(
        "SELECT approved_by FROM program_ai_field_overrides WHERE program_id = $1",
    )
    .bind(id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(overrides, vec![None; 4], "no human approver on overrides");

    let rows = rows(&pool, id).await;
    let age = row(&rows, "age");
    assert_eq!(
        (
            age.status.as_str(),
            age.applied_by.as_deref(),
            age.reviewer_id,
            age.auto_rule.as_deref(),
            age.auto_parser_version.as_deref(),
            age.auto_run_id,
            age.audit_sample
        ),
        (
            "approved",
            Some("auto_verified"),
            None,
            Some("age_range"),
            Some(PARSER_VERSION),
            Some(summary.run_id),
            true
        )
    );
    assert_eq!(
        row(&rows, "application_window").auto_rule.as_deref(),
        Some("window_range")
    );
    // Rule-verified but not in the enabled scope: a human decides.
    for key in ["income", "region"] {
        let fact = row(&rows, key);
        assert_eq!(fact.status, "pending", "{key}");
        let check = fact.auto_check.as_ref().unwrap();
        assert_eq!(check["verdict"], "agree", "{key}");
        assert_eq!(check["hold"], "type_not_enabled", "{key}");
    }
    assert_eq!(row(&rows, "employment").status, "flagged");

    let persisted: (i32, i32, i32, bool) = sqlx::query_as(
        "SELECT auto_applied, auto_held, audit_sampled, auto_apply_enabled
         FROM program_ai_extraction_runs WHERE id = $1",
    )
    .bind(summary.run_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(persisted, (4, 2, 4, true));

    // Admin views: exceptions by default, auto-applied and audit filters.
    let (_, token) = create_test_admin(&base).await;
    let client = Client::new();
    let list = |status: &'static str| {
        let (client, token, base) = (client.clone(), token.clone(), base.clone());
        async move {
            let body: Value = client
                .get(format!(
                    "{base}/api/v1/admin/ai-extraction/candidates?status={status}&limit=200"
                ))
                .bearer_auth(&token)
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            body["items"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|item| item["program_id"] == json!(id))
                .cloned()
                .collect::<Vec<Value>>()
        }
    };
    let exceptions = list("pending").await;
    assert_eq!(exceptions.len(), 2, "only the exceptions: {exceptions:?}");
    assert!(exceptions
        .iter()
        .all(|i| i["auto_check"]["hold"] == "type_not_enabled"));
    assert_eq!(list("auto_applied").await.len(), 4);
    let audit = list("audit").await;
    assert_eq!(audit.len(), 4);
    let audited = audit[0]["id"].as_str().unwrap().to_string();
    let confirm = client
        .post(format!(
            "{base}/api/v1/admin/ai-extraction/candidates/{audited}/audit"
        ))
        .bearer_auth(&token)
        .json(&json!({"note": "원문과 대조해 자동 반영 값을 확인했습니다."}))
        .send()
        .await
        .unwrap();
    assert_eq!(confirm.status(), 200);
    assert_eq!(
        list("audit").await.len(),
        3,
        "a confirmed sample leaves the audit queue"
    );
    let again = client
        .post(format!(
            "{base}/api/v1/admin/ai-extraction/candidates/{audited}/audit"
        ))
        .bearer_auth(&token)
        .json(&json!({"note": "원문과 대조해 자동 반영 값을 확인했습니다."}))
        .send()
        .await
        .unwrap();
    assert_eq!(again.status(), 409);

    // The program panel shows the marker and the rule reading.
    let panel: Value = client
        .get(format!("{base}/api/v1/admin/programs/{id}/ai-extraction"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let age = panel["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["fact_key"] == "age")
        .unwrap();
    assert_eq!(age["applied_by"], "auto_verified");
    assert_eq!(
        age["auto_check"]["parsed"],
        json!({"min_age": 19, "max_age": 34})
    );
    cleanup(&pool, &[id]).await;
}

/// A notice that still prints last round's dates: the rule parser agrees,
/// but filling an end date already past would close the programme that
/// night. The window waits for a human; the other facts still apply.
#[tokio::test]
async fn an_auto_window_whose_end_has_passed_is_held_for_review() {
    let _ai_guard = AI_JOB_LOCK.lock().await;
    let _base = start_test_server().await;
    let pool = pool().await;
    let id = program(&pool).await;
    sqlx::query("UPDATE programs SET application_method = '온라인 신청 (신청기간: 2025.03.02 ~ 2025.04.30)' WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let past = MockExtractionProvider::new("mock-past-window-v1", |request| {
        let text = &request.source_text;
        let mut answer = mock_answer(text);
        answer["application_window"] = json!({
            "kind": "dated", "start_date": "2025-03-02", "end_date": "2025-04-30",
            "confidence": "HIGH", "evidence": evidence(text, "신청기간: 2025.03.02 ~ 2025.04.30")
        });
        Ok(answer)
    });
    let summary = run(&pool, &past, &[id], auto(true, DEFAULT_TYPES, 0, 25)).await;
    assert_eq!(summary.auto.applied, 3, "{summary:?}");
    let (min, max, start, end) = columns(&pool, id).await;
    assert_eq!((min, max, start, end), (Some(19), Some(34), None, None));
    let rows = rows(&pool, id).await;
    let window = row(&rows, "application_window");
    assert_eq!(window.status, "pending");
    let check = window.auto_check.as_ref().unwrap();
    assert_eq!(check["verdict"], "agree", "{check}");
    assert_eq!(check["hold"], "window_end_passed", "{check}");
    cleanup(&pool, &[id]).await;
}

/// A pin made against an earlier payload no longer applies. The automatic
/// actor replaces it, so the value it fills survives the next sync instead
/// of being reset while the candidate reads `approved`.
#[tokio::test]
async fn auto_apply_replaces_a_pin_of_an_earlier_payload() {
    let _ai_guard = AI_JOB_LOCK.lock().await;
    let _base = start_test_server().await;
    let pool = pool().await;
    let id = program(&pool).await;
    let cache: Uuid = sqlx::query_scalar("INSERT INTO program_ai_extraction_cache(program_id,source_type,source_hash,provider,model,prompt_version,schema_version,request_hash,source_text,status,response_json)
        VALUES($1,'test',repeat('a',64),'mock','m','p',1,repeat('b',64),'x','succeeded','{}') RETURNING id")
        .bind(id).fetch_one(&pool).await.unwrap();
    let old: Uuid = sqlx::query_scalar("INSERT INTO program_ai_extraction_candidates(cache_id,program_id,source_hash,provider,model,prompt_version,schema_version,fact_key,fact_type,value,evidence,confidence,status,reviewed_at,review_note)
        VALUES($1,$2,repeat('a',64),'mock','m','p',1,'age_old','age','{}','{}','HIGH','approved',now(),'earlier payload approval') RETURNING id")
        .bind(cache).bind(id).fetch_one(&pool).await.unwrap();
    // Pinned against a payload the programme no longer has.
    sqlx::query("INSERT INTO program_ai_field_overrides(program_id,field,int_value,raw_payload_md5,candidate_id)
        VALUES($1,'min_age',25,md5('earlier payload'),$2)")
        .bind(id).bind(old).execute(&pool).await.unwrap();

    let summary = run(&pool, &mock(), &[id], auto(true, DEFAULT_TYPES, 0, 25)).await;
    assert!(summary.auto.applied >= 1, "{summary:?}");
    assert_eq!(row(&rows(&pool, id).await, "age").status, "approved");
    let (value, current): (Option<i32>, bool) = sqlx::query_as(
        "SELECT o.int_value, o.raw_payload_md5 = md5(ingestion_stable_payload(p.source_type, p.raw_payload)::text)
         FROM program_ai_field_overrides o JOIN programs p ON p.id = o.program_id
         WHERE o.program_id = $1 AND o.field = 'min_age'",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((value, current), (Some(19), true));
    cleanup(&pool, &[id]).await;
}

#[tokio::test]
async fn ai_not_equal_to_rule_stays_pending_with_the_disagreement() {
    let _ai_guard = AI_JOB_LOCK.lock().await;
    let _base = start_test_server().await;
    let pool = pool().await;
    let id = program(&pool).await;
    // Valid by the validator (34 and both dates are in the quotes) but not
    // what the sentences say.
    let wrong = MockExtractionProvider::new("mock-disagree-v1", |request| {
        let text = &request.source_text;
        Ok(json!({
            "age": {"min_age": 34, "max_age": null, "basis": "man_age", "confidence": "HIGH",
                    "evidence": evidence(text, "만 19세~34세")},
            "documents": [],
            "application_window": {"kind": "dated", "start_date": "2099-04-30", "end_date": null,
                                   "confidence": "HIGH", "evidence": evidence(text, "신청기간: 2099.03.02 ~ 2099.04.30")}
        }))
    });
    let summary = run(&pool, &wrong, &[id], auto(true, DEFAULT_TYPES, 0, 25)).await;
    assert_eq!(summary.auto.applied, 0, "{summary:?}");
    assert_eq!(columns(&pool, id).await, (None, None, None, None));
    let rows = rows(&pool, id).await;
    for key in ["age", "application_window"] {
        let fact = row(&rows, key);
        assert_eq!(fact.status, "pending", "{key}: the validator passed it");
        let check = fact.auto_check.as_ref().unwrap();
        assert_eq!(check["verdict"], "disagree", "{key}");
        assert!(
            check["reason"].as_str().unwrap().contains("rule read"),
            "{check}"
        );
        assert!(fact.applied_by.is_none());
        assert!(
            check.get("hold").is_none(),
            "{key}: a disagreeing fact never reaches the apply step: {check}"
        );
    }
    cleanup(&pool, &[id]).await;
}

#[tokio::test]
async fn kill_switch_off_applies_nothing_and_says_why() {
    let _ai_guard = AI_JOB_LOCK.lock().await;
    let _base = start_test_server().await;
    let pool = pool().await;
    let id = program(&pool).await;
    let summary = run(&pool, &mock(), &[id], auto(false, DEFAULT_TYPES, 100, 25)).await;
    assert_eq!(summary.auto.applied, 0);
    assert_eq!(
        summary.auto.verified, 6,
        "the cross-check still runs: {summary:?}"
    );
    assert!(!summary.auto_apply_enabled);
    assert_eq!(columns(&pool, id).await, (None, None, None, None));
    assert!(documents(&pool, id).await.is_empty());
    let rows = rows(&pool, id).await;
    assert!(rows.iter().all(|r| r.applied_by.is_none()));
    assert_eq!(
        row(&rows, "age").auto_check.as_ref().unwrap()["hold"],
        "auto_apply_disabled"
    );
    // Same source again: the hold is not a change, nothing is rewritten.
    let again = run(&pool, &mock(), &[id], auto(false, DEFAULT_TYPES, 100, 25)).await;
    assert_eq!(again.candidates_written, 0, "{again:?}");
    cleanup(&pool, &[id]).await;
}

#[tokio::test]
async fn an_authoritative_value_set_meanwhile_keeps_the_fact_pending() {
    let _ai_guard = AI_JOB_LOCK.lock().await;
    let _base = start_test_server().await;
    let pool = pool().await;
    let id = program(&pool).await;
    run(&pool, &mock(), &[id], auto(false, DEFAULT_TYPES, 0, 25)).await;
    // The collector now has a structured age that differs from the text.
    sqlx::query("UPDATE programs SET min_age = 20 WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let summary = run(&pool, &mock(), &[id], auto(true, DEFAULT_TYPES, 0, 25)).await;
    assert_eq!(
        summary.auto.applied, 3,
        "window and two documents: {summary:?}"
    );
    let (min, max, ..) = columns(&pool, id).await;
    assert_eq!(
        (min, max),
        (Some(20), None),
        "the authoritative value stays"
    );
    let rows = rows(&pool, id).await;
    let age = row(&rows, "age");
    assert_eq!(age.status, "pending");
    assert_eq!(
        age.auto_check.as_ref().unwrap()["hold"],
        "overwrite_requires_confirmation"
    );
    cleanup(&pool, &[id]).await;
}

#[tokio::test]
async fn a_human_rejection_of_the_same_fact_wins_over_auto_apply() {
    let _ai_guard = AI_JOB_LOCK.lock().await;
    let base = start_test_server().await;
    let pool = pool().await;
    let id = program(&pool).await;
    // Two models produce the same age fact; a reviewer rejects the first.
    run(&pool, &mock(), &[id], auto(false, DEFAULT_TYPES, 0, 25)).await;
    let (_, token) = create_test_admin(&base).await;
    let first = candidate_id(&candidates(&pool, id).await, "age");
    let (status, body) = post_review(
        &base,
        &token,
        id,
        json!({"decisions": [{"candidate_id": first, "decision": "reject"}], "note": NOTE}),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let other = MockExtractionProvider::new("mock-second-model", |request| {
        Ok(mock_answer(&request.source_text))
    });
    let summary = run(&pool, &other, &[id], auto(true, DEFAULT_TYPES, 0, 25)).await;
    assert_eq!(columns(&pool, id).await.0, None, "{summary:?}");
    let held: Value = sqlx::query_scalar(
        "SELECT auto_check FROM program_ai_extraction_candidates
         WHERE program_id = $1 AND fact_key = 'age' AND model = 'mock-second-model'",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(held["hold"], "human_decision_exists");
    cleanup(&pool, &[id]).await;
}

#[tokio::test]
async fn the_run_cap_holds_the_rest_and_raises_an_operational_event() {
    let _ai_guard = AI_JOB_LOCK.lock().await;
    let _base = start_test_server().await;
    let pool = pool().await;
    let id = program(&pool).await;
    let summary = run(&pool, &mock(), &[id], auto(true, DEFAULT_TYPES, 0, 1)).await;
    assert_eq!(
        (summary.auto.applied, summary.auto.cap_skips),
        (1, 3),
        "{summary:?}"
    );
    let open = |pool: sqlx::PgPool| async move {
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM operational_events
             WHERE fingerprint = 'ai_extraction:auto_apply_spike' AND status = 'open'",
        )
        .fetch_one(&pool)
        .await
        .unwrap()
    };
    assert_eq!(open(pool.clone()).await, 1);
    let held = rows(&pool, id)
        .await
        .into_iter()
        .filter(|r| {
            r.auto_check
                .as_ref()
                .is_some_and(|c| c["hold"] == "run_cap_reached")
        })
        .count();
    assert_eq!(held, 3);
    // The next run under the cap applies the rest and resolves the event.
    let next = run(&pool, &mock(), &[id], auto(true, DEFAULT_TYPES, 0, 25)).await;
    assert_eq!(next.auto.applied, 3, "{next:?}");
    assert_eq!(open(pool.clone()).await, 0);
    cleanup(&pool, &[id]).await;
}

async fn revert(base: &str, token: &str, body: Value) -> (u16, Value) {
    let response = Client::new()
        .post(format!("{base}/api/v1/admin/ai-extraction/revert"))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    (status, response.json().await.unwrap_or(Value::Null))
}

#[tokio::test]
async fn revert_restores_prior_values_for_a_whole_run() {
    let _ai_guard = AI_JOB_LOCK.lock().await;
    let base = start_test_server().await;
    let pool = pool().await;
    let id = program(&pool).await;
    let types = [
        FactType::Age,
        FactType::ApplicationWindow,
        FactType::Document,
        FactType::Income,
    ];
    let summary = run(&pool, &mock(), &[id], auto(true, &types, 0, 25)).await;
    assert_eq!(summary.auto.applied, 5, "{summary:?}");
    // A document added by hand after the run must survive the revert.
    sqlx::query(
        "INSERT INTO program_documents (program_id, document_name, is_required, sort_order)
         VALUES ($1, '여권', true, 99)",
    )
    .bind(id)
    .execute(&pool)
    .await
    .unwrap();
    let rule_count = |pool: sqlx::PgPool| async move {
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM eligibility_rules WHERE program_id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap()
    };
    assert_eq!(rule_count(pool.clone()).await, 1);
    assert_ne!(columns(&pool, id).await, (None, None, None, None));

    let (_, user_token, _) = create_test_user(&base).await;
    let note = "자동 반영 규칙 오류로 이 실행 전체를 되돌립니다.";
    let (status, _) = revert(
        &base,
        &user_token,
        json!({"run_id": summary.run_id, "note": note}),
    )
    .await;
    assert_eq!(status, 403, "admins only");
    let (admin, token) = create_test_admin(&base).await;
    let (status, body) = revert(
        &base,
        &token,
        json!({"run_id": summary.run_id, "note": "짧음"}),
    )
    .await;
    assert_eq!(status, 400, "{body}");

    let (status, dry) = revert(
        &base,
        &token,
        json!({"run_id": summary.run_id, "note": note, "dry_run": true}),
    )
    .await;
    assert_eq!(status, 200, "{dry}");
    assert_eq!(
        (
            dry["matched"].as_u64(),
            dry["reverted"].as_array().map(Vec::len)
        ),
        (Some(5), Some(0))
    );
    assert_ne!(
        columns(&pool, id).await,
        (None, None, None, None),
        "dry run changes nothing"
    );

    let (status, report) = revert(
        &base,
        &token,
        json!({"run_id": summary.run_id, "note": note}),
    )
    .await;
    assert_eq!(status, 200, "{report}");
    assert_eq!(report["reverted"].as_array().unwrap().len(), 5, "{report}");
    assert!(report["skipped"].as_array().unwrap().is_empty(), "{report}");
    assert_eq!(columns(&pool, id).await, (None, None, None, None));
    assert_eq!(
        documents(&pool, id).await,
        vec![("여권".into(), None)],
        "only auto-written documents are removed"
    );
    assert_eq!(
        rule_count(pool.clone()).await,
        0,
        "the auto rule version is gone"
    );
    let overrides: i64 =
        sqlx::query_scalar("SELECT count(*) FROM program_ai_field_overrides WHERE program_id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(overrides, 0);
    let reverted: Vec<(String, Option<Uuid>)> = sqlx::query_as(
        "SELECT status, reverted_by FROM program_ai_extraction_candidates
         WHERE program_id = $1 AND applied_by = 'auto_verified'",
    )
    .bind(id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(reverted.len(), 5);
    assert!(reverted
        .iter()
        .all(|(s, by)| s == "reverted" && *by == Some(admin)));

    // Idempotent, and a reverted fact is never applied again.
    let (_, again) = revert(
        &base,
        &token,
        json!({"run_id": summary.run_id, "note": note}),
    )
    .await;
    assert_eq!(again["matched"], 0);
    let rerun = run(&pool, &mock(), &[id], auto(true, &types, 0, 25)).await;
    assert_eq!(rerun.auto.applied, 0, "{rerun:?}");
    assert_eq!(columns(&pool, id).await, (None, None, None, None));
    // A source-text change (new hash, new candidates) must not bring the
    // reverted values back either.
    sqlx::query("UPDATE programs SET screening_method = '서류 심사' WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let changed = run(&pool, &mock(), &[id], auto(true, &types, 0, 25)).await;
    assert_eq!(changed.calls, 1, "{changed:?}");
    assert_eq!(changed.auto.applied, 0, "{changed:?}");
    assert_eq!(columns(&pool, id).await, (None, None, None, None));
    cleanup(&pool, &[id]).await;
}

#[tokio::test]
async fn revert_by_parser_version_keeps_values_edited_since() {
    let _ai_guard = AI_JOB_LOCK.lock().await;
    let base = start_test_server().await;
    let pool = pool().await;
    let id = program(&pool).await;
    run(&pool, &mock(), &[id], auto(true, DEFAULT_TYPES, 0, 25)).await;
    // An admin corrected the maximum age by hand after the auto-apply.
    sqlx::query("UPDATE programs SET max_age = 39 WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let (_, token) = create_test_admin(&base).await;
    let (status, report) = revert(
        &base,
        &token,
        json!({"parser_version": PARSER_VERSION, "rule": "age_range",
               "note": "나이 파서 검증 중 문제가 발견되어 되돌립니다."}),
    )
    .await;
    assert_eq!(status, 200, "{report}");
    let mine: Vec<&Value> = report["kept"].as_array().unwrap().iter().collect();
    assert_eq!(mine.len(), 1, "{report}");
    assert_eq!(mine[0]["target"], "programs.max_age");
    let (min, max, start, _) = columns(&pool, id).await;
    assert_eq!((min, max), (None, Some(39)), "the hand edit is kept");
    assert!(start.is_some(), "other rules are untouched");
    cleanup(&pool, &[id]).await;
}

#[tokio::test]
async fn ingestion_text_changes_enqueue_and_the_queue_drain_skips_unchanged_sources() {
    let _ai_guard = AI_JOB_LOCK.lock().await;
    let _base = start_test_server().await;
    let pool = pool().await;
    let id = program(&pool).await;
    let queued = |pool: sqlx::PgPool| async move {
        sqlx::query_scalar::<_, Option<String>>(
            "SELECT reason FROM program_ai_extraction_queue WHERE program_id = $1",
        )
        .bind(id)
        .fetch_optional(&pool)
        .await
        .unwrap()
        .flatten()
    };
    assert_eq!(queued(pool.clone()).await.as_deref(), Some("created"));

    let provider = mock();
    let queue_config = || JobConfig {
        queue_only: true,
        trigger: Trigger::Queue,
        ..auto(false, DEFAULT_TYPES, 0, 25)
    };
    let drained = run(&pool, &provider, &[id], queue_config()).await;
    assert_eq!(
        (drained.calls, drained.queue_drained),
        (1, 1),
        "{drained:?}"
    );
    assert_eq!(queued(pool.clone()).await, None);
    let trigger: String =
        sqlx::query_scalar("SELECT trigger FROM program_ai_extraction_runs WHERE id = $1")
            .bind(drained.run_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(trigger, "queue");

    // Non-text columns and an identical rewrite enqueue nothing.
    sqlx::query("UPDATE programs SET updated_at = now(), summary = summary WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(queued(pool.clone()).await, None);
    let idle = run(&pool, &provider, &[id], queue_config()).await;
    assert_eq!(idle.selected, 0, "an empty queue selects nothing");

    // A text change is queued and served first; an unchanged masked source
    // is a cache hit, not a model call.
    sqlx::query(
        "UPDATE programs SET raw_payload = raw_payload || '{\"_internal\": \"x\"}' WHERE id = $1",
    )
    .bind(id)
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(
        queued(pool.clone()).await.as_deref(),
        Some("source_changed")
    );
    let cached = run(&pool, &provider, &[id], queue_config()).await;
    assert_eq!(cached.calls, 0, "masked source unchanged: {cached:?}");
    assert_eq!(cached.queue_drained, 1);
    assert_eq!(provider.calls(), 1);

    sqlx::query("UPDATE programs SET summary = summary || ' (2차 공고)' WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let changed = run(&pool, &provider, &[id], queue_config()).await;
    assert_eq!(changed.calls, 1, "{changed:?}");
    assert_eq!(provider.calls(), 2);
    cleanup(&pool, &[id]).await;
}
