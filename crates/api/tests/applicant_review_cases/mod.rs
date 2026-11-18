//! Reviewed applicant classes (`program_applicant_reviews`) applied to the
//! recommendation list: hide, demote with a note, stale text, no un-hide,
//! and the admin route's access control and evidence check.

use super::*;

const NOTE: &str = "기업·대학 등 참여기관을 통해 참여해요";

async fn pool() -> sqlx::PgPool {
    PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .unwrap()
}

/// A busan 온통청년 row in a throwaway category, with the given target text.
async fn youth_row(pool: &sqlx::PgPool, id: Uuid, category: &str, title: &str, target: &str) {
    let raw = json!({
        "plcyNm": title,
        "plcyExplnCn": "청년 역량 강화를 위한 사업",
        "addAplyQlfcCndCn": target,
        "plcyAplyMthdCn": "온라인 신청",
        "zipCd": "26410"
    });
    sqlx::query("INSERT INTO programs(id,program_type,source_type,source_id,title,program_status,regions,is_active,raw_payload) VALUES($1,$2,'youth_center',$1::text,$3,'active',ARRAY['busan'],true,$4)")
        .bind(id).bind(category).bind(title).bind(raw).execute(pool).await.unwrap();
}

async fn review(pool: &sqlx::PgPool, id: Uuid, class: &str, hash: &str) {
    sqlx::query("INSERT INTO program_applicant_reviews(source_type,source_id,class,evidence,reviewer,text_hash) VALUES('youth_center',$1::text,$2,'청년 역량 강화를 위한 사업','test:A+B',$3)")
        .bind(id).bind(class).bind(hash).execute(pool).await.unwrap();
}

