//! F7.1–F7.3: personalised recommendations need a login and a saved
//! profile; saving and alerts need a login.
use super::*;

#[tokio::test]
async fn recommendations_read_the_saved_profile_of_the_signed_in_user() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let base = start_test_server().await;
    let client = Client::new();
    let response = client
        .get(format!("{base}/api/v1/recommendations"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);

    let (_, token, _) = create_test_user(&base).await;
    let response = client
        .get(format!("{base}/api/v1/recommendations?scope=all"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.headers()["cache-control"].to_str().unwrap(),
        "private, no-store"
    );
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["scope"], "all");
    assert!(body["items"].is_array());
    assert!(body["filter_summary"]["considered"].as_u64().is_some());

    // The same conditions through the anonymous preview give the same list.
    let preview: Value = client
        .post(format!("{base}/api/v1/recommend/preview?scope=all"))
        .json(&json!({"birth_year": 2000, "region_code": "busan"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let ids = |v: &Value| -> Vec<String> {
        v["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["program_id"].as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(ids(&body), ids(&preview));
}

#[tokio::test]
async fn recommendations_without_saved_conditions_ask_for_them() {
    let base = start_test_server().await;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .unwrap();
    let (user, token, _) = create_test_user(&base).await;
    sqlx::query("UPDATE user_profiles SET birth_year = NULL WHERE user_id = $1")
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
    let response = Client::new()
        .get(format!("{base}/api/v1/recommendations"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 409);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["code"], "profile_required");
}

#[tokio::test]
async fn anonymous_preview_can_be_switched_off() {
    // A server built with the switch off; no process-wide env change.
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .unwrap();
    let app = api::build_app_with_preview(pool, JWT_SECRET.to_string(), "local".into(), false);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    let off = format!("http://{addr}");
    let on = start_test_server().await;
    let request = |base: String| {
        Client::new()
            .post(format!("{base}/api/v1/recommend/preview"))
            .json(&json!({"birth_year": 2000, "region_code": "busan"}))
            .send()
    };
    let response = request(off).await.unwrap();
    assert_eq!(response.status(), 401);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["code"], "login_required");
    // Default: on (the web /preview page still uses it).
    assert_eq!(request(on).await.unwrap().status(), 200);
}

#[tokio::test]
async fn guests_cannot_save_or_set_alerts() {
    let base = start_test_server().await;
    let client = Client::new();
    let program = Uuid::new_v4();
    for (method, path, body) in [
        ("POST", format!("/api/v1/programs/{program}/bookmark"), None),
        (
            "POST",
            format!("/api/v1/programs/{program}/state"),
            Some(json!({"state": "interested"})),
        ),
        (
            "POST",
            "/api/v1/alerts/preferences".to_owned(),
            Some(json!({"program_id": program, "enabled": true})),
        ),
        ("GET", format!("/api/v1/alerts/preferences/{program}"), None),
        ("GET", "/api/v1/my/saved".to_owned(), None),
        ("GET", "/api/v1/recommendations".to_owned(), None),
    ] {
        let url = format!("{base}{path}");
        let request = match method {
            "GET" => client.get(url),
            _ => client.post(url),
        };
        let request = match body {
            Some(body) => request.json(&body),
            None => request,
        };
        assert_eq!(
            request.send().await.unwrap().status(),
            401,
            "{method} {path}"
        );
    }
}
