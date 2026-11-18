//! Replay cases for the recommendation filter: client vocabulary, the 만나이
//! boundary, the effective deadline and the rule-less ranking floor.

use super::*;

async fn pool() -> sqlx::PgPool {
    PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .unwrap()
}

/// A busan-scoped, active, undated program worth 300k/month in a throwaway
/// category so assertions never depend on the seeded catalogue.
async fn program(pool: &sqlx::PgPool, category: &str, title: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO programs(id,program_type,source_type,source_id,title,program_status,regions,is_active,benefit_amount_monthly) VALUES($1,$2,'manual',$1::text,$3,'active',ARRAY['busan'],true,300000)")
        .bind(id).bind(category).bind(title).execute(pool).await.unwrap();
    id
}

async fn rule(pool: &sqlx::PgPool, program_id: Uuid, rule_json: &Value, hard_filter: &Value) {
    sqlx::query("INSERT INTO eligibility_rules(program_id,rule_json,hard_filter_json,version,compiled_at) VALUES($1,$2,$3,1,now())")
        .bind(program_id).bind(rule_json).bind(hard_filter).execute(pool).await.unwrap();
}

async fn preview(client: &Client, base: &str, payload: Value) -> reqwest::Response {
    client
        .post(format!("{base}/api/v1/recommend/preview?scope=all"))
        .json(&payload)
        .send()
        .await
        .unwrap()
}

fn find_item(body: &Value, id: Uuid) -> Option<&Value> {
    let expected = id.to_string();
    body["items"]
        .as_array()?
        .iter()
        .find(|item| item["program_id"].as_str() == Some(expected.as_str()))
}

fn position(body: &Value, id: Uuid) -> usize {
    let expected = id.to_string();
    body["items"]
        .as_array()
        .unwrap()
        .iter()
        .position(|item| item["program_id"].as_str() == Some(expected.as_str()))
        .unwrap_or_else(|| panic!("{expected} missing from {body}"))
}

fn joined(item: &Value, field: &str) -> String {
    item[field]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect::<Vec<_>>()
        .join("\n")
}

