//! 신청 진행 단계 (`GET /api/v1/programs/{id}/progress`) and 신청 방법
//! (`application_procedure` in the public detail).
use super::*;

async fn stages(client: &Client, base: &str, id: Uuid, token: &str) -> Vec<String> {
    let body: Value = client
        .get(format!("{base}/api/v1/programs/{id}/progress"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    body["stages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["state"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn progress_is_private_per_user_and_follows_checklist_report_and_connector() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let base = start_test_server().await;
    let client = Client::new();
    let pool = PgPoolOptions::new()
        .max_connections(3)
        .connect(&DATABASE_URL)
        .await
        .unwrap();
    let (owner, token, _) = create_test_user(&base).await;
    let (_, other_token, _) = create_test_user(&base).await;
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO programs(id,program_type,source_type,title,program_status,is_active,official_url,application_method,screening_method,submission_documents) VALUES($1,'policy','youth_center','Progress fixture','active',true,'https://provider.example.org/notice/7',$2,$3,$4)")
        .bind(id)
        .bind("① 신청대상자 안내 문자\n② 주거안정지원금 신청\n③ 적합여부 및 서류 검토")
        .bind("- 지원금 지급대상자로 결정 시, 결정 통보 및 지급 안내 문자 발송")
        .bind("1. 신청서\n2. 통장 사본")
        .execute(&pool)
        .await
        .unwrap();

    // Public detail: the guest stepper and the notice's own procedure.
    let detail: Value = client
        .get(format!("{base}/api/v1/programs/{id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let guest: Vec<_> = detail["application_progress"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| (s["label"].as_str().unwrap(), s["state"].as_str().unwrap()))
        .collect();
    assert_eq!(
        guest,
        [
            ("발견", "current"),
            ("자격 확인", "upcoming"),
            ("서류 준비", "upcoming"),
            ("신청", "upcoming")
        ]
    );
    let steps: Vec<_> = detail["application_procedure"]["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["title"].as_str().unwrap())
        .collect();
    assert_eq!(
        steps,
        [
            "신청대상자 안내 문자",
            "주거안정지원금 신청",
            "적합여부 및 서류 검토",
            "지원금 지급대상자로 결정 시, 결정 통보 및 지급 안내 문자 발송"
        ]
    );
    assert_eq!(detail["application_procedure"]["ordered"], true);

    // Private: a token is required and the answer is never shared-cached.
    let anonymous = client
        .get(format!("{base}/api/v1/programs/{id}/progress"))
        .send()
        .await
        .unwrap();
    assert_eq!(anonymous.status(), 401);
    let response = client
        .get(format!("{base}/api/v1/programs/{id}/progress"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.headers()["cache-control"], "private, no-store");
    assert_eq!(
        stages(&client, &base, id, &token).await,
        ["done", "current", "upcoming", "upcoming"]
    );
    let missing = client
        .get(format!(
            "{base}/api/v1/programs/{}/progress",
            Uuid::new_v4()
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), 404, "no programme, no stages");

    // The existing checklist write: eligibility and one of two documents.
    let prep = format!("{base}/api/v1/programs/{id}/preparation");
    let plan: Value = client
        .get(&prep)
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let saved = client.put(&prep).bearer_auth(&token)
        .json(&json!({"revision": plan["revision"], "completed_tasks": ["eligibility", "document:신청서"]}))
        .send().await.unwrap();
    assert!(
        saved.status().is_success(),
        "{}",
        saved.text().await.unwrap()
    );
    assert_eq!(
        stages(&client, &base, id, &token).await,
        ["done", "done", "current", "upcoming"]
    );
    // Another user's progress is their own.
    assert_eq!(
        stages(&client, &base, id, &other_token).await,
        ["done", "current", "upcoming", "upcoming"]
    );

    // A future connector submission (fixture rows) drives 신청 without a user report.
    let connector: Uuid = sqlx::query_scalar("INSERT INTO application_connectors(program_id,provider_name,authority_reference,authorized_until,privacy_notice,schema,schema_hash,token_hash) VALUES($1,'fixture institution','fixture-authority',now()+interval '1 day','fixture notice','{}'::jsonb,'s','t') RETURNING id")
        .bind(id).fetch_one(&pool).await.unwrap();
    let submission = Uuid::new_v4();
    sqlx::query("INSERT INTO application_submissions(id,user_id,program_id,connector_id,request_hash,schema_hash,plan_revision,state,consent_snapshot,program_snapshot) VALUES($1,$2,$3,$4,'r','s','p','queued','{}'::jsonb,'{}'::jsonb)")
        .bind(submission).bind(owner).bind(id).bind(connector).execute(&pool).await.unwrap();
    assert_eq!(
        stages(&client, &base, id, &token).await,
        ["done", "done", "done", "current"]
    );
    sqlx::query("UPDATE application_submissions SET state='accepted',receipt_reference='FIX-1',received_at=now(),updated_at=now() WHERE id=$1")
        .bind(submission).execute(&pool).await.unwrap();
    assert_eq!(
        stages(&client, &base, id, &token).await,
        ["done", "done", "done", "done"]
    );
    assert_eq!(
        stages(&client, &base, id, &other_token).await,
        ["done", "current", "upcoming", "upcoming"]
    );

    sqlx::query("DELETE FROM application_submissions WHERE program_id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM application_connectors WHERE program_id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM application_preparations WHERE program_id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM programs WHERE id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
}

/// A user's own report ("신청했어요") completes 신청, through the existing journey routes.
#[tokio::test]
async fn reported_application_completes_the_apply_stage() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let base = start_test_server().await;
    let client = Client::new();
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .unwrap();
    let (_, token, _) = create_test_user(&base).await;
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO programs(id,program_type,source_type,title,program_status,is_active,official_url,application_method) VALUES($1,'policy','manual','Report fixture','active',true,'https://provider.example.org/notice/8','방문신청')")
        .bind(id).execute(&pool).await.unwrap();
    let plan: Value = client
        .get(format!("{base}/api/v1/programs/{id}/preparation"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let journey: Value = client
        .post(format!("{base}/api/v1/programs/{id}/handoff"))
        .bearer_auth(&token)
        .json(&json!({"revision": plan["revision"], "channel": "visit"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        stages(&client, &base, id, &token).await,
        ["done", "done", "done", "current"]
    );
    let journey_id = journey["journey"]["id"]
        .as_str()
        .or(journey["id"].as_str())
        .unwrap()
        .to_owned();
    let reported = client
        .put(format!("{base}/api/v1/my/application-journeys/{id}"))
        .bearer_auth(&token)
        .json(&json!({"journey_id": journey_id, "outcome": "reported_applied", "confirmed": true}))
        .send()
        .await
        .unwrap();
    assert!(
        reported.status().is_success(),
        "{}",
        reported.text().await.unwrap()
    );
    assert_eq!(
        stages(&client, &base, id, &token).await,
        ["done", "done", "done", "done"]
    );
}
