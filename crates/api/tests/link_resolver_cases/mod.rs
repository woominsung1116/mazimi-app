//! The worker's link selection reaches the public programme detail; the
//! reviewed application resource stays on top; checklists keep their
//! progress when the selection moves.
use super::*;

async fn program(base: &str, id: Uuid) -> Value {
    Client::new()
        .get(format!("{base}/api/v1/programs/{id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

async fn candidate(
    pool: &sqlx::PgPool,
    program: Uuid,
    field: &str,
    url: &str,
    kind: &str,
    access: &str,
) -> Uuid {
    let rank: i16 = match kind {
        "application" => 0,
        "notice" => 1,
        "information" => 2,
        _ => 3,
    };
    sqlx::query_scalar(
        "INSERT INTO program_link_candidates(program_id,field,field_rank,url,host,origin,kind,rank,sub,access,check_status)
         VALUES($1,$2,0,$3,'test.example','agency',$4,$5,'test',$6,'ok') RETURNING id",
    )
    .bind(program)
    .bind(field)
    .bind(url)
    .bind(kind)
    .bind(rank)
    .bind(access)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn select(
    pool: &sqlx::PgPool,
    program: Uuid,
    state: &str,
    selected: Option<Uuid>,
    notice: Option<Uuid>,
) {
    sqlx::query(
        "INSERT INTO program_link_selections(program_id,input_hash,state,selected_id,notice_id) VALUES($1,'',$2,$3,$4)
         ON CONFLICT(program_id) DO UPDATE SET state=excluded.state,selected_id=excluded.selected_id,notice_id=excluded.notice_id",
    )
    .bind(program)
    .bind(state)
    .bind(selected)
    .bind(notice)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn the_selected_link_replaces_the_official_url_with_a_server_chosen_label() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let base = start_test_server().await;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .unwrap();
    let id = Uuid::new_v4();
    // The audit's closed Google form as the stored official URL.
    sqlx::query("INSERT INTO programs(id,program_type,source_type,title,program_status,is_active,official_url,application_method) VALUES($1,'policy','manual','2026년 링크 선택 검증 fixture','active',true,'https://forms.gle/35Dr51sLkNxSBvBH8','온라인신청')")
        .bind(id).execute(&pool).await.unwrap();
    let (_, token, _) = create_test_user(&base).await;
    let prep = format!("{base}/api/v1/programs/{id}/preparation");
    let revision = |plan: Value| plan["revision"].as_str().unwrap().to_string();
    let before: Value = Client::new()
        .get(&prep)
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    // Not resolved yet: today's link.
    let policy = &program(&base, id).await["application_policy"];
    assert_eq!(
        policy["destination"]["url"],
        "https://forms.gle/35Dr51sLkNxSBvBH8"
    );
    assert_eq!(policy["destination"]["kind"], "unverified_page");

    let app = "https://apply.example.go.kr/youth/aplyReqst.do?id=7";
    let notice = "https://www.example.go.kr/bbs/view.do?idx=7";
    let a = candidate(&pool, id, "aplyUrlAddr", app, "application", "direct").await;
    let b = candidate(&pool, id, "refUrlAddr1", notice, "notice", "direct").await;
    select(&pool, id, "selected", Some(a), Some(b)).await;
    let detail = program(&base, id).await;
    let policy = &detail["application_policy"];
    assert_eq!(policy["destination"]["url"], app);
    assert_eq!(policy["destination"]["kind"], "application");
    assert_eq!(policy["destination"]["label"], "신청 페이지로 바로 가기");
    assert_eq!(
        policy["destination"]["origin"],
        "https://apply.example.go.kr"
    );
    assert_eq!(policy["destination"]["evidence"], "link_resolver");
    assert_eq!(policy["notice_destination"]["url"], notice);
    assert_eq!(policy["notice_destination"]["label"], "공고 안내 보기");
    assert_eq!(policy["can_submit"], false);
    // The selection is not part of the API contract beyond the destination.
    assert!(policy["resources"].get("link").is_none());
    // A moved link never resets a saved checklist.
    let after: Value = Client::new()
        .get(&prep)
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(revision(after.clone()), revision(before));
    assert_eq!(after["policy"]["destination"]["url"], app);

    sqlx::query("UPDATE program_link_candidates SET access='login_required' WHERE id=$1")
        .bind(a)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        program(&base, id).await["application_policy"]["destination"]["label"],
        "로그인 후 신청하기"
    );

    // Everything broken: no button, and not the dead official URL either.
    select(&pool, id, "none", None, None).await;
    let policy = &program(&base, id).await["application_policy"];
    assert!(policy["destination"].is_null(), "{policy}");
    assert!(policy["notice_destination"].is_null());
    sqlx::query("DELETE FROM programs WHERE id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_reviewed_application_resource_stays_above_the_resolver_link() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let base = start_test_server().await;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .unwrap();
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO programs(id,program_type,source_type,title,program_status,is_active,official_url,application_method) VALUES($1,'policy','manual','2026년 링크 선택 검토 우선 fixture','active',true,'https://official.example.org/notice?intake=2026','온라인신청')")
        .bind(id).execute(&pool).await.unwrap();
    let (_, admin_token) = create_test_admin(&base).await;
    let client = Client::new();
    let row: Value = client
        .post(format!("{base}/api/v1/admin/programs/{id}/application-resources"))
        .bearer_auth(&admin_token)
        .json(&json!({"kind":"application","url":"https://form.example.org/apply?intake=2026","source_url":"https://official.example.org/notice?intake=2026","label":"신청하기"}))
        .send().await.unwrap().json().await.unwrap();
    let listing: Value = client
        .get(format!(
            "{base}/api/v1/admin/programs/{id}/application-resources"
        ))
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let review = client
        .put(format!("{base}/api/v1/admin/application-resources/{}", row["id"].as_str().unwrap()))
        .bearer_auth(&admin_token)
        .json(&json!({"decision":"verified","expected_revision":row["revision"],"program_fingerprint":listing["program_fingerprint"],
            "access":"direct","same_program_confirmed":true,"current_intake_confirmed":true,"destination_confirmed":true,
            "note":"공식 공고의 2026년 모집 회차와 신청 화면 주소를 대조한 테스트 근거입니다.","validity_hours":24}))
        .send().await.unwrap();
    assert_eq!(review.status(), 200, "{}", review.text().await.unwrap());
    let a = candidate(
        &pool,
        id,
        "aplyUrlAddr",
        "https://apply.example.go.kr/aplyReqst.do?id=8",
        "application",
        "direct",
    )
    .await;
    select(&pool, id, "selected", Some(a), None).await;
    let policy = &program(&base, id).await["application_policy"];
    assert_eq!(
        policy["destination"]["url"],
        "https://form.example.org/apply?intake=2026"
    );
    assert_eq!(policy["destination"]["evidence"], "reviewed_application");
    assert_eq!(policy["destination"]["label"], "신청하러 가기");
    sqlx::query("DELETE FROM programs WHERE id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
}
