//! Reviewed eligibility conditions (`program_condition_reviews`) applied to
//! the recommendation list ("조건 확인됨" first, stale text, reviewed hides)
//! and the admin routes' access control, text binding and RLS.

use super::*;
use mazimi_core::condition_review::{self, store};
use std::collections::HashMap;

async fn pool() -> sqlx::PgPool {
    PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .unwrap()
}

/// A busan 온통청년 row (19-39세) in a throwaway category.
async fn youth_row(pool: &sqlx::PgPool, id: Uuid, category: &str, title: &str) -> Value {
    let raw = json!({
        "plcyNm": title,
        "plcyExplnCn": "부산 거주 미취업 청년의 구직 활동 지원",
        "addAplyQlfcCndCn": "만 19~39세 부산 거주 미취업 청년",
        "plcyAplyMthdCn": "온라인으로 본인이 직접 신청",
        "sprtTrgtMinAge": "19", "sprtTrgtMaxAge": "39", "zipCd": "26410",
        "jobCd": "0013003", "aplyPrdSeCd": "0057001", "aplyYmd": "20260101 ~ 20271231"
    });
    sqlx::query("INSERT INTO programs(id,program_type,source_type,source_id,title,program_status,regions,min_age,max_age,is_active,raw_payload) VALUES($1,$2,'youth_center',$1::text,$3,'active',ARRAY['busan'],19,39,true,$4)")
        .bind(id).bind(category).bind(title).bind(&raw).execute(pool).await.unwrap();
    raw
}

fn seoul_date(offset_days: i64) -> String {
    mazimi_core::seoul_date(Utc::now() + Duration::days(offset_days))
        .format("%Y-%m-%d")
        .to_string()
}

/// A complete review of `id` with a reviewed employment rule and a dated
/// window, written through the same validation as the importer.
async fn review(pool: &sqlx::PgPool, id: Uuid, raw: &Value, window: Value) {
    let input: store::ReviewInput = serde_json::from_value(json!({
        "source_type": "youth_center", "source_id": id.to_string(),
        "text_hash": condition_review::text_hash("youth_center", raw).unwrap(),
        "status": "complete",
        "rules": [{"id": "R1", "field": "employment_status", "op": "in",
                   "value": ["unemployed", "preparing"], "evidence": "부산 거주 미취업 청년"}],
        "window": window,
        "coverage": {"labels": condition_review::coverage::segments("youth_center", raw)
            .unwrap()
            .into_iter()
            .map(|s| (s.seg_id, json!({"label": "condition", "rule_refs": ["R1"]})))
            .collect::<serde_json::Map<String, Value>>()},
        "reviewer": "test:A+B"
    }))
    .unwrap();
    let mut connection = pool.acquire().await.unwrap();
    let written = store::upsert(&mut connection, &input, "import:test", true)
        .await
        .unwrap();
    assert!(written.is_ok(), "{written:?}");
    let hash = mazimi_core::applicant_review::text_hash("youth_center", raw).unwrap();
    sqlx::query("INSERT INTO program_applicant_reviews(source_type,source_id,class,evidence,reviewer,text_hash) VALUES('youth_center',$1::text,'individual','온라인으로 본인이 직접 신청','test:A+B',$2)")
        .bind(id).bind(hash).execute(pool).await.unwrap();
}

fn open_window() -> Value {
    json!({"kind": "dated", "opens_at": seoul_date(-1), "closes_at": seoul_date(10),
           "source": "review", "evidence": "온라인으로 본인이 직접 신청"})
}

