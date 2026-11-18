//! F2.5 date of birth and F2.6 nickname on `POST /api/v1/profile`.
use super::*;

async fn pool() -> sqlx::PgPool {
    PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .unwrap()
}

async fn save(base: &str, token: &str, body: Value) -> (u16, Value) {
    let response = Client::new()
        .post(format!("{base}/api/v1/profile"))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    (status, response.json().await.unwrap_or(Value::Null))
}

#[tokio::test]
async fn birth_date_is_stored_with_its_year_and_old_clients_keep_working() {
    let base = start_test_server().await;
    let (_, token, _) = create_test_user(&base).await;

    let (status, body) = save(
        &base,
        &token,
        json!({"birth_date": "2001-03-15", "region_code": "busan"}),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["profile"]["birth_date"], "2001-03-15");
    assert_eq!(body["profile"]["birth_year"], 2001);

    // An old client sends only the same birth year: the date stays.
    let (status, body) = save(
        &base,
        &token,
        json!({"birth_year": 2001, "region_code": "daegu"}),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["profile"]["birth_date"], "2001-03-15");
    assert_eq!(body["profile"]["region_code"], "daegu");

    // An old client changes the year: the stale date is dropped.
    let (status, body) = save(
        &base,
        &token,
        json!({"birth_year": 2002, "region_code": "daegu"}),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["profile"]["birth_date"], Value::Null);
    assert_eq!(body["profile"]["birth_year"], 2002);

    // An explicit null clears a stored date.
    save(
        &base,
        &token,
        json!({"birth_date": "2002-07-01", "region_code": "daegu"}),
    )
    .await;
    let (status, body) = save(
        &base,
        &token,
        json!({"birth_date": null, "birth_year": 2002, "region_code": "daegu"}),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["profile"]["birth_date"], Value::Null);
}

#[tokio::test]
async fn birth_date_errors_are_field_errors() {
    let base = start_test_server().await;
    let (_, token, _) = create_test_user(&base).await;
    for (body, code) in [
        (
            json!({"birth_date": "2001-03-15", "birth_year": 2000, "region_code": "busan"}),
            "birth_date_mismatch",
        ),
        (
            json!({"birth_date": "2001/03/15", "region_code": "busan"}),
            "birth_date_invalid",
        ),
        (
            json!({"birth_date": "2001-02-30", "region_code": "busan"}),
            "birth_date_invalid",
        ),
        (
            json!({"birth_date": "2999-01-01", "region_code": "busan"}),
            "birth_date_invalid",
        ),
        (json!({"region_code": "busan"}), "birth_date_required"),
        (
            json!({"birth_date": null, "region_code": "busan"}),
            "birth_date_required",
        ),
    ] {
        let (status, response) = save(&base, &token, body.clone()).await;
        assert_eq!(status, 400, "{body} → {response}");
        assert_eq!(response["code"], code, "{body} → {response}");
        assert_eq!(response["field"], "birth_date", "{body}");
    }
    let year = mazimi_core::seoul_date(Utc::now()) - Duration::days(365 * 13);
    let (status, response) = save(
        &base,
        &token,
        json!({"birth_date": year.to_string(), "region_code": "busan"}),
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(response["code"], "under_minimum_age");
}

#[tokio::test]
async fn onboarding_completion_requires_a_nickname() {
    let base = start_test_server().await;
    let pool = pool().await;
    let (user, token, _) = create_test_user(&base).await;
    // Sign-in creates the row without a birth year (and here, no nickname).
    sqlx::query(
        "UPDATE user_profiles SET birth_year = NULL, region_code = NULL, nickname = NULL WHERE user_id = $1",
    )
    .bind(user)
    .execute(&pool)
    .await
    .unwrap();
    let conditions = json!({"birth_year": 2000, "region_code": "busan"});
    let asked = json!({"birth_year": 2000, "region_code": "busan", "nickname": null});

    let (status, body) = save(&base, &token, asked).await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["code"], "nickname_required");
    assert_eq!(body["field"], "nickname");
    assert_eq!(body["error"], "닉네임을 입력해 주세요.");

    let (status, body) = save(
        &base,
        &token,
        json!({"birth_year": 2000, "region_code": "busan", "nickname": "   "}),
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(body["code"], "nickname_invalid");

    // A client without a nickname input (web) sends no key: not blocked.
    let (web, web_token, _) = create_test_user(&base).await;
    sqlx::query("UPDATE user_profiles SET birth_year = NULL, nickname = NULL WHERE user_id = $1")
        .bind(web)
        .execute(&pool)
        .await
        .unwrap();
    let (status, body) = save(&base, &web_token, conditions.clone()).await;
    assert_eq!(status, 200, "{body}");

    let (status, body) = save(
        &base,
        &token,
        json!({"birth_year": 2000, "region_code": "busan", "nickname": " 마지미 "}),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["profile"]["nickname"], "마지미");

    // After onboarding a save may omit the nickname.
    let (status, body) = save(&base, &token, conditions.clone()).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["profile"]["nickname"], "마지미");

    // A nickname stored at sign-in (Kakao) satisfies onboarding.
    let (other, other_token, _) = create_test_user(&base).await;
    sqlx::query(
        "UPDATE user_profiles SET birth_year = NULL, nickname = '카카오' WHERE user_id = $1",
    )
    .bind(other)
    .execute(&pool)
    .await
    .unwrap();
    let (status, body) = save(
        &base,
        &other_token,
        json!({"birth_year": 2000, "region_code": "busan", "nickname": null}),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["profile"]["nickname"], "카카오");
}