async fn current_hash(pool: &sqlx::PgPool, id: Uuid) -> String {
    let raw: Value = sqlx::query_scalar("SELECT raw_payload FROM programs WHERE id=$1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap();
    mazimi_core::applicant_review::text_hash("youth_center", &raw).unwrap()
}

fn index_of(body: &Value, id: Uuid) -> Option<usize> {
    let expected = id.to_string();
    body["items"]
        .as_array()
        .unwrap()
        .iter()
        .position(|item| item["program_id"].as_str() == Some(expected.as_str()))
}

fn not_individual(body: &Value) -> u64 {
    body["filter_summary"]["excluded_by_reason"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["reason"] == "not_individual")
        .map_or(0, |r| r["count"].as_u64().unwrap())
}

async fn cleanup(pool: &sqlx::PgPool, category: &str) {
    sqlx::query("DELETE FROM program_applicant_reviews WHERE source_type='youth_center' AND source_id IN (SELECT id::text FROM programs WHERE program_type=$1)")
        .bind(category).execute(pool).await.unwrap();
    // program_applicant_review_log is append-only (a trigger rejects
    // DELETE); its rows name throwaway source ids and stay.
    sqlx::query("DELETE FROM programs WHERE program_type=$1")
        .bind(category)
        .execute(pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn reviewed_applicant_class_hides_demotes_and_never_unhides() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let base = start_test_server().await;
    let pool = pool().await;
    let client = Client::new();
    let category = format!("applicant_{}", Uuid::new_v4().simple());
    let target = "만 19~34세 청년";
    let organisation = Uuid::new_v4();
    let stale = Uuid::new_v4();
    let text_hidden = Uuid::new_v4();
    // The demoted row gets the smaller id so that, without the demotion,
    // the catalogue's id tie-break would list it first.
    let (mut indirect, mut direct) = (Uuid::new_v4(), Uuid::new_v4());
    if indirect > direct {
        std::mem::swap(&mut indirect, &mut direct);
    }
    youth_row(
        &pool,
        organisation,
        &category,
        "청년 정책 협의체 운영 검증",
        target,
    )
    .await;
    youth_row(&pool, indirect, &category, "청년 인재 양성 검증", target).await;
    youth_row(&pool, direct, &category, "청년 인재 양성 검증", target).await;
    youth_row(&pool, stale, &category, "청년 일경험 지원 검증", target).await;
    youth_row(
        &pool,
        text_hidden,
        &category,
        "AI 혁신인재 검증",
        "지원대학 4곳 대상(개인 신청 불가)",
    )
    .await;

    let list = || {
        let client = client.clone();
        let base = base.clone();
        async move {
            let response = client
                .post(format!("{base}/api/v1/recommend/preview"))
                .json(&json!({"birth_year":2000,"region_code":"busan"}))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), 200);
            response.json::<Value>().await.unwrap()
        }
    };
    let before = list().await;
    for id in [organisation, indirect, direct, stale] {
        assert!(index_of(&before, id).is_some(), "{id} listed before review");
    }
    assert!(index_of(&before, text_hidden).is_none());

    review(
        &pool,
        organisation,
        "organisation",
        &current_hash(&pool, organisation).await,
    )
    .await;
    review(
        &pool,
        indirect,
        "indirect",
        &current_hash(&pool, indirect).await,
    )
    .await;
    // Reviewed as `business`, then the upstream text changed.
    review(&pool, stale, "business", &current_hash(&pool, stale).await).await;
    sqlx::query("UPDATE programs SET raw_payload = raw_payload || '{\"plcySprtCn\":\"월 50만원\"}' WHERE id=$1")
        .bind(stale).execute(&pool).await.unwrap();
    // An `individual` review cannot list what the text rules hide.
    review(
        &pool,
        text_hidden,
        "individual",
        &current_hash(&pool, text_hidden).await,
    )
    .await;

    let after = list().await;
    assert!(
        index_of(&after, organisation).is_none(),
        "organisation hidden"
    );
    assert_eq!(
        not_individual(&after),
        not_individual(&before) + 1,
        "counted as not_individual"
    );
    assert!(
        index_of(&after, stale).is_some(),
        "stale review has no effect"
    );
    assert!(index_of(&after, text_hidden).is_none(), "no un-hide");

    let (i, d) = (
        index_of(&after, indirect).expect("indirect stays listed"),
        index_of(&after, direct).unwrap(),
    );
    assert!(i > d, "indirect {i} ranked below identical direct {d}");
    let items = after["items"].as_array().unwrap();
    assert_eq!(items[i]["missing_checks"][0], NOTE, "{}", items[i]);
    assert!(
        !items[d]["missing_checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c == NOTE),
        "{}",
        items[d]
    );
    let summary = &after["filter_summary"];
    assert_eq!(
        summary["included"].as_u64().unwrap() + summary["excluded"].as_u64().unwrap(),
        summary["considered"].as_u64().unwrap(),
        "{summary}"
    );

    cleanup(&pool, &category).await;
}

#[tokio::test]
async fn applicant_review_admin_route_is_admin_only_and_checks_evidence() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let base = start_test_server().await;
    let pool = pool().await;
    let client = Client::new();
    let category = format!("applicant_admin_{}", Uuid::new_v4().simple());
    let id = Uuid::new_v4();
    youth_row(
        &pool,
        id,
        &category,
        "청년 정책 협의체 운영 검증",
        "만 19~34세 청년",
    )
    .await;
    let (_, user_token, _) = create_test_user(&base).await;
    let (admin_id, admin_token) = create_test_admin(&base).await;
    let url = format!("{base}/api/v1/admin/programs/{id}/applicant-review");
    let list_url = format!("{base}/api/v1/admin/applicant-reviews");
    let body =
        json!({"class":"organisation","evidence":"청년 정책 협의체 운영","reviewer":"curator:A+B"});

    // No token, then a non-admin token.
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

    // Evidence that is not a quote of the notice is rejected; so is an
    // unknown class.
    for bad in [
        json!({"class":"organisation","evidence":"기관만 신청할 수 있는 사업","reviewer":"curator:A"}),
        json!({"class":"employer","evidence":"청년 정책 협의체 운영","reviewer":"curator:A"}),
        json!({"class":"organisation","evidence":"청년 정책 협의체 운영","reviewer":""}),
        // Review M1: the admin read another version of the text.
        json!({"class":"organisation","evidence":"청년 정책 협의체 운영","reviewer":"curator:A","expected_text_hash":"0".repeat(64)}),
    ] {
        let response = client
            .put(&url)
            .bearer_auth(&admin_token)
            .json(&bad)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400, "{bad}");
    }
    let stored: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM program_applicant_reviews WHERE source_id=$1::text",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(stored, 0);

    let response = client
        .put(&url)
        .bearer_auth(&admin_token)
        .json(&json!({"class":"organisation","evidence":"청년 정책 협의체 운영","reviewer":"curator:A","expected_text_hash":"0".repeat(64)}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.json::<Value>().await.unwrap()["code"],
        "text_changed"
    );
    let mut body = body;
    body["expected_text_hash"] = json!(current_hash(&pool, id).await);
    let response = client
        .put(&url)
        .bearer_auth(&admin_token)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let saved: Value = response.json().await.unwrap();
    assert_eq!(saved["class"], "organisation");
    assert_eq!(saved["stale"], false);
    let (hash, reviewer): (String, String) = sqlx::query_as(
        "SELECT text_hash, reviewer FROM program_applicant_reviews WHERE source_type='youth_center' AND source_id=$1::text",
    )
    .bind(id).fetch_one(&pool).await.unwrap();
    assert_eq!(hash, current_hash(&pool, id).await);
    assert_eq!(reviewer, "curator:A+B");

    let listed: Value = client
        .get(format!("{list_url}?class=organisation&limit=200"))
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
    // The listing gives the hash a later PUT passes as expected_text_hash.
    assert_eq!(row["current_text_hash"], current_hash(&pool, id).await);
    assert_eq!(row["stale"], false);

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
        "SELECT action, actor FROM program_applicant_review_log WHERE source_type='youth_center' AND source_id=$1::text ORDER BY id",
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

    // Service-only tables: RLS on and forced, one policy on the service role.
    for table in ["program_applicant_reviews", "program_applicant_review_log"] {
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