fn item(body: &Value, id: Uuid) -> Option<(usize, Value)> {
    let expected = id.to_string();
    body["items"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
        .find(|(_, item)| item["program_id"].as_str() == Some(expected.as_str()))
        .map(|(i, item)| (i, item.clone()))
}

fn excluded(body: &Value, reason: &str) -> u64 {
    body["filter_summary"]["excluded_by_reason"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["reason"] == reason)
        .map_or(0, |r| r["count"].as_u64().unwrap())
}

async fn cleanup(pool: &sqlx::PgPool, category: &str) {
    for table in ["program_condition_reviews", "program_applicant_reviews"] {
        sqlx::query(&format!("DELETE FROM {table} WHERE source_type='youth_center' AND source_id IN (SELECT id::text FROM programs WHERE program_type=$1)"))
            .bind(category).execute(pool).await.unwrap();
    }
    // The review logs are append-only (a trigger rejects DELETE); their
    // rows name throwaway source ids and stay.
    sqlx::query("DELETE FROM programs WHERE program_type=$1")
        .bind(category)
        .execute(pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn reviewed_conditions_verify_rank_first_go_stale_and_hide_on_reviewed_facts() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let base = start_test_server().await;
    let pool = pool().await;
    let client = Client::new();
    let category = format!("condition_{}", Uuid::new_v4().simple());
    let [verified, stale, past, unreviewed] = [(); 4].map(|_| Uuid::new_v4());
    let mut raws = HashMap::new();
    for (id, title) in [
        (verified, "부산 청년 구직 지원 검증"),
        (stale, "부산 청년 면접 지원 검증"),
        (past, "부산 청년 자격증 지원 검증"),
        (unreviewed, "부산 청년 교통비 지원 검증"),
    ] {
        raws.insert(id, youth_row(&pool, id, &category, title).await);
    }

    let list = |body: Value| {
        let client = client.clone();
        let base = base.clone();
        async move {
            let response = client
                .post(format!("{base}/api/v1/recommend/preview"))
                .json(&body)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), 200);
            response.json::<Value>().await.unwrap()
        }
    };
    let jobless =
        json!({"birth_year": 2000, "region_code": "busan", "employment_status": "unemployed"});
    let before = list(jobless.clone()).await;
    for id in [verified, stale, past, unreviewed] {
        let (_, row) = item(&before, id).expect("listed before review");
        assert_eq!(row["verified"], false, "{row}");
    }

    review(&pool, verified, &raws[&verified], open_window()).await;
    review(&pool, stale, &raws[&stale], open_window()).await;
    review(
        &pool,
        past,
        &raws[&past],
        json!({"kind": "dated", "opens_at": seoul_date(-30), "closes_at": seoul_date(-2),
               "evidence": "온라인으로 본인이 직접 신청"}),
    )
    .await;
    // The notice changed after it was reviewed.
    sqlx::query("UPDATE programs SET raw_payload = raw_payload || '{\"plcySprtCn\":\"월 50만원\"}' WHERE id=$1")
        .bind(stale).execute(&pool).await.unwrap();

    let after = list(jobless.clone()).await;
    // VERIFIED_SECTION_ENABLED is off here: the row keeps today's status,
    // checks and place (merge review M2; the flag-on display and order are
    // unit-tested in services::condition_reviews), and no eligibility claim
    // is served: `verified` false, `verified_count` 0 (review 2026-10-21 M2).
    let (_, row) = item(&after, verified).expect("verified row listed");
    let (_, unreviewed_row) = item(&after, unreviewed).unwrap();
    assert_eq!(row["verified"], false, "{row}");
    assert_eq!(
        row["eligibility_status"],
        unreviewed_row["eligibility_status"]
    );
    assert_eq!(row["missing_checks"], unreviewed_row["missing_checks"]);
    assert_eq!(row["remaining_checks"], json!([]));
    assert_eq!(row["window"]["kind"], "dated");
    assert_eq!(row["window"]["open_now"], true);
    assert_eq!(after["verified_count"], 0);
    assert_eq!(
        after["verified_section_enabled"], false,
        "flag off by default"
    );

    let (_, row) = item(&after, stale).expect("stale review hides nothing");
    assert_eq!(row["verified"], false);
    assert_eq!(row["window"], Value::Null);
    assert_eq!(row["remaining_checks"][0]["kind"], "conditions_changed");
    assert_eq!(
        row["missing_checks"][0],
        condition_review::labels::CONDITIONS_CHANGED,
        "old clients see the remaining checks"
    );

    assert!(item(&after, past).is_none(), "a past reviewed window hides");
    assert_eq!(
        excluded(&after, "not_open"),
        excluded(&before, "not_open") + 1
    );

    let (_, row) = item(&after, unreviewed).unwrap();
    assert_eq!(row["verified"], false);
    assert_eq!(row["remaining_checks"][0]["kind"], "conditions_unreviewed");

    // A reviewed rule the profile contradicts hides; an unanswered one asks.
    let employed =
        list(json!({"birth_year": 2000, "region_code": "busan", "employment_status": "employed"}))
            .await;
    assert!(item(&employed, verified).is_none());
    assert_eq!(
        excluded(&employed, "hard_rule"),
        excluded(&before, "hard_rule") + 1
    );
    let unknown = list(json!({"birth_year": 2000, "region_code": "busan"})).await;
    let (_, row) = item(&unknown, verified).expect("missing data never hides");
    assert_eq!(row["verified"], false);
    let question = row["remaining_checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["kind"] == "profile")
        .unwrap_or_else(|| panic!("{row}"));
    assert_eq!(question["question_id"], "employment_status");
    assert!(row["missing_checks"]
        .as_array()
        .unwrap()
        .contains(&question["label"]));

    let summary = &after["filter_summary"];
    assert_eq!(
        summary["included"].as_u64().unwrap() + summary["excluded"].as_u64().unwrap(),
        summary["considered"].as_u64().unwrap(),
        "{summary}"
    );
    cleanup(&pool, &category).await;
}

#[tokio::test]
async fn condition_review_admin_route_is_admin_only_and_bound_to_the_text() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let base = start_test_server().await;
    let pool = pool().await;
    let client = Client::new();
    let category = format!("condition_admin_{}", Uuid::new_v4().simple());
    let id = Uuid::new_v4();
    let raw = youth_row(&pool, id, &category, "부산 청년 구직 지원 관리 검증").await;
    let hash = condition_review::text_hash("youth_center", &raw).unwrap();
    let (_, user_token, _) = create_test_user(&base).await;
    let (admin_id, admin_token) = create_test_admin(&base).await;
    let url = format!("{base}/api/v1/admin/programs/{id}/condition-review");
    let list_url = format!("{base}/api/v1/admin/condition-reviews");
    let body = json!({
        "status": "complete",
        "rules": [{"id": "R1", "field": "employment_status", "op": "in",
                   "value": ["unemployed"], "evidence": "부산 거주 미취업 청년"}],
        "window": {"kind": "rolling", "evidence": "온라인으로 본인이 직접 신청"},
        // Every segment of the current text labelled (audit H1).
        "coverage": {"labels": condition_review::coverage::segments("youth_center", &raw)
            .unwrap()
            .into_iter()
            .map(|s| (s.seg_id, json!({"label": "non_condition"})))
            .collect::<serde_json::Map<String, Value>>()},
        "reviewer": "curator:A+B",
        "expected_text_hash": hash
    });

    assert_eq!(
        client.put(&url).json(&body).send().await.unwrap().status(),
        401
    );
    assert_eq!(client.get(&list_url).send().await.unwrap().status(), 401);
    for request in [
        client.put(&url).json(&body),
        client.delete(&url),
        client.get(&list_url),
    ] {
        assert_eq!(
            request
                .bearer_auth(&user_token)
                .send()
                .await
                .unwrap()
                .status(),
            403
        );
    }

    let with = |patch: Value| {
        let mut b = body.clone();
        b.as_object_mut()
            .unwrap()
            .extend(patch.as_object().unwrap().clone());
        b
    };
    for (bad, code) in [
        (json!({"expected_text_hash": null}), "missing_text_hash"),
        (
            json!({"expected_text_hash": "0".repeat(64)}),
            "text_changed",
        ),
        (
            json!({"rules": [{"id": "R1", "field": "employment_status", "op": "in",
                              "value": ["unemployed"], "evidence": "구직 중인 청년"}]}),
            "evidence_not_found",
        ),
        (
            json!({"rules": [{"id": "R1", "field": "shoe_size", "op": "eq", "value": 1,
                              "evidence": "부산 거주 미취업 청년"}]}),
            "invalid_rule",
        ),
        (json!({"status": "approved"}), "unknown_status"),
        (json!({"window": {"kind": "soon"}}), "invalid_window"),
        (json!({"window": {"kind": "closed"}}), "evidence_not_found"),
        (json!({"reviewer": ""}), "invalid_reviewer"),
        // Audit H1: "complete" with no coverage, or one segment unlabelled.
        (json!({"coverage": null}), "coverage_incomplete"),
        (json!({"coverage": {"labels": {}}}), "coverage_incomplete"),
    ] {
        let response = client
            .put(&url)
            .bearer_auth(&admin_token)
            .json(&with(bad.clone()))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400, "{bad}");
        assert_eq!(
            response.json::<Value>().await.unwrap()["code"],
            code,
            "{bad}"
        );
    }
    let stored: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM program_condition_reviews WHERE source_id=$1::text",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(stored, 0);

    let response = client
        .put(&url)
        .bearer_auth(&admin_token)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let saved: Value = response.json().await.unwrap();
    assert_eq!(saved["written"], "inserted");

    let listed: Value = client
        .get(format!("{list_url}?status=complete&limit=500"))
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let row = listed["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["program_id"] == id.to_string())
        .unwrap_or_else(|| panic!("{listed}"));
    assert_eq!(row["current_text_hash"], hash);
    assert_eq!(row["stale"], false);
    assert_eq!(row["rule_json"]["all"][0]["origin"], "review");
    assert_eq!(
        client
            .get(format!("{list_url}?status=approved"))
            .bearer_auth(&admin_token)
            .send()
            .await
            .unwrap()
            .status(),
        400
    );

    assert_eq!(
        client
            .delete(&url)
            .bearer_auth(&admin_token)
            .send()
            .await
            .unwrap()
            .status(),
        204
    );
    assert_eq!(
        client
            .delete(&url)
            .bearer_auth(&admin_token)
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    let log: Vec<(String, String)> = sqlx::query_as(
        "SELECT action, actor FROM program_condition_review_log WHERE source_type='youth_center' AND source_id=$1::text ORDER BY id",
    )
    .bind(id).fetch_all(&pool).await.unwrap();
    let actor = format!("admin:{admin_id}");
    assert_eq!(
        log,
        [
            ("upsert".to_owned(), actor.clone()),
            ("delete".to_owned(), actor)
        ]
    );
    for statement in [
        "UPDATE program_condition_review_log SET actor = 'x' WHERE source_id = $1::text",
        "DELETE FROM program_condition_review_log WHERE source_id = $1::text",
    ] {
        let error = sqlx::query(statement)
            .bind(id)
            .execute(&pool)
            .await
            .expect_err(statement);
        assert!(error.to_string().contains("append-only"), "{error}");
    }

    // Service-only tables: RLS on and forced, one policy on the service role.
    for table in ["program_condition_reviews", "program_condition_review_log"] {
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
        assert_eq!(policies, ["is_service_role()"], "{table}");
    }

    cleanup(&pool, &category).await;
}

// ── Code overrides (independent-check cases) ────────────────────────────────

const ALL_SIDOS: [&str; 17] = [
    "busan",
    "chungbuk",
    "chungnam",
    "daegu",
    "daejeon",
    "gangwon",
    "gwangju",
    "gyeongbuk",
    "gyeonggi",
    "gyeongnam",
    "incheon",
    "jeju",
    "jeonbuk",
    "jeonnam",
    "sejong",
    "seoul",
    "ulsan",
];

struct Overridden {
    id: Uuid,
    raw: Value,
    overrides: Value,
    /// Hidden by the code rule today; listed once the override is imported.
    profile: Value,
    reason: &'static str,
    check: &'static str,
}

/// A 온통청년 row with the given columns (and compiled source conditions).
async fn coded_row(
    pool: &sqlx::PgPool,
    category: &str,
    title: &str,
    text: &str,
    columns: Value,
    hard: Option<Value>,
) -> (Uuid, Value) {
    let id = Uuid::new_v4();
    let raw = json!({
        "plcyNm": title, "addAplyQlfcCndCn": text,
        "plcyAplyMthdCn": "온라인으로 본인이 직접 신청",
        "sprtTrgtMinAge": columns["min_age"].to_string(),
        "sprtTrgtMaxAge": columns["max_age"].to_string(),
        "aplyPrdSeCd": "0057002"
    });
    let strings = |key: &str| -> Option<Vec<String>> {
        columns[key].as_array().map(|items| {
            items
                .iter()
                .map(|v| v.as_str().unwrap().to_owned())
                .collect()
        })
    };
    let normalized = columns
        .get("enrollment_statuses")
        .map(|statuses| json!({"enrollment_statuses": statuses}));
    sqlx::query("INSERT INTO programs(id,program_type,source_type,source_id,title,program_status,regions,region_districts,min_age,max_age,normalized_payload,is_active,raw_payload) VALUES($1,$2,'youth_center',$1::text,$3,'active',$4,$5,$6,$7,$8,true,$9)")
        .bind(id).bind(category).bind(title).bind(strings("regions")).bind(strings("region_districts"))
        .bind(columns["min_age"].as_i64().map(|v| v as i32))
        .bind(columns["max_age"].as_i64().map(|v| v as i32))
        .bind(normalized).bind(&raw).execute(pool).await.unwrap();
    if let Some(hard) = hard {
        sqlx::query("INSERT INTO eligibility_rules(program_id,rule_json,hard_filter_json,version,created_by) VALUES($1,$2,$2,1,'ingestion:source_codes')")
            .bind(id).bind(&hard).execute(pool).await.unwrap();
    }
    let hash = mazimi_core::applicant_review::text_hash("youth_center", &raw).unwrap();
    sqlx::query("INSERT INTO program_applicant_reviews(source_type,source_id,class,evidence,reviewer,text_hash) VALUES('youth_center',$1::text,'individual','온라인으로 본인이 직접 신청','test:A+B',$2)")
        .bind(id).bind(hash).execute(pool).await.unwrap();
    (id, raw)
}

/// The review line a curator would import for `case`.
fn override_review(case: &Overridden) -> Value {
    json!({
        "source_type": "youth_center", "source_id": case.id.to_string(),
        "text_hash": condition_review::text_hash("youth_center", &case.raw).unwrap(),
        "status": "incomplete", "remaining_checks": ["other"], "rules": [],
        "window": {"kind": "unknown"}, "reviewer": "test:A+B",
        "code_overrides": case.overrides
    })
}

/// Four of the independent-check defects: hidden by a code rule narrower
/// than the notice, listed (확인 필요, never verified) once a current review
/// widens or drops it, hidden again when the text changes.
#[tokio::test]
async fn code_overrides_list_rows_the_codes_hid_until_the_text_changes() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let base = start_test_server().await;
    let pool = pool().await;
    let client = Client::new();
    let category = format!("condition_override_{}", Uuid::new_v4().simple());
    let year = mazimi_core::seoul_year(Utc::now());
    let job = json!({"all": [{"field": "employment_status", "op": "in",
        "value": ["preparing", "student", "unemployed"], "origin": "source_codes", "weight": 20}]});

    let mut cases = Vec::new();
    // 20250122005400110346: 18-34, 군 경력자 up to 39.
    let text =
        "※ 군 경력자는 복무기간에 비례하여 참여 제한 연령을 연동하여 적용(최고 만 39세로 한정)";
    let (id, raw) = coded_row(
        &pool,
        &category,
        "청년도전지원사업 검증",
        text,
        json!({"min_age": 18, "max_age": 34, "regions": ALL_SIDOS}),
        Some(job.clone()),
    )
    .await;
    cases.push(Overridden {
        id, raw,
        overrides: json!([{"target": "S-age", "action": "widen",
            "replacement": {"field": "age", "op": "between", "value": [18, 39]},
            "check_kind": "military_service",
            "evidence": "군 경력자는 복무기간에 비례하여 참여 제한 연령을 연동하여 적용(최고 만 39세로 한정)"}]),
        profile: json!({"birth_year": year - 37, "region_code": "busan",
                        "employment_status": "unemployed"}),
        reason: "age", check: "military_service",
    });
    // 20260930005400213702: 충북 only, text 도내(인접 시도 가능).
    let near = json!({"field": "region_code", "op": "in", "value":
        ["chungbuk", "gyeonggi", "gangwon", "chungnam", "gyeongbuk", "daejeon", "sejong", "jeonbuk"]});
    let (id, raw) = coded_row(&pool, &category, "충북 청년 일자리 검증", "도내(인접 시도 가능) 소재 기업 재직자",
        json!({"min_age": 20, "max_age": 75, "regions": ["chungbuk"],
               "region_districts": ["43111", "43112", "43113", "43114", "43130", "43150", "43720",
                                    "43730", "43740", "43745", "43750", "43760", "43770", "43800"]}),
        None).await;
    cases.push(Overridden {
        id,
        raw,
        overrides: json!([
            {"target": "S-region", "action": "widen", "replacement": near,
             "check_kind": "target_group_other", "evidence": "도내(인접 시도 가능)"},
            {"target": "S-district", "action": "widen", "replacement": near,
             "check_kind": "target_group_other", "evidence": "도내(인접 시도 가능)"}]),
        profile: json!({"birth_year": year - 30, "region_code": "gyeonggi"}),
        reason: "region",
        check: "target_group_other",
    });
    // 20260710005400113256: jobCd 미취업, text 불완전 취업자 예외.
    let (id, raw) = coded_row(
        &pool,
        &category,
        "국민취업지원제도 검증",
        "참여제한대상 : 현재 취업 중인 자 (단, 불완전 취업자는 예외)",
        json!({"min_age": 15, "max_age": 69, "regions": ALL_SIDOS}),
        Some(job.clone()),
    )
    .await;
    cases.push(Overridden {
        id,
        raw,
        overrides: json!([{"target": "C-job", "action": "drop",
            "check_kind": "employer_or_job_detail",
            "evidence": "현재 취업 중인 자 (단, 불완전 취업자는 예외)"}]),
        profile: json!({"birth_year": year - 30, "region_code": "seoul",
                        "employment_status": "employed"}),
        reason: "hard_rule",
        check: "employer_or_job_detail",
    });
    // 20260313005400212142: 용인 구 only, text 거주자 또는 관내 대학 재학생.
    let (id, raw) = coded_row(
        &pool,
        &category,
        "용인 청년 구직 검증",
        "용인시 거주자 또는 관내 대학 재(휴학)중인 구직자",
        json!({"min_age": 18, "max_age": 39, "regions": ["gyeonggi"],
               "region_districts": ["41461", "41463", "41465"]}),
        Some(job.clone()),
    )
    .await;
    cases.push(Overridden {
        id,
        raw,
        overrides: json!([{"target": "S-district", "action": "widen",
            "replacement": {"any": [
                {"field": "city_code", "op": "in", "value": ["41461", "41463", "41465"]},
                {"field": "enrollment_status", "op": "in", "value": ["enrolled", "on_leave"]}]},
            "check_kind": "school_detail",
            "evidence": "용인시 거주자 또는 관내 대학 재(휴학)중인 구직자"}]),
        profile: json!({"birth_year": year - 22, "region_code": "gyeonggi", "city_code": "41110",
                        "enrollment_status": "enrolled", "employment_status": "student"}),
        reason: "region",
        check: "school_detail",
    });

    let list = |body: Value| {
        let client = client.clone();
        let base = base.clone();
        async move {
            let response = client
                .post(format!("{base}/api/v1/recommend/preview"))
                .json(&body)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), 200);
            response.json::<Value>().await.unwrap()
        }
    };

    let mut before = Vec::new();
    for case in &cases {
        let body = list(case.profile.clone()).await;
        assert!(
            item(&body, case.id).is_none(),
            "{}: hidden by the code rule",
            case.check
        );
        before.push(excluded(&body, case.reason));
    }

    // The first case through the admin route (AdminUser only), the rest
    // through the importer's write path.
    let (_, admin_token) = create_test_admin(&base).await;
    let (_, user_token, _) = create_test_user(&base).await;
    let mut put = override_review(&cases[0]);
    let object = put.as_object_mut().unwrap();
    object.insert("expected_text_hash".into(), object["text_hash"].clone());
    let url = format!(
        "{base}/api/v1/admin/programs/{}/condition-review",
        cases[0].id
    );
    assert_eq!(
        client
            .put(&url)
            .bearer_auth(&user_token)
            .json(&put)
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    let mut narrower = put.clone();
    narrower["code_overrides"][0]["replacement"]["value"] = json!([20, 39]);
    let response = client
        .put(&url)
        .bearer_auth(&admin_token)
        .json(&narrower)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    assert_eq!(
        response.json::<Value>().await.unwrap()["code"],
        "override_not_wider"
    );
    let response = client
        .put(&url)
        .bearer_auth(&admin_token)
        .json(&put)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let mut connection = pool.acquire().await.unwrap();
    for case in &cases[1..] {
        let input: store::ReviewInput = serde_json::from_value(override_review(case)).unwrap();
        let written = store::upsert(&mut connection, &input, "import:test", true)
            .await
            .unwrap();
        assert!(written.is_ok(), "{}: {written:?}", case.check);
    }
    drop(connection);

    for (case, before) in cases.iter().zip(&before) {
        let body = list(case.profile.clone()).await;
        let (_, row) = item(&body, case.id)
            .unwrap_or_else(|| panic!("{}: listed after the override", case.check));
        assert_eq!(row["verified"], false, "{row}");
        assert_ne!(row["eligibility_status"], "pass", "{row}");
        assert!(
            row["remaining_checks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|c| c["kind"] == case.check),
            "{row}"
        );
        // The row left its bucket (another case's row may have too: the
        // gyeonggi profiles see both the 충북 and the 용인 row).
        assert!(excluded(&body, case.reason) < *before, "{}", case.check);
        let summary = &body["filter_summary"];
        assert_eq!(
            summary["included"].as_u64().unwrap() + summary["excluded"].as_u64().unwrap(),
            summary["considered"].as_u64().unwrap()
        );
    }

    // The text changes: the review is stale and the code rules hide again.
    for case in &cases {
        sqlx::query("UPDATE programs SET raw_payload = raw_payload || '{\"plcySprtCn\":\"월 50만원\"}' WHERE id=$1")
            .bind(case.id).execute(&pool).await.unwrap();
    }
    for case in &cases {
        let body = list(case.profile.clone()).await;
        assert!(
            item(&body, case.id).is_none(),
            "{}: stale review applies nothing",
            case.check
        );
    }
    sqlx::query("DELETE FROM eligibility_rules WHERE program_id = ANY($1)")
        .bind(cases.iter().map(|c| c.id).collect::<Vec<_>>())
        .execute(&pool)
        .await
        .unwrap();
    cleanup(&pool, &category).await;
}

/// Production: a 부산 student saw "충청북도 청년위원회 위원 모집" at rank 2.
/// The region override admits "도내 직장·대학" people, which no profile
/// field confirms. A row listed only because an override admitted the user
/// ranks after every row listed without one and says why; a 충북 resident,
/// whom the code rule already admits, keeps the row's place.
#[tokio::test]
async fn rows_listed_only_through_an_override_rank_last() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let base = start_test_server().await;
    let pool = pool().await;
    let client = Client::new();
    let category = format!("condition_override_rank_{}", Uuid::new_v4().simple());
    let year = mazimi_core::seoul_year(Utc::now());
    let quote = "도내 직장·대학에 재직·재학 중인 청년";
    let (committee, raw) = coded_row(
        &pool,
        &category,
        "충청북도 청년위원회 위원 모집 검증",
        "충청북도 거주 청년 또는 도내 직장·대학에 재직·재학 중인 청년",
        json!({"min_age": 19, "max_age": 39, "regions": ["chungbuk"]}),
        None,
    )
    .await;
    // A checked notice rule (not source codes): rule-evidence tier, above
    // rows shown on trust.
    let age = json!({"all": [{"field": "age", "op": "between", "value": [19, 39]}]});
    sqlx::query("INSERT INTO eligibility_rules(program_id,rule_json,hard_filter_json,version,created_by) VALUES($1,$2,$2,1,'test:reviewed')")
        .bind(committee).bind(&age).execute(&pool).await.unwrap();
    // Nationwide, no rule: shown on trust, the lowest evidence tier.
    let (nationwide, _) = coded_row(
        &pool,
        &category,
        "전국 청년 교통비 지원 순위 검증",
        "전국 청년",
        json!({"min_age": 19, "max_age": 39, "regions": ALL_SIDOS}),
        None,
    )
    .await;

    let list = |body: Value| {
        let client = client.clone();
        let base = base.clone();
        async move {
            let response = client
                .post(format!("{base}/api/v1/recommend/preview"))
                .json(&body)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), 200);
            response.json::<Value>().await.unwrap()
        }
    };
    let student = json!({"birth_year": year - 22, "region_code": "busan",
                         "enrollment_status": "enrolled", "employment_status": "student"});
    let resident = json!({"birth_year": year - 30, "region_code": "chungbuk",
                          "employment_status": "employed"});
    assert!(
        item(&list(student.clone()).await, committee).is_none(),
        "hidden by the region code"
    );

    let case = Overridden {
        id: committee,
        raw,
        overrides: json!([{"target": "S-region", "action": "widen",
            "replacement": {"any": [
                {"field": "region_code", "op": "in", "value": ["chungbuk"]},
                {"field": "employment_status", "op": "in", "value": ["employed"]},
                {"field": "enrollment_status", "op": "in", "value": ["enrolled", "on_leave"]}]},
            "check_kind": "target_group_other", "evidence": quote}]),
        profile: student.clone(),
        reason: "region",
        check: "target_group_other",
    };
    let input: store::ReviewInput = serde_json::from_value(override_review(&case)).unwrap();
    let mut connection = pool.acquire().await.unwrap();
    let written = store::upsert(&mut connection, &input, "import:test", true)
        .await
        .unwrap();
    assert!(written.is_ok(), "{written:?}");
    drop(connection);

    let exception = |row: &Value| {
        row["remaining_checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["kind"] == "code_exception")
    };
    let label = "지역 조건 예외에 해당하는지 확인이 필요해요";

    let body = list(student).await;
    let (rank, row) = item(&body, committee).expect("listed through the override");
    let check = row["remaining_checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["kind"] == "code_exception")
        .unwrap_or_else(|| panic!("{row}"));
    assert_eq!(check["label"], label);
    assert!(row["missing_checks"]
        .as_array()
        .unwrap()
        .contains(&json!(label)));
    assert_eq!(row["verified"], false);
    let student_score = row["match_score"].as_i64().unwrap();
    assert!(
        student_score < 50,
        "below the clients' 보통 cutoff, shown as 낮음: {student_score}"
    );
    let (control, _) = item(&body, nationwide).expect("nationwide row listed");
    assert!(
        control < rank,
        "a rule-tier override row still ranks after a row shown on trust"
    );
    let items = body["items"].as_array().unwrap();
    for (i, other) in items.iter().enumerate() {
        if !exception(other) {
            assert!(i < rank, "a row without an exception after it: {other}");
        }
    }

    let body = list(resident).await;
    let (rank, row) = item(&body, committee).expect("listed for a 충북 resident");
    assert!(!exception(&row), "the code rule admits a resident: {row}");
    assert!(
        row["remaining_checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["kind"] == "target_group_other"),
        "{row}"
    );
    let (control, _) = item(&body, nationwide).unwrap();
    assert!(rank < control, "not demoted: rule tier above trust tier");
    assert!(
        row["match_score"].as_i64().unwrap() >= 50,
        "not capped for a resident: {row}"
    );

    sqlx::query("DELETE FROM eligibility_rules WHERE program_id = $1")
        .bind(committee)
        .execute(&pool)
        .await
        .unwrap();
    cleanup(&pool, &category).await;
}

/// Review 2026-10-21 M21: the anonymous list reads rows of other sidos with
/// their ages only (no notice text) and loads overrides for the rows left.
/// Exclude-only: every row is still decided and counted for the same reason
/// as when the whole catalogue was read, and a row an `S-region` override
/// admits is still listed. Passes before and after the narrowing.
#[tokio::test]
async fn region_narrowing_keeps_every_decision_and_reason() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let base = start_test_server().await;
    let pool = pool().await;
    let client = Client::new();
    let category = format!("condition_narrow_{}", Uuid::new_v4().simple());
    let year = mazimi_core::seoul_year(Utc::now());
    let profile = json!({"birth_year": year - 25, "region_code": "busan",
                         "employment_status": "unemployed"});
    let list = || {
        let client = client.clone();
        let base = base.clone();
        let profile = profile.clone();
        async move {
            let response = client
                .post(format!("{base}/api/v1/recommend/preview?scope=all"))
                .json(&profile)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), 200);
            response.json::<Value>().await.unwrap()
        }
    };
    let before = list().await;

    let row = |title: &'static str, columns: Value| {
        let pool = pool.clone();
        let category = category.clone();
        async move { coded_row(&pool, &category, title, "청년 지원", columns, None).await }
    };
    let (elsewhere, _) = row("서울 청년 지원 검증", json!({"regions": ["seoul"]})).await;
    let (elsewhere_old, _) = row(
        "서울 중장년 지원 검증",
        json!({"regions": ["seoul"], "min_age": 50, "max_age": 64}),
    )
    .await;
    let (elsewhere_later, _) = row("서울 예정 지원 검증", json!({"regions": ["seoul"]})).await;
    // Boundary cohort (24 or 25 by a bare birth year): age unknown, so the
    // region check is the reason.
    let (elsewhere_boundary, _) = row(
        "서울 경계 연령 지원 검증",
        json!({"regions": ["seoul"], "min_age": 19, "max_age": 24}),
    )
    .await;
    sqlx::query(
        "UPDATE programs SET application_start_at = now() + interval '3 days' WHERE id = $1",
    )
    .bind(elsewhere_later)
    .execute(&pool)
    .await
    .unwrap();
    let (unresolved, _) = row("지역 미상 지원 검증", json!({"regions": ["unknown"]})).await;
    let (home, _) = row("부산 청년 지원 검증", json!({"regions": ["busan"]})).await;
    let (widened, raw) = row("인접 시도 청년 지원 검증", json!({"regions": ["ulsan"]})).await;
    let input: store::ReviewInput = serde_json::from_value(json!({
        "source_type": "youth_center", "source_id": widened.to_string(),
        "text_hash": condition_review::text_hash("youth_center", &raw).unwrap(),
        "status": "incomplete", "remaining_checks": ["other"], "rules": [],
        "window": {"kind": "unknown"}, "reviewer": "test:A+B",
        "code_overrides": [{"target": "S-region", "action": "widen",
            "replacement": {"field": "region_code", "op": "in", "value": ["ulsan", "busan"]},
            "check_kind": "target_group_other", "evidence": "청년 지원"}]
    }))
    .unwrap();
    let mut connection = pool.acquire().await.unwrap();
    let written = store::upsert(&mut connection, &input, "import:test", true)
        .await
        .unwrap();
    assert!(written.is_ok(), "{written:?}");
    drop(connection);

    let after = list().await;
    for id in [
        elsewhere,
        elsewhere_old,
        elsewhere_later,
        elsewhere_boundary,
        unresolved,
    ] {
        assert!(item(&after, id).is_none(), "{id} hidden");
    }
    assert!(item(&after, home).is_some(), "home sido listed");
    assert!(
        item(&after, widened).is_some(),
        "listed through the override"
    );
    let summary = |body: &Value, key: &str| body["filter_summary"][key].as_u64().unwrap();
    assert_eq!(
        summary(&after, "considered"),
        summary(&before, "considered") + 7
    );
    assert_eq!(
        summary(&after, "included"),
        summary(&before, "included") + 2
    );
    for (reason, added) in [
        ("region", 2),
        ("age", 1),
        ("not_open", 1),
        ("scope_unconfirmed", 1),
    ] {
        assert_eq!(
            excluded(&after, reason),
            excluded(&before, reason) + added,
            "{reason}"
        );
    }

    cleanup(&pool, &category).await;
}
