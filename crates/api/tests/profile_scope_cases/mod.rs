use super::*;

#[tokio::test]
async fn alert_channel_replay_keeps_delivery_history_without_exposing_push_only_records() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let base = start_test_server().await;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .unwrap();
    let client = Client::new();
    let (user, token, _) = create_test_user(&base).await;
    let program = fixture_program(&pool, "알림 채널 범위 회귀 테스트", Some(vec![])).await;
    let visible = Uuid::new_v4();
    for (id, alert_type, in_app) in [(visible, "d7", true), (Uuid::new_v4(), "d3", false)] {
        sqlx::query("INSERT INTO alert_deliveries(id,user_id,program_id,alert_type,alert_date,in_app_visible) VALUES($1,$2,$3,$4,CURRENT_DATE,$5)")
            .bind(id).bind(user).bind(program).bind(alert_type).bind(in_app).execute(&pool).await.unwrap();
    }
    for enabled in [false, true] {
        let response = client.put(format!("{base}/api/v1/alerts/notification-preferences"))
            .bearer_auth(&token).json(&json!({"deadline":true,"new_program":true,"profile_update":true,"channels":{"in_app":enabled,"push":true,"kakao":false,"email":false}}))
            .send().await.unwrap();
        assert_eq!(response.status(), 200);
        let response = client
            .get(format!("{base}/api/v1/alerts"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let body: Value = response.json().await.unwrap();
        assert_eq!(body["total"], 1);
        let items = body["items"].as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["id"], visible.to_string());
    }
    let recorded: i64 =
        sqlx::query_scalar("SELECT count(*) FROM alert_deliveries WHERE user_id=$1")
            .bind(user)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        recorded, 2,
        "channel filtering must preserve delivery and deduplication history"
    );
    sqlx::query("DELETE FROM users WHERE id=$1")
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM programs WHERE id=$1")
        .bind(program)
        .execute(&pool)
        .await
        .unwrap();
}

async fn fixture_program(pool: &sqlx::PgPool, title: &str, regions: Option<Vec<&str>>) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO programs(id,program_type,source_type,source_id,title,program_status,regions,is_active) VALUES($1,'policy','manual',$2,$3,'active',$4,true)")
        .bind(id).bind(id.to_string()).bind(title).bind(regions)
        .execute(pool).await.unwrap();
    id
}

