//! HTTP contracts, lifecycle and access controls; no external submissions.
use super::*;

async fn fixture(pool: &sqlx::PgPool) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO programs(id,program_type,source_type,title,program_status,is_active,official_url,application_method) VALUES($1,'policy','manual','2026년 공개 양식 검증 fixture','active',true,'https://official.example.org/notice?intake=2026','온라인신청||방문신청')")
        .bind(id).execute(pool).await.unwrap();
    id
}
async fn list(base: &str, token: &str, id: Uuid) -> Value {
    Client::new()
        .get(format!(
            "{base}/api/v1/admin/programs/{id}/application-resources"
        ))
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}
fn approval(row: &Value, fingerprint: &Value) -> Value {
    json!({"decision":"verified","expected_revision":row["revision"],"program_fingerprint":fingerprint,
        "access":"login_required","same_program_confirmed":true,"current_intake_confirmed":true,"destination_confirmed":true,
        "note":"공식 공고의 2026년 모집 회차와 기관 로그인 복귀 주소를 대조한 테스트 근거입니다.","validity_hours":24})
}
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
async fn queue(base: &str, token: &str, id: Uuid, kind: &str, url: &str, label: &str) -> Value {
    let response=Client::new().post(format!("{base}/api/v1/admin/programs/{id}/application-resources")).bearer_auth(token)
        .json(&json!({"kind":kind,"url":url,"source_url":"https://official.example.org/notice?intake=2026","label":label})).send().await.unwrap();
    assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
    response.json().await.unwrap()
}

