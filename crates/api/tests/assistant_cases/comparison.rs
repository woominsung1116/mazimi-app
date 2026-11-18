use super::*;

#[tokio::test]
async fn comparison_is_read_only_scoped_and_preserves_document_evidence() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let (base, pool, user, token) = setup().await;
    let (other_user, other_token, _) = create_test_user(&base).await;
    let a = program(&pool).await;
    let b = program(&pool).await;
    profile(&pool, user, None).await;
    sqlx::query("UPDATE programs SET benefit_amount_once=12345 WHERE id=$1")
        .bind(a)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE program_documents SET is_required=false,description='원본 · 최근 30일' WHERE program_id=$1").bind(b).execute(&pool).await.unwrap();
    let client = Client::new();
    let url = format!("{base}/api/v1/assistant/compare");
    let body = json!({"program_ids":[b,a]});
    assert_eq!(
        client.post(&url).json(&body).send().await.unwrap().status(),
        401
    );
    let plan = guidance(&base, &token, a).await;
    let task = plan["preparation"]["tasks"][0]["id"].as_str().unwrap();
    let saved = client.put(format!("{base}/api/v1/programs/{a}/preparation")).bearer_auth(&token)
        .json(&json!({"revision":plan["preparation"]["revision"],"completed_tasks":[task],"selected_channel":null})).send().await.unwrap();
    assert_eq!(saved.status(), 200);
    let response = client
        .post(&url)
        .bearer_auth(&token)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["cache-control"], "private, no-store");
    let result: Value = response.json().await.unwrap();
    assert_eq!(result["programs"][0]["program_id"], b.to_string());
    assert_eq!(result["programs"][1]["amounts"]["once"], 12345);
    assert_eq!(
        result["programs"][1]["preparation"]["completed_tasks"],
        json!([task])
    );
    assert_eq!(result["amounts_additive"], false);
    assert_eq!(result["pairs"][0]["status"], "unknown");
    let shared = &result["shared_documents"][0];
    assert_eq!(shared["name"], "재학증명서");
    assert_eq!(shared["uses"][0]["required"], false);
    assert!(shared["uses"][0]["evidence"]
        .as_str()
        .unwrap()
        .contains("최근 30일"));
    assert_eq!(shared["uses"][1]["required"], true);
    let other: Value = client
        .post(&url)
        .bearer_auth(&other_token)
        .json(&body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        other["programs"][1]["preparation"]["completed_tasks"],
        json!([])
    );
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM application_preparations WHERE user_id IN ($1,$2)",
    )
    .bind(user)
    .bind(other_user)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, 1, "comparison cannot create preparation rows");
    for invalid in [
        json!({"program_ids":[a,a]}),
        json!({"program_ids":[a,b,a,b,a]}),
    ] {
        assert_eq!(
            client
                .post(&url)
                .bearer_auth(&token)
                .json(&invalid)
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
            .json(&json!({"program_ids":[a,b],"user_id":user}))
            .send()
            .await
            .unwrap()
            .status(),
        422
    );
    sqlx::query("UPDATE programs SET deadline_at=now()-interval '1 minute' WHERE id=$1")
        .bind(b)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        client
            .post(&url)
            .bearer_auth(&token)
            .json(&body)
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
}

