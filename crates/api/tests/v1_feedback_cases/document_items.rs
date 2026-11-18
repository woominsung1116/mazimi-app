//! F6.2b: the user's own document checklist entries.
use super::*;

#[tokio::test]
async fn document_items_are_scoped_to_their_owner() {
    let base = start_test_server().await;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .unwrap();
    let client = Client::new();
    let program = Uuid::new_v4();
    sqlx::query("INSERT INTO programs(id,program_type,source_type,source_id,title,program_status,regions,is_active) VALUES($1,'policy','manual',$2,'직접 추가 테스트','active','{}',true)")
        .bind(program).bind(program.to_string()).execute(&pool).await.unwrap();
    let (_, token, _) = create_test_user(&base).await;
    let (_, other, _) = create_test_user(&base).await;
    let url = format!("{base}/api/v1/my/document-items");

    assert_eq!(client.get(&url).send().await.unwrap().status(), 401);
    assert_eq!(
        client
            .post(&url)
            .json(&json!({"name": "x"}))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );

    let created = client
        .post(&url)
        .bearer_auth(&token)
        .json(&json!({"name": "  가족관계증명서  ", "program_id": program}))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 201);
    let item: Value = created.json().await.unwrap();
    assert_eq!(item["name"], "가족관계증명서");
    assert_eq!(item["status"], "todo");
    assert_eq!(item["program_id"], program.to_string());
    let id = item["id"].as_str().unwrap().to_owned();
    client
        .post(&url)
        .bearer_auth(&token)
        .json(&json!({"name": "여권 사본", "status": "done"}))
        .send()
        .await
        .unwrap();

    let mine: Value = client
        .get(format!("{url}?program_id={program}"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(mine["items"].as_array().unwrap().len(), 1);
    let all: Value = client
        .get(&url)
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(all["items"].as_array().unwrap().len(), 2);
    let theirs: Value = client
        .get(&url)
        .bearer_auth(&other)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(theirs["items"], json!([]));

    // Another user can neither change nor delete the item.
    let item_url = format!("{url}/{id}");
    assert_eq!(
        client
            .put(&item_url)
            .bearer_auth(&other)
            .json(&json!({"status": "done"}))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    assert_eq!(
        client
            .delete(&item_url)
            .bearer_auth(&other)
            .send()
            .await
            .unwrap()
            .status(),
        404
    );

    let updated: Value = client
        .put(&item_url)
        .bearer_auth(&token)
        .json(&json!({"status": "done"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(updated["status"], "done");
    assert_eq!(updated["name"], "가족관계증명서");

    for (body, code) in [
        (json!({"name": " "}), "invalid_name"),
        (json!({"name": "가".repeat(61)}), "invalid_name"),
        (json!({"name": "a\u{0007}b"}), "invalid_name"),
        (json!({"name": "등본", "status": "lost"}), "invalid_status"),
    ] {
        let response = client
            .post(&url)
            .bearer_auth(&token)
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400, "{body}");
        let error: Value = response.json().await.unwrap();
        assert_eq!(error["code"], code);
    }
    assert_eq!(
        client
            .post(&url)
            .bearer_auth(&token)
            .json(&json!({"name": "등본", "program_id": Uuid::new_v4()}))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    // Files are not accepted: unknown keys are rejected.
    assert_eq!(
        client
            .post(&url)
            .bearer_auth(&token)
            .json(&json!({"name": "등본", "file": "base64"}))
            .send()
            .await
            .unwrap()
            .status(),
        422
    );

    // Per-programme limit.
    for n in 1..30 {
        let status = client
            .post(&url)
            .bearer_auth(&token)
            .json(&json!({"name": format!("서류 {n}"), "program_id": program}))
            .send()
            .await
            .unwrap()
            .status();
        assert_eq!(status, 201, "{n}");
    }
    let response = client
        .post(&url)
        .bearer_auth(&token)
        .json(&json!({"name": "초과", "program_id": program}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 409);

    assert_eq!(
        client
            .delete(&item_url)
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .status(),
        204
    );
    assert_eq!(
        client
            .delete(&item_url)
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .status(),
        404
    );

    // RLS is on and forced.
    let (rls, forced): (bool, bool) = sqlx::query_as(
        "SELECT relrowsecurity, relforcerowsecurity FROM pg_class WHERE relname = 'user_document_items'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(rls && forced);
    // Leave no active fixture behind: other tests rank the shared catalogue.
    sqlx::query("UPDATE programs SET is_active = false WHERE id = ANY($1)")
        .bind(vec![program])
        .execute(&pool)
        .await
        .unwrap();
}