#[tokio::test]
async fn application_resources_require_admin_and_review_before_direct_handoff() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let base = start_test_server().await;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .unwrap();
    let id = fixture(&pool).await;
    let (admin, admin_token) = create_test_admin(&base).await;
    let (user, token, _) = create_test_user(&base).await;
    let client = Client::new();
    let endpoint = format!("{base}/api/v1/admin/programs/{id}/application-resources");
    assert_eq!(client.get(&endpoint).send().await.unwrap().status(), 401);
    assert_eq!(
        client
            .get(&endpoint)
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        client
            .post(&endpoint)
            .bearer_auth(&token)
            .json(&json!({}))
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    for url in [
        "https://www.uctf.or.kr/?lang=ko",
        "http://form.example.org/a",
        "https://127.0.0.1/form",
        "https://form.example.org/a?access_token=secret",
    ] {
        assert_eq!(client.post(&endpoint).bearer_auth(&admin_token).json(&json!({"kind":"application","url":url,"source_url":"https://official.example.org/notice","label":"신청하기"})).send().await.unwrap().status(),400);
    }
    let row = queue(
        &base,
        &admin_token,
        id,
        "application",
        "https://form.example.org/apply?intake=2026#step",
        "신청하기",
    )
    .await;
    assert_eq!(row["status"], "candidate");
    let repeated = queue(
        &base,
        &admin_token,
        id,
        "application",
        "https://form.example.org/apply?intake=2026#step",
        "신청하기",
    )
    .await;
    assert_eq!(row["id"], repeated["id"]);
    // Before review the button leads to the official notice, not the candidate.
    let official_kind =
        program(&base, id).await["application_policy"]["destination"]["kind"].clone();
    assert_eq!(official_kind, "unverified_page");
    let listing = list(&base, &admin_token, id).await;
    // Security M1: a verified destination must sit on the program's official
    // registrable domain or an approved source domain; look-alike and IDN
    // hosts are refused even with every confirmation box ticked.
    for foreign in [
        "https://bokjiro-go-kr.evil-example.net/apply",
        "https://xn--80ak6aa92e.example.org/apply",
    ] {
        let candidate = queue(&base, &admin_token, id, "application", foreign, "신청하기").await;
        let response = client
            .put(format!(
                "{base}/api/v1/admin/application-resources/{}",
                candidate["id"].as_str().unwrap()
            ))
            .bearer_auth(&admin_token)
            .json(&approval(&candidate, &listing["program_fingerprint"]))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400, "{foreign}");
    }
    let review_url = format!(
        "{base}/api/v1/admin/application-resources/{}",
        row["id"].as_str().unwrap()
    );
    let mut input = approval(&row, &listing["program_fingerprint"]);
    assert_eq!(
        client
            .put(&review_url)
            .bearer_auth(&token)
            .json(&input)
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    input["current_intake_confirmed"] = json!(false);
    assert_eq!(
        client
            .put(&review_url)
            .bearer_auth(&admin_token)
            .json(&input)
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    input["current_intake_confirmed"] = json!(true);
    input["expected_revision"] = json!(999);
    assert_eq!(
        client
            .put(&review_url)
            .bearer_auth(&admin_token)
            .json(&input)
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    input["expected_revision"] = row["revision"].clone();
    assert_eq!(
        client
            .put(&review_url)
            .bearer_auth(&admin_token)
            .json(&input)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let detail = program(&base, id).await;
    assert_eq!(
        detail["application_policy"]["destination"]["kind"],
        "login_required"
    );
    assert_eq!(
        detail["application_policy"]["destination"]["url"],
        row["url"]
    );
    assert_eq!(detail["application_policy"]["can_submit"], false);
    assert_eq!(detail["application_policy"]["can_autofill"], false);
    assert!(detail["application_policy"]["resources"]["application"]
        .get("reviewed_by")
        .is_none());
    let prep_url = format!("{base}/api/v1/programs/{id}/preparation");
    let plan: Value = client
        .get(&prep_url)
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let handoff: Value = client
        .post(format!("{base}/api/v1/programs/{id}/handoff"))
        .bearer_auth(&token)
        .json(&json!({"revision":plan["revision"],"channel":"visit"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(handoff["snapshot"]["destination"]["kind"], official_kind);
    // An unchanged technical check must not erase user progress, and neither
    // does the worker's first probe recording a page hash after approval.
    assert_eq!(client.put(&prep_url).bearer_auth(&token).json(&json!({"revision":plan["revision"],"completed_tasks":["eligibility"],"selected_channel":"visit"})).send().await.unwrap().status(),200);
    sqlx::query("UPDATE program_application_resources SET verified_at=now(),valid_until=now()+interval '2 days',checked_at=now(),content_hash='first-probe-page-hash',source_hash='first-probe-notice-hash',check_status='ok' WHERE program_id=$1 AND status='verified'").bind(id).execute(&pool).await.unwrap();
    let renewed: Value = client
        .get(&prep_url)
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(renewed["revision"], plan["revision"]);
    assert_eq!(renewed["progress_reset"], false);
    // Ingestion rewriting display text is not a new round (narrow fingerprint).
    sqlx::query(
        "UPDATE programs SET title='2026년 공개 양식 검증 fixture (문구 수정)' WHERE id=$1",
    )
    .bind(id)
    .execute(&pool)
    .await
    .unwrap();
    assert!(!program(&base, id).await["application_policy"]["resources"]["application"].is_null());
    sqlx::query("UPDATE programs SET application_end_at=now()+interval '400 days' WHERE id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    assert!(program(&base, id).await["application_policy"]["resources"]["application"].is_null());
    let changed: Value = client
        .get(&prep_url)
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(changed["progress_reset"], true);
    // An approved source domain also binds a destination outside the
    // official site (e.g. a regional application portal).
    let approved = format!("apply-{}.example.net", Uuid::new_v4().simple());
    let domain_id: Uuid = sqlx::query_scalar("INSERT INTO benefit_source_domains(organization_name,domain,source_role,crawl_allowed,verification_status,verification_reference,
          compliance_status,approval_basis,approval_reference,terms_checked_at,terms_review_outcome,robots_status,robots_checked_at,robots_content_hash,
          allowed_path_prefixes,robots_reviewed_path_prefixes,crawler_contact_url,compliance_reviewed_by,compliance_reviewed_at,approval_expires_at,collection_mode,min_request_interval_ms)
          VALUES('resource binding fixture',$1,'brand_official',true,'verified','fixture','approved','written_permission','fixture',now(),'explicit_permission','allowed',now(),repeat('a',32),
          ARRAY['/'],ARRAY['/'],'https://mazimi.example/bot','fixture',now(),now()+interval '30 days','direct_public',10000) RETURNING id")
        .bind(&approved).fetch_one(&pool).await.unwrap();
    sqlx::query("UPDATE programs SET application_end_at=NULL WHERE id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let fp = list(&base, &admin_token, id).await["program_fingerprint"].clone();
    let portal = queue(
        &base,
        &admin_token,
        id,
        "application",
        &format!("https://{approved}/apply/2026"),
        "신청하기",
    )
    .await;
    let response = client
        .put(format!(
            "{base}/api/v1/admin/application-resources/{}",
            portal["id"].as_str().unwrap()
        ))
        .bearer_auth(&admin_token)
        .json(&approval(&portal, &fp))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
    sqlx::query("DELETE FROM benefit_source_domains WHERE id=$1")
        .bind(domain_id)
        .execute(&pool)
        .await
        .unwrap();
    let reviews: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM application_resource_reviews WHERE reviewer_id=$1",
    )
    .bind(admin)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(reviews, 2);
    sqlx::query("DELETE FROM users WHERE id=ANY($1)")
        .bind(vec![admin, user])
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM programs WHERE id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
}

/// Code review M4 / security M1: a shared-hosting site is not an institution's
/// domain, and an admin-written official_url cannot vouch for itself.
#[tokio::test]
async fn domain_binding_excludes_shared_hosting_and_admin_set_official_urls() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let base = start_test_server().await;
    let client = Client::new();
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .unwrap();
    let (admin, token) = create_test_admin(&base).await;
    let approve = |id: Uuid, url: &'static str| {
        let (base, token, client) = (base.clone(), token.clone(), client.clone());
        async move {
            let row = queue(&base, &token, id, "application", url, "신청하기").await;
            let fp = list(&base, &token, id).await["program_fingerprint"].clone();
            client
                .put(format!(
                    "{base}/api/v1/admin/application-resources/{}",
                    row["id"].as_str().unwrap()
                ))
                .bearer_auth(&token)
                .json(&approval(&row, &fp))
                .send()
                .await
                .unwrap()
                .status()
        }
    };
    // Ingested notice on a blog platform: the platform is not the agency.
    let blog = Uuid::new_v4();
    sqlx::query("INSERT INTO programs(id,program_type,source_type,title,program_status,is_active,official_url) VALUES($1,'policy','manual','2026년 블로그 공지 fixture','active',true,'https://blog.naver.com/busan_youth/223000')")
        .bind(blog).execute(&pool).await.unwrap();
    assert_eq!(
        approve(blog, "https://form.naver.com/response/abc").await,
        400
    );
    assert_eq!(approve(blog, "https://blog.naver.com/someone/1").await, 400);
    // A tenant subdomain binds only its own exact host.
    let pages = Uuid::new_v4();
    sqlx::query("INSERT INTO programs(id,program_type,source_type,title,program_status,is_active,official_url) VALUES($1,'policy','manual','2026년 페이지 공지 fixture','active',true,'https://busan-youth.github.io/notice/2026')")
        .bind(pages).execute(&pool).await.unwrap();
    assert_eq!(
        approve(pages, "https://attacker.github.io/apply").await,
        400
    );
    assert_eq!(
        approve(pages, "https://busan-youth.github.io/apply").await,
        200
    );
    // An admin-written official_url does not bind, even on its own domain.
    let edited = fixture(&pool).await;
    let response = client
        .put(format!("{base}/api/v1/admin/programs/{edited}"))
        .bearer_auth(&token)
        .json(&json!({"official_url":"https://www.agency-moved.example.net/notice/7"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
    assert_eq!(
        approve(edited, "https://apply.agency-moved.example.net/form").await,
        400
    );
    // ...until that host is an approved source domain.
    let domain_id: Uuid = sqlx::query_scalar("INSERT INTO benefit_source_domains(organization_name,domain,source_role,crawl_allowed,verification_status,verification_reference,
          compliance_status,approval_basis,approval_reference,terms_checked_at,terms_review_outcome,robots_status,robots_checked_at,robots_content_hash,
          allowed_path_prefixes,robots_reviewed_path_prefixes,crawler_contact_url,compliance_reviewed_by,compliance_reviewed_at,approval_expires_at,collection_mode,min_request_interval_ms)
          VALUES('admin official fixture','apply.agency-moved.example.net','brand_official',true,'verified','fixture','approved','written_permission','fixture',now(),'explicit_permission','allowed',now(),repeat('a',32),
          ARRAY['/'],ARRAY['/'],'https://mazimi.example/bot','fixture',now(),now()+interval '30 days','direct_public',10000) RETURNING id")
        .fetch_one(&pool).await.unwrap();
    let row = list(&base, &token, edited).await["items"][0].clone();
    let fp = list(&base, &token, edited).await["program_fingerprint"].clone();
    let status = client
        .put(format!(
            "{base}/api/v1/admin/application-resources/{}",
            row["id"].as_str().unwrap()
        ))
        .bearer_auth(&token)
        .json(&approval(&row, &fp))
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(status, 200);
    // Ingestion replacing the URL restores the source binding.
    sqlx::query("UPDATE programs SET official_url='https://www.agency-moved.example.net/notice/8' WHERE id=$1")
        .bind(edited)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        approve(edited, "https://www2.agency-moved.example.net/form").await,
        200
    );
    sqlx::query("DELETE FROM benefit_source_domains WHERE id=$1")
        .bind(domain_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM programs WHERE id=ANY($1)")
        .bind(vec![blog, pages, edited])
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM users WHERE id=$1")
        .bind(admin)
        .execute(&pool)
        .await
        .unwrap();
}

/// Code review H1: verification freshness is not identity. A destination that
/// expires and is re-approved at the same URL must not erase the checklist.
#[tokio::test]
async fn preparation_progress_survives_verification_expiry_and_reapproval() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let base = start_test_server().await;
    let client = Client::new();
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .unwrap();
    // No method text: the reviewed destination alone turns on the online
    // channel, so expiry also exercises the workflow input of the revision.
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO programs(id,program_type,source_type,title,program_status,is_active,official_url) VALUES($1,'policy','manual','2026년 만료 재승인 fixture','active',true,'https://official.example.org/notice?intake=2026')")
        .bind(id).execute(&pool).await.unwrap();
    let (admin, admin_token) = create_test_admin(&base).await;
    let (user, token, _) = create_test_user(&base).await;
    let row = queue(
        &base,
        &admin_token,
        id,
        "application",
        "https://form.example.org/apply?intake=2026",
        "신청하기",
    )
    .await;
    let review_url = format!(
        "{base}/api/v1/admin/application-resources/{}",
        row["id"].as_str().unwrap()
    );
    // Code review L6: an approval also clears the previous probe error.
    sqlx::query("UPDATE program_application_resources SET check_error='redirect_requires_review',last_failure_code='redirect_requires_review',check_status='blocked' WHERE id=$1")
        .bind(Uuid::parse_str(row["id"].as_str().unwrap()).unwrap())
        .execute(&pool)
        .await
        .unwrap();
    let approve = |listing: Value| {
        let current = listing["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["id"] == row["id"])
            .unwrap()
            .clone();
        approval(&current, &listing["program_fingerprint"])
    };
    let response = client
        .put(&review_url)
        .bearer_auth(&admin_token)
        .json(&approve(list(&base, &admin_token, id).await))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
    let approved = list(&base, &admin_token, id).await["items"][0].clone();
    assert!(approved["check_error"].is_null(), "{approved}");
    assert!(approved["last_failure_code"].is_null(), "{approved}");
    let prep_url = format!("{base}/api/v1/programs/{id}/preparation");
    let get = || async {
        client
            .get(&prep_url)
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap()
    };
    let plan = get().await;
    assert_eq!(plan["policy"]["resources"]["status"], "verified");
    assert_eq!(
        client
            .put(&prep_url)
            .bearer_auth(&token)
            .json(&json!({"revision":plan["revision"],"completed_tasks":["eligibility","information"]}))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    // valid_until passes: the destination is hidden, the checklist stays.
    sqlx::query("UPDATE program_application_resources SET verified_at=now()-interval '2 days',valid_until=now()-interval '1 second' WHERE id=$1")
        .bind(Uuid::parse_str(row["id"].as_str().unwrap()).unwrap())
        .execute(&pool)
        .await
        .unwrap();
    let expired = get().await;
    assert_eq!(expired["policy"]["resources"]["status"], "recheck_required");
    assert_eq!(expired["progress_reset"], false);
    assert_eq!(expired["revision"], plan["revision"]);
    assert_eq!(
        expired["completed_tasks"],
        json!(["eligibility", "information"])
    );
    // The same URL re-approved later is the same destination.
    let response = client
        .put(&review_url)
        .bearer_auth(&admin_token)
        .json(&approve(list(&base, &admin_token, id).await))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
    let reapproved = get().await;
    assert_eq!(reapproved["policy"]["resources"]["status"], "verified");
    assert_eq!(reapproved["progress_reset"], false);
    assert_eq!(reapproved["revision"], plan["revision"]);
    assert_eq!(
        reapproved["completed_tasks"],
        json!(["eligibility", "information"])
    );
    // A different destination is a real change and still resets once.
    let other = queue(
        &base,
        &admin_token,
        id,
        "application",
        "https://form.example.org/apply?intake=2026&step=2",
        "신청하기",
    )
    .await;
    let listing = list(&base, &admin_token, id).await;
    let response = client
        .put(format!(
            "{base}/api/v1/admin/application-resources/{}",
            other["id"].as_str().unwrap()
        ))
        .bearer_auth(&admin_token)
        .json(&approval(&other, &listing["program_fingerprint"]))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
    let moved = get().await;
    assert_eq!(moved["progress_reset"], true);
    assert_ne!(moved["revision"], plan["revision"]);
    sqlx::query("DELETE FROM users WHERE id=ANY($1)")
        .bind(vec![admin, user])
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
async fn application_templates_require_probe_and_expire_without_leaking_review_records() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let base = start_test_server().await;
    let client = Client::new();
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .unwrap();
    let id = fixture(&pool).await;
    let (admin, token) = create_test_admin(&base).await;
    let row = queue(
        &base,
        &token,
        id,
        "template",
        "https://official.example.org/download?file=3",
        "신청서.hwp",
    )
    .await;
    let rid = Uuid::parse_str(row["id"].as_str().unwrap()).unwrap();
    let listing = list(&base, &token, id).await;
    let mut input = approval(&row, &listing["program_fingerprint"]);
    input["access"] = json!("direct");
    let url = format!("{base}/api/v1/admin/application-resources/{rid}");
    let unprobed = client
        .put(&url)
        .bearer_auth(&token)
        .json(&input)
        .send()
        .await
        .unwrap();
    assert_eq!(unprobed.status(), 412);
    let body: Value = unprobed.json().await.unwrap();
    assert_eq!(body["code"], "template_probe_required");
    // Simulated bounded worker observation; no external site called by tests.
    sqlx::query("UPDATE program_application_resources SET content_hash=$2,source_hash=$3,file_format='hwp',byte_size=41472,checked_at=now() WHERE id=$1")
        .bind(rid).bind("a".repeat(64)).bind("b".repeat(64)).execute(&pool).await.unwrap();
    assert_eq!(
        client
            .put(&url)
            .bearer_auth(&token)
            .json(&input)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let detail = program(&base, id).await;
    let template = &detail["application_policy"]["resources"]["templates"][0];
    assert_eq!(template["file_format"], "hwp");
    assert_eq!(template["byte_size"], 41472);
    assert!(template.get("review_note").is_none());
    assert_eq!(
        client
            .put(&url)
            .bearer_auth(&token)
            .json(&input)
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    sqlx::query("UPDATE program_application_resources SET verified_at=now()-interval '2 days',valid_until=now()-interval '1 day' WHERE id=$1").bind(rid).execute(&pool).await.unwrap();
    assert_eq!(
        program(&base, id).await["application_policy"]["resources"]["templates"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    let rls:(bool,bool)=sqlx::query_as("SELECT relrowsecurity,relforcerowsecurity FROM pg_class WHERE oid='program_application_resources'::regclass").fetch_one(&pool).await.unwrap();
    assert_eq!(rls, (true, true));
    sqlx::query("DELETE FROM programs WHERE id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM users WHERE id=$1")
        .bind(admin)
        .execute(&pool)
        .await
        .unwrap();
}

/// Production stamps `verified_at` with the database clock and the API
/// compares it with its own: a database clock 1 second ahead must not hide a
/// template the reviewer just approved (10-19 full-run flake), while a stamp
/// far in the future still never counts.
#[tokio::test]
async fn a_database_clock_ahead_of_the_api_host_does_not_hide_a_fresh_approval() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let base = start_test_server().await;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .unwrap();
    let (_admin, token) = create_test_admin(&base).await;
    let hash = "e".repeat(64);
    let (id, rid) =
        super::form_template_cases::verified_template(&pool, &base, &token, "pdf", &hash).await;
    let shown = |base: String| async move {
        program(&base, id).await["application_policy"]["resources"]["templates"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["id"] == json!(rid))
    };
    // As if the database clock ran 1 second ahead when it stamped the approval.
    sqlx::query("UPDATE program_application_resources SET verified_at=now()+interval '1 second' WHERE id=$1")
        .bind(rid)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        shown(base.clone()).await,
        "a 1 s lead must not hide the approval"
    );
    // Far beyond the tolerance: not a verification.
    sqlx::query(
        "UPDATE program_application_resources SET verified_at=now()+interval '1 hour' WHERE id=$1",
    )
    .bind(rid)
    .execute(&pool)
    .await
    .unwrap();
    assert!(
        !shown(base.clone()).await,
        "a far-future stamp never counts"
    );
    sqlx::query("DELETE FROM programs WHERE id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
}

/// A link the worker verified by itself (`apply_entry`) is the programme's
/// destination when its host is the official page's institution, a
/// government domain or a listed institution; any other host stays hidden
/// even though the row says verified.
#[tokio::test]
async fn automatically_verified_links_are_preferred_on_vouched_hosts_only() {
    use mazimi_core::application_resources::{fingerprint, AUTOMATED_REVIEWER};
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let base = start_test_server().await;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .unwrap();
    let official =
        "https://plus.gov.kr/portal/benefitV2/benefitTotalSrvcList/benefitSrvcDtl?svcSeq=1&svcId=1";
    let mut shown = vec![];
    for url in [
        "https://www.work24.go.kr/cm/apply.do?x=1",
        "https://www.unvetted-example.or.kr/apply",
    ] {
        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO programs(id,program_type,source_type,title,program_status,is_active,official_url,application_method) VALUES($1,'policy','gov_benefits','자동 신청 링크 fixture','active',true,$2,'정부24온라인신청||방문신청')")
            .bind(id).bind(official).execute(&pool).await.unwrap();
        let p: mazimi_core::models::Program = sqlx::query_as("SELECT * FROM programs WHERE id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO program_application_resources(program_id,kind,url,source_url,label,status,access,program_fingerprint,
               verified_at,valid_until,reviewed_by,review_note,reader,check_status)
             VALUES($1,'application',$2,$3,'온라인 신청','verified','direct',$4,now(),now()+interval '7 days',$5,
               '자동 확인(gov24): 공식 페이지의 신청 버튼이 가리키는 곳을 두 번 열어 확인했어요.','gov24','ok')",
        )
        .bind(id).bind(url).bind(official).bind(fingerprint(&p)).bind(AUTOMATED_REVIEWER)
        .execute(&pool).await.unwrap();
        let detail = program(&base, id).await;
        shown.push(detail["application_policy"]["destination"].clone());
        sqlx::query("DELETE FROM programs WHERE id=$1")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
    }
    assert_eq!(shown[0]["kind"], "application", "{}", shown[0]);
    assert_eq!(shown[0]["url"], "https://www.work24.go.kr/cm/apply.do?x=1");
    assert_eq!(shown[0]["evidence"], "reviewed_application");
    // The unvouched host is not offered; the official page is.
    assert_ne!(
        shown[1]["url"], "https://www.unvetted-example.or.kr/apply",
        "{}",
        shown[1]
    );
    assert_ne!(shown[1]["kind"], "application", "{}", shown[1]);
}