#[tokio::test]
async fn stack_never_guesses_from_provider_or_kosaf_and_rejects_unavailable_ids() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let (base, pool, _, _) = setup().await;
    let a = program(&pool).await;
    let b = program(&pool).await;
    let client = Client::new();
    let url = format!("{base}/api/v1/stack-check");
    for same in [true, false] {
        sqlx::query("UPDATE programs SET provider_name='같은 기관',source_id='kosaf:'||id::text WHERE id=$1 OR id=$2").bind(a).bind(b).execute(&pool).await.unwrap();
        if !same {
            sqlx::query(
                "UPDATE programs SET provider_name='다른 기관',source_id=id::text WHERE id=$1",
            )
            .bind(b)
            .execute(&pool)
            .await
            .unwrap();
        }
        let response = client
            .post(&url)
            .json(&json!({"program_ids":[a,b]}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["deprecation"], "true");
        assert_eq!(
            response.headers()["sunset"],
            "Sun, 31 Jan 2027 00:00:00 GMT"
        );
        let result: Value = response.json().await.unwrap();
        assert_eq!(result["pairs"][0]["status"], "unknown");
        assert_eq!(result["all_stackable"], false);
    }
    assert_eq!(
        client
            .post(&url)
            .json(&json!({"program_ids":[a,b,Uuid::new_v4()]}))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    sqlx::query("UPDATE programs SET is_active=false WHERE id=$1")
        .bind(b)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        client
            .post(&url)
            .json(&json!({"program_ids":[a,b]}))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
}

#[tokio::test]
async fn bundles_degrade_a_stale_stored_city_code_instead_of_failing() {
    let (base, pool, user, token) = setup().await;
    profile(&pool, user, None).await;
    // Rows written before city_code was validated (or after the table changed).
    for city in ["gangnam", "11110", "2641000000", "26999"] {
        sqlx::query("UPDATE user_profiles SET city_code=$2 WHERE user_id=$1")
            .bind(user)
            .bind(city)
            .execute(&pool)
            .await
            .unwrap();
        let response = Client::new()
            .get(format!(
                "{base}/api/v1/assistant/bundles?scenario=independent_living"
            ))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200, "{city}");
    }
    sqlx::query("DELETE FROM users WHERE id=$1")
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn bundles_share_hard_filters_and_never_change_profile_or_invent_missing_scope() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let (base, pool, user, token) = setup().await;
    sqlx::query("DELETE FROM user_profiles WHERE user_id=$1")
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
    let client = Client::new();
    let url = format!("{base}/api/v1/assistant/bundles?scenario=independent_living");
    assert_eq!(client.get(&url).send().await.unwrap().status(), 401);
    let missing: Value = client
        .get(&url)
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        missing["required_profile_fields"],
        json!(["birth_year", "region_code"])
    );
    assert_eq!(missing["groups"], json!([]));
    profile(&pool, user, None).await;
    let a = program(&pool).await;
    let b = program(&pool).await;
    let c = program(&pool).await;
    sqlx::query("UPDATE programs SET title='월세 지원 검증 후보',deadline_at=now()+interval '1 day' WHERE id=ANY($1)").bind(vec![a,b,c]).execute(&pool).await.unwrap();
    // OR/hard filter exclusions are shared, never broadened by situation.
    rules(
        &pool,
        b,
        json!({"field":"region_code","op":"eq","value":"seoul"}),
    )
    .await;
    sqlx::query("UPDATE programs SET regions=ARRAY['gyeonggi'] WHERE id=$1")
        .bind(c)
        .execute(&pool)
        .await
        .unwrap();
    rules(
        &pool,
        a,
        json!({"field":"region_code","op":"eq","value":"busan"}),
    )
    .await;
    let version: i32 =
        sqlx::query_scalar("SELECT profile_version FROM user_profiles WHERE user_id=$1")
            .bind(user)
            .fetch_one(&pool)
            .await
            .unwrap();
    let response = client.get(&url).bearer_auth(&token).send().await.unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["cache-control"], "private, no-store");
    let result: Value = response.json().await.unwrap();
    assert_eq!(result["profile_version"], version);
    let preview: Value = client
        .post(format!("{base}/api/v1/recommend/preview?scope=all"))
        .json(&json!({"birth_year":2000,"region_code":"busan","enrollment_status":"enrolled"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let allowed: std::collections::HashSet<_> = preview["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["program_id"].as_str().unwrap())
        .collect();
    let mut seen = std::collections::HashSet::new();
    for group in result["groups"].as_array().unwrap() {
        assert!(group["items"].as_array().unwrap().len() <= 3);
        for item in group["items"].as_array().unwrap() {
            let id = item["program_id"].as_str().unwrap();
            assert!(allowed.contains(id));
            assert_ne!(id, c.to_string());
            assert_ne!(id, b.to_string());
            assert!(seen.insert(id));
        }
    }
    assert_eq!(
        sqlx::query_scalar::<_, i32>("SELECT profile_version FROM user_profiles WHERE user_id=$1")
            .bind(user)
            .fetch_one(&pool)
            .await
            .unwrap(),
        version
    );
    assert_eq!(
        client
            .get(format!("{base}/api/v1/assistant/bundles?scenario=made_up"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
}

#[tokio::test]
async fn financial_products_are_not_compared_or_bundled() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let (base, pool, user, token) = setup().await;
    // Groups show the top 3; drop fixtures earlier bundle tests left behind.
    sqlx::query("UPDATE programs SET is_active=false WHERE title LIKE '월세 %검증%'")
        .execute(&pool)
        .await
        .unwrap();
    profile(&pool, user, None).await;
    let plain = program(&pool).await;
    let loan = program(&pool).await;
    let deposit = program(&pool).await;
    let culture = program(&pool).await;
    let card = program(&pool).await;
    // Code review L4: an enrichment-assigned finance category is financial
    // even when the program type is a corporate benefit.
    sqlx::query("UPDATE programs SET title='월세 카드 검증',program_type='corporate_benefit',benefit_category='finance' WHERE id=$1")
        .bind(card)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE programs SET title='월세 대출 검증',program_type='financial_product' WHERE id=$1",
    )
    .bind(loan)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE programs SET title='월세 적금 검증',source_type='fss_financial' WHERE id=$1",
    )
    .bind(deposit)
    .execute(&pool)
    .await
    .unwrap();
    // Published category decides; the title keyword alone would say housing.
    // "청년 대학생" earns the youth and student relevance the seeded culture
    // rows have, so the fixture makes the group's top three.
    sqlx::query(
        "UPDATE programs SET title='청년 대학생 월세 문화패스 검증',benefit_category='culture' WHERE id=$1",
    )
    .bind(culture)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("UPDATE programs SET title='월세 지원 검증' WHERE id=$1")
        .bind(plain)
        .execute(&pool)
        .await
        .unwrap();
    let client = Client::new();
    for ids in [
        json!([plain, loan]),
        json!([deposit, plain]),
        json!([card, plain]),
    ] {
        let response = client
            .post(format!("{base}/api/v1/assistant/compare"))
            .bearer_auth(&token)
            .json(&json!({ "program_ids": ids }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400);
        let body: Value = response.json().await.unwrap();
        assert!(
            body["error"].as_str().unwrap().contains("금융상품"),
            "{body}"
        );
    }
    let bundle: Value = client
        .get(format!(
            "{base}/api/v1/assistant/bundles?scenario=independent_living"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let group_of = |id: Uuid| {
        bundle["groups"].as_array().unwrap().iter().find(|g| {
            g["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|i| i["program_id"] == id.to_string())
        })
    };
    assert!(group_of(loan).is_none(), "loan bundled: {bundle}");
    assert!(group_of(deposit).is_none(), "deposit bundled: {bundle}");
    assert!(group_of(card).is_none(), "finance card bundled: {bundle}");
    assert_eq!(
        group_of(culture).unwrap_or_else(|| panic!("culture not bundled: {bundle}"))["title"],
        "일상 소비 혜택"
    );
    assert_eq!(group_of(plain).unwrap()["title"], "주거·생활 기반");
    sqlx::query("UPDATE programs SET is_active=false WHERE id=ANY($1)")
        .bind(vec![plain, loan, deposit, culture, card])
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM users WHERE id=$1")
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
}