fn has_check(item: &Value, label: &str) -> bool {
    item["missing_checks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|value| value == label)
}

fn kst_year() -> i32 {
    mazimi_core::seoul_year(Utc::now())
}

#[tokio::test]
async fn korean_enrollment_and_employment_answers_pass_seeded_hard_filters() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let base = start_test_server().await;
    let pool = pool().await;
    let client = Client::new();
    let category = format!("vocab_{}", Uuid::new_v4().simple());
    let id = program(&pool, &category, "어휘 정규화 회귀테스트").await;
    let hard = json!({"all":[
        {"field":"enrollment_status","op":"in","value":["enrolled"]},
        {"field":"employment_status","op":"in","value":["employed"]}
    ]});
    let soft = json!({"all":[
        {"field":"enrollment_status","op":"in","value":["enrolled"]},
        {"field":"employment_status","op":"in","value":["employed"]},
        {"field":"has_disability","op":"is_true"}
    ]});
    rule(&pool, id, &soft, &hard).await;
    let profile = |enrollment: &str, employment: &str| json!({"birth_year":2000,"region_code":"busan","enrollment_status":enrollment,"employment_status":employment});

    // Web "대학생", mobile "재학"/"재직", canonical codes with noise.
    for (enrollment, employment) in [
        ("대학생", "재직"),
        ("재학", "재직자"),
        (" ENROLLED ", "employed"),
        ("재학중", "직장인"),
    ] {
        let response = preview(&client, &base, profile(enrollment, employment)).await;
        assert_eq!(response.status(), 200, "{enrollment}/{employment}");
        let body: Value = response.json().await.unwrap();
        let item = find_item(&body, id)
            .unwrap_or_else(|| panic!("{enrollment}/{employment} must pass: {body}"));
        let reasons = joined(item, "reasons");
        assert!(reasons.contains("재학 상태 조건 맞음 (재학)"), "{reasons}");
        assert!(reasons.contains("취업 상태 조건 맞음 (재직)"), "{reasons}");
        assert!(reasons.contains("장애 여부 정보가 필요합니다"), "{reasons}");
        for leaked in ["enrolled", "employed", "has_disability"] {
            assert!(!reasons.contains(leaked), "{leaked} leaked into {reasons}");
        }
        assert!(has_check(item, "장애 여부 확인 필요"), "{item}");
    }

    // A real contradiction still excludes.
    for (enrollment, employment) in [
        ("졸업", "재직"),
        ("재학", "미취업"),
        ("휴학생", "취업준비생"),
    ] {
        let body: Value = preview(&client, &base, profile(enrollment, employment))
            .await
            .json()
            .await
            .unwrap();
        assert!(
            find_item(&body, id).is_none(),
            "{enrollment}/{employment} must be excluded"
        );
    }

    // Blank answers stay unknown instead of contradicting.
    for (enrollment, employment) in [("", ""), ("none", "해당없음")] {
        let body: Value = preview(&client, &base, profile(enrollment, employment))
            .await
            .json()
            .await
            .unwrap();
        let item = find_item(&body, id)
            .unwrap_or_else(|| panic!("blank {enrollment:?}/{employment:?} must stay visible"));
        assert_eq!(item["eligibility_status"], "unknown");
        assert!(has_check(item, "재학 상태 확인 필요"), "{item}");
        assert!(has_check(item, "취업 상태 확인 필요"), "{item}");
    }

    // Unrecognised answers are rejected in Korean.
    for payload in [profile("학생아님", "재직"), profile("재학", "typo")] {
        let response = preview(&client, &base, payload.clone()).await;
        assert_eq!(response.status(), 400, "{payload}");
        let body: Value = response.json().await.unwrap();
        assert!(
            body["error"].as_str().unwrap().contains("선택해 주세요"),
            "{body}"
        );
    }

    // Saved profiles store canonical codes; the web "취업준비생" enrollment
    // pick is kept as the employment answer it is.
    let (user, token, _) = create_test_user(&base).await;
    let body: Value = client
        .post(format!("{base}/api/v1/profile"))
        .bearer_auth(&token)
        .json(&profile("재학", "재직"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["profile"]["enrollment_status"], "enrolled");
    assert_eq!(body["profile"]["employment_status"], "employed");
    let body: Value = client
        .post(format!("{base}/api/v1/profile"))
        .bearer_auth(&token)
        .json(&json!({"birth_year":2000,"region_code":"busan","enrollment_status":"취업준비생"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["profile"]["enrollment_status"], Value::Null);
    assert_eq!(body["profile"]["employment_status"], "preparing");
    let stored: (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT enrollment_status, employment_status FROM user_profiles WHERE user_id=$1",
    )
    .bind(user)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(stored, (None, Some("preparing".to_owned())));

    // The campus catalogue accepts the mobile vocabulary and rejects garbage.
    let campus = format!("{base}/api/v1/campus/programs");
    let korean: Value = client
        .get(&campus)
        .query(&[("school", "pusan"), ("enrollment_status", "재학")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let canonical: Value = client
        .get(&campus)
        .query(&[("school", "pusan"), ("enrollment_status", "enrolled")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(korean["total"].as_i64().unwrap() > 0, "{korean}");
    assert_eq!(korean["total"], canonical["total"]);
    assert_eq!(
        client
            .get(&campus)
            .query(&[("school", "pusan"), ("enrollment_status", "학생아님")])
            .send()
            .await
            .unwrap()
            .status(),
        400
    );

    sqlx::query("DELETE FROM users WHERE id=$1")
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM programs WHERE program_type=$1")
        .bind(&category)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn age_gate_keeps_the_boundary_cohort_visible_at_both_ends() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let base = start_test_server().await;
    let pool = pool().await;
    let client = Client::new();
    let category = format!("age_{}", Uuid::new_v4().simple());
    let plain = program(&pool, &category, "연령 경계 회귀테스트 (룰 없음)").await;
    let ruled = program(&pool, &category, "연령 경계 회귀테스트 (룰)").await;
    sqlx::query("UPDATE programs SET min_age=19, max_age=34 WHERE id=ANY($1)")
        .bind(vec![plain, ruled])
        .execute(&pool)
        .await
        .unwrap();
    let band = json!({"field":"age","op":"between","value":[19,34]});
    rule(&pool, ruled, &band, &band).await;

    let year = kst_year();
    for (birth_year, expectation) in [
        (year - 20, "pass"),
        (year - 34, "pass"),
        (year - 35, "boundary"),
        (year - 19, "boundary"),
        (year - 36, "excluded"),
        (year - 18, "excluded"),
    ] {
        let body: Value = preview(
            &client,
            &base,
            json!({"birth_year":birth_year,"region_code":"busan"}),
        )
        .await
        .json()
        .await
        .unwrap();
        for id in [plain, ruled] {
            let item = find_item(&body, id);
            match expectation {
                "excluded" => assert!(
                    item.is_none(),
                    "birth_year {birth_year} is outside under both readings: {body}"
                ),
                "boundary" => {
                    let item = item.unwrap_or_else(|| {
                        panic!("birth_year {birth_year} may still qualify: {body}")
                    });
                    assert_eq!(item["eligibility_status"], "unknown", "{item}");
                    assert!(has_check(item, "생년월일 확인 필요"), "{item}");
                    assert!(
                        !joined(item, "reasons").contains("현재 연령 조건에 맞아요"),
                        "{item}"
                    );
                }
                _ => {
                    let item =
                        item.unwrap_or_else(|| panic!("birth_year {birth_year} qualifies: {body}"));
                    assert!(!has_check(item, "생년월일 확인 필요"), "{item}");
                    if id == ruled {
                        assert_eq!(item["eligibility_status"], "pass", "{item}");
                    }
                }
            }
        }
    }

    sqlx::query("DELETE FROM programs WHERE program_type=$1")
        .bind(&category)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn effective_deadline_orders_collected_programs_and_labels_today() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let base = start_test_server().await;
    let pool = pool().await;
    let client = Client::new();
    let category = format!("deadline_{}", Uuid::new_v4().simple());
    let today = program(&pool, &category, "오늘 마감").await;
    let validity = program(&pool, &category, "유효기간 임박").await;
    let explicit = program(&pool, &category, "명시 마감").await;
    let undated = program(&pool, &category, "무기한").await;
    sqlx::query("UPDATE programs SET application_end_at=now()+interval '1 hour' WHERE id=$1")
        .bind(today)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE programs SET benefit_valid_until=now()+interval '1 day 1 hour' WHERE id=$1",
    )
    .bind(validity)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("UPDATE programs SET deadline_at=now()+interval '5 days 1 hour' WHERE id=$1")
        .bind(explicit)
        .execute(&pool)
        .await
        .unwrap();

    let body: Value = preview(
        &client,
        &base,
        json!({"birth_year":2000,"region_code":"busan"}),
    )
    .await
    .json()
    .await
    .unwrap();

    // Same urgency tier → same score → the SQL order (earliest effective
    // deadline first) decides; the undated program scores lower.
    assert!(position(&body, today) < position(&body, validity), "{body}");
    assert!(
        position(&body, validity) < position(&body, explicit),
        "{body}"
    );
    assert!(
        position(&body, explicit) < position(&body, undated),
        "{body}"
    );
    let today_item = find_item(&body, today).unwrap();
    let validity_item = find_item(&body, validity).unwrap();
    let explicit_item = find_item(&body, explicit).unwrap();
    let undated_item = find_item(&body, undated).unwrap();
    assert_eq!(today_item["match_score"], explicit_item["match_score"]);
    assert!(
        today_item["match_score"].as_i64() > undated_item["match_score"].as_i64(),
        "{body}"
    );
    let reasons = joined(today_item, "reasons");
    assert!(reasons.contains("오늘 마감"), "{reasons}");
    assert!(!reasons.contains("0일"), "{reasons}");
    assert!(
        joined(validity_item, "reasons").contains("마감이 1일 남았어요"),
        "{validity_item}"
    );
    assert!(
        validity_item["deadline"].is_string(),
        "validity-only programs must show their deadline: {validity_item}"
    );
    assert!(undated_item["deadline"].is_null(), "{undated_item}");

    sqlx::query("DELETE FROM programs WHERE program_type=$1")
        .bind(&category)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn unverified_programs_never_outrank_fully_passing_rule_programs() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let base = start_test_server().await;
    let pool = pool().await;
    let client = Client::new();
    let category = format!("floor_{}", Uuid::new_v4().simple());
    let ruled = program(&pool, &category, "룰 통과 프로그램").await;
    let plain = program(&pool, &category, "룰 없는 프로그램").await;
    // Give the rule-less program every ranking advantage except evidence.
    sqlx::query("UPDATE programs SET benefit_amount_monthly=1000000, deadline_at=now()+interval '2 days' WHERE id=$1")
        .bind(plain).execute(&pool).await.unwrap();
    sqlx::query("UPDATE programs SET benefit_amount_monthly=NULL, regions=ARRAY[]::text[], deadline_at=now()+interval '300 days' WHERE id=$1")
        .bind(ruled).execute(&pool).await.unwrap();
    let busan = json!({"field":"region_code","op":"eq","value":"busan"});
    rule(&pool, ruled, &busan, &busan).await;

    let body: Value = preview(
        &client,
        &base,
        json!({"birth_year":2000,"region_code":"busan"}),
    )
    .await
    .json()
    .await
    .unwrap();
    let ruled_item = find_item(&body, ruled).unwrap();
    let plain_item = find_item(&body, plain).unwrap();
    assert_eq!(ruled_item["eligibility_status"], "pass", "{ruled_item}");
    assert_eq!(plain_item["eligibility_status"], "unknown", "{plain_item}");
    assert!(
        ruled_item["match_score"].as_i64() > plain_item["match_score"].as_i64(),
        "{body}"
    );
    assert!(position(&body, ruled) < position(&body, plain), "{body}");

    sqlx::query("DELETE FROM programs WHERE program_type=$1")
        .bind(&category)
        .execute(&pool)
        .await
        .unwrap();
}

/// §28: through the real catalogue SELECT, an auto-published corporate
/// offer (`normalized_payload.display = 'verbatim'`) is never "조건 확인됨"
/// even with a passing reviewed rule; a curated corporate row keeps it.
#[tokio::test]
async fn an_auto_published_corporate_offer_is_never_conditions_verified_in_the_list() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let base = start_test_server().await;
    let pool = pool().await;
    let client = Client::new();
    let mut ids = Vec::new();
    for (title, payload) in [
        ("큐레이션 기업 혜택", json!({})),
        ("자동 게시 기업 혜택", json!({"display": "verbatim"})),
    ] {
        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO programs(id,program_type,source_type,source_id,title,program_status,regions,is_active,provider_kind,benefit_category,normalized_payload) VALUES($1,'corporate_benefit','manual',$1::text,$2,'always_open',ARRAY['busan'],true,'corporate','discount',$3)")
            .bind(id).bind(title).bind(&payload).execute(&pool).await.unwrap();
        let busan = json!({"field":"region_code","op":"eq","value":"busan"});
        sqlx::query("INSERT INTO eligibility_rules(program_id,rule_json,hard_filter_json,version,compiled_at,created_by) VALUES($1,$2,$2,1,now(),'review:fixture')")
            .bind(id).bind(&busan).execute(&pool).await.unwrap();
        ids.push(id);
    }
    let body: Value = preview(
        &client,
        &base,
        json!({"birth_year":2000,"region_code":"busan"}),
    )
    .await
    .json()
    .await
    .unwrap();
    let curated = find_item(&body, ids[0]).unwrap_or_else(|| panic!("curated missing: {body}"));
    let published = find_item(&body, ids[1]).unwrap_or_else(|| panic!("published missing: {body}"));
    assert_eq!(curated["conditions_verified"], true, "{curated}");
    assert_eq!(published["conditions_verified"], false, "{published}");
    assert!(
        joined(published, "missing_checks").contains("자격 조건 미확인"),
        "{published}"
    );
    for id in ids {
        sqlx::query("DELETE FROM programs WHERE id=$1")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
    }
}

/// Audit F1/F2/F5 end to end: the default list holds youth rows only, the
/// audience exclusions hold in every scope, a 시·군·구 programme needs the
/// user's district, and a rule-less row claims nothing it did not check.
#[tokio::test]
async fn youth_scope_audience_gates_and_district_matching() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let base = start_test_server().await;
    let pool = pool().await;
    let client = Client::new();
    let category = format!("audience_{}", Uuid::new_v4().simple());
    let youth = program(&pool, &category, "청년 월세 지원 검증").await;
    let general = program(&pool, &category, "주민 생활 지원 검증").await;
    let senior = program(&pool, &category, "100세 어르신 장수축하 검증").await;
    let facility = program(&pool, &category, "공공시설 운영 지원 검증").await;
    let district = program(&pool, &category, "청년 자격증 응시료 검증").await;
    // Review H4: no profile flag can match 산모, so the row is only outside
    // the youth scope, never a target-group exclusion.
    let birth = program(&pool, &category, "산모·신생아 건강관리 검증").await;
    sqlx::query("UPDATE programs SET source_type='gov_benefits', raw_payload=$2 WHERE id=$1")
        .bind(birth)
        .bind(json!({"사용자구분": "개인", "지원대상": "○ 출산가정"}))
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE programs SET source_type='gov_benefits', raw_payload=$2 WHERE id=$1")
        .bind(facility)
        .bind(json!({"사용자구분": "법인/시설/단체", "지원대상": "○ 공공 체육시설 운영 기관"}))
        .execute(&pool)
        .await
        .unwrap();
    // 부산 수영구 only (audit 00638ac6).
    sqlx::query("UPDATE programs SET region_districts=ARRAY['26500'] WHERE id=$1")
        .bind(district)
        .execute(&pool)
        .await
        .unwrap();

    let list = |scope: &'static str, city: Option<&'static str>| {
        let client = client.clone();
        let base = base.clone();
        async move {
            let response = client
                .post(format!("{base}/api/v1/recommend/preview{scope}"))
                .json(&json!({"birth_year":2000,"region_code":"busan","city_code":city}))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), 200);
            response.json::<Value>().await.unwrap()
        }
    };

    // A 시·군·구 outside the sido, or one that does not exist, is rejected,
    // not read as "no district".
    for city in ["11110", "26999"] {
        let response = client
            .post(format!("{base}/api/v1/recommend/preview"))
            .json(&json!({"birth_year":2000,"region_code":"busan","city_code":city}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400, "{city}");
    }

    // Default scope, 금정구 resident.
    let body = list("", Some("26410")).await;
    assert_eq!(body["scope"], "youth");
    assert!(find_item(&body, youth).is_some(), "{body}");
    for hidden in [general, senior, facility, district, birth] {
        assert!(
            find_item(&body, hidden).is_none(),
            "{hidden} listed: {body}"
        );
    }
    let summary = &body["filter_summary"]["excluded_by_reason"];
    let count = |reason: &str| {
        summary
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["reason"] == reason)
            .map_or(0, |r| r["count"].as_u64().unwrap())
    };
    assert!(count("outside_youth_scope") >= 1, "{summary}");
    assert!(count("target_group") >= 1, "{summary}");
    assert!(count("not_individual") >= 1, "{summary}");

    // F5: no age band, no rule → no age claim, no "청년 대상" claim, and an
    // explicit unverified label.
    let item = find_item(&body, youth).unwrap();
    assert_eq!(item["conditions_verified"], false, "{item}");
    assert_eq!(
        joined(item, "reasons"),
        "부산 거주 조건에 맞아요.",
        "{item}"
    );
    assert!(
        joined(item, "missing_checks").contains("자격 조건 미확인"),
        "{item}"
    );

    // "전체 혜택": general rows appear; group and organisation rows never do.
    let body = list("?scope=all", Some("26410")).await;
    assert_eq!(body["scope"], "all");
    assert!(find_item(&body, general).is_some(), "{body}");
    assert!(find_item(&body, birth).is_some(), "{body}");
    for hidden in [senior, facility, district] {
        assert!(
            find_item(&body, hidden).is_none(),
            "{hidden} listed: {body}"
        );
    }

    // District: home district lists it, a blank district asks for it.
    let body = list("", Some("26500")).await;
    let item = find_item(&body, district).expect("수영구 resident sees it");
    assert!(
        !joined(item, "missing_checks").contains("시·군·구 확인 필요"),
        "{item}"
    );
    let body = list("", None).await;
    let item = find_item(&body, district).expect("listed as unknown");
    assert!(
        joined(item, "missing_checks").contains("시·군·구 확인 필요"),
        "{item}"
    );

    assert_eq!(
        client
            .post(format!("{base}/api/v1/recommend/preview?scope=everything"))
            .json(&json!({"birth_year":2000,"region_code":"busan"}))
            .send()
            .await
            .unwrap()
            .status(),
        400
    );

    sqlx::query("DELETE FROM programs WHERE program_type=$1")
        .bind(&category)
        .execute(&pool)
        .await
        .unwrap();
}

/// Only the birth year is collected, so the age is a range. A profile that
/// may still belong to a 13-year-old (born 14 calendar years ago, birthday
/// not yet reached) is rejected; the gate accepts from 15 calendar years.
#[tokio::test]
async fn profiles_under_fourteen_are_rejected() {
    let base = start_test_server().await;
    let pool = pool().await;
    let client = Client::new();
    let year = kst_year();
    for birth_year in [year - 13, year - 14] {
        let response = preview(
            &client,
            &base,
            json!({"birth_year":birth_year,"region_code":"busan"}),
        )
        .await;
        assert_eq!(response.status(), 400, "{birth_year}");
        let body: Value = response.json().await.unwrap();
        assert_eq!(body["error"], "만 14세 이상만 이용할 수 있어요.", "{body}");
    }
    assert_eq!(
        preview(
            &client,
            &base,
            json!({"birth_year":year-15,"region_code":"busan"})
        )
        .await
        .status(),
        200
    );

    let (user, token, _) = create_test_user(&base).await;
    assert_eq!(
        client
            .post(format!("{base}/api/v1/profile"))
            .bearer_auth(&token)
            .json(&json!({"birth_year":year-14,"region_code":"busan"}))
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    sqlx::query("DELETE FROM users WHERE id=$1")
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
}

/// Review M3: the curated transport passes (migration
/// 20261015000003) reach a 20-year-old's default youth list in their city.
#[tokio::test]
async fn curated_transport_passes_are_in_the_youth_list() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let base = start_test_server().await;
    let client = Client::new();
    let birth_year = mazimi_core::seoul_year(Utc::now()) - 20;
    for (region, source_id) in [
        ("busan", "verified:20261012:gap:dongbaek-pass"),
        ("seoul", "verified:20261012:gap:seoul-climate-pass"),
    ] {
        let id: Uuid = sqlx::query_scalar(
            "SELECT id FROM programs WHERE source_type='government_official' AND source_id=$1",
        )
        .bind(source_id)
        .fetch_one(&pool().await)
        .await
        .unwrap();
        let body: Value = client
            .post(format!("{base}/api/v1/recommend/preview"))
            .json(&json!({"birth_year": birth_year, "region_code": region}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(body["scope"], "youth");
        assert!(find_item(&body, id).is_some(), "{source_id}: {body}");
    }
}

/// Audit H2 / L3 / L2: catalogue search matches every term inside a word
/// (unspaced Korean compounds), a blank `q` is no search, wildcards in `q`
/// are literal, and an unknown `status` is a 400, as unknown chips are.
#[tokio::test]
async fn catalogue_search_matches_terms_inside_compound_words() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let base = start_test_server().await;
    let pool = pool().await;
    let client = Client::new();
    let category = format!("search_{}", Uuid::new_v4().simple());
    let compound = program(&pool, &category, "청년월세 특별지원").await;
    let other = program(&pool, &category, "대학생 장학금").await;
    let list = |query: String| {
        let client = client.clone();
        let base = base.clone();
        async move {
            let response = client
                .get(format!("{base}/api/v1/programs?{query}"))
                .send()
                .await
                .unwrap();
            let status = response.status();
            (status, response.json::<Value>().await.unwrap())
        }
    };
    let ids = |body: &Value| -> Vec<String> {
        body["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["id"].as_str().unwrap().to_owned())
            .collect()
    };
    for q in ["월세", "청년 지원", "특별", "청년월세 특별지원"] {
        let (status, body) = list(format!("q={}&category={category}", urlencode(q))).await;
        assert_eq!(status, 200, "{q}: {body}");
        assert_eq!(ids(&body), [compound.to_string()], "{q}");
        assert_eq!(body["total"], 1, "{q}");
    }
    // Every term must match: no row has both.
    let (_, body) = list(format!(
        "q={}&category={category}",
        urlencode("월세 장학금")
    ))
    .await;
    assert_eq!(body["total"], 0);
    // `%` and `_` are literal characters, not wildcards.
    for q in ["%", "_", "월%세"] {
        let (_, body) = list(format!("q={}&category={category}", urlencode(q))).await;
        assert_eq!(body["total"], 0, "{q}");
    }
    // A blank search lists what no search lists.
    let (_, all) = list(format!("category={category}")).await;
    let (_, blank) = list(format!("q=%20%20&category={category}")).await;
    assert_eq!(all["total"], 2);
    assert_eq!(blank["total"], all["total"]);
    assert!(ids(&all).contains(&other.to_string()));
    let (status, _) = list(format!("status=nope&category={category}")).await;
    assert_eq!(status, 400);
    sqlx::query("DELETE FROM programs WHERE program_type=$1")
        .bind(&category)
        .execute(&pool)
        .await
        .unwrap();
}

fn urlencode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Audit M6: a recommendation item carries its card summary; the list reads
/// it for the listed rows only.
#[tokio::test]
async fn a_recommendation_item_carries_its_card_summary() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let base = start_test_server().await;
    let pool = pool().await;
    let client = Client::new();
    let category = format!("summary_{}", Uuid::new_v4().simple());
    let id = program(&pool, &category, "부산 청년 요약 검증").await;
    sqlx::query("UPDATE programs SET summary = $2 WHERE id = $1")
        .bind(id)
        .bind("  부산 청년에게\n월세를 지원합니다.  ")
        .execute(&pool)
        .await
        .unwrap();
    let year = mazimi_core::seoul_year(Utc::now());
    let response = preview(
        &client,
        &base,
        json!({"birth_year": year - 25, "region_code": "busan"}),
    )
    .await;
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.unwrap();
    let item = body["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["program_id"] == id.to_string())
        .unwrap_or_else(|| panic!("listed: {body}"));
    // Audit L8: the sums add only "조건 확인됨" rows (none while the section
    // is off), never a 확인 필요 row's 300,000원.
    assert_eq!(
        body["estimated_monthly"], 0,
        "{}",
        body["estimated_monthly"]
    );
    let summary = item["summary_display"].as_str().expect("summary_display");
    assert!(
        summary.starts_with("부산 청년에게") && summary.ends_with("지원합니다."),
        "{summary}"
    );
    sqlx::query("DELETE FROM programs WHERE program_type=$1")
        .bind(&category)
        .execute(&pool)
        .await
        .unwrap();
}
