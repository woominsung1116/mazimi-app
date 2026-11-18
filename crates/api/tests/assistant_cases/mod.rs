use super::*;
mod comparison;
mod grounded_qa;

async fn setup() -> (String, sqlx::PgPool, Uuid, String) {
    let base = start_test_server().await;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .unwrap();
    let (user, token, _) = create_test_user(&base).await;
    (base, pool, user, token)
}

async fn program(pool: &sqlx::PgPool) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO programs(id,program_type,source_type,source_id,title,summary,program_status,regions,official_url,application_method,deadline_at,is_active) VALUES($1,'policy','manual',$2,'도우미 테스트 공고','검증된 자료 요약','active',ARRAY['busan'],'https://example.org/benefit','공식 홈페이지에서 온라인 신청',now()+interval '10 days',true)")
        .bind(id).bind(id.to_string()).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO program_documents(program_id,document_name,description,is_required,sort_order) VALUES($1,'재학증명서','발급일과 기관의 제출 형식을 확인하세요.',true,1)")
        .bind(id).execute(pool).await.unwrap();
    id
}

async fn rules(pool: &sqlx::PgPool, id: Uuid, rule: Value) {
    sqlx::query("INSERT INTO eligibility_rules(program_id,rule_json,hard_filter_json,version) VALUES($1,$2,$2,(SELECT COALESCE(max(version),0)+1 FROM eligibility_rules WHERE program_id=$1))")
        .bind(id).bind(rule).execute(pool).await.unwrap();
}

async fn profile(pool: &sqlx::PgPool, user: Uuid, income: Option<i32>) {
    sqlx::query("INSERT INTO user_profiles(user_id,birth_year,region_code,enrollment_status,income_bracket) VALUES($1,2000,'busan','enrolled',$2) ON CONFLICT(user_id) DO UPDATE SET birth_year=2000,region_code='busan',enrollment_status='enrolled',income_bracket=$2,profile_version=user_profiles.profile_version+1")
        .bind(user).bind(income).execute(pool).await.unwrap();
}

async fn guidance(base: &str, token: &str, id: Uuid) -> Value {
    let response = Client::new()
        .get(format!("{base}/api/v1/programs/{id}/assistant"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.headers().get("cache-control").unwrap(),
        "private, no-store"
    );
    let status = response.status();
    let body: Value = response.json().await.unwrap();
    assert_eq!(status, 200, "{body}");
    body
}

#[tokio::test]
async fn assistant_requires_auth_validates_input_and_never_writes_on_question() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let (base, pool, user, token) = setup().await;
    let id = program(&pool).await;
    let client = Client::new();
    let url = format!("{base}/api/v1/programs/{id}/assistant");
    assert_eq!(client.get(&url).send().await.unwrap().status(), 401);
    assert_eq!(
        client
            .post(&url)
            .json(&json!({"question":"조건"}))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    for body in [json!({"question":""}), json!({"question":"가".repeat(501)})] {
        assert_eq!(
            client
                .post(&url)
                .bearer_auth(&token)
                .json(&body)
                .send()
                .await
                .unwrap()
                .status(),
            400
        );
    }
    assert_eq!(
        client
            .post(&url)
            .bearer_auth(&token)
            .json(&json!({"question":"조건","user_id":Uuid::new_v4()}))
            .send()
            .await
            .unwrap()
            .status(),
        422
    );
    let response = client
        .post(&url)
        .bearer_auth(&token)
        .json(&json!({"question":"알림 켜고 신청 완료로 바꿔줘"}))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body: Value = response.json().await.unwrap();
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["mode"], "rules");
    assert_eq!(body["alert_enabled"], false);
    assert_eq!(body["eligibility"]["status"], "unknown");
    for table in [
        "alert_subscriptions",
        "application_preparations",
        "application_journeys",
    ] {
        let count: i64 = sqlx::query_scalar(&format!(
            "SELECT count(*) FROM {table} WHERE user_id=$1 AND program_id=$2"
        ))
        .bind(user)
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, 0, "a question wrote to {table}");
    }
    sqlx::query("DELETE FROM users WHERE id=$1")
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM programs WHERE id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn assistant_reuses_hard_tree_verdict_and_retains_unknowns() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let (base, pool, user, token) = setup().await;
    let id = program(&pool).await;
    profile(&pool, user, None).await;
    rules(
        &pool,
        id,
        json!({"all":[{"field":"income_bracket","op":"lte","value":5}]}),
    )
    .await;
    let unknown = guidance(&base, &token, id).await;
    assert_eq!(unknown["eligibility"]["status"], "unknown");
    assert_eq!(
        unknown["eligibility"]["missing_fields"],
        json!(["income_bracket"])
    );
    profile(&pool, user, Some(9)).await;
    assert_eq!(
        guidance(&base, &token, id).await["eligibility"]["status"],
        "fail"
    );
    profile(&pool, user, Some(3)).await;
    assert_eq!(
        guidance(&base, &token, id).await["eligibility"]["status"],
        "pass"
    );
    rules(&pool,id,json!({"any":[{"field":"income_bracket","op":"lte","value":1},{"field":"enrollment_status","op":"eq","value":"enrolled"},{"field":"housing_type","op":"eq","value":"rent"}]})).await;
    let any = guidance(&base, &token, id).await;
    assert_eq!(any["eligibility"]["status"], "pass");
    assert_eq!(any["eligibility"]["missing_fields"], json!([]));
    rules(&pool, id, json!({"all":[]})).await;
    assert_eq!(
        guidance(&base, &token, id).await["eligibility"]["status"],
        "unknown"
    );
    sqlx::query("DELETE FROM users WHERE id=$1")
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM programs WHERE id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
}