#[tokio::test]
async fn region_replay_busan_excludes_yongin_and_unknown_but_keeps_nationwide_and_pagination() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let base = start_test_server().await;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .unwrap();
    let client = Client::new();
    let term = format!("regionreplay{}", Uuid::new_v4().simple());
    let busan = fixture_program(&pool, &format!("{term} 부산"), Some(vec!["busan"])).await;
    let yongin = fixture_program(&pool, &format!("{term} 용인"), Some(vec!["gyeonggi"])).await;
    let nationwide = fixture_program(&pool, &format!("{term} 전국"), Some(vec![])).await;
    let unknown = fixture_program(&pool, &format!("{term} 미확정"), Some(vec!["unknown"])).await;
    let legacy = fixture_program(&pool, &format!("{term} 전국 기존자료"), None).await;
    let fixture_ids = vec![busan, yongin, nationwide, unknown, legacy];
    let mut seen = Vec::new();
    for page in 1..=3 {
        let response = client
            .get(format!("{base}/api/v1/programs"))
            .query(&[
                ("q", term.as_str()),
                ("region", "부산광역시"),
                ("per_page", "1"),
                ("page", &page.to_string()),
            ])
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let body: Value = response.json().await.unwrap();
        assert_eq!(body["total"], 3);
        assert_eq!(body["has_next"], page < 3);
        let items = body["items"].as_array().unwrap();
        assert_eq!(items.len(), 1);
        seen.push(Uuid::parse_str(items[0]["id"].as_str().unwrap()).unwrap());
    }
    seen.sort();
    let mut expected = vec![busan, nationwide, legacy];
    expected.sort();
    assert_eq!(
        seen, expected,
        "count, items and pagination must share the same region filter"
    );

    for region in ["busan", "부산광역시", " BUSAN "] {
        let response = client
            .post(format!("{base}/api/v1/recommend/preview?scope=all"))
            .json(&json!({"birth_year":2000,"region_code":region}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let body: Value = response.json().await.unwrap();
        for id in &expected {
            assert!(
                response_contains_program(&body, *id),
                "matching/nationwide program {id} must remain"
            );
        }
        for id in [yongin, unknown] {
            assert!(
                !response_contains_program(&body, id),
                "foreign/unknown region must not be recommended"
            );
        }
    }
    for region in ["unknown", "용인시", ""] {
        assert_eq!(
            client
                .get(format!("{base}/api/v1/programs"))
                .query(&[("region", region)])
                .send()
                .await
                .unwrap()
                .status(),
            400
        );
        assert_eq!(
            client
                .post(format!("{base}/api/v1/recommend/preview?scope=all"))
                .json(&json!({"birth_year":2000,"region_code":region}))
                .send()
                .await
                .unwrap()
                .status(),
            400
        );
    }
    sqlx::query("DELETE FROM programs WHERE id=ANY($1)")
        .bind(fixture_ids)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn campus_replay_requires_the_selected_university_and_respects_enrollment_without_rules() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let base = start_test_server().await;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .unwrap();
    let client = Client::new();
    let suffix = Uuid::new_v4().simple().to_string();
    let school_id = format!("scope-school-{suffix}");
    let school_name = format!("범위검증대학교{suffix}");
    let alias = format!("범위검증대{suffix}");
    sqlx::query("INSERT INTO universities(id,name,aliases,official_url) VALUES($1,$2,$3,'https://example.edu')")
        .bind(&school_id).bind(&school_name).bind(vec![&alias]).execute(&pool).await.unwrap();
    let id = fixture_program(&pool, "대학선택 범위 회귀테스트", Some(vec![])).await;
    sqlx::query("UPDATE programs SET program_type='campus_benefit',university_id=$2,school_scope=$3,normalized_payload=$4 WHERE id=$1")
        .bind(id).bind(&school_id).bind(json!([school_name])).bind(json!({"enrollment_statuses":["enrolled","재학"]}))
        .execute(&pool).await.unwrap();
    for (school, status, eligible) in [
        (None, Some("enrolled"), false),
        (Some("선택하지않은대학교"), Some("enrolled"), false),
        (Some(school_name.as_str()), Some("enrolled"), true),
        (Some(alias.as_str()), Some("재학"), true),
        (Some(school_id.as_str()), Some("on_leave"), false),
    ] {
        let response = client.post(format!("{base}/api/v1/recommend/preview?scope=all"))
            .json(&json!({"birth_year":2000,"region_code":"busan","school_name":school,"enrollment_status":status}))
            .send().await.unwrap();
        assert_eq!(response.status(), 200);
        let body: Value = response.json().await.unwrap();
        assert_eq!(
            response_contains_program(&body, id),
            eligible,
            "school={school:?} status={status:?}"
        );
    }
    sqlx::query("DELETE FROM programs WHERE id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM universities WHERE id=$1")
        .bind(school_id)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn profile_replay_normalizes_region_persists_nickname_and_rejects_invalid_changes() {
    let base = start_test_server().await;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .unwrap();
    let client = Client::new();
    let (user, token, _) = create_test_user(&base).await;
    let response = client
        .post(format!("{base}/api/v1/profile"))
        .bearer_auth(&token)
        .json(&json!({"birth_year":2000,"region_code":"부산광역시","nickname":"  마지미  "}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["profile"]["region_code"], "busan");
    assert_eq!(body["profile"]["nickname"], "마지미");
    let response = client
        .post(format!("{base}/api/v1/profile"))
        .bearer_auth(&token)
        .json(&json!({"birth_year":2000,"region_code":"daegu"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.unwrap();
    assert_eq!(
        body["profile"]["nickname"], "마지미",
        "legacy clients must preserve the name"
    );
    let response = client
        .post(format!("{base}/api/v1/profile"))
        .bearer_auth(&token)
        .json(&json!({"birth_year":2000,"region_code":"daegu","city_code":" 27110 "}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["profile"]["city_code"], "27110");
    for payload in [
        json!({"birth_year":2000,"region_code":"typo"}),
        json!({"birth_year":3000,"region_code":"busan"}),
        json!({"birth_year":2000,"region_code":"busan","nickname":" "}),
        json!({"birth_year":2000,"region_code":"busan","nickname":"a".repeat(31)}),
        // A 시·군·구 outside the sido, or not a 시군구 code at all.
        json!({"birth_year":2000,"region_code":"busan","city_code":"27110"}),
        json!({"birth_year":2000,"region_code":"daegu","city_code":"gangnam"}),
        json!({"birth_year":2000,"region_code":"gwangju","city_code":"12300"}),
        // Well-formed but not a current 시·군·구.
        json!({"birth_year":2000,"region_code":"busan","city_code":"26999"}),
    ] {
        assert_eq!(
            client
                .post(format!("{base}/api/v1/profile"))
                .bearer_auth(&token)
                .json(&payload)
                .send()
                .await
                .unwrap()
                .status(),
            400
        );
    }
    let (region, city): (String, Option<String>) =
        sqlx::query_as("SELECT region_code, city_code FROM user_profiles WHERE user_id=$1")
            .bind(user)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        (region.as_str(), city.as_deref()),
        ("daegu", Some("27110")),
        "rejected writes must leave the profile intact"
    );
    sqlx::query("DELETE FROM users WHERE id=$1")
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
}

/// The optional answers are saved, read back, kept by a save that does not
/// name them (an older client), cleared by an explicit null, and 신혼 goes
/// with 기혼.
#[tokio::test]
async fn optional_answers_survive_partial_saves_and_clear_on_null() {
    let base = start_test_server().await;
    let client = Client::new();
    let (_, token, _) = create_test_user(&base).await;
    let save = |body: Value| {
        let client = client.clone();
        let token = token.clone();
        let base = base.clone();
        async move {
            let response = client
                .post(format!("{base}/api/v1/profile"))
                .bearer_auth(&token)
                .json(&body)
                .send()
                .await
                .unwrap();
            let status = response.status();
            (status, response.json::<Value>().await.unwrap())
        }
    };
    let (status, body) = save(json!({"birth_year": 2000, "region_code": "busan",
        "homeless_household": true, "marital_status": "married", "newlywed": true,
        "children_count": 1, "income_median_band": "le100"}))
    .await;
    assert_eq!(status, 200, "{body}");
    let p = &body["profile"];
    assert_eq!(p["homeless_household"], true);
    assert_eq!(p["marital_status"], "married");
    assert_eq!(p["newlywed"], true);
    assert_eq!(p["children_count"], 1);
    assert_eq!(p["income_median_band"], "le100");

    // An older client's save: the answers stay.
    let (status, body) = save(
        json!({"birth_year": 2000, "region_code": "busan", "employment_status": "unemployed"}),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let p = &body["profile"];
    assert_eq!(p["homeless_household"], true, "{p}");
    assert_eq!(p["marital_status"], "married");
    assert_eq!(p["newlywed"], true);
    assert_eq!(p["income_median_band"], "le100");

    // Unmarried drops 신혼; null clears an answer.
    let (status, body) = save(json!({"birth_year": 2000, "region_code": "busan",
        "marital_status": "unmarried", "newlywed": true, "homeless_household": null}))
    .await;
    assert_eq!(status, 200, "{body}");
    let p = &body["profile"];
    assert_eq!(p["marital_status"], "unmarried");
    assert_eq!(p["newlywed"], Value::Null);
    assert_eq!(p["homeless_household"], Value::Null);
    assert_eq!(p["children_count"], 1);

    let (status, _) = save(json!({"birth_year": 2000, "region_code": "busan",
        "income_median_band": "le70"}))
    .await;
    assert_eq!(status, 400);
    let response = client
        .get(format!("{base}/api/v1/profile"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["profile"]["income_median_band"], "le100");
}

/// The API contract (review2-client M-1, arch M1): every answer outside the
/// base profile is kept when a save omits its key, and a group flag is
/// three-state at rest: an answered "no" reads back as `false`, an
/// unanswered one as `null`. One sample value per keep-when-absent field
/// (`routes::profile::KEEP_WHEN_ABSENT`, pinned there).
#[tokio::test]
async fn every_answer_beyond_the_base_profile_is_kept_when_a_save_omits_it() {
    let base = start_test_server().await;
    let client = Client::new();
    let (user, token, _) = create_test_user(&base).await;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .unwrap();
    // Review B-M2: the old bool columns an older image reads are never NULL
    // and follow the answers ("no" and unanswered both read `false` there).
    let old_columns = |user: Uuid| {
        let pool = pool.clone();
        async move {
            sqlx::query_as::<_, (Option<bool>, Option<bool>, Option<bool>, Option<bool>)>(
                "SELECT has_disability, is_multicultural_family, is_low_income_household, veteran_family
                 FROM user_profiles WHERE user_id = $1",
            )
            .bind(user)
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };
    let save = |body: Value| {
        let client = client.clone();
        let token = token.clone();
        let base = base.clone();
        async move {
            let response = client
                .post(format!("{base}/api/v1/profile"))
                .bearer_auth(&token)
                .json(&body)
                .send()
                .await
                .unwrap();
            let status = response.status();
            (status, response.json::<Value>().await.unwrap())
        }
    };
    let answers = json!({
        "homeless_household": true, "marital_status": "married", "newlywed": true,
        "children_count": 2, "income_median_band": "le100", "telecom_carrier": "skt",
        "card_issuers": ["kb"], "pay_apps": ["payco"], "kosaf_support_bracket": 3,
        "has_disability": false, "is_multicultural_family": true,
        "is_low_income_household": false, "veteran_family": true
    });
    let mut first = json!({"birth_year": 2000, "region_code": "busan"});
    first
        .as_object_mut()
        .unwrap()
        .extend(answers.as_object().unwrap().clone());
    let (status, body) = save(first).await;
    assert_eq!(status, 200, "{body}");
    for (key, value) in answers.as_object().unwrap() {
        assert_eq!(&body["profile"][key], value, "{key}");
    }
    assert_eq!(
        old_columns(user).await,
        (Some(false), Some(true), Some(false), Some(true))
    );
    // An older client's save names none of them: every answer stays.
    let (status, body) = save(json!({"birth_year": 2000, "region_code": "busan"})).await;
    assert_eq!(status, 200, "{body}");
    for (key, value) in answers.as_object().unwrap() {
        assert_eq!(&body["profile"][key], value, "{key} kept");
    }
    // An explicit null clears a flag to unanswered.
    let (status, body) = save(json!({"birth_year": 2000, "region_code": "busan",
        "has_disability": null, "veteran_family": null}))
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["profile"]["has_disability"], Value::Null);
    assert_eq!(body["profile"]["veteran_family"], Value::Null);
    assert_eq!(body["profile"]["is_multicultural_family"], true);
    assert_eq!(
        old_columns(user).await,
        (Some(false), Some(true), Some(false), Some(false))
    );
    // A new account has answered no flag.
    let (fresh_user, fresh, _) = create_test_user(&base).await;
    let response = client
        .post(format!("{base}/api/v1/profile"))
        .bearer_auth(&fresh)
        .json(&json!({"birth_year": 2001, "region_code": "seoul"}))
        .send()
        .await
        .unwrap();
    let body: Value = response.json().await.unwrap();
    for flag in [
        "has_disability",
        "is_multicultural_family",
        "is_low_income_household",
        "veteran_family",
    ] {
        assert_eq!(body["profile"][flag], Value::Null, "{flag}");
    }
    assert_eq!(
        old_columns(fresh_user).await,
        (Some(false), Some(false), Some(false), Some(false))
    );
}

/// Holder answers (review C2): saved and read back; a save that omits them
/// keeps them (installed app builds do not know the fields); an explicit
/// null clears one; an empty list is the answer "none"; codes outside the
/// domain, duplicates and a list for the carrier are refused. The user id
/// is the token's, never the body's.
#[tokio::test]
async fn holder_answers_are_kept_when_absent_and_validated() {
    let base = start_test_server().await;
    let client = Client::new();
    let (user, token, _) = create_test_user(&base).await;
    let save = |body: Value| {
        let client = client.clone();
        let token = token.clone();
        let base = base.clone();
        async move {
            let response = client
                .post(format!("{base}/api/v1/profile"))
                .bearer_auth(&token)
                .json(&body)
                .send()
                .await
                .unwrap();
            let status = response.status();
            (status, response.json::<Value>().await.unwrap())
        }
    };
    // Before any answer: unanswered (null), not "none".
    let (status, body) = save(json!({"birth_year": 2001, "region_code": "busan"})).await;
    assert_eq!(status, 200, "{body}");
    for field in ["telecom_carrier", "card_issuers", "pay_apps"] {
        assert_eq!(body["profile"][field], Value::Null, "{field}");
    }
    let (status, body) = save(json!({"birth_year": 2001, "region_code": "busan",
        "telecom_carrier": "skt", "card_issuers": ["kb", "shinhan"], "pay_apps": [],
        "user_id": Uuid::new_v4()}))
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["user_id"], json!(user), "identity from the token");
    let p = &body["profile"];
    assert_eq!(p["telecom_carrier"], "skt");
    assert_eq!(p["card_issuers"], json!(["kb", "shinhan"]));
    assert_eq!(p["pay_apps"], json!([]), "an empty list is an answer");

    // An installed build's save names none of them: all three are kept.
    let (status, body) = save(
        json!({"birth_year": 2001, "region_code": "busan", "employment_status": "unemployed"}),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let p = &body["profile"];
    assert_eq!(p["telecom_carrier"], "skt", "{p}");
    assert_eq!(p["card_issuers"], json!(["kb", "shinhan"]));
    assert_eq!(p["pay_apps"], json!([]));

    // An explicit null clears one answer and keeps the others.
    let (status, body) = save(json!({"birth_year": 2001, "region_code": "busan",
        "card_issuers": null}))
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["profile"]["card_issuers"], Value::Null);
    assert_eq!(body["profile"]["telecom_carrier"], "skt");

    for bad in [
        json!({"telecom_carrier": "sk"}),
        json!({"telecom_carrier": ["skt"]}),
        json!({"card_issuers": ["kb", "kb"]}),
        json!({"card_issuers": ["visa"]}),
        json!({"pay_apps": "payco"}),
    ] {
        let mut body = json!({"birth_year": 2001, "region_code": "busan"});
        for (k, v) in bad.as_object().unwrap() {
            body[k] = v.clone();
        }
        let (status, reply) = save(body.clone()).await;
        assert!(status.is_client_error(), "{body}: {status} {reply}");
        let text = reply.to_string();
        assert!(
            !text.contains("violates") && !text.contains("SQL"),
            "{text}"
        );
    }
    let response = client
        .get(format!("{base}/api/v1/profile"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["profile"]["telecom_carrier"], "skt");
    assert_eq!(body["profile"]["card_issuers"], Value::Null);
}
