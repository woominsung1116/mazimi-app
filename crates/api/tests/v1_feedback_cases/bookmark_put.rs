//! H1 (review 7): PUT /programs/{id}/bookmark sets the bookmark to the
//! client's intent, so a retry after a lost response never flips it back.
use super::*;

async fn put(
    client: &Client,
    base: &str,
    token: &str,
    program: Uuid,
    bookmarked: bool,
) -> (u16, Value) {
    let response = client
        .put(format!("{base}/api/v1/programs/{program}/bookmark"))
        .bearer_auth(token)
        .json(&json!({ "bookmarked": bookmarked }))
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    (status, response.json().await.unwrap_or(Value::Null))
}

async fn rows(pool: &sqlx::PgPool, user: Uuid, program: Uuid) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM user_bookmarks WHERE user_id = $1 AND program_id = $2")
        .bind(user)
        .bind(program)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn put_bookmark_is_idempotent_owner_scoped_and_checks_the_programme() {
    let base = start_test_server().await;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .unwrap();
    let client = Client::new();
    let program = Uuid::new_v4();
    sqlx::query("INSERT INTO programs(id,program_type,source_type,source_id,title,program_status,regions,is_active) VALUES($1,'policy','manual',$2,'찜 PUT 테스트','active','{}',true)")
        .bind(program).bind(program.to_string()).execute(&pool).await.unwrap();
    let (user, token, _) = create_test_user(&base).await;
    let (other, other_token, _) = create_test_user(&base).await;

    // Unauthenticated: rejected.
    let anonymous = client
        .put(format!("{base}/api/v1/programs/{program}/bookmark"))
        .json(&json!({ "bookmarked": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(anonymous.status(), 401);

    // Save, then the same request again (a retry after a lost response): still saved, one row.
    assert_eq!(
        put(&client, &base, &token, program, true).await,
        (200, json!({ "bookmarked": true }))
    );
    assert_eq!(
        put(&client, &base, &token, program, true).await,
        (200, json!({ "bookmarked": true }))
    );
    assert_eq!(
        rows(&pool, user, program).await,
        1,
        "a repeated save never duplicates or removes"
    );

    // Unsave, twice: still unsaved.
    assert_eq!(
        put(&client, &base, &token, program, false).await,
        (200, json!({ "bookmarked": false }))
    );
    assert_eq!(
        put(&client, &base, &token, program, false).await,
        (200, json!({ "bookmarked": false }))
    );
    assert_eq!(
        rows(&pool, user, program).await,
        0,
        "a repeated unsave never adds one back"
    );

    // IDOR: the other user's requests touch only their own row, whatever the body.
    assert_eq!(
        put(&client, &base, &token, program, true).await.1,
        json!({ "bookmarked": true })
    );
    let body_with_user = client
        .put(format!("{base}/api/v1/programs/{program}/bookmark"))
        .bearer_auth(&other_token)
        .json(&json!({ "bookmarked": false, "user_id": user }))
        .send()
        .await
        .unwrap();
    assert_eq!(body_with_user.status(), 200);
    assert_eq!(
        rows(&pool, user, program).await,
        1,
        "another user cannot unsave mine"
    );
    assert_eq!(
        put(&client, &base, &other_token, program, true).await.1,
        json!({ "bookmarked": true })
    );
    assert_eq!(rows(&pool, other, program).await, 1);
    assert_eq!(
        put(&client, &base, &other_token, program, false).await.1,
        json!({ "bookmarked": false })
    );
    assert_eq!(
        rows(&pool, user, program).await,
        1,
        "their unsave leaves my bookmark"
    );
    assert_eq!(rows(&pool, other, program).await, 0);

    // Unknown programme: 404, nothing written.
    let unknown = Uuid::new_v4();
    let (status, body) = put(&client, &base, &token, unknown, true).await;
    assert_eq!(status, 404, "{body}");
    assert_eq!(rows(&pool, user, unknown).await, 0);

    // A malformed body is rejected before any write.
    let bad = client
        .put(format!("{base}/api/v1/programs/{program}/bookmark"))
        .bearer_auth(&token)
        .json(&json!({ "bookmarked": "yes" }))
        .send()
        .await
        .unwrap();
    assert!(bad.status().is_client_error(), "{}", bad.status());
    assert_eq!(rows(&pool, user, program).await, 1);

    // POST (the toggle) still works for older app builds.
    let toggled: Value = client
        .post(format!("{base}/api/v1/programs/{program}/bookmark"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(toggled, json!({ "bookmarked": false }));
    assert_eq!(rows(&pool, user, program).await, 0);
}
