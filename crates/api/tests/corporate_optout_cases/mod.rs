use super::*;

/// The public opt-out form records a request and returns its proof token,
/// and changes nothing else: the offer stays shown, collection continues and
/// stored evidence is kept until the requester proves control of the domain
/// (worker proof pass, `corporate_auto::optout`). Review M22: an
/// unauthenticated request must not be able to hide a source.
#[tokio::test]
async fn publisher_optout_request_changes_nothing_until_domain_control_is_proven() {
    std::env::set_var("PUBLISHER_OPTOUT_ENABLED", "true");
    let base = start_test_server().await;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .unwrap();
    let client = Client::new();
    let tag = Uuid::new_v4().simple().to_string();
    let host = format!("optout-{tag}.com");
    let url = format!("https://www.{host}/student");
    let domain_id: Uuid = sqlx::query_scalar(
        "INSERT INTO benefit_source_domains(organization_name,domain,source_role,verification_status,verification_reference,verified_at,compliance_status,collection_mode)
         VALUES('opt-out fixture',$1,'brand_official','verified','fixture',now(),'pending','manual_review') RETURNING id",
    )
    .bind(format!("www.{host}"))
    .fetch_one(&pool)
    .await
    .unwrap();
    let candidate_id: Uuid = sqlx::query_scalar(
        "INSERT INTO benefit_source_candidates(domain_id,url,canonical_url,discovered_via,status,candidate_kind,raw_text)
         VALUES($1,$2,$2,'manual','approved','detail','page excerpt') RETURNING id",
    )
    .bind(domain_id)
    .bind(&url)
    .fetch_one(&pool)
    .await
    .unwrap();
    let program_id: Uuid = sqlx::query_scalar(
        "INSERT INTO programs(program_type,source_type,source_id,title,summary,provider_name,official_url,program_status,provider_kind,benefit_category,is_active,source_health,regions)
         VALUES('corporate_benefit','corporate_official',$1,'옵트아웃 픽스처','학생 10% 할인','픽스처',$2,'always_open','corporate','discount',true,'current',ARRAY[]::text[]) RETURNING id",
    )
    .bind(format!("optout:{tag}"))
    .bind(&url)
    .fetch_one(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO program_source_monitors(program_id,candidate_id,verified_at,review_due_at) VALUES($1,$2,now(),now()+interval '30 days')")
        .bind(program_id).bind(candidate_id).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO corporate_offer_facts(program_id,fact_type,quote,quote_key,evidence_source) VALUES($1,'presence','학생 10% 할인 혜택',repeat('a',64),'fixture')")
        .bind(program_id).execute(&pool).await.unwrap();

    let bad = client
        .post(format!("{base}/api/v1/publisher/opt-out"))
        .json(&json!({"scope": "everything", "domain": host}))
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), 400);
    let suffix = client
        .post(format!("{base}/api/v1/publisher/opt-out"))
        .json(&json!({"scope": "both", "domain": "co.kr"}))
        .send()
        .await
        .unwrap();
    assert_eq!(suffix.status(), 400, "a public suffix is refused");
    let unknown = client
        .post(format!("{base}/api/v1/publisher/opt-out"))
        .json(&json!({"scope": "both", "domain": format!("not-ours-{host}")}))
        .send()
        .await
        .unwrap();
    assert_eq!(unknown.status(), 422, "only our own source domains");
    let response = client.post(format!("{base}/api/v1/publisher/opt-out"))
        .json(&json!({"scope": "both", "domain": format!("WWW.{host}"), "contact": "partner@example.com"}))
        .send().await.unwrap();
    assert_eq!(response.status(), 202);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["status"], "pending_proof");
    assert_eq!(body["effective"], false);
    assert_eq!(body["verification"]["token"].as_str().unwrap().len(), 64);
    assert_eq!(
        body["verification"]["confirm_url"],
        format!("https://{host}/.well-known/mazimi-optout.txt")
    );

    let (active, reason, hold): (bool, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT is_active, retirement_reason,
                (SELECT hold_reason FROM program_source_holds h WHERE h.program_id = p.id AND h.resolved_at IS NULL)
         FROM programs p WHERE id=$1",
    )
    .bind(program_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        (active, reason.as_deref(), hold.as_deref()),
        (true, None, None)
    );
    let (crawl, mode, compliance): (bool, String, String) = sqlx::query_as(
        "SELECT crawl_allowed, collection_mode, compliance_status FROM benefit_source_domains WHERE id=$1",
    )
    .bind(domain_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        (crawl, mode.as_str(), compliance.as_str()),
        (false, "manual_review", "pending")
    );
    let quotes: i64 =
        sqlx::query_scalar("SELECT count(*) FROM corporate_offer_facts WHERE program_id=$1")
            .bind(program_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(quotes, 1);
    let raw: Option<String> =
        sqlx::query_scalar("SELECT raw_text FROM benefit_source_candidates WHERE id=$1")
            .bind(candidate_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(raw.is_some());
    let (effective, withdrawn, contact): (bool, i32, Option<String>) = sqlx::query_as(
        "SELECT effective_at IS NOT NULL, programs_withdrawn, contact_hash
         FROM benefit_source_optouts WHERE domain=$1",
    )
    .bind(&host)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(!effective);
    assert_eq!(withdrawn, 0);
    assert!(
        contact.is_some_and(|c| c.len() == 64 && !c.contains('@')),
        "contact is stored only as a hash"
    );

    // Operators: non-admins are refused; an admin sees the pending opt-out
    // (no token or contact hash) and can lift it.
    let optout_id = body["id"].as_str().unwrap().to_string();
    let (_, user_token, _) = create_test_user(&base).await;
    let refused = client
        .get(format!("{base}/api/v1/admin/publisher-optouts"))
        .bearer_auth(&user_token)
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), 403);
    let (_, admin_token) = create_test_admin(&base).await;
    let listed: serde_json::Value = client
        .get(format!("{base}/api/v1/admin/publisher-optouts"))
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let row = listed["optouts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == optout_id)
        .expect("listed");
    assert!(row.get("contact_hash").is_none());
    let lifted = client
        .post(format!(
            "{base}/api/v1/admin/publisher-optouts/{optout_id}/lift"
        ))
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(lifted.status(), 200);
    let again = client
        .post(format!(
            "{base}/api/v1/admin/publisher-optouts/{optout_id}/lift"
        ))
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(again.status(), 404);
    let (active, hold): (bool, Option<String>) = sqlx::query_as(
        "SELECT is_active, (SELECT hold_reason FROM program_source_holds h WHERE h.program_id = p.id AND h.resolved_at IS NULL)
         FROM programs p WHERE id=$1",
    )
    .bind(program_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        (active, hold.as_deref()),
        (true, None),
        "lifting an unproven request leaves the offer as it was"
    );

    sqlx::query("DELETE FROM benefit_source_optouts WHERE domain=$1")
        .bind(&host)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM programs WHERE id=$1")
        .bind(program_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM benefit_source_candidates WHERE id=$1")
        .bind(candidate_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM benefit_source_domains WHERE id=$1")
        .bind(domain_id)
        .execute(&pool)
        .await
        .unwrap();
}