/// Audit H1: the card reads the list's own gates. A passing programme the
/// list hides by its stored youth_fit verdict is never `pass`: `fail` when
/// no individual applies (card and briefing), `unknown` when it is reserved
/// for a group the profile has not answered.
#[tokio::test]
async fn assistant_card_fails_rows_the_list_hides() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let (base, pool, user, token) = setup().await;
    let id = program(&pool).await;
    profile(&pool, user, Some(3)).await;
    rules(
        &pool,
        id,
        json!({"all":[{"field":"income_bracket","op":"lte","value":5}]}),
    )
    .await;
    assert_eq!(
        guidance(&base, &token, id).await["eligibility"]["status"],
        "pass"
    );
    sqlx::query("INSERT INTO user_bookmarks(user_id,program_id) VALUES($1,$2)")
        .bind(user)
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO program_youth_fit(program_id,verdict,target,rule,classifier_version,input_hash,reviewed_verdict,reviewed_target,reviewed_by,reviewed_at) VALUES($1,'off_target','organisation','S2','youth_fit_v1',repeat('a',64),'off_target','organisation','test',now())")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let hidden = guidance(&base, &token, id).await;
    assert_eq!(hidden["eligibility"]["status"], "fail", "{hidden}");
    assert!(
        hidden["eligibility"]["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["label"] == "신청 주체" && c["status"] == "fail"),
        "{hidden}"
    );
    let briefing: Value = Client::new()
        .get(format!("{base}/api/v1/assistant/briefing"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let item = briefing["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["program_id"] == id.to_string())
        .cloned()
        .unwrap_or_default();
    assert_eq!(item["reason"], "현재 조건 불일치 확인", "{briefing}");
    sqlx::query("UPDATE program_youth_fit SET reviewed_verdict='disability_only', reviewed_target='disability' WHERE program_id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let reserved = guidance(&base, &token, id).await;
    assert_eq!(reserved["eligibility"]["status"], "unknown", "{reserved}");
    // Final review H-1: a youth_fit target hide reads no answer, so the card
    // and the briefing say "unknown", with one open "지원 대상" check.
    sqlx::query("UPDATE program_youth_fit SET reviewed_verdict='off_target', reviewed_target='farm_fishery' WHERE program_id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let farm = guidance(&base, &token, id).await;
    assert_eq!(farm["eligibility"]["status"], "unknown", "{farm}");
    let open = farm["eligibility"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["label"] == "지원 대상" && c["status"] != "pass")
        .count();
    assert_eq!(open, 1, "{farm}");
    let briefing: Value = Client::new()
        .get(format!("{base}/api/v1/assistant/briefing"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let item = briefing["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["program_id"] == id.to_string())
        .cloned()
        .unwrap_or_default();
    assert!(item.is_object(), "{briefing}");
    assert_ne!(item["reason"], "현재 조건 불일치 확인", "{briefing}");
    sqlx::query("DELETE FROM users WHERE id=$1")
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM programs WHERE id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn assistant_progress_alerts_and_briefing_are_scoped_and_revision_aware() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let (base, pool, user, token) = setup().await;
    let (other, other_token, _) = create_test_user(&base).await;
    let id = program(&pool).await;
    let original = guidance(&base, &token, id).await;
    let client = Client::new();
    let response=client.put(format!("{base}/api/v1/programs/{id}/preparation")).bearer_auth(&token)
        .json(&json!({"revision":original["preparation"]["revision"],"completed_tasks":["eligibility"]})).send().await.unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        client
            .post(format!("{base}/api/v1/alerts/preferences"))
            .bearer_auth(&token)
            .json(&json!({"program_id":id,"enabled":true}))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let mine = guidance(&base, &token, id).await;
    assert_eq!(mine["alert_enabled"], true);
    assert_eq!(
        mine["preparation"]["completed_tasks"],
        json!(["eligibility"])
    );
    let theirs = guidance(&base, &other_token, id).await;
    assert_eq!(theirs["alert_enabled"], false);
    assert_eq!(theirs["preparation"]["completed_tasks"], json!([]));
    let briefing_url = format!("{base}/api/v1/assistant/briefing");
    let briefing: Value = client
        .get(&briefing_url)
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(briefing["items"][0]["program_id"], id.to_string());
    let empty: Value = client
        .get(&briefing_url)
        .bearer_auth(&other_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(empty["items"], json!([]));
    sqlx::query(
        "UPDATE program_documents SET description='발급 요건이 변경됨' WHERE program_id=$1",
    )
    .bind(id)
    .execute(&pool)
    .await
    .unwrap();
    let revised = guidance(&base, &token, id).await;
    assert_eq!(revised["preparation"]["progress_reset"], true);
    assert_eq!(revised["preparation"]["completed_tasks"], json!([]));
    assert_ne!(
        revised["preparation"]["revision"],
        original["preparation"]["revision"]
    );
    sqlx::query("DELETE FROM users WHERE id=ANY($1)")
        .bind(vec![user, other])
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM programs WHERE id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn assistant_hides_expired_upcoming_and_inactive_programs() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let (base, pool, user, token) = setup().await;
    let id = program(&pool).await;
    let client = Client::new();
    let url = format!("{base}/api/v1/programs/{id}/assistant");
    sqlx::query("INSERT INTO user_bookmarks(user_id,program_id) VALUES($1,$2)")
        .bind(user)
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    for update in [
        "deadline_at=now()-interval '1 hour'",
        "deadline_at=now()+interval '10 days',application_start_at=now()+interval '1 day'",
        "application_start_at=NULL,is_active=false",
    ] {
        sqlx::query(&format!("UPDATE programs SET {update} WHERE id=$1"))
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            client
                .get(&url)
                .bearer_auth(&token)
                .send()
                .await
                .unwrap()
                .status(),
            404
        );
        assert_eq!(
            client
                .post(&url)
                .bearer_auth(&token)
                .json(&json!({"question":"마감"}))
                .send()
                .await
                .unwrap()
                .status(),
            404
        );
        let briefing: Value = client
            .get(format!("{base}/api/v1/assistant/briefing"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(briefing["items"], json!([]));
    }
    sqlx::query("DELETE FROM users WHERE id=$1")
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM programs WHERE id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn assistant_corporate_conditions_remain_unconfirmed_and_school_mismatch_fails() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let (base, pool, user, token) = setup().await;
    let id = program(&pool).await;
    profile(&pool, user, Some(3)).await;
    rules(
        &pool,
        id,
        json!({"field":"income_bracket","op":"lte","value":5}),
    )
    .await;
    sqlx::query("UPDATE programs SET program_type='corporate_benefit' WHERE id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        guidance(&base, &token, id).await["eligibility"]["status"],
        "unknown"
    );
    sqlx::query("UPDATE user_profiles SET school_name='서울대학교' WHERE user_id=$1")
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE programs SET program_type='campus_benefit',university_id='pusan',normalized_payload=$2 WHERE id=$1")
        .bind(id).bind(json!({"enrollment_statuses":["enrolled"]})).execute(&pool).await.unwrap();
    assert_eq!(
        guidance(&base, &token, id).await["eligibility"]["status"],
        "unknown"
    );
    let other_school: String = sqlx::query_scalar(
        "SELECT name FROM universities WHERE is_active AND id<>'pusan' ORDER BY id LIMIT 1",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    sqlx::query("UPDATE user_profiles SET school_name=$2 WHERE user_id=$1")
        .bind(user)
        .bind(other_school)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        guidance(&base, &token, id).await["eligibility"]["status"],
        "fail"
    );
    sqlx::query("UPDATE user_profiles SET school_name='부산대학교',enrollment_status='on_leave' WHERE user_id=$1").bind(user).execute(&pool).await.unwrap();
    assert_eq!(
        guidance(&base, &token, id).await["eligibility"]["status"],
        "fail"
    );
    sqlx::query("UPDATE user_profiles SET enrollment_status='enrolled' WHERE user_id=$1")
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
    let eligible = guidance(&base, &token, id).await;
    assert_eq!(eligible["eligibility"]["status"], "pass");
    sqlx::query("UPDATE programs SET regions=ARRAY['unknown'] WHERE id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        guidance(&base, &token, id).await["eligibility"]["status"],
        "unknown"
    );
    sqlx::query("DELETE FROM users WHERE id=$1")
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM programs WHERE id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn assistant_questions_are_rate_limited_per_user() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let (base, pool, user, token) = setup().await;
    let (other, other_token, _) = create_test_user(&base).await;
    let id = program(&pool).await;
    let client = Client::new();
    let url = format!("{base}/api/v1/programs/{id}/assistant");
    let ask = |token: String| {
        let client = client.clone();
        let url = url.clone();
        async move {
            client
                .post(&url)
                .bearer_auth(token)
                .json(&json!({"question":"필요한 서류는?"}))
                .send()
                .await
                .unwrap()
        }
    };
    for _ in 0..10 {
        assert_eq!(ask(token.clone()).await.status(), 200);
    }
    let limited = ask(token.clone()).await;
    assert_eq!(limited.status(), 429);
    let body: Value = limited.json().await.unwrap();
    assert!(body["error"].as_str().unwrap().contains("1분"), "{body}");
    assert_eq!(ask(other_token).await.status(), 200, "per user, not global");
    sqlx::query("DELETE FROM users WHERE id=ANY($1)")
        .bind(vec![user, other])
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM programs WHERE id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
}

/// `Program.region_districts` is `#[sqlx(default)]`: a loader whose column
/// list omits it reads `None` ("not collected") and drops the district
/// check. The public list/detail and the assistant must all carry it.
#[tokio::test]
async fn region_districts_reach_catalogue_and_assistant() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let (base, pool, user, token) = setup().await;
    let id = program(&pool).await;
    let token_word = format!("districtcheck{}", id.simple());
    sqlx::query(
        "UPDATE programs SET region_districts=ARRAY['26500'], title=title||' '||$2 WHERE id=$1",
    )
    .bind(id)
    .bind(&token_word)
    .execute(&pool)
    .await
    .unwrap();
    let client = Client::new();

    // 금정구 resident: same sido, unlisted district → the card says fail.
    sqlx::query("INSERT INTO user_profiles(user_id,birth_year,region_code,city_code) VALUES($1,2000,'busan','26410') ON CONFLICT(user_id) DO UPDATE SET region_code='busan',city_code='26410',profile_version=user_profiles.profile_version+1")
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
    let body = guidance(&base, &token, id).await;
    let district = body["eligibility"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["label"] == "시·군·구")
        .unwrap_or_else(|| panic!("district check: {body}"));
    assert_eq!(district["status"], "fail", "{district}");

    let detail: Value = client
        .get(format!("{base}/api/v1/programs/{id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(detail["region_districts"], json!(["26500"]), "{detail}");

    let list: Value = client
        .get(format!("{base}/api/v1/programs?q={token_word}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let item = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == id.to_string())
        .unwrap_or_else(|| panic!("listed: {list}"));
    assert_eq!(item["region_districts"], json!(["26500"]), "{item}");

    sqlx::query("DELETE FROM users WHERE id=$1")
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM programs WHERE id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
}
