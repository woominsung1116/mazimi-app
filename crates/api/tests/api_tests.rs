use chrono::{Duration, Utc};
use reqwest::Client;
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use std::{
    net::SocketAddr,
    sync::{
        atomic::{AtomicUsize, Ordering},
        LazyLock,
    },
};
use tokio::net::TcpListener;
use uuid::Uuid;

static DATABASE_URL: LazyLock<String> = LazyLock::new(|| {
    std::env::var("TEST_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://wello:wello@localhost:5432/wello".into())
});
const JWT_SECRET: &str = "test-secret";
mod agent_cases;
mod agent_tools_cases;
mod ai_extraction_cases;
mod applicant_review_cases;
mod application_progress_cases;
mod application_resource_cases;
mod assistant_cases;
mod condition_review_cases;
mod corporate_category_cases;
mod corporate_optout_cases;
mod filtering_cases;
mod form_template_cases;
mod grounded_cases;
mod holder_cases;
mod link_resolver_cases;
mod profile_scope_cases;
mod v1_feedback_cases;
mod youth_fit_cases;

// These integration tests intentionally share a persistent developer DB. A
// catalogue mutation must not overlap the two-call reproducibility assertion.
static PROGRAM_CATALOG_TEST_LOCK: LazyLock<tokio::sync::Mutex<()>> =
    LazyLock::new(|| tokio::sync::Mutex::new(()));

#[tokio::test]
async fn account_deletion_is_confirmed_scoped_and_invalidates_old_tokens() {
    let base = start_test_server().await;
    let (id, token, refresh) = create_test_user(&base).await;
    let (other_id, other_token, _) = create_test_user(&base).await;
    let client = Client::new();
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .unwrap();
    let bypass_uses_owner: bool = sqlx::query_scalar(
        "SELECT prosecdef FROM pg_proc WHERE oid='is_service_role()'::regprocedure",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        !bypass_uses_owner,
        "RLS service check must use the caller's identity"
    );
    sqlx::query("INSERT INTO user_documents(user_id,document_type,file_name,file_size_bytes,encrypted_data,iv_hex) VALUES($1,'other','test.enc',1,$2,'00')")
        .bind(id).bind(vec![1u8]).execute(&pool).await.unwrap();
    assert_eq!(
        client
            .delete(format!("{base}/api/v1/auth/me"))
            .json(&json!({"confirmation":"회원 탈퇴"}))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        client
            .delete(format!("{base}/api/v1/auth/me"))
            .bearer_auth(&token)
            .json(&json!({"confirmation":"no"}))
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    assert!(!client
        .delete(format!("{base}/api/v1/auth/me"))
        .bearer_auth(&token)
        .json(&json!({"confirmation":"회원 탈퇴","user_id":other_id}))
        .send()
        .await
        .unwrap()
        .status()
        .is_success());
    assert_eq!(
        client
            .delete(format!("{base}/api/v1/auth/me"))
            .bearer_auth(&token)
            .json(&json!({"confirmation":"회원 탈퇴"}))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    for table in ["users", "user_profiles", "user_documents", "refresh_tokens"] {
        let column = if table == "users" { "id" } else { "user_id" };
        let count: i64 =
            sqlx::query_scalar(&format!("SELECT count(*) FROM {table} WHERE {column}=$1"))
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(count, 0, "{table} should cascade");
    }
    assert_eq!(
        client
            .get(format!("{base}/api/v1/auth/me"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        client
            .post(format!("{base}/api/v1/auth/refresh"))
            .json(&json!({"refresh_token":refresh}))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        client
            .get(format!("{base}/api/v1/auth/me"))
            .bearer_auth(&other_token)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let elevated_claim = api::auth::create_token(other_id, "admin", JWT_SECRET).unwrap();
    assert_eq!(
        client
            .get(format!("{base}/api/v1/admin/program-maintenance"))
            .bearer_auth(&elevated_claim)
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    sqlx::query("DELETE FROM users WHERE id=$1")
        .bind(other_id)
        .execute(&pool)
        .await
        .unwrap();
}

struct MockEnrichmentProvider {
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl mazimi_worker::enrichment::EnrichmentProvider for MockEnrichmentProvider {
    fn provider_name(&self) -> &'static str {
        "test_provider"
    }

    fn model_name(&self) -> &str {
        "test-model-v1"
    }

    async fn extract(
        &self,
        _page_title: Option<&str>,
        _review_text: &str,
    ) -> anyhow::Result<mazimi_worker::enrichment::ProviderOutput> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(mazimi_worker::enrichment::ProviderOutput {
            payload: json!({
                "title": "검토용 청년 할인",
                "summary": "만 19~34세 청년에게 제공되는 할인 혜택입니다.",
                "provider_name": "테스트 제공처",
                "benefit_category": "discount",
                "program_status": "active",
                "application_start_at": null,
                "application_end_at": null,
                "deadline_at": null,
                "benefit_amount_monthly": null,
                "benefit_amount_semester": null,
                "benefit_amount_once": 10000,
                "min_age": 19,
                "max_age": 34,
                "regions": [],
                "tags": ["청년", "할인"],
                "confidence": 0.9
            }),
            input_token_count: Some(100),
            output_token_count: Some(50),
        })
    }
}

/// Runs the migrations once per test process, before any test server
/// starts. Concurrent `migrate!().run()` calls on a fresh database deadlock
/// on the migration lock and on CREATE INDEX CONCURRENTLY, which waits for
/// every other open transaction.
static MIGRATED: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();

async fn migrate_once() {
    MIGRATED
        .get_or_init(|| async {
            let pool = PgPoolOptions::new()
                .max_connections(1)
                .connect(&DATABASE_URL)
                .await
                .expect("Failed to connect to test database");
            sqlx::migrate!("../../infra/migrations")
                .run(&pool)
                .await
                .expect("Failed to run migrations");
            pool.close().await;
        })
        .await;
}

/// Starts the app on a random port, returns the base URL.
async fn start_test_server() -> String {
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&DATABASE_URL)
        .await
        .expect("Failed to connect to test database");

    migrate_once().await;

    let app = api::build_app_with_env(pool, JWT_SECRET.to_string(), "local".to_string());

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("Failed to bind random port");
    let addr: SocketAddr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        // GovernorLayer's SmartIpKeyExtractor (used by every route group in
        // build_app_with_env) requires `ConnectInfo<SocketAddr>` in the
        // request extensions. Without it every request 500s with
        // "Unable To Extract Key!" before ever reaching a handler. Mirror
        // main.rs's production server setup so the test harness actually
        // exercises the real request path.
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });

    format!("http://{addr}")
}

/// Create a test user in the DB and return (user_id, access_token, refresh_token).
async fn create_test_user(base: &str) -> (Uuid, String, String) {
    // Insert user directly via a dedicated endpoint is not available for arbitrary users,
    // so we insert into DB via a direct pool connection and generate tokens ourselves.
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .expect("Failed to connect for test user setup");

    let user_id: Uuid = sqlx::query_scalar(
        r#"
        INSERT INTO users (auth_provider, auth_provider_id, role, created_at, updated_at)
        VALUES ('test', $1, 'user', now(), now())
        ON CONFLICT (auth_provider, auth_provider_id) DO UPDATE SET updated_at = now()
        RETURNING id
        "#,
    )
    .bind(Uuid::new_v4().to_string())
    .fetch_one(&pool)
    .await
    .expect("Failed to insert test user");

    // Insert a minimal profile so endpoints that read it don't 404
    sqlx::query(
        r#"
        INSERT INTO user_profiles (user_id, birth_year, region_code, updated_at)
        VALUES ($1, 2000, 'busan', now())
        ON CONFLICT (user_id) DO NOTHING
        "#,
    )
    .bind(user_id)
    .execute(&pool)
    .await
    .expect("Failed to insert test user profile");

    let _ = base; // unused but keeps signature consistent

    let access_token = api::auth::create_token(user_id, "user", JWT_SECRET)
        .expect("Failed to create access token");
    let refresh_token = api::auth::issue_refresh_token(&pool, user_id, "user", JWT_SECRET)
        .await
        .expect("Failed to create refresh token");

    (user_id, access_token, refresh_token)
}

/// A refresh token shaped like the ones issued before server-side revocation
/// existed: valid signature, no `jti`, no `refresh_tokens` row.
fn legacy_refresh_token(user_id: Uuid) -> String {
    use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
    let now = Utc::now().timestamp() as usize;
    let claims = api::auth::Claims {
        sub: user_id.to_string(),
        role: "user".into(),
        token_type: "refresh".into(),
        iat: now,
        exp: now + 3600,
        jti: None,
    };
    encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(JWT_SECRET.as_bytes()),
    )
    .unwrap()
}

async fn refresh_status(base: &str, client: &Client, refresh_token: &str) -> (u16, Value) {
    let resp = client
        .post(format!("{base}/api/v1/auth/refresh"))
        .json(&json!({ "refresh_token": refresh_token }))
        .send()
        .await
        .expect("refresh request failed");
    let status = resp.status().as_u16();
    (status, resp.json().await.unwrap_or(Value::Null))
}

/// Create a test admin user in the DB and return (user_id, access_token).
async fn create_test_admin(base: &str) -> (Uuid, String) {
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .expect("Failed to connect for admin setup");

    let user_id: Uuid = sqlx::query_scalar(
        r#"
        INSERT INTO users (auth_provider, auth_provider_id, role, created_at, updated_at)
        VALUES ('test', $1, 'admin', now(), now())
        ON CONFLICT (auth_provider, auth_provider_id) DO UPDATE SET updated_at = now()
        RETURNING id
        "#,
    )
    .bind(format!("admin-{}", Uuid::new_v4()))
    .fetch_one(&pool)
    .await
    .expect("Failed to insert test admin user");

    let _ = base;

    let access_token = api::auth::create_token(user_id, "admin", JWT_SECRET)
        .expect("Failed to create admin access token");

    (user_id, access_token)
}

// ── GET /health ──

#[tokio::test]
async fn health_returns_200_ok() {
    let base = start_test_server().await;
    let client = Client::new();

    let resp = client
        .get(format!("{base}/health"))
        .send()
        .await
        .expect("request failed");

    let status = resp.status().as_u16();
    let body: Value = resp.json().await.expect("invalid json");
    assert_eq!(status, 200, "health check failed: {body}");
    assert_eq!(body["status"], "ok");
}

#[tokio::test]
async fn readiness_requires_database_and_current_schema() {
    let base = start_test_server().await;
    let client = Client::new();

    let resp = client
        .get(format!("{base}/ready"))
        .send()
        .await
        .expect("request failed");

    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.expect("invalid json");
    assert_eq!(body["status"], "ready");
    assert_eq!(body["db"], true);
    assert_eq!(body["schema"], true);
}

#[tokio::test]
async fn readiness_fails_closed_when_database_pool_is_closed() {
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&DATABASE_URL)
        .await
        .expect("Failed to connect to test database");
    pool.close().await;

    let app = api::build_app_with_env(pool, JWT_SECRET.to_string(), "local".to_string());
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("Failed to bind random port");
    let addr = listener.local_addr().expect("missing listener address");
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });

    let resp = Client::new()
        .get(format!("http://{addr}/ready"))
        .send()
        .await
        .expect("request failed");
    assert_eq!(resp.status(), 503);
    let body: Value = resp.json().await.expect("invalid json");
    assert_eq!(body["status"], "not_ready");
    assert_eq!(body["db"], false);
    assert_eq!(body["schema"], false);
}

// ── GET /api/v1/programs ──

#[tokio::test]
async fn list_programs_returns_list() {
    let base = start_test_server().await;
    let client = Client::new();

    let resp = client
        .get(format!("{base}/api/v1/programs"))
        .send()
        .await
        .expect("request failed");

    assert_eq!(resp.status(), 200);

    let body: Value = resp.json().await.expect("invalid json");
    assert!(
        body["total"].is_number(),
        "response must have 'total' field"
    );
    assert!(body["items"].is_array(), "response must have 'items' array");
}

// ── POST /api/v1/recommend/preview (busan, birth_year 2000) ──

#[tokio::test]
async fn recommend_preview_busan_2000_returns_items() {
    let base = start_test_server().await;
    let client = Client::new();

    let payload = json!({
        "birth_year": 2000,
        "region_code": "busan"
    });

    let resp = client
        .post(format!("{base}/api/v1/recommend/preview"))
        .json(&payload)
        .send()
        .await
        .expect("request failed");

    assert_eq!(resp.status(), 200);

    let body: Value = resp.json().await.expect("invalid json");
    assert!(body["total_available"].is_number());
    assert!(body["items"].is_array());
    assert!(body["estimated_monthly"].is_number());
    assert!(body["estimated_semester"].is_number());
}

#[tokio::test]
async fn recommend_preview_enforces_hard_rules_but_keeps_unknowns() {
    let _catalog_guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let base = start_test_server().await;
    let client = Client::new();
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .expect("Failed to connect for hard-filter test setup");
    let program_id = Uuid::new_v4();
    let source_id = format!("hard-filter-test-{program_id}");
    let rules = json!({
        "all": [
            { "field": "enrollment_status", "op": "eq", "value": "enrolled" },
            { "field": "income_bracket", "op": "lte", "value": 5 }
        ]
    });

    sqlx::query(
        r#"
        INSERT INTO programs (
            id, program_type, source_type, source_id, title, program_status,
            min_age, max_age, regions, deadline_at, is_active
        )
        VALUES ($1, 'scholarship', 'manual', $2, '하드필터 API 회귀 테스트',
                'active', 19, 40, ARRAY['busan'], now() + interval '30 days', true)
        "#,
    )
    .bind(program_id)
    .bind(source_id)
    .execute(&pool)
    .await
    .expect("Failed to insert hard-filter test program");

    sqlx::query(
        r#"
        INSERT INTO eligibility_rules (
            program_id, rule_json, hard_filter_json, version, compiled_at
        )
        VALUES ($1, $2, $2, 1, now())
        "#,
    )
    .bind(program_id)
    .bind(&rules)
    .execute(&pool)
    .await
    .expect("Failed to insert hard-filter test rule");

    let contradicted: Value = client
        .post(format!("{base}/api/v1/recommend/preview"))
        .json(&json!({
            "birth_year": 2000,
            "region_code": "busan",
            "enrollment_status": "enrolled",
            "income_bracket": 9
        }))
        .send()
        .await
        .expect("contradicted request failed")
        .json()
        .await
        .expect("contradicted response was not JSON");
    assert!(
        !response_contains_program(&contradicted, program_id),
        "an explicitly failed hard filter must exclude the program: {contradicted}"
    );

    let unknown: Value = client
        .post(format!("{base}/api/v1/recommend/preview"))
        .json(&json!({
            "birth_year": 2000,
            "region_code": "busan",
            "enrollment_status": "enrolled"
        }))
        .send()
        .await
        .expect("unknown request failed")
        .json()
        .await
        .expect("unknown response was not JSON");
    let item = unknown["items"]
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .find(|item| item["program_id"].as_str() == Some(program_id.to_string().as_str()))
        })
        .expect("an unknown hard-filter field must keep the program visible");
    assert!(
        item["missing_checks"]
            .as_array()
            .is_some_and(|checks| checks.iter().any(|value| value == "소득 기준 확인 필요")),
        "the response must request the missing income field: {item}"
    );

    sqlx::query("DELETE FROM programs WHERE id = $1")
        .bind(program_id)
        .execute(&pool)
        .await
        .expect("Failed to clean up hard-filter test program");
}

fn response_contains_program(body: &Value, program_id: Uuid) -> bool {
    let expected = program_id.to_string();
    body["items"].as_array().is_some_and(|items| {
        items
            .iter()
            .any(|item| item["program_id"].as_str() == Some(expected.as_str()))
    })
}

/// 탐색 소득 구간 (income_band): a programme that flags the band stays, one
/// that flags only other bands goes, and a programme with no band limit
/// (NULL or empty, as every corporate offer and most sources) stays for
/// every band. Rows are found by a unique search token and removed before
/// and after, so a failed run leaves nothing in the shared catalogue.
#[tokio::test]
async fn income_band_keeps_programmes_without_a_band_limit() {
    let _catalog_guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let base = start_test_server().await;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .unwrap();
    let token = format!("incomeband{}", Uuid::new_v4().simple());
    // A trigger builds search_tsv from the title, so the token is in each title.
    let cleanup = "DELETE FROM programs WHERE title LIKE $1 || ' %'";
    sqlx::query(cleanup)
        .bind(&token)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO programs (title,source_type,program_type,program_status,is_active,income_median_bands,provider_kind) VALUES \
         ($1 || ' band match','manual','policy','active',true,ARRAY['51-75'],NULL), \
         ($1 || ' band other','manual','policy','active',true,ARRAY['0-50'],NULL), \
         ($1 || ' no limit','manual','welfare','active',true,NULL,NULL), \
         ($1 || ' empty list','manual','scholarship','active',true,ARRAY[]::text[],NULL), \
         ($1 || ' corporate offer','manual','corporate_benefit','active',true,NULL,'corporate')",
    )
    .bind(&token)
    .execute(&pool)
    .await
    .unwrap();
    let client = Client::new();
    let mut seen = Vec::new();
    for band in [Some("51-75"), None] {
        let mut query = vec![("q", token.clone()), ("per_page", "50".to_string())];
        if let Some(band) = band {
            query.push(("income_band", band.to_string()));
        }
        let response = client
            .get(format!("{base}/api/v1/programs"))
            .query(&query)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let body: Value = response.json().await.unwrap();
        let mut titles: Vec<String> = body["items"]
            .as_array()
            .unwrap()
            .iter()
            // "<token> band match" → "band match"
            .map(|item| {
                item["title"]
                    .as_str()
                    .unwrap()
                    .split_once(' ')
                    .unwrap()
                    .1
                    .to_owned()
            })
            .collect();
        titles.sort();
        seen.push(titles);
    }
    let bad = client
        .get(format!("{base}/api/v1/programs"))
        .query(&[("income_band", "le100")])
        .send()
        .await
        .unwrap();
    sqlx::query(cleanup)
        .bind(&token)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        seen[0],
        ["band match", "corporate offer", "empty list", "no limit"]
    );
    assert_eq!(seen[1].len(), 5);
    assert_eq!(bad.status(), 400);
}

#[tokio::test]
async fn deadline_sort_uses_all_expiration_fields_before_undated_programs() {
    let _catalog_guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let base = start_test_server().await;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .unwrap();
    let category = format!("deadline_sort_{}", Uuid::new_v4());
    sqlx::query("INSERT INTO programs (title,source_type,program_type,program_status,is_active,deadline_at,application_end_at,benefit_valid_until) VALUES ('explicit','manual',$1,'active',true,now()+interval '3 days',null,null), ('application','manual',$1,'active',true,null,now()+interval '1 day',null), ('validity','manual',$1,'active',true,null,null,now()+interval '2 days'), ('undated','manual',$1,'active',true,null,null,null)")
        .bind(&category).execute(&pool).await.unwrap();
    let client = Client::new();
    let mut titles = Vec::new();
    for page in ["1", "2"] {
        let response = client
            .get(format!("{base}/api/v1/programs"))
            .query(&[
                ("program_type", category.as_str()),
                ("sort", "deadline_asc"),
                ("status", "open"),
                ("per_page", "2"),
                ("page", page),
            ])
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let body: Value = response.json().await.unwrap();
        assert_eq!(body["total"], 4);
        titles.extend(
            body["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|item| item["title"].as_str().unwrap().to_owned()),
        );
    }
    assert_eq!(titles, ["application", "validity", "explicit", "undated"]);
    sqlx::query("DELETE FROM programs WHERE program_type=$1")
        .bind(&category)
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
}

#[tokio::test]
async fn stored_snapshot_renormalization_replays_without_upstream_fetch() {
    let _catalog_guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let _base = start_test_server().await;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .expect("Failed to connect for renormalization test");
    let suffix = Uuid::new_v4();
    let source_name = format!("normalizer_test_{suffix}");
    let source_id = format!("record_{suffix}");
    let content_hash = "0123456789abcdef0123456789abcdef";

    sqlx::query(
        r#"
        INSERT INTO programs (
            program_type, source_type, source_id, title, program_status,
            is_active, normalizer_version
        )
        VALUES ('policy', $1, $2, '이전 정규화 제목', 'active', false, 0)
        "#,
    )
    .bind(&source_name)
    .bind(&source_id)
    .execute(&pool)
    .await
    .expect("Failed to insert legacy normalized program");

    sqlx::query(
        r#"
        INSERT INTO source_snapshots (
            source_name, source_id, content_hash, raw_payload,
            normalizer_version, fetched_at
        )
        VALUES ($1, $2, $3, $4, 0, now() - interval '1 day')
        "#,
    )
    .bind(&source_name)
    .bind(&source_id)
    .bind(content_hash)
    .bind(json!({ "title": "새 정규화 제목" }))
    .execute(&pool)
    .await
    .expect("Failed to insert stale source snapshot");

    let fetched_at_before: chrono::DateTime<Utc> = sqlx::query_scalar(
        "SELECT fetched_at FROM source_snapshots WHERE source_name = $1 AND source_id = $2",
    )
    .bind(&source_name)
    .bind(&source_id)
    .fetch_one(&pool)
    .await
    .unwrap();

    let summary =
        mazimi_worker::pipeline::renormalize_stored_snapshots(&pool, Some(&source_name), 10)
            .await
            .expect("Stored snapshot replay failed");
    assert_eq!(summary.selected, 1);
    assert_eq!(summary.renormalized, 1);
    assert_eq!(summary.failed, 0);

    let (title, program_version, program_type, is_active): (String, i32, String, bool) =
        sqlx::query_as(
            "SELECT title, normalizer_version, program_type, is_active FROM programs \
         WHERE source_type = $1 AND source_id = $2",
        )
        .bind(&source_name)
        .bind(&source_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let (snapshot_version, fetched_at_after): (i32, chrono::DateTime<Utc>) = sqlx::query_as(
        "SELECT normalizer_version, fetched_at FROM source_snapshots \
         WHERE source_name = $1 AND source_id = $2",
    )
    .bind(&source_name)
    .bind(&source_id)
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(title, "새 정규화 제목");
    assert_eq!(
        program_type, "benefit",
        "replay must update the old browsing category"
    );
    assert!(
        !is_active,
        "replay must preserve a deliberately hidden program"
    );
    assert_eq!(program_version, 1);
    assert_eq!(snapshot_version, 1);
    assert_eq!(
        fetched_at_after, fetched_at_before,
        "network-free replay must not rewrite the upstream fetched_at timestamp"
    );

    sqlx::query("DELETE FROM programs WHERE source_type = $1 AND source_id = $2")
        .bind(&source_name)
        .bind(&source_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM source_snapshots WHERE source_name = $1 AND source_id = $2")
        .bind(&source_name)
        .bind(&source_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM ingestion_runs WHERE source_name = $1")
        .bind(&source_name)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn llm_enrichment_is_cached_and_never_auto_publishes() {
    let _catalog_guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let _base = start_test_server().await;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .expect("Failed to connect for enrichment test");
    let unique = Uuid::new_v4().simple().to_string();
    let domain = format!("llm-{unique}.example.com");
    let source_url = format!("https://{domain}/benefits/youth");
    let content_hash = "abcdef0123456789abcdef0123456789";

    let domain_id = sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO benefit_source_domains (
             organization_name, domain, source_role, crawl_allowed,
             verification_status, verification_reference,
             compliance_status, collection_mode, is_active
         ) VALUES (
             'LLM Test', $1, 'brand_official', false,
             'verified', $2, 'pending', 'manual_review', true
         ) RETURNING id",
    )
    .bind(&domain)
    .bind(&source_url)
    .fetch_one(&pool)
    .await
    .expect("enrichment source domain fixture");
    let candidate_id = sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO benefit_source_candidates (
             domain_id, url, canonical_url, discovered_via, page_title,
             raw_text, content_hash, status, candidate_kind
         ) VALUES (
             $1, $2, $2, 'manual', '청년 할인',
             '만 19~34세 청년에게 1만원 할인', $3, 'review', 'detail'
         ) RETURNING id",
    )
    .bind(domain_id)
    .bind(&source_url)
    .bind(content_hash)
    .fetch_one(&pool)
    .await
    .expect("enrichment candidate fixture");

    let provider = MockEnrichmentProvider {
        calls: AtomicUsize::new(0),
    };
    let first = mazimi_worker::enrichment::run_pending(&pool, &provider, 10)
        .await
        .expect("first enrichment run");
    assert_eq!(first.succeeded, 1);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);

    let (payload, confidence, program_id): (Option<Value>, Option<f32>, Option<Uuid>) =
        sqlx::query_as(
            "SELECT extracted_payload, extraction_confidence, program_id
             FROM benefit_source_candidates WHERE id = $1",
        )
        .bind(candidate_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(payload.as_ref().unwrap()["title"], "검토용 청년 할인");
    assert_eq!(confidence, Some(0.9));
    assert!(
        program_id.is_none(),
        "LLM enrichment must not create or publish a program"
    );

    sqlx::query(
        "UPDATE benefit_source_candidates
         SET extracted_payload = NULL, extraction_confidence = NULL
         WHERE id = $1",
    )
    .bind(candidate_id)
    .execute(&pool)
    .await
    .unwrap();
    let second = mazimi_worker::enrichment::run_pending(&pool, &provider, 10)
        .await
        .expect("cached enrichment run");
    assert_eq!(second.cache_hits, 1);
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        1,
        "same content/model/prompt/schema must be served from cache"
    );

    let cache: (String, i32, i32) = sqlx::query_as(
        "SELECT status, input_token_count, output_token_count
         FROM benefit_candidate_enrichments WHERE candidate_id = $1",
    )
    .bind(candidate_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(cache, ("succeeded".to_string(), 100, 50));

    sqlx::query("DELETE FROM benefit_source_candidates WHERE id = $1")
        .bind(candidate_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM benefit_source_domains WHERE id = $1")
        .bind(domain_id)
        .execute(&pool)
        .await
        .unwrap();
}

// ── POST /api/v1/profile → creates user and profile ──

#[tokio::test]
async fn save_profile_creates_user_and_profile() {
    let base = start_test_server().await;
    let client = Client::new();
    let (_, token, _) = create_test_user(&base).await;

    let payload = json!({
        "birth_year": 2001,
        "region_code": "busan"
    });

    let resp = client
        .post(format!("{base}/api/v1/profile"))
        .bearer_auth(&token)
        .json(&payload)
        .send()
        .await
        .expect("request failed");

    assert_eq!(resp.status(), 200);

    let body: Value = resp.json().await.expect("invalid json");
    assert!(
        body["user_id"].is_string(),
        "response must include user_id: {body}"
    );
    assert_eq!(
        body["profile"]["birth_year"], 2001,
        "profile must echo birth_year"
    );
    assert_eq!(
        body["profile"]["region_code"], "busan",
        "profile must echo region_code"
    );
}

// ── Recommendation reproducibility: same input → same output ──

#[tokio::test]
async fn recommend_preview_is_reproducible() {
    let _catalog_guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let base = start_test_server().await;
    let client = Client::new();

    let payload = json!({
        "birth_year": 2000,
        "region_code": "busan"
    });

    let resp1 = client
        .post(format!("{base}/api/v1/recommend/preview"))
        .json(&payload)
        .send()
        .await
        .expect("first request failed");
    assert_eq!(resp1.status(), 200);
    let body1: Value = resp1.json().await.expect("invalid json on first call");

    let resp2 = client
        .post(format!("{base}/api/v1/recommend/preview"))
        .json(&payload)
        .send()
        .await
        .expect("second request failed");
    assert_eq!(resp2.status(), 200);
    let body2: Value = resp2.json().await.expect("invalid json on second call");

    assert_eq!(
        body1["total_available"], body2["total_available"],
        "total_available must be identical across calls"
    );
    assert_eq!(
        body1["estimated_monthly"], body2["estimated_monthly"],
        "estimated_monthly must be identical across calls"
    );
    assert_eq!(
        body1["items"], body2["items"],
        "items (including order) must be identical across calls"
    );
}

// ── POST /api/v1/auth/refresh ──

#[tokio::test]
async fn auth_refresh_returns_new_tokens() {
    let base = start_test_server().await;
    let client = Client::new();
    let (_, _, refresh_token) = create_test_user(&base).await;

    let resp = client
        .post(format!("{base}/api/v1/auth/refresh"))
        .json(&json!({ "refresh_token": refresh_token }))
        .send()
        .await
        .expect("request failed");

    assert_eq!(resp.status(), 200, "refresh should succeed");

    let body: Value = resp.json().await.expect("invalid json");
    assert!(
        body["token"].is_string(),
        "response must have 'token': {body}"
    );
    assert!(
        body["refresh_token"].is_string(),
        "response must have 'refresh_token': {body}"
    );
}

#[tokio::test]
async fn auth_refresh_rotates_once_and_replay_revokes_every_session() {
    let base = start_test_server().await;
    let client = Client::new();
    let (user_id, _, first) = create_test_user(&base).await;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .unwrap();
    // A second device holds its own, independent refresh token.
    let other_device = api::auth::issue_refresh_token(&pool, user_id, "user", JWT_SECRET)
        .await
        .unwrap();

    let (status, body) = refresh_status(&base, &client, &first).await;
    assert_eq!(status, 200, "{body}");
    let second = body["refresh_token"].as_str().unwrap().to_string();
    assert_ne!(second, first, "refresh must rotate the credential");
    assert!(body["token"].is_string());
    let rotated: Option<bool> = sqlx::query_scalar(
        "SELECT revoked_at IS NOT NULL FROM refresh_tokens \
         WHERE user_id=$1 AND replaced_by IS NOT NULL",
    )
    .bind(user_id)
    .fetch_optional(&pool)
    .await
    .unwrap();
    assert_eq!(rotated, Some(true), "old row must be revoked and linked");

    // The rotated-away token is usable by the second device until replay.
    let (status, _) = refresh_status(&base, &client, &second).await;
    assert_eq!(status, 200);
    let (status, _) = refresh_status(&base, &client, &other_device).await;
    assert_eq!(status, 200);

    // Replaying the already-rotated first token once the grace window has
    // passed is theft: 401 and every outstanding session of the account is
    // revoked. (Inside the window the same replay is refused without the
    // account-wide revocation; see the grace-window test below.)
    sqlx::query("UPDATE refresh_tokens SET revoked_at = now() - interval '2 minutes' WHERE jti=$1")
        .bind(refresh_jti(&first))
        .execute(&pool)
        .await
        .unwrap();
    let (status, _) = refresh_status(&base, &client, &first).await;
    assert_eq!(status, 401);
    let live: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM refresh_tokens WHERE user_id=$1 AND revoked_at IS NULL",
    )
    .bind(user_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(live, 0, "replay must revoke all sessions");
    let (status, body) = refresh_status(&base, &client, &second).await;
    assert_eq!(status, 401, "{body}");

    sqlx::query("DELETE FROM users WHERE id=$1")
        .bind(user_id)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn auth_refresh_rejects_tokens_without_server_record() {
    let base = start_test_server().await;
    let client = Client::new();
    let (user_id, _, _) = create_test_user(&base).await;

    // Pre-rotation token shape: valid signature, no jti → cannot be revoked → rejected.
    let (status, body) = refresh_status(&base, &client, &legacy_refresh_token(user_id)).await;
    assert_eq!(status, 401, "{body}");

    // A forged jti the server never issued is rejected without side effects.
    let forged = {
        use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
        let now = Utc::now().timestamp() as usize;
        encode(
            &Header::new(Algorithm::HS256),
            &api::auth::Claims {
                sub: user_id.to_string(),
                role: "user".into(),
                token_type: "refresh".into(),
                iat: now,
                exp: now + 3600,
                jti: Some(Uuid::new_v4().to_string()),
            },
            &EncodingKey::from_secret(JWT_SECRET.as_bytes()),
        )
        .unwrap()
    };
    let (status, _) = refresh_status(&base, &client, &forged).await;
    assert_eq!(status, 401);
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .unwrap();
    let live: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM refresh_tokens WHERE user_id=$1 AND revoked_at IS NULL",
    )
    .bind(user_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(live, 1, "unknown jti must not revoke the real session");

    // The legacy endpoint that accepted a client-supplied Kakao token is gone.
    let resp = client
        .post(format!("{base}/api/v1/auth/kakao"))
        .json(&json!({ "access_token": "anything" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

/// `jti` of a refresh token as the server would read it.
fn refresh_jti(token: &str) -> Uuid {
    let claims = api::auth::verify_token(token, JWT_SECRET).expect("valid refresh token");
    Uuid::parse_str(claims.jti.as_deref().expect("refresh token carries a jti")).unwrap()
}

/// `(revoked, replaced_by)` of one refresh token row.
async fn refresh_row(pool: &sqlx::PgPool, jti: Uuid) -> (bool, Option<Uuid>) {
    sqlx::query_as("SELECT revoked_at IS NOT NULL, replaced_by FROM refresh_tokens WHERE jti=$1")
        .bind(jti)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn live_refresh_tokens(pool: &sqlx::PgPool, user_id: Uuid) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM refresh_tokens WHERE user_id=$1 AND revoked_at IS NULL",
    )
    .bind(user_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// A rotation whose response never reached the client (timeout, app killed
/// between the server commit and token storage) leaves the client holding
/// the old token. Retrying with it inside the grace window must continue the
/// chain instead of logging out every device; the same token replayed later
/// is theft.
#[tokio::test]
async fn auth_refresh_retry_inside_grace_window_continues_the_chain() {
    let base = start_test_server().await;
    let client = Client::new();
    let (user_id, _, first) = create_test_user(&base).await;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .unwrap();
    let other_device = api::auth::issue_refresh_token(&pool, user_id, "user", JWT_SECRET)
        .await
        .unwrap();

    // Rotation commits server-side; pretend the response was lost.
    let (status, body) = refresh_status(&base, &client, &first).await;
    assert_eq!(status, 200, "{body}");
    let second = body["refresh_token"].as_str().unwrap().to_string();

    // The client retries with the token it still holds.
    let (status, body) = refresh_status(&base, &client, &first).await;
    assert_eq!(status, 200, "retry inside the grace window: {body}");
    let third = body["refresh_token"].as_str().unwrap().to_string();
    assert_ne!(third, second, "a fresh pair is issued");
    assert!(body["token"].is_string());

    // Chain: first -> second (unused, now revoked) -> third.
    assert_eq!(
        refresh_row(&pool, refresh_jti(&first)).await,
        (true, Some(refresh_jti(&second)))
    );
    assert_eq!(
        refresh_row(&pool, refresh_jti(&second)).await,
        (true, Some(refresh_jti(&third))),
        "the unused successor is revoked and chained forward"
    );
    assert_eq!(
        live_refresh_tokens(&pool, user_id).await,
        2,
        "third and the other device stay live; nothing was mass-revoked"
    );
    let (status, _) = refresh_status(&base, &client, &other_device).await;
    assert_eq!(status, 200, "the other device is untouched");
    let (status, body) = refresh_status(&base, &client, &third).await;
    assert_eq!(status, 200, "the chain continues: {body}");

    // The same shape of reuse outside the window is theft: third was rotated
    // to an unused successor, but more than 60 seconds ago.
    sqlx::query("UPDATE refresh_tokens SET revoked_at = now() - interval '2 minutes' WHERE jti=$1")
        .bind(refresh_jti(&third))
        .execute(&pool)
        .await
        .unwrap();
    let (status, _) = refresh_status(&base, &client, &third).await;
    assert_eq!(status, 401);
    assert_eq!(
        live_refresh_tokens(&pool, user_id).await,
        0,
        "replay outside the grace window revokes every session"
    );

    sqlx::query("DELETE FROM users WHERE id=$1")
        .bind(user_id)
        .execute(&pool)
        .await
        .unwrap();
}

/// Inside the grace window a rotated token whose successor was already used
/// is refused and its descendant chain is revoked (security L1): a thief
/// who rotated first must not keep rotating after the owner's client gives
/// up on a 401. Other devices are not touched. Each auth test server allows
/// five auth requests, so this scenario has its own.
#[tokio::test]
async fn auth_refresh_reuse_inside_grace_window_revokes_only_its_chain() {
    let base = start_test_server().await;
    let client = Client::new();
    let (user_id, _, first) = create_test_user(&base).await;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .unwrap();
    let other_device = api::auth::issue_refresh_token(&pool, user_id, "user", JWT_SECRET)
        .await
        .unwrap();

    // first -> second (response lost) -> third (reclaimed by the retry).
    let (status, body) = refresh_status(&base, &client, &first).await;
    assert_eq!(status, 200, "{body}");
    let (status, body) = refresh_status(&base, &client, &first).await;
    assert_eq!(status, 200, "retry inside the grace window: {body}");
    let third = body["refresh_token"].as_str().unwrap().to_string();

    // The same token a third time, still inside the window: its successor was
    // already used, so nothing is reclaimed. The request is refused and the
    // chain it started (third) is revoked; the other device stays live.
    let (status, body) = refresh_status(&base, &client, &first).await;
    assert_eq!(status, 401, "{body}");
    assert_eq!(
        live_refresh_tokens(&pool, user_id).await,
        1,
        "only the other device survives"
    );
    assert!(
        refresh_row(&pool, refresh_jti(&third)).await.0,
        "chain revoked"
    );
    let (status, _) = refresh_status(&base, &client, &other_device).await;
    assert_eq!(status, 200, "the other device is untouched");

    sqlx::query("DELETE FROM users WHERE id=$1")
        .bind(user_id)
        .execute(&pool)
        .await
        .unwrap();
}

/// A token revoked by logout and presented to /refresh seconds later is the
/// client's own 401 retry overlapping its sign-out, not theft; the same token
/// after the window is reuse of a dead credential.
#[tokio::test]
async fn auth_logout_revoked_token_is_theft_only_outside_the_grace_window() {
    let base = start_test_server().await;
    let client = Client::new();
    let (user_id, _, refresh) = create_test_user(&base).await;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .unwrap();
    let other_device = api::auth::issue_refresh_token(&pool, user_id, "user", JWT_SECRET)
        .await
        .unwrap();

    let resp = client
        .post(format!("{base}/api/v1/auth/logout"))
        .json(&json!({ "refresh_token": refresh }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Inside the window: 401 only, the other device keeps its session.
    let (status, _) = refresh_status(&base, &client, &refresh).await;
    assert_eq!(status, 401, "revoked token must not refresh");
    assert_eq!(
        live_refresh_tokens(&pool, user_id).await,
        1,
        "a logout-revoked token inside the grace window revokes nothing else"
    );
    assert_eq!(
        refresh_row(&pool, refresh_jti(&other_device)).await,
        (false, None)
    );

    // After the window: 401 and account-wide revocation.
    sqlx::query("UPDATE refresh_tokens SET revoked_at = now() - interval '2 minutes' WHERE jti=$1")
        .bind(refresh_jti(&refresh))
        .execute(&pool)
        .await
        .unwrap();
    let (status, _) = refresh_status(&base, &client, &refresh).await;
    assert_eq!(status, 401);
    assert_eq!(live_refresh_tokens(&pool, user_id).await, 0);

    sqlx::query("DELETE FROM users WHERE id=$1")
        .bind(user_id)
        .execute(&pool)
        .await
        .unwrap();
}

/// Review L-4: a sign-out whose access token already expired cannot reach
/// DELETE /push/register, so /auth/logout (authenticated by the refresh
/// token) removes this device's push row. Otherwise the next account on the
/// phone would get 409 from /push/register for up to 30 days.
#[tokio::test]
async fn auth_logout_removes_this_devices_push_token() {
    let base = start_test_server().await;
    let client = Client::new();
    let (user_id, _, refresh) = create_test_user(&base).await;
    let (other_id, _, _) = create_test_user(&base).await;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .unwrap();
    let mine = format!("ExponentPushToken[logout-{}]", Uuid::new_v4().simple());
    let my_other_device = format!("ExponentPushToken[logout-{}]", Uuid::new_v4().simple());
    let theirs = format!("ExponentPushToken[logout-{}]", Uuid::new_v4().simple());
    for (owner, token) in [
        (user_id, &mine),
        (user_id, &my_other_device),
        (other_id, &theirs),
    ] {
        sqlx::query("INSERT INTO push_tokens (user_id, token, platform) VALUES ($1, $2, 'ios')")
            .bind(owner)
            .bind(token)
            .execute(&pool)
            .await
            .unwrap();
    }
    let holders = |pool: sqlx::PgPool| async move {
        let rows: Vec<String> = sqlx::query_scalar(
            "SELECT token FROM push_tokens WHERE token LIKE 'ExponentPushToken[logout-%' AND user_id = ANY($1) ORDER BY token",
        )
        .bind(vec![user_id, other_id])
        .fetch_all(&pool)
        .await
        .unwrap();
        rows
    };

    // Someone else's token in the body is never deleted.
    let resp = client
        .post(format!("{base}/api/v1/auth/logout"))
        .json(&json!({ "refresh_token": refresh, "push_token": theirs }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert!(holders(pool.clone()).await.contains(&theirs));

    let refresh = api::auth::issue_refresh_token(&pool, user_id, "user", JWT_SECRET)
        .await
        .unwrap();
    let resp = client
        .post(format!("{base}/api/v1/auth/logout"))
        .json(&json!({ "refresh_token": refresh, "push_token": mine }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let left = holders(pool.clone()).await;
    assert!(!left.contains(&mine), "this device's row is gone");
    assert!(
        left.contains(&my_other_device),
        "the account's other devices stay"
    );
    assert!(left.contains(&theirs));

    // An invalid refresh token deletes nothing.
    let resp = client
        .post(format!("{base}/api/v1/auth/logout"))
        .json(&json!({ "refresh_token": "not-a-jwt", "push_token": my_other_device }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert!(holders(pool.clone()).await.contains(&my_other_device));

    sqlx::query("DELETE FROM users WHERE id = ANY($1)")
        .bind(vec![user_id, other_id])
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn auth_logout_revokes_the_refresh_token_server_side() {
    let base = start_test_server().await;
    let client = Client::new();
    let (user_id, access, refresh) = create_test_user(&base).await;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .unwrap();
    let other_device = api::auth::issue_refresh_token(&pool, user_id, "user", JWT_SECRET)
        .await
        .unwrap();
    let logout = |token: &str| {
        client
            .post(format!("{base}/api/v1/auth/logout"))
            .json(&json!({ "refresh_token": token }))
            .send()
    };

    // The refresh token is the credential: no access token is needed, so a
    // client whose access token expired 30 minutes ago can still end its
    // session. Only the presented jti is revoked.
    let resp = logout(&refresh).await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["logged_out"], true);
    assert_eq!(
        refresh_row(&pool, refresh_jti(&refresh)).await,
        (true, None)
    );
    assert_eq!(
        refresh_row(&pool, refresh_jti(&other_device)).await,
        (false, None),
        "other devices of the same account keep their session"
    );

    // (Presenting the logout-revoked token to /refresh is covered by
    // auth_logout_revoked_token_is_theft_only_outside_the_grace_window.)

    // Idempotent and uniform: repeated, malformed and wrong-type tokens all
    // return the same 200 so a client can always finish signing out locally
    // and the response never reveals whether a token was valid.
    for token in [refresh.as_str(), "not-a-token", access.as_str()] {
        let resp = logout(token).await.unwrap();
        assert_eq!(resp.status(), 200, "{token}");
        let body: Value = resp.json().await.unwrap();
        assert_eq!(body["logged_out"], true);
    }

    sqlx::query("DELETE FROM users WHERE id=$1")
        .bind(user_id)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn auth_refresh_rejects_access_token() {
    let base = start_test_server().await;
    let client = Client::new();
    let (_, access_token, _) = create_test_user(&base).await;

    // Passing an access token to the refresh endpoint should be rejected
    let resp = client
        .post(format!("{base}/api/v1/auth/refresh"))
        .json(&json!({ "refresh_token": access_token }))
        .send()
        .await
        .expect("request failed");

    assert_eq!(resp.status(), 401);
}

// ── GET /api/v1/auth/me ──

#[tokio::test]
async fn auth_me_returns_user_info() {
    let base = start_test_server().await;
    let client = Client::new();
    let (user_id, token, _) = create_test_user(&base).await;

    let resp = client
        .get(format!("{base}/api/v1/auth/me"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("request failed");

    assert_eq!(resp.status(), 200, "auth/me should succeed");

    let body: Value = resp.json().await.expect("invalid json");
    assert_eq!(
        body["id"].as_str().unwrap_or(""),
        user_id.to_string(),
        "returned id must match"
    );
    assert!(
        body["role"].is_string(),
        "response must have 'role': {body}"
    );
}

#[tokio::test]
async fn auth_me_rejects_unauthenticated() {
    let base = start_test_server().await;
    let client = Client::new();

    let resp = client
        .get(format!("{base}/api/v1/auth/me"))
        .send()
        .await
        .expect("request failed");

    assert_eq!(resp.status(), 401);
}

// ── GET /api/v1/profile ──

#[tokio::test]
async fn get_profile_returns_own_profile() {
    let base = start_test_server().await;
    let client = Client::new();
    let (user_id, token, _) = create_test_user(&base).await;

    let resp = client
        .get(format!("{base}/api/v1/profile"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("request failed");

    assert_eq!(
        resp.status(),
        200,
        "GET /profile should succeed: body omitted"
    );

    let body: Value = resp.json().await.expect("invalid json");
    assert_eq!(
        body["user_id"].as_str().unwrap_or(""),
        user_id.to_string(),
        "user_id must match"
    );
}

#[tokio::test]
async fn get_profile_rejects_unauthenticated() {
    let base = start_test_server().await;
    let client = Client::new();

    let resp = client
        .get(format!("{base}/api/v1/profile"))
        .send()
        .await
        .expect("request failed");

    assert_eq!(resp.status(), 401);
}

// ── POST /api/v1/programs/{id}/bookmark ──

#[tokio::test]
async fn bookmark_toggle_works() {
    let base = start_test_server().await;
    let client = Client::new();
    let (_, token, _) = create_test_user(&base).await;

    // Get a program id to bookmark
    let programs_resp = client
        .get(format!("{base}/api/v1/programs"))
        .send()
        .await
        .expect("programs request failed");
    let programs_body: Value = programs_resp.json().await.expect("invalid json");
    let items = programs_body["items"]
        .as_array()
        .expect("items must be array");

    if items.is_empty() {
        // No programs to bookmark; skip
        return;
    }

    let program_id = items[0]["id"].as_str().expect("program must have id");

    // First toggle: should bookmark
    let resp1 = client
        .post(format!("{base}/api/v1/programs/{program_id}/bookmark"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("request failed");
    assert_eq!(resp1.status(), 200);
    let body1: Value = resp1.json().await.expect("invalid json");
    assert_eq!(body1["bookmarked"], true, "first toggle must bookmark");

    // Second toggle: should unbookmark
    let resp2 = client
        .post(format!("{base}/api/v1/programs/{program_id}/bookmark"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("request failed");
    assert_eq!(resp2.status(), 200);
    let body2: Value = resp2.json().await.expect("invalid json");
    assert_eq!(body2["bookmarked"], false, "second toggle must unbookmark");
}

#[tokio::test]
async fn bookmark_rejects_unauthenticated() {
    let base = start_test_server().await;
    let client = Client::new();
    let fake_id = Uuid::new_v4();

    let resp = client
        .post(format!("{base}/api/v1/programs/{fake_id}/bookmark"))
        .send()
        .await
        .expect("request failed");

    assert_eq!(resp.status(), 401);
}

/// Audit L5: an unknown programme is a 404 on both bookmark routes (POST
/// returned 500 on the foreign key); an unpublished one cannot be newly
/// saved, and can still be unsaved.
#[tokio::test]
async fn bookmarks_save_published_programmes_only() {
    let base = start_test_server().await;
    let client = Client::new();
    let (user, token, _) = create_test_user(&base).await;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .unwrap();
    let post = |id: Uuid| {
        client
            .post(format!("{base}/api/v1/programs/{id}/bookmark"))
            .bearer_auth(&token)
            .send()
    };
    let put = |id: Uuid, bookmarked: bool| {
        client
            .put(format!("{base}/api/v1/programs/{id}/bookmark"))
            .bearer_auth(&token)
            .json(&json!({ "bookmarked": bookmarked }))
            .send()
    };
    assert_eq!(post(Uuid::new_v4()).await.unwrap().status(), 404);
    assert_eq!(put(Uuid::new_v4(), true).await.unwrap().status(), 404);
    let hidden = Uuid::new_v4();
    sqlx::query("INSERT INTO programs(id,program_type,source_type,source_id,title,program_status,is_active) VALUES($1,'policy','manual',$1::text,'비공개 혜택','active',false)")
        .bind(hidden)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(post(hidden).await.unwrap().status(), 404);
    assert_eq!(put(hidden, true).await.unwrap().status(), 404);
    // A row saved before it was unpublished can still be unsaved.
    sqlx::query("INSERT INTO user_bookmarks(user_id,program_id) VALUES($1,$2)")
        .bind(user)
        .bind(hidden)
        .execute(&pool)
        .await
        .unwrap();
    let response = put(hidden, false).await.unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.json::<Value>().await.unwrap()["bookmarked"], false);
    sqlx::query("INSERT INTO user_bookmarks(user_id,program_id) VALUES($1,$2)")
        .bind(user)
        .bind(hidden)
        .execute(&pool)
        .await
        .unwrap();
    let response = post(hidden).await.unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.json::<Value>().await.unwrap()["bookmarked"], false);
    sqlx::query("DELETE FROM programs WHERE id = $1")
        .bind(hidden)
        .execute(&pool)
        .await
        .unwrap();
}

/// Audit L9: an alert names the deadline day the worker alerted on, the
/// earliest of the three deadline columns as a Korean date.
#[tokio::test]
async fn an_alert_carries_the_deadline_day_it_was_sent_for() {
    let base = start_test_server().await;
    let client = Client::new();
    let (user, token, _) = create_test_user(&base).await;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .unwrap();
    let program = Uuid::new_v4();
    // Valid until 2026-12-01 00:00 KST: the last open day is 11-30.
    sqlx::query("INSERT INTO programs(id,program_type,source_type,source_id,title,program_status,is_active,benefit_valid_until,application_end_at) VALUES($1,'corporate_benefit','manual',$1::text,'알림 마감 검증','active',true,'2026-11-30T15:00:00Z','2026-12-20T00:00:00Z')")
        .bind(program)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO alert_deliveries(user_id,program_id,alert_type) VALUES($1,$2,'d3')")
        .bind(user)
        .bind(program)
        .execute(&pool)
        .await
        .unwrap();
    let body: Value = client
        .get(format!("{base}/api/v1/alerts"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let item = body["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["program_id"] == program.to_string())
        .unwrap_or_else(|| panic!("{body}"));
    assert_eq!(item["deadline_date"], "2026-11-30", "{item}");
    sqlx::query("DELETE FROM programs WHERE id = $1")
        .bind(program)
        .execute(&pool)
        .await
        .unwrap();
}

// ── POST /api/v1/programs/{id}/state ──

#[tokio::test]
async fn upsert_state_works() {
    let base = start_test_server().await;
    let client = Client::new();
    let (_, token, _) = create_test_user(&base).await;

    let programs_resp = client
        .get(format!("{base}/api/v1/programs"))
        .send()
        .await
        .expect("programs request failed");
    let programs_body: Value = programs_resp.json().await.expect("invalid json");
    let items = programs_body["items"]
        .as_array()
        .expect("items must be array");

    if items.is_empty() {
        return;
    }

    let program_id = items[0]["id"].as_str().expect("program must have id");

    let resp = client
        .post(format!("{base}/api/v1/programs/{program_id}/state"))
        .bearer_auth(&token)
        .json(&json!({ "state": "interested", "memo": "test memo" }))
        .send()
        .await
        .expect("request failed");

    assert_eq!(resp.status(), 200, "upsert state should succeed");

    let body: Value = resp.json().await.expect("invalid json");
    assert_eq!(body["state"], "interested", "state must match");
}

#[tokio::test]
async fn upsert_state_rejects_invalid_state() {
    let base = start_test_server().await;
    let client = Client::new();
    let (_, token, _) = create_test_user(&base).await;
    let fake_id = Uuid::new_v4();

    let resp = client
        .post(format!("{base}/api/v1/programs/{fake_id}/state"))
        .bearer_auth(&token)
        .json(&json!({ "state": "invalid_state_xyz" }))
        .send()
        .await
        .expect("request failed");

    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn upsert_state_rejects_unauthenticated() {
    let base = start_test_server().await;
    let client = Client::new();
    let fake_id = Uuid::new_v4();

    let resp = client
        .post(format!("{base}/api/v1/programs/{fake_id}/state"))
        .json(&json!({ "state": "interested" }))
        .send()
        .await
        .expect("request failed");

    assert_eq!(resp.status(), 401);
}

// ── GET /api/v1/dashboard ──

#[tokio::test]
async fn dashboard_returns_stats() {
    let base = start_test_server().await;
    let client = Client::new();
    let (_, token, _) = create_test_user(&base).await;

    let resp = client
        .get(format!("{base}/api/v1/dashboard"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("request failed");

    assert_eq!(resp.status(), 200, "dashboard should succeed");

    let body: Value = resp.json().await.expect("invalid json");
    assert!(
        body["bookmarked_count"].is_number(),
        "must have bookmarked_count: {body}"
    );
    assert!(
        body["applying_count"].is_number(),
        "must have applying_count: {body}"
    );
    assert!(
        body["upcoming_deadlines"].is_array(),
        "must have upcoming_deadlines: {body}"
    );
    assert!(
        body["estimated_monthly"].is_number(),
        "must have estimated_monthly: {body}"
    );
    assert!(
        body["estimated_semester"].is_number(),
        "must have estimated_semester: {body}"
    );
    assert!(
        body["todo_items"].is_array(),
        "must have todo_items: {body}"
    );
}

#[tokio::test]
async fn dashboard_rejects_unauthenticated() {
    let base = start_test_server().await;
    let client = Client::new();

    let resp = client
        .get(format!("{base}/api/v1/dashboard"))
        .send()
        .await
        .expect("request failed");

    assert_eq!(resp.status(), 401);
}

// ── GET /api/v1/alerts ──

#[tokio::test]
async fn list_alerts_returns_paginated_list() {
    let base = start_test_server().await;
    let client = Client::new();
    let (_, token, _) = create_test_user(&base).await;

    let resp = client
        .get(format!("{base}/api/v1/alerts"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("request failed");

    assert_eq!(resp.status(), 200, "list alerts should succeed");

    let body: Value = resp.json().await.expect("invalid json");
    assert!(body["total"].is_number(), "must have total: {body}");
    assert!(body["items"].is_array(), "must have items: {body}");
}

#[tokio::test]
async fn list_alerts_rejects_unauthenticated() {
    let base = start_test_server().await;
    let client = Client::new();

    let resp = client
        .get(format!("{base}/api/v1/alerts"))
        .send()
        .await
        .expect("request failed");

    assert_eq!(resp.status(), 401);
}

// ── POST /api/v1/alerts/preferences ──

#[tokio::test]
async fn upsert_alert_preferences_returns_subscriptions() {
    let base = start_test_server().await;
    let client = Client::new();
    let (_, token, _) = create_test_user(&base).await;

    // Need an active program to subscribe to
    let programs_resp = client
        .get(format!("{base}/api/v1/programs"))
        .send()
        .await
        .expect("programs request failed");
    let programs_body: Value = programs_resp.json().await.expect("invalid json");
    let items = programs_body["items"]
        .as_array()
        .expect("items must be array");

    // Filter to active programs
    let active = items
        .iter()
        .find(|p| p["is_active"].as_bool().unwrap_or(false));

    if active.is_none() {
        // No active programs; the handler returns 404 — just verify that
        let fake_id = Uuid::new_v4();
        let resp = client
            .post(format!("{base}/api/v1/alerts/preferences"))
            .bearer_auth(&token)
            .json(&json!({ "program_id": fake_id, "enabled": true }))
            .send()
            .await
            .expect("request failed");
        assert_eq!(resp.status(), 404);
        return;
    }

    let program_id = active.unwrap()["id"]
        .as_str()
        .expect("program must have id");

    let resp = client
        .post(format!("{base}/api/v1/alerts/preferences"))
        .bearer_auth(&token)
        .json(&json!({ "program_id": program_id, "enabled": true }))
        .send()
        .await
        .expect("request failed");

    assert_eq!(resp.status(), 200, "upsert preferences should succeed");

    let body: Value = resp.json().await.expect("invalid json");
    assert!(
        body["subscriptions"].is_array(),
        "must have subscriptions: {body}"
    );
    assert_eq!(body["updated"]["enabled"], true);
}

#[tokio::test]
async fn upsert_alert_preferences_rejects_unauthenticated() {
    let base = start_test_server().await;
    let client = Client::new();
    let fake_id = Uuid::new_v4();

    let resp = client
        .post(format!("{base}/api/v1/alerts/preferences"))
        .json(&json!({ "program_id": fake_id, "enabled": true }))
        .send()
        .await
        .expect("request failed");

    assert_eq!(resp.status(), 401);
}

// ── GET /api/v1/alerts/preferences/{program_id} ──

#[tokio::test]
async fn get_alert_preference_reflects_subscription_state() {
    let base = start_test_server().await;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .unwrap();
    let client = Client::new();
    let (_, token, _) = create_test_user(&base).await;

    let program_id = Uuid::new_v4();
    sqlx::query("INSERT INTO programs(id,program_type,source_type,title,program_status,is_active) VALUES($1,'scholarship','manual','알림 설정 조회 회귀 테스트','active',true)")
        .bind(program_id)
        .execute(&pool)
        .await
        .unwrap();

    // No subscription row yet: must default to false, not 404.
    let resp = client
        .get(format!("{base}/api/v1/alerts/preferences/{program_id}"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("request failed");
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.expect("invalid json");
    assert_eq!(body["program_id"], program_id.to_string());
    assert_eq!(body["enabled"], false);

    let resp = client
        .post(format!("{base}/api/v1/alerts/preferences"))
        .bearer_auth(&token)
        .json(&json!({ "program_id": program_id, "enabled": true }))
        .send()
        .await
        .expect("request failed");
    assert_eq!(resp.status(), 200);

    let resp = client
        .get(format!("{base}/api/v1/alerts/preferences/{program_id}"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("request failed");
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.expect("invalid json");
    assert_eq!(body["enabled"], true);
}

#[tokio::test]
async fn get_alert_preference_404_for_unknown_program() {
    let base = start_test_server().await;
    let client = Client::new();
    let (_, token, _) = create_test_user(&base).await;
    let fake_id = Uuid::new_v4();

    let resp = client
        .get(format!("{base}/api/v1/alerts/preferences/{fake_id}"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("request failed");

    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn get_alert_preference_rejects_unauthenticated() {
    let base = start_test_server().await;
    let client = Client::new();
    let fake_id = Uuid::new_v4();

    let resp = client
        .get(format!("{base}/api/v1/alerts/preferences/{fake_id}"))
        .send()
        .await
        .expect("request failed");

    assert_eq!(resp.status(), 401);
}

// ── POST /api/v1/alerts/read-all ──

#[tokio::test]
async fn mark_all_read_only_affects_callers_own_deliveries() {
    let base = start_test_server().await;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .unwrap();
    let client = Client::new();
    let (owner, owner_token, _) = create_test_user(&base).await;
    let (other, _other_token, _) = create_test_user(&base).await;

    let program_id = Uuid::new_v4();
    sqlx::query("INSERT INTO programs(id,program_type,source_type,title,program_status,is_active) VALUES($1,'scholarship','manual','알림 읽음 처리 회귀 테스트','active',true)")
        .bind(program_id)
        .execute(&pool)
        .await
        .unwrap();

    // Two unread, in-app-visible deliveries for the owner.
    for alert_type in ["d7", "d3"] {
        sqlx::query("INSERT INTO alert_deliveries(id,user_id,program_id,alert_type,alert_date,in_app_visible) VALUES($1,$2,$3,$4,CURRENT_DATE,true)")
            .bind(Uuid::new_v4())
            .bind(owner)
            .bind(program_id)
            .bind(alert_type)
            .execute(&pool)
            .await
            .unwrap();
    }
    // One unread delivery for a different user — must stay untouched.
    sqlx::query("INSERT INTO alert_deliveries(id,user_id,program_id,alert_type,alert_date,in_app_visible) VALUES($1,$2,$3,'d1',CURRENT_DATE,true)")
        .bind(Uuid::new_v4())
        .bind(other)
        .bind(program_id)
        .execute(&pool)
        .await
        .unwrap();

    let resp = client
        .get(format!("{base}/api/v1/alerts"))
        .bearer_auth(&owner_token)
        .send()
        .await
        .expect("request failed");
    let body: Value = resp.json().await.expect("invalid json");
    assert_eq!(body["unread"], 2, "owner must start with 2 unread: {body}");

    let resp = client
        .post(format!("{base}/api/v1/alerts/read-all"))
        .bearer_auth(&owner_token)
        .send()
        .await
        .expect("request failed");
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.expect("invalid json");
    assert_eq!(body["updated"], 2);

    let resp = client
        .get(format!("{base}/api/v1/alerts"))
        .bearer_auth(&owner_token)
        .send()
        .await
        .expect("request failed");
    let body: Value = resp.json().await.expect("invalid json");
    assert_eq!(body["unread"], 0, "unread must drop to 0: {body}");
    let items = body["items"].as_array().expect("items must be array");
    assert!(
        items.iter().all(|item| !item["read_at"].is_null()),
        "every returned item must now have read_at set: {body}"
    );

    let other_unread: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM alert_deliveries WHERE user_id = $1 AND read_at IS NULL",
    )
    .bind(other)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(other_unread, 1, "read-all must not touch other users' rows");
}

#[tokio::test]
async fn mark_all_read_rejects_unauthenticated() {
    let base = start_test_server().await;
    let client = Client::new();

    let resp = client
        .post(format!("{base}/api/v1/alerts/read-all"))
        .send()
        .await
        .expect("request failed");

    assert_eq!(resp.status(), 401);
}

// ── GET /api/v1/my/applications ──

#[tokio::test]
async fn list_applications_returns_list() {
    let base = start_test_server().await;
    let client = Client::new();
    let (_, token, _) = create_test_user(&base).await;

    let resp = client
        .get(format!("{base}/api/v1/my/applications"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("request failed");

    assert_eq!(resp.status(), 200, "list applications should succeed");

    let body: Value = resp.json().await.expect("invalid json");
    assert!(body["total"].is_number(), "must have total: {body}");
    assert!(body["items"].is_array(), "must have items: {body}");
}

#[tokio::test]
async fn list_applications_rejects_unauthenticated() {
    let base = start_test_server().await;
    let client = Client::new();

    let resp = client
        .get(format!("{base}/api/v1/my/applications"))
        .send()
        .await
        .expect("request failed");

    assert_eq!(resp.status(), 401);
}

// ── PUT /api/v1/my/applications/{id} ──

#[tokio::test]
async fn update_application_works() {
    let base = start_test_server().await;
    let client = Client::new();
    let (_, token, _) = create_test_user(&base).await;

    let programs_resp = client
        .get(format!("{base}/api/v1/programs"))
        .send()
        .await
        .expect("programs request failed");
    let programs_body: Value = programs_resp.json().await.expect("invalid json");
    let items = programs_body["items"]
        .as_array()
        .expect("items must be array");

    if items.is_empty() {
        return;
    }

    let program_id = items[0]["id"].as_str().expect("program must have id");

    // First set an initial state so the row exists
    let _ = client
        .post(format!("{base}/api/v1/programs/{program_id}/state"))
        .bearer_auth(&token)
        .json(&json!({ "state": "interested" }))
        .send()
        .await
        .expect("state setup failed");

    // Now update via PUT /my/applications/{id}
    let resp = client
        .put(format!("{base}/api/v1/my/applications/{program_id}"))
        .bearer_auth(&token)
        .json(&json!({ "status": "applying", "memo": "preparing docs" }))
        .send()
        .await
        .expect("request failed");

    assert_eq!(resp.status(), 200, "update application should succeed");

    let body: Value = resp.json().await.expect("invalid json");
    assert_eq!(body["status"], "applying", "status must be updated");
}

#[tokio::test]
async fn update_application_rejects_invalid_status() {
    let base = start_test_server().await;
    let client = Client::new();
    let (_, token, _) = create_test_user(&base).await;
    let fake_id = Uuid::new_v4();

    let resp = client
        .put(format!("{base}/api/v1/my/applications/{fake_id}"))
        .bearer_auth(&token)
        .json(&json!({ "status": "not_a_real_status" }))
        .send()
        .await
        .expect("request failed");

    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn update_application_rejects_unauthenticated() {
    let base = start_test_server().await;
    let client = Client::new();
    let fake_id = Uuid::new_v4();

    let resp = client
        .put(format!("{base}/api/v1/my/applications/{fake_id}"))
        .json(&json!({ "status": "applying" }))
        .send()
        .await
        .expect("request failed");

    assert_eq!(resp.status(), 401);
}

// ── GET /api/v1/my/saved ──

#[tokio::test]
async fn get_saved_returns_bookmarks() {
    let base = start_test_server().await;
    let client = Client::new();
    let (_, token, _) = create_test_user(&base).await;

    let resp = client
        .get(format!("{base}/api/v1/my/saved"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("request failed");

    assert_eq!(resp.status(), 200, "get saved should succeed");

    let body: Value = resp.json().await.expect("invalid json");
    assert!(body["total"].is_number(), "must have total: {body}");
    assert!(body["items"].is_array(), "must have items: {body}");
}

/// Review 2026-10-21 H1: a saved programme that expired or was retired stays
/// in the list (saved data is never dropped) but says so: `available` is the
/// public catalogue's visibility (`current_programs`, what the detail page
/// serves) and `effective_deadline` the earliest end date.
#[tokio::test]
async fn saved_rows_say_whether_the_programme_is_still_open() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let base = start_test_server().await;
    let client = Client::new();
    let (user, token, _) = create_test_user(&base).await;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .unwrap();
    let (open, expired, retired) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    sqlx::query(
        "INSERT INTO programs(id,program_type,source_type,title,program_status,is_active,
                              deadline_at,application_end_at,benefit_valid_until)
         VALUES ($1,'corporate_benefit','manual','저장 열림',
                 'active',true,now()+interval '5 days',now()+interval '3 days',NULL),
                ($2,'corporate_benefit','manual','저장 만료',
                 'always_open',true,NULL,NULL,now()-interval '1 day'),
                ($3,'policy','manual','저장 종료','closed',true,NULL,NULL,NULL)",
    )
    .bind(open)
    .bind(expired)
    .bind(retired)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO user_bookmarks(user_id,program_id) SELECT $1, unnest($2::uuid[])")
        .bind(user)
        .bind(vec![open, expired, retired])
        .execute(&pool)
        .await
        .unwrap();

    let body: Value = client
        .get(format!("{base}/api/v1/my/saved"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["total"], 3, "{body}");
    let row = |id: Uuid| {
        body["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["id"] == json!(id))
            .cloned()
            .unwrap_or_else(|| panic!("{id} missing: {body}"))
    };
    assert_eq!(row(open)["available"], true);
    assert_eq!(row(expired)["available"], false);
    assert_eq!(row(retired)["available"], false);
    let deadline = |id: Uuid| {
        row(id)["effective_deadline"]
            .as_str()
            .map(|s| chrono::DateTime::parse_from_rfc3339(s).unwrap())
    };
    let open_end = deadline(open).expect("open row has a deadline");
    assert!(
        open_end < Utc::now() + Duration::days(4),
        "earliest of the three"
    );
    assert!(deadline(expired).expect("validity end") < Utc::now());
    assert_eq!(row(retired)["effective_deadline"], Value::Null);
    // The detail page agrees with `available`.
    for (id, status) in [(open, 200), (expired, 404), (retired, 404)] {
        let got = client
            .get(format!("{base}/api/v1/programs/{id}"))
            .send()
            .await
            .unwrap()
            .status();
        assert_eq!(got, status, "{id}");
    }

    sqlx::query("DELETE FROM users WHERE id=$1")
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM programs WHERE id=ANY($1)")
        .bind(vec![open, expired, retired])
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn get_saved_rejects_unauthenticated() {
    let base = start_test_server().await;
    let client = Client::new();

    let resp = client
        .get(format!("{base}/api/v1/my/saved"))
        .send()
        .await
        .expect("request failed");

    assert_eq!(resp.status(), 401);
}

// ── Extractor rejections answer the standard JSON error ──

#[tokio::test]
async fn extractor_rejections_are_generic_json_not_framework_text() {
    let base = start_test_server().await;
    let client = Client::new();
    let (_, token, _) = create_test_user(&base).await;

    let cases = [
        // Path<Uuid> that does not parse.
        (
            client.get(format!("{base}/api/v1/programs/not-a-uuid")),
            400,
        ),
        // Query struct missing a required field.
        (client.get(format!("{base}/api/v1/campus/programs")), 400),
        // Json body of the wrong shape and a missing JSON content type.
        (
            client
                .post(format!("{base}/api/v1/push/register"))
                .bearer_auth(&token)
                .json(&json!({ "token": 1 })),
            422,
        ),
        (
            client
                .post(format!("{base}/api/v1/push/register"))
                .bearer_auth(&token)
                .body("token=x"),
            415,
        ),
    ];
    for (request, status) in cases {
        let resp = request.send().await.expect("request failed");
        assert_eq!(resp.status(), status);
        assert_eq!(
            resp.headers()["content-type"],
            "application/json",
            "status {status}"
        );
        let body = resp.text().await.unwrap();
        let value: Value = serde_json::from_str(&body).expect("JSON error body");
        assert!(value["error"].is_string(), "{body}");
        for leak in ["UUID", "parse", "missing field", "deserialize", "`"] {
            assert!(!body.contains(leak), "framework text leaked: {body}");
        }
    }
}

// ── POST /api/v1/push/register ──

#[tokio::test]
async fn register_push_token_succeeds() {
    let base = start_test_server().await;
    let client = Client::new();
    let (_, token, _) = create_test_user(&base).await;

    let resp = client
        .post(format!("{base}/api/v1/push/register"))
        .bearer_auth(&token)
        .json(&json!({
            "token": format!("ExponentPushToken[test-{}]", Uuid::new_v4()),
            "platform": "ios"
        }))
        .send()
        .await
        .expect("request failed");

    assert_eq!(resp.status(), 200, "push register should succeed");

    let body: Value = resp.json().await.expect("invalid json");
    assert_eq!(body["success"], true);
}

#[tokio::test]
async fn register_push_token_rejects_invalid_platform() {
    let base = start_test_server().await;
    let client = Client::new();
    let (_, token, _) = create_test_user(&base).await;

    let resp = client
        .post(format!("{base}/api/v1/push/register"))
        .bearer_auth(&token)
        .json(&json!({
            "token": "some-token",
            "platform": "windows"
        }))
        .send()
        .await
        .expect("request failed");

    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn register_push_token_rejects_web_platform_instead_of_failing_db_check() {
    let base = start_test_server().await;
    let client = Client::new();
    let (_, token, _) = create_test_user(&base).await;

    let resp = client
        .post(format!("{base}/api/v1/push/register"))
        .bearer_auth(&token)
        .json(&json!({
            "token": format!("ExponentPushToken[web-{}]", Uuid::new_v4()),
            "platform": "web"
        }))
        .send()
        .await
        .expect("request failed");

    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn register_push_token_keeps_only_the_ten_newest_devices() {
    let base = start_test_server().await;
    let client = Client::new();
    let (user_id, token, _) = create_test_user(&base).await;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .unwrap();

    let mut tokens = Vec::new();
    for i in 0..12 {
        let push_token = format!("ExponentPushToken[cap-{i}-{}]", Uuid::new_v4().simple());
        let resp = client
            .post(format!("{base}/api/v1/push/register"))
            .bearer_auth(&token)
            .json(&json!({ "token": push_token, "platform": "android" }))
            .send()
            .await
            .expect("request failed");
        assert_eq!(resp.status(), 200);
        // Each registration is its own transaction, so `updated_at` (set by
        // the row trigger to that transaction's now()) orders the devices.
        tokens.push(push_token);
    }

    let stored: Vec<String> =
        sqlx::query_scalar("SELECT token FROM push_tokens WHERE user_id=$1 ORDER BY updated_at")
            .bind(user_id)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(stored.len(), 10, "{stored:?}");
    assert!(!stored.contains(&tokens[0]), "oldest device evicted");
    assert!(!stored.contains(&tokens[1]), "second oldest device evicted");
    assert!(stored.contains(&tokens[11]), "newest device kept");

    // Re-registering an existing device is not a new slot.
    let resp = client
        .post(format!("{base}/api/v1/push/register"))
        .bearer_auth(&token)
        .json(&json!({ "token": tokens[5], "platform": "ios" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM push_tokens WHERE user_id=$1")
        .bind(user_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 10);
}

#[tokio::test]
async fn register_push_token_moves_another_accounts_token_only_when_stale() {
    let base = start_test_server().await;
    let client = Client::new();
    let (owner_id, owner, _) = create_test_user(&base).await;
    let (other_id, other, _) = create_test_user(&base).await;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .unwrap();
    let push_token = format!("ExponentPushToken[takeover-{}]", Uuid::new_v4().simple());
    let register = |bearer: String| {
        let client = client.clone();
        let url = format!("{base}/api/v1/push/register");
        let body = json!({ "token": push_token, "platform": "ios" });
        async move {
            client
                .post(url)
                .bearer_auth(bearer)
                .json(&body)
                .send()
                .await
                .unwrap()
                .status()
        }
    };
    // The row trigger stamps now() on every UPDATE; age the row with
    // triggers off for this one transaction (the test role owns the schema).
    let age = |pool: sqlx::PgPool, token: String, days: i32| async move {
        let mut tx = pool.begin().await.unwrap();
        sqlx::query("SET LOCAL session_replication_role = replica")
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query("UPDATE push_tokens SET updated_at = now() - make_interval(days => $2) WHERE token = $1")
            .bind(token)
            .bind(days)
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
    };
    let holder = |pool: sqlx::PgPool, token: String| async move {
        sqlx::query_scalar::<_, Uuid>("SELECT user_id FROM push_tokens WHERE token = $1")
            .bind(token)
            .fetch_one(&pool)
            .await
            .unwrap()
    };

    assert_eq!(register(owner.clone()).await, 200);
    // A live token of another account is not taken over.
    assert_eq!(register(other.clone()).await, 409);
    assert_eq!(holder(pool.clone(), push_token.clone()).await, owner_id);
    // The owner re-registering its own device still succeeds.
    assert_eq!(register(owner.clone()).await, 200);
    assert_eq!(holder(pool.clone(), push_token.clone()).await, owner_id);

    // One day short of stale: still refused.
    age(pool.clone(), push_token.clone(), 29).await;
    assert_eq!(register(other.clone()).await, 409);
    assert_eq!(holder(pool.clone(), push_token.clone()).await, owner_id);

    // Not re-registered for longer than the refresh-token lifetime: moves.
    age(pool.clone(), push_token.clone(), 31).await;
    assert_eq!(register(other.clone()).await, 200);
    assert_eq!(holder(pool.clone(), push_token.clone()).await, other_id);
}

#[tokio::test]
async fn register_push_token_rejects_unauthenticated() {
    let base = start_test_server().await;
    let client = Client::new();

    let resp = client
        .post(format!("{base}/api/v1/push/register"))
        .json(&json!({ "token": "some-token", "platform": "ios" }))
        .send()
        .await
        .expect("request failed");

    assert_eq!(resp.status(), 401);
}

// ── DELETE /api/v1/push/register ──

#[tokio::test]
async fn unregister_push_token_succeeds() {
    let base = start_test_server().await;
    let client = Client::new();
    let (_, token, _) = create_test_user(&base).await;

    // Register first so there's something to delete
    let _ = client
        .post(format!("{base}/api/v1/push/register"))
        .bearer_auth(&token)
        .json(&json!({
            "token": format!("ExponentPushToken[del-{}]", Uuid::new_v4()),
            "platform": "android"
        }))
        .send()
        .await
        .expect("register failed");

    let resp = client
        .delete(format!("{base}/api/v1/push/register"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("request failed");

    assert_eq!(resp.status(), 200, "push unregister should succeed");

    let body: Value = resp.json().await.expect("invalid json");
    assert_eq!(body["success"], true);
}

#[tokio::test]
async fn unregister_push_token_with_token_keeps_other_devices() {
    let base = start_test_server().await;
    let client = Client::new();
    let (user, token, _) = create_test_user(&base).await;
    let phone = format!("ExponentPushToken[phone-{}]", Uuid::new_v4());
    let tablet = format!("ExponentPushToken[tablet-{}]", Uuid::new_v4());
    for device in [&phone, &tablet] {
        let resp = client
            .post(format!("{base}/api/v1/push/register"))
            .bearer_auth(&token)
            .json(&json!({ "token": device, "platform": "ios" }))
            .send()
            .await
            .expect("register failed");
        assert_eq!(resp.status(), 200);
    }

    let resp = client
        .delete(format!("{base}/api/v1/push/register"))
        .bearer_auth(&token)
        .json(&json!({ "token": phone }))
        .send()
        .await
        .expect("request failed");
    assert_eq!(resp.status(), 200);

    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&DATABASE_URL)
        .await
        .unwrap();
    let remaining: Vec<String> =
        sqlx::query_scalar("SELECT token FROM push_tokens WHERE user_id = $1")
            .bind(user)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        remaining,
        vec![tablet],
        "logout on one device keeps the others"
    );
}

#[tokio::test]
async fn unregister_push_token_rejects_unauthenticated() {
    let base = start_test_server().await;
    let client = Client::new();

    let resp = client
        .delete(format!("{base}/api/v1/push/register"))
        .send()
        .await
        .expect("request failed");

    assert_eq!(resp.status(), 401);
}

// ── GET /api/v1/admin/stats ──

#[tokio::test]
async fn admin_stats_returns_counts() {
    let base = start_test_server().await;
    let client = Client::new();
    let (_, admin_token) = create_test_admin(&base).await;

    let resp = client
        .get(format!("{base}/api/v1/admin/stats"))
        .bearer_auth(&admin_token)
        .send()
        .await
        .expect("request failed");

    assert_eq!(resp.status(), 200, "admin stats should succeed");

    let body: Value = resp.json().await.expect("invalid json");
    assert!(
        body["total_programs"].is_number(),
        "must have total_programs: {body}"
    );
    assert!(
        body["active_programs"].is_number(),
        "must have active_programs: {body}"
    );
    assert!(
        body["total_users"].is_number(),
        "must have total_users: {body}"
    );
}

#[tokio::test]
async fn admin_stats_rejects_regular_user() {
    let base = start_test_server().await;
    let client = Client::new();
    let (_, token, _) = create_test_user(&base).await;

    let resp = client
        .get(format!("{base}/api/v1/admin/stats"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("request failed");

    assert_eq!(
        resp.status(),
        403,
        "regular user must not access admin stats"
    );
}

#[tokio::test]
async fn admin_stats_rejects_unauthenticated() {
    let base = start_test_server().await;
    let client = Client::new();

    let resp = client
        .get(format!("{base}/api/v1/admin/stats"))
        .send()
        .await
        .expect("request failed");

    assert_eq!(resp.status(), 401);
}

// ── GET /api/v1/admin/application-routing-stats ──

#[tokio::test]
async fn admin_application_routing_stats_counts_destinations_and_channels() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let base = start_test_server().await;
    let client = Client::new();
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .unwrap();
    let (_, admin_token) = create_test_admin(&base).await;
    let (_, user_token, _) = create_test_user(&base).await;
    let ids: Vec<Uuid> = (0..2).map(|_| Uuid::new_v4()).collect();
    sqlx::query("INSERT INTO programs(id,program_type,source_type,title,program_status,is_active,official_url,application_method) VALUES($1,'welfare','manual','routing stats information','active',true,'https://www.gov.kr/portal/rcvfvrSvc/dtlEx/1','정부24온라인신청||방문신청'),($2,'scholarship','manual','routing stats unknown','active',true,'https://provider.example.org/apply','홈페이지 또는 QR코드')")
        .bind(ids[0]).bind(ids[1]).execute(&pool).await.unwrap();

    let before: Value = client
        .get(format!("{base}/api/v1/admin/application-routing-stats"))
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(before["programs"].as_u64().unwrap() >= 2, "{before}");
    assert!(
        before["destination"]["by_kind"]["information"]
            .as_u64()
            .unwrap()
            >= 1
    );
    assert!(
        before["destination"]["by_kind"]["unverified_page"]
            .as_u64()
            .unwrap()
            >= 1
    );
    assert!(before["destination"]["by_kind_platform"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["kind"] == "information" && row["platform"] == "government24"));
    assert!(before["workflow"]["by_channel"]["online"].as_u64().unwrap() >= 1);
    assert!(before["workflow"]["by_channel"]["visit"].as_u64().unwrap() >= 1);
    assert!(before["workflow"]["unknown_programs"].as_u64().unwrap() >= 1);
    let ratio = before["workflow"]["unknown_ratio"].as_f64().unwrap();
    assert!((0.0..=1.0).contains(&ratio), "{before}");
    // Counts only: no program URL or title is echoed back.
    let text = before.to_string();
    assert!(!text.contains("provider.example.org/apply"));
    assert!(!text.contains("routing stats"));

    assert_eq!(
        client
            .get(format!("{base}/api/v1/admin/application-routing-stats"))
            .bearer_auth(&user_token)
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        client
            .get(format!("{base}/api/v1/admin/application-routing-stats"))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    sqlx::query("DELETE FROM programs WHERE id=ANY($1)")
        .bind(&ids)
        .execute(&pool)
        .await
        .unwrap();
}

// ── GET /api/v1/admin/program-maintenance ──

#[tokio::test]
async fn admin_program_maintenance_returns_operator_queues() {
    let base = start_test_server().await;
    let client = Client::new();
    let (_, admin_token) = create_test_admin(&base).await;

    let resp = client
        .get(format!("{base}/api/v1/admin/program-maintenance?limit=5"))
        .bearer_auth(&admin_token)
        .send()
        .await
        .expect("request failed");

    assert_eq!(resp.status(), 200, "maintenance queues should succeed");
    let body: Value = resp.json().await.expect("invalid json");
    assert_eq!(body["limit"], 5);
    for queue in ["expired", "expiring", "duplicates", "source_reviews"] {
        assert!(body[queue]["total"].is_number(), "missing {queue} count");
        assert!(body[queue]["items"].is_array(), "missing {queue} items");
    }
}

#[tokio::test]
async fn admin_program_maintenance_rejects_regular_user() {
    let base = start_test_server().await;
    let client = Client::new();
    let (_, token, _) = create_test_user(&base).await;

    let resp = client
        .get(format!("{base}/api/v1/admin/program-maintenance"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("request failed");

    assert_eq!(resp.status(), 403);
}

// ── GET /api/v1/admin/programs ──

#[tokio::test]
async fn admin_list_programs_returns_list() {
    let base = start_test_server().await;
    let client = Client::new();
    let (_, admin_token) = create_test_admin(&base).await;

    let resp = client
        .get(format!("{base}/api/v1/admin/programs"))
        .bearer_auth(&admin_token)
        .send()
        .await
        .expect("request failed");

    assert_eq!(resp.status(), 200, "admin list programs should succeed");

    let body: Value = resp.json().await.expect("invalid json");
    assert!(body["total"].is_number(), "must have total: {body}");
    assert!(body["items"].is_array(), "must have items: {body}");
}

#[tokio::test]
async fn admin_get_program_shows_hidden_listing_in_public_detail_shape() {
    let base = start_test_server().await;
    let client = Client::new();
    let (_, admin_token) = create_test_admin(&base).await;
    let (_, user_token, _) = create_test_user(&base).await;

    // Admin-created programs start unpublished (is_active=false).
    let created: Value = client
        .post(format!("{base}/api/v1/admin/programs"))
        .bearer_auth(&admin_token)
        .json(&json!({
            "program_type": "support",
            "title": "비공개 편집 테스트",
            "application_end_at": "2020-01-31T23:59:59Z",
            "regions": ["busan"]
        }))
        .send()
        .await
        .expect("create failed")
        .json()
        .await
        .expect("json");
    let id = created["id"].as_str().expect("created id").to_string();

    // Public detail hides it (inactive and expired) …
    assert_eq!(
        client
            .get(format!("{base}/api/v1/programs/{id}"))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    // … the admin route still serves it, in the same shape as the public detail.
    let resp = client
        .get(format!("{base}/api/v1/admin/programs/{id}"))
        .bearer_auth(&admin_token)
        .send()
        .await
        .expect("admin get failed");
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["id"], id);
    assert_eq!(body["is_active"], false);
    assert_eq!(body["title"], "비공개 편집 테스트");
    for key in [
        "eligibility_rules",
        "required_documents",
        "brands",
        "application_policy",
        "benefit_conditions",
    ] {
        assert!(body.get(key).is_some(), "missing {key}: {body}");
    }
    assert!(
        body.get("raw_payload").is_none(),
        "provenance never leaves the API"
    );

    assert_eq!(
        client
            .get(format!("{base}/api/v1/admin/programs/{id}"))
            .bearer_auth(&user_token)
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        client
            .get(format!("{base}/api/v1/admin/programs/{id}"))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        client
            .get(format!("{base}/api/v1/admin/programs/{}", Uuid::new_v4()))
            .bearer_auth(&admin_token)
            .send()
            .await
            .unwrap()
            .status(),
        404
    );

    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&DATABASE_URL)
        .await
        .unwrap();
    sqlx::query("DELETE FROM programs WHERE id=$1::uuid")
        .bind(&id)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn admin_list_programs_rejects_regular_user() {
    let base = start_test_server().await;
    let client = Client::new();
    let (_, token, _) = create_test_user(&base).await;

    let resp = client
        .get(format!("{base}/api/v1/admin/programs"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("request failed");

    assert_eq!(resp.status(), 403);
}

#[tokio::test]
async fn admin_program_writes_enforce_data_quality_contract() {
    let base = start_test_server().await;
    let client = Client::new();
    let (_, admin_token) = create_test_admin(&base).await;

    let invalid_date = client
        .post(format!("{base}/api/v1/admin/programs"))
        .bearer_auth(&admin_token)
        .json(&json!({
            "program_type": "support",
            "title": "잘못된 날짜",
            "application_start_at": "tomorrow"
        }))
        .send()
        .await
        .expect("invalid date request failed");
    assert_eq!(invalid_date.status(), 400);

    let negative_amount = client
        .post(format!("{base}/api/v1/admin/programs"))
        .bearer_auth(&admin_token)
        .json(&json!({
            "program_type": "support",
            "title": "잘못된 금액",
            "benefit_amount_once": -1
        }))
        .send()
        .await
        .expect("negative amount request failed");
    assert_eq!(negative_amount.status(), 400);

    let unknown_region = client
        .post(format!("{base}/api/v1/admin/programs"))
        .bearer_auth(&admin_token)
        .json(&json!({
            "program_type": "support",
            "title": "잘못된 지역",
            "regions": ["해운대구"]
        }))
        .send()
        .await
        .expect("unknown region request failed");
    assert_eq!(unknown_region.status(), 400);

    let valid = client
        .post(format!("{base}/api/v1/admin/programs"))
        .bearer_auth(&admin_token)
        .json(&json!({
            "program_type": "support",
            "title": "데이터 품질 테스트",
            "application_start_at": "2026-08-01T00:00:00Z",
            "application_end_at": "2026-08-31T23:59:59Z",
            "benefit_amount_once": 10000,
            "min_age": 19,
            "max_age": 34,
            "regions": ["부산광역시", "BUSAN"],
            "is_active": false
        }))
        .send()
        .await
        .expect("valid admin program request failed");
    assert_eq!(valid.status(), 200);
    let body: Value = valid.json().await.expect("valid program response json");
    assert_eq!(body["regions"], json!(["busan"]));
    let program_id = Uuid::parse_str(body["id"].as_str().expect("program id")).unwrap();

    let invalid_update = client
        .put(format!("{base}/api/v1/admin/programs/{program_id}"))
        .bearer_auth(&admin_token)
        .json(&json!({ "application_end_at": "not-rfc3339" }))
        .send()
        .await
        .expect("invalid update request failed");
    assert_eq!(invalid_update.status(), 400);

    // program_type is a category key compared exactly by the public filter
    // and both clients' tabs: only the canonical list is accepted.
    for (method, url) in [
        ("post", format!("{base}/api/v1/admin/programs")),
        ("put", format!("{base}/api/v1/admin/programs/{program_id}")),
    ] {
        let request = if method == "post" {
            client.post(&url)
        } else {
            client.put(&url)
        };
        let resp = request
            .bearer_auth(&admin_token)
            .json(&json!({ "program_type": "coupon", "title": "알 수 없는 분류" }))
            .send()
            .await
            .expect("unknown program_type request failed");
        assert_eq!(resp.status(), 400, "{method}");
        let body: Value = resp.json().await.unwrap();
        assert!(
            body["error"].as_str().unwrap().contains("scholarship"),
            "the error names the allowed keys: {body}"
        );
    }

    // official_url is opened by both clients: only a credential-free public
    // HTTPS URL is stored, on create and on update.
    for bad in [
        "http://www.gov.kr/notice",
        "javascript:alert(1)",
        "https://user:pw@www.gov.kr/",
        "https://www.gov.kr:8443/",
        "https://127.0.0.1/admin",
        "https://localhost/notice",
        "notice page",
    ] {
        let create = client
            .post(format!("{base}/api/v1/admin/programs"))
            .bearer_auth(&admin_token)
            .json(
                &json!({ "program_type": "support", "title": "잘못된 링크", "official_url": bad }),
            )
            .send()
            .await
            .expect("bad official_url create failed");
        assert_eq!(create.status(), 400, "create {bad}");
        let update = client
            .put(format!("{base}/api/v1/admin/programs/{program_id}"))
            .bearer_auth(&admin_token)
            .json(&json!({ "official_url": bad }))
            .send()
            .await
            .expect("bad official_url update failed");
        assert_eq!(update.status(), 400, "update {bad}");
    }
    let with_url: Value = client
        .put(format!("{base}/api/v1/admin/programs/{program_id}"))
        .bearer_auth(&admin_token)
        .json(&json!({ "official_url": " https://www.x.go.kr/board/view.do?keyword=%C3%BB%B3%E2&board&no=3#/apply/12 " }))
        .send()
        .await
        .expect("official_url update failed")
        .json()
        .await
        .unwrap();
    // Code review M5: the trimmed original is stored; canonicalization only
    // validates. An SPA fragment, EUC-KR query bytes and a bare key survive
    // (the canonical form would be `keyword=%EF%BF%BD..&board=` without `#`).
    assert_eq!(
        with_url["official_url"],
        "https://www.x.go.kr/board/view.do?keyword=%C3%BB%B3%E2&board&no=3#/apply/12"
    );

    // benefit_valid_until is the third input of the effective deadline and
    // must round-trip through the admin API; the detail exposes the minimum.
    let with_validity: Value = client
        .put(format!("{base}/api/v1/admin/programs/{program_id}"))
        .bearer_auth(&admin_token)
        .json(&json!({ "benefit_valid_until": "2026-08-15T14:59:59Z" }))
        .send()
        .await
        .expect("validity update failed")
        .json()
        .await
        .unwrap();
    assert_eq!(with_validity["benefit_valid_until"], "2026-08-15T14:59:59Z");
    assert_eq!(with_validity["application_end_at"], "2026-08-31T23:59:59Z");
    let detail: Value = client
        .get(format!("{base}/api/v1/admin/programs/{program_id}"))
        .bearer_auth(&admin_token)
        .send()
        .await
        .expect("admin detail failed")
        .json()
        .await
        .unwrap();
    assert_eq!(
        detail["effective_deadline"], "2026-08-15T14:59:59Z",
        "earliest of the three deadline columns: {detail}"
    );

    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .expect("test DB cleanup connection");

    // Publishing clears a retirement stamped by automation; unpublishing is
    // an operator hold that automation never reverses.
    sqlx::query(
        "UPDATE programs SET retired_at = now(), retirement_reason = 'expired' WHERE id = $1",
    )
    .bind(program_id)
    .execute(&pool)
    .await
    .unwrap();
    let retirement = |pool: sqlx::PgPool| async move {
        sqlx::query_as::<_, (bool, Option<String>, bool)>(
            "SELECT is_active, retirement_reason, retired_at IS NOT NULL FROM programs WHERE id = $1",
        )
        .bind(program_id)
        .fetch_one(&pool)
        .await
        .unwrap()
    };
    let publish = || {
        client
            .post(format!("{base}/api/v1/admin/programs/{program_id}/publish"))
            .bearer_auth(&admin_token)
            .send()
    };
    assert_eq!(publish().await.unwrap().status(), 200);
    assert_eq!(retirement(pool.clone()).await, (true, None, false));
    assert_eq!(publish().await.unwrap().status(), 200);
    assert_eq!(
        retirement(pool.clone()).await,
        (false, Some("manual".to_string()), true)
    );
    let republished = client
        .put(format!("{base}/api/v1/admin/programs/{program_id}"))
        .bearer_auth(&admin_token)
        .json(&json!({ "is_active": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(republished.status(), 200);
    assert_eq!(retirement(pool.clone()).await, (true, None, false));
    let unpublished = client
        .put(format!("{base}/api/v1/admin/programs/{program_id}"))
        .bearer_auth(&admin_token)
        .json(&json!({ "is_active": false }))
        .send()
        .await
        .unwrap();
    assert_eq!(unpublished.status(), 200);
    assert_eq!(
        retirement(pool.clone()).await,
        (false, Some("manual".to_string()), true)
    );

    sqlx::query("DELETE FROM programs WHERE id = $1")
        .bind(program_id)
        .execute(&pool)
        .await
        .expect("test program cleanup");
}

#[tokio::test]
async fn operational_and_data_quality_queues_have_auditable_lifecycles() {
    let base = start_test_server().await;
    let client = Client::new();
    let (_, admin_token) = create_test_admin(&base).await;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .expect("test DB connection");
    let unique = Uuid::new_v4().simple().to_string();
    let fingerprint = format!("test_operation:{unique}");

    for occurrence in 1..=2 {
        mazimi_worker::operations::record_event(
            &pool,
            &mazimi_worker::operations::EventCommand {
                fingerprint: &fingerprint,
                severity: mazimi_worker::operations::Severity::Warning,
                component: "integration_test",
                event_type: "test_incident",
                subject_type: Some("test"),
                subject_id: Some(&unique),
                message: "Integration-test operator incident",
                details: json!({ "occurrence": occurrence }),
            },
        )
        .await
        .expect("record operational event");
    }

    let listed = client
        .get(format!(
            "{base}/api/v1/admin/operational-events?status=open&limit=100"
        ))
        .bearer_auth(&admin_token)
        .send()
        .await
        .expect("operational queue request");
    assert_eq!(listed.status(), 200);
    let listed_body: Value = listed.json().await.expect("operational queue json");
    let event = listed_body["items"]
        .as_array()
        .and_then(|items| items.iter().find(|item| item["fingerprint"] == fingerprint))
        .expect("deduplicated event must be listed");
    assert_eq!(event["occurrence_count"], 2);
    assert_eq!(event["details"]["occurrence"], 2);
    let event_id = Uuid::parse_str(event["id"].as_str().expect("event id")).unwrap();

    let missing_note = client
        .put(format!("{base}/api/v1/admin/operational-events/{event_id}"))
        .bearer_auth(&admin_token)
        .json(&json!({ "action": "resolve" }))
        .send()
        .await
        .expect("missing resolution note request");
    assert_eq!(missing_note.status(), 400);

    let acknowledged = client
        .put(format!("{base}/api/v1/admin/operational-events/{event_id}"))
        .bearer_auth(&admin_token)
        .json(&json!({ "action": "acknowledge", "note": "담당자 확인" }))
        .send()
        .await
        .expect("acknowledge event request");
    assert_eq!(acknowledged.status(), 200);
    let acknowledged_body: Value = acknowledged.json().await.expect("ack event json");
    assert_eq!(acknowledged_body["status"], "acknowledged");

    let resolved = client
        .put(format!("{base}/api/v1/admin/operational-events/{event_id}"))
        .bearer_auth(&admin_token)
        .json(&json!({
            "action": "resolve",
            "note": "통합 테스트에서 원인 제거 확인"
        }))
        .send()
        .await
        .expect("resolve event request");
    assert_eq!(resolved.status(), 200);
    let resolved_body: Value = resolved.json().await.expect("resolved event json");
    assert_eq!(resolved_body["status"], "resolved");

    mazimi_worker::operations::record_event(
        &pool,
        &mazimi_worker::operations::EventCommand {
            fingerprint: &fingerprint,
            severity: mazimi_worker::operations::Severity::Error,
            component: "integration_test",
            event_type: "test_incident_recurred",
            subject_type: Some("test"),
            subject_id: Some(&unique),
            message: "Integration-test incident recurred",
            details: json!({ "occurrence": 3 }),
        },
    )
    .await
    .expect("reopen recurring event");
    let recurring: (String, i32, Option<chrono::DateTime<Utc>>) = sqlx::query_as(
        "SELECT status, occurrence_count, delivered_at
         FROM operational_events WHERE id = $1",
    )
    .bind(event_id)
    .fetch_one(&pool)
    .await
    .expect("recurring event query");
    assert_eq!(recurring.0, "open");
    assert_eq!(recurring.1, 3);
    assert!(recurring.2.is_none());

    let program = client
        .post(format!("{base}/api/v1/admin/programs"))
        .bearer_auth(&admin_token)
        .json(&json!({
            "program_type": "support",
            "title": format!("품질 큐 테스트 {unique}"),
            "min_age": 19,
            "max_age": 34,
            "is_active": false
        }))
        .send()
        .await
        .expect("data quality program request");
    assert_eq!(program.status(), 200);
    let program_body: Value = program.json().await.expect("quality program json");
    let program_id = Uuid::parse_str(program_body["id"].as_str().expect("program id")).unwrap();
    let issue_id = sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO program_data_quality_issues (
             program_id, issue_code, original_value
         ) VALUES ($1, 'invalid_age_range', $2) RETURNING id",
    )
    .bind(program_id)
    .bind(json!({ "min_age": -1, "max_age": 34 }))
    .fetch_one(&pool)
    .await
    .expect("quality issue fixture insert");

    let rechecked = client
        .put(format!(
            "{base}/api/v1/admin/data-quality-issues/{issue_id}"
        ))
        .bearer_auth(&admin_token)
        .json(&json!({ "action": "recheck", "note": "프로그램 값 수정 확인" }))
        .send()
        .await
        .expect("quality issue recheck request");
    assert_eq!(rechecked.status(), 200);
    let quality_status = sqlx::query_scalar::<_, String>(
        "SELECT status FROM program_data_quality_issues WHERE id = $1",
    )
    .bind(issue_id)
    .fetch_one(&pool)
    .await
    .expect("quality issue status query");
    assert_eq!(quality_status, "resolved");

    sqlx::query("DELETE FROM operational_events WHERE id = $1")
        .bind(event_id)
        .execute(&pool)
        .await
        .expect("operational event cleanup");
    sqlx::query("DELETE FROM programs WHERE id = $1")
        .bind(program_id)
        .execute(&pool)
        .await
        .expect("quality program cleanup");
}

// ── POST /api/v1/admin/sync ──

#[tokio::test]
async fn admin_sync_returns_started() {
    let base = start_test_server().await;
    let client = Client::new();
    let (_, admin_token) = create_test_admin(&base).await;

    let resp = client
        .post(format!("{base}/api/v1/admin/sync"))
        .bearer_auth(&admin_token)
        .send()
        .await
        .expect("request failed");

    assert_eq!(resp.status(), 200, "admin sync should succeed");

    let body: Value = resp.json().await.expect("invalid json");
    assert_eq!(
        body["status"], "sync_started",
        "status must be sync_started: {body}"
    );
}

#[tokio::test]
async fn admin_sync_rejects_regular_user() {
    let base = start_test_server().await;
    let client = Client::new();
    let (_, token, _) = create_test_user(&base).await;

    let resp = client
        .post(format!("{base}/api/v1/admin/sync"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("request failed");

    assert_eq!(resp.status(), 403);
}

#[tokio::test]
async fn admin_sync_rejects_unauthenticated() {
    let base = start_test_server().await;
    let client = Client::new();

    let resp = client
        .post(format!("{base}/api/v1/admin/sync"))
        .send()
        .await
        .expect("request failed");

    assert_eq!(resp.status(), 401);
}

// ── Stack Check tests ──────────────────────────────────────────────────────

#[tokio::test]
async fn stack_check_returns_results_for_valid_ids() {
    let base = start_test_server().await;
    let client = Client::new();

    // Get two program IDs from the listing
    let resp = client
        .get(format!("{base}/api/v1/programs"))
        .send()
        .await
        .expect("request failed");
    let body: Value = resp.json().await.expect("json");
    let items = body["items"].as_array().expect("items");
    if items.len() < 2 {
        return; // not enough seed data
    }
    let id1 = items[0]["id"].as_str().unwrap();
    let id2 = items[1]["id"].as_str().unwrap();

    let resp = client
        .post(format!("{base}/api/v1/stack-check"))
        .json(&json!({ "program_ids": [id1, id2] }))
        .send()
        .await
        .expect("request failed");

    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.expect("json");
    assert!(body["pairs"].is_array());
    assert!(body["all_stackable"].is_boolean());
}

#[tokio::test]
async fn stack_check_rejects_single_id() {
    let base = start_test_server().await;
    let client = Client::new();

    let resp = client
        .post(format!("{base}/api/v1/stack-check"))
        .json(&json!({ "program_ids": ["00000000-0000-0000-0000-000000000001"] }))
        .send()
        .await
        .expect("request failed");

    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn stack_check_rejects_too_many_ids() {
    let base = start_test_server().await;
    let client = Client::new();

    let ids: Vec<String> = (0..21)
        .map(|i| format!("00000000-0000-0000-0000-{:012}", i))
        .collect();

    let resp = client
        .post(format!("{base}/api/v1/stack-check"))
        .json(&json!({ "program_ids": ids }))
        .send()
        .await
        .expect("request failed");

    assert_eq!(resp.status(), 400);
}

// ── Payment tests ──────────────────────────────────────────────────────────

#[tokio::test]
async fn payment_verify_rejects_empty_payment_id() {
    let base = start_test_server().await;
    let client = Client::new();
    let (_uid, token, _) = create_test_user(&base).await;

    let resp = client
        .post(format!("{base}/api/v1/payments/verify"))
        .bearer_auth(&token)
        .json(&json!({
            "payment_id": "",
            "amount": 4900,
            "product_type": "premium_monthly",
        }))
        .send()
        .await
        .expect("request failed");

    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn payment_verify_rejects_wrong_amount() {
    let base = start_test_server().await;
    let client = Client::new();
    let (_uid, token, _) = create_test_user(&base).await;

    let resp = client
        .post(format!("{base}/api/v1/payments/verify"))
        .bearer_auth(&token)
        .json(&json!({
            "payment_id": "test_pay_999",
            "amount": 1,
            "product_type": "premium_monthly",
        }))
        .send()
        .await
        .expect("request failed");

    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn payment_verify_rejects_unknown_product() {
    let base = start_test_server().await;
    let client = Client::new();
    let (_uid, token, _) = create_test_user(&base).await;

    let resp = client
        .post(format!("{base}/api/v1/payments/verify"))
        .bearer_auth(&token)
        .json(&json!({
            "payment_id": "test_pay_000",
            "amount": 4900,
            "product_type": "nonexistent_plan",
        }))
        .send()
        .await
        .expect("request failed");

    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn payment_status_returns_inactive_for_new_user() {
    let base = start_test_server().await;
    let client = Client::new();
    let (_uid, token, _) = create_test_user(&base).await;

    let resp = client
        .get(format!("{base}/api/v1/payments/status"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("request failed");

    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.expect("json");
    assert_eq!(body["active"], false);
}

#[tokio::test]
async fn payment_rejects_unauthenticated() {
    let base = start_test_server().await;
    let client = Client::new();

    let resp = client
        .post(format!("{base}/api/v1/payments/verify"))
        .json(&json!({
            "payment_id": "test",
            "amount": 4900,
            "product_type": "premium_monthly",
        }))
        .send()
        .await
        .expect("request failed");

    assert_eq!(resp.status(), 401);
}

// ── Compliance-gated corporate benefit discovery ──

#[tokio::test]
async fn corporate_benefit_requires_evidence_and_human_review() {
    let _catalog_guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let base = start_test_server().await;
    let client = Client::new();
    let (admin_id, admin_token) = create_test_admin(&base).await;
    let unique = Uuid::new_v4().simple().to_string();
    let domain = format!("benefits-{unique}.example.com");

    let response = client
        .post(format!("{base}/api/v1/admin/benefit-source-domains"))
        .bearer_auth(&admin_token)
        .json(&json!({
            "organization_name": "Example Benefits",
            "domain": domain,
            "source_role": "brand_official",
            "verification_reference": "https://example.com/official-domain-directory",
            "is_active": true
        }))
        .send()
        .await
        .expect("domain registration request failed");
    assert_eq!(response.status(), 200);
    let domain_body: Value = response.json().await.expect("domain response json");
    let domain_id = Uuid::parse_str(domain_body["id"].as_str().unwrap()).unwrap();
    assert_eq!(domain_body["crawl_allowed"], false);
    assert_eq!(domain_body["compliance_status"], "pending");

    // The database invariant also prevents bypassing the dedicated approval
    // endpoint with a direct boolean update.
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .expect("test DB connection");
    let bypass =
        sqlx::query("UPDATE benefit_source_domains SET crawl_allowed = true WHERE id = $1")
            .bind(domain_id)
            .execute(&pool)
            .await;
    assert!(bypass.is_err(), "crawl approval DB invariant was bypassed");

    let expires_at = (Utc::now() + Duration::days(30)).to_rfc3339();
    let no_preflight = client
        .put(format!(
            "{base}/api/v1/admin/benefit-source-domains/{domain_id}/approval"
        ))
        .bearer_auth(&admin_token)
        .json(&json!({
            "approval_basis": "terms_explicit",
            "approval_reference": "missing-preflight",
            "terms_url": "https://example.com/terms",
            "allowed_path_prefixes": ["/events"],
            "crawler_contact_url": "https://mazimi.kr/bot",
            "approval_expires_at": expires_at.clone()
        }))
        .send()
        .await
        .expect("missing preflight approval request failed");
    assert_eq!(no_preflight.status(), 400);

    sqlx::query(
        "UPDATE benefit_source_domains SET
             robots_url = 'https://' || domain || '/robots.txt',
             robots_status = 'allowed', robots_checked_at = now(),
             robots_content_hash = $2,
             robots_reviewed_path_prefixes = ARRAY['/events']::TEXT[]
         WHERE id = $1",
    )
    .bind(domain_id)
    .bind("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
    .execute(&pool)
    .await
    .expect("robots preflight fixture update");

    let restricted = client
        .put(format!(
            "{base}/api/v1/admin/benefit-source-domains/{domain_id}/approval"
        ))
        .bearer_auth(&admin_token)
        .json(&json!({
            "approval_basis": "public_facts_review",
            "approval_reference": "compliance-ticket-restricted",
            "terms_url": "https://example.com/terms",
            "terms_review_outcome": "restricted",
            "collection_mode": "direct_public",
            "allowed_path_prefixes": ["/events"],
            "crawler_contact_url": "https://mazimi.kr/bot",
            "approval_expires_at": expires_at.clone()
        }))
        .send()
        .await
        .expect("restricted approval request failed");
    assert_eq!(restricted.status(), 400);

    let response = client
        .put(format!(
            "{base}/api/v1/admin/benefit-source-domains/{domain_id}/approval"
        ))
        .bearer_auth(&admin_token)
        .json(&json!({
            "approval_basis": "terms_explicit",
            "approval_reference": "compliance-ticket-2026-001",
            "terms_url": "https://example.com/terms",
            "allowed_path_prefixes": ["/events"],
            "allow_subdomains": false,
            "min_request_interval_ms": 10000,
            "crawler_contact_url": "https://mazimi.kr/bot",
            "approval_expires_at": expires_at
        }))
        .send()
        .await
        .expect("domain approval request failed");
    assert_eq!(response.status(), 200);
    let approval_body: Value = response.json().await.expect("approval response json");
    assert_eq!(approval_body["crawl_allowed"], true);
    assert_eq!(approval_body["compliance_status"], "approved");
    assert_eq!(
        approval_body["compliance_reviewed_by"],
        json!(admin_id.to_string())
    );

    let restricted_bypass = sqlx::query(
        "UPDATE benefit_source_domains
         SET terms_review_outcome = 'restricted'
         WHERE id = $1",
    )
    .bind(domain_id)
    .execute(&pool)
    .await;
    assert!(
        restricted_bypass.is_err(),
        "restricted terms must invalidate direct collection at the DB boundary"
    );
    let scope_bypass = sqlx::query(
        "UPDATE benefit_source_domains
         SET allowed_path_prefixes = ARRAY['/unreviewed']::TEXT[]
         WHERE id = $1",
    )
    .bind(domain_id)
    .execute(&pool)
    .await;
    assert!(
        scope_bypass.is_err(),
        "crawl paths must stay within the latest robots preflight scope"
    );
    let first_slot = sqlx::query(
        "UPDATE benefit_source_domains
         SET next_request_at = now() + make_interval(secs => 10)
         WHERE id = $1 AND (next_request_at IS NULL OR next_request_at <= now())",
    )
    .bind(domain_id)
    .execute(&pool)
    .await
    .expect("first domain request-slot reservation");
    let second_slot = sqlx::query(
        "UPDATE benefit_source_domains
         SET next_request_at = now() + make_interval(secs => 10)
         WHERE id = $1 AND (next_request_at IS NULL OR next_request_at <= now())",
    )
    .bind(domain_id)
    .execute(&pool)
    .await
    .expect("second domain request-slot reservation");
    assert_eq!(first_slot.rows_affected(), 1);
    assert_eq!(
        second_slot.rows_affected(),
        0,
        "concurrent workers must not reserve the same domain request window"
    );
    sqlx::query("UPDATE benefit_source_domains SET next_request_at = NULL WHERE id = $1")
        .bind(domain_id)
        .execute(&pool)
        .await
        .expect("request-slot fixture reset");

    let outside = client
        .post(format!("{base}/api/v1/admin/benefit-candidates"))
        .bearer_auth(&admin_token)
        .json(&json!({
            "url": format!("https://{domain}/account/private"),
            "discovered_via": "manual"
        }))
        .send()
        .await
        .expect("outside-scope queue request failed");
    assert_eq!(outside.status(), 400);

    let source_url = format!("https://{domain}/events/youth-discount");
    let queued = client
        .post(format!("{base}/api/v1/admin/benefit-candidates"))
        .bearer_auth(&admin_token)
        .json(&json!({
            "url": source_url,
            "discovered_via": "manual"
        }))
        .send()
        .await
        .expect("candidate queue request failed");
    assert_eq!(queued.status(), 200);
    let queued_body: Value = queued.json().await.expect("candidate response json");
    let candidate_id = Uuid::parse_str(queued_body["id"].as_str().unwrap()).unwrap();

    // Avoid an external network request in the integration test: emulate the
    // fetcher's admin-only review output, then exercise the real review API.
    sqlx::query(
        "UPDATE benefit_source_candidates
         SET status = 'review', page_title = '청년 할인',
             raw_text = '만 19~34세 대상 20% 할인, 2026년 8월 31일까지'
         WHERE id = $1",
    )
    .bind(candidate_id)
    .execute(&pool)
    .await
    .expect("candidate review fixture update");

    let reviewed = client
        .put(format!(
            "{base}/api/v1/admin/benefit-candidates/{candidate_id}/review"
        ))
        .bearer_auth(&admin_token)
        .json(&json!({
            "decision": "approve",
            "title": "Example 청년 20% 할인",
            "summary": "만 19~34세 고객이 행사 기간에 이용할 수 있는 20% 할인 혜택입니다.",
            "provider_name": "Example Benefits",
            "benefit_category": "discount",
            "program_status": "active",
            "application_end_at": (Utc::now() + Duration::days(30)).to_rfc3339(),
            "deadline_at": (Utc::now() + Duration::days(30)).to_rfc3339(),
            "benefit_amount_once": 20000,
            "min_age": 19,
            "max_age": 34,
            "regions": [],
            "tags": ["청년", "할인"],
            "publish": false
        }))
        .send()
        .await
        .expect("candidate review request failed");
    assert_eq!(reviewed.status(), 200);
    let review_body: Value = reviewed.json().await.expect("review response json");
    let program_id = Uuid::parse_str(review_body["program_id"].as_str().unwrap()).unwrap();
    assert_eq!(review_body["published"], false);

    let retained: (Option<String>, Option<Value>, Option<Value>, bool) = sqlx::query_as(
        "SELECT c.raw_text, p.raw_payload, p.normalized_payload, p.is_active
         FROM benefit_source_candidates c
         JOIN programs p ON p.id = c.program_id
         WHERE c.id = $1",
    )
    .bind(candidate_id)
    .fetch_one(&pool)
    .await
    .expect("review retention query");
    assert!(retained.0.is_none(), "review source text must be deleted");
    assert!(
        retained.1.is_none(),
        "source payload must not be copied to program"
    );
    assert!(
        retained.2.is_none(),
        "normalized source payload must stay private/empty"
    );
    assert!(!retained.3, "reviewed benefits must not auto-publish");
    // Review 2026-10-21 H2: a corporate offer's amounts are no support money
    // (`pays_support`); the approval stores none, as the worker's publish.
    let amounts: (Option<i32>, Option<i32>, Option<i32>) = sqlx::query_as(
        "SELECT benefit_amount_monthly, benefit_amount_semester, benefit_amount_once
         FROM programs WHERE id = $1",
    )
    .bind(program_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(amounts, (None, None, None));

    let hidden_draft = client
        .get(format!("{base}/api/v1/programs/{program_id}"))
        .send()
        .await
        .expect("public program request failed");
    assert_eq!(
        hidden_draft.status(),
        404,
        "publish=false benefits must not be reachable from the public API"
    );

    let publish = client
        .post(format!("{base}/api/v1/admin/programs/{program_id}/publish"))
        .bearer_auth(&admin_token)
        .send()
        .await
        .expect("admin publish request failed");
    assert_eq!(publish.status(), 200);

    let public = client
        .get(format!("{base}/api/v1/programs/{program_id}"))
        .send()
        .await
        .expect("public program request failed after publish");
    assert_eq!(public.status(), 200);
    let public_body: Value = public.json().await.expect("public program json");
    let program = &public_body["program"];
    assert!(program.get("raw_payload").is_none());
    assert!(program.get("normalized_payload").is_none());

    // This suite may run against a persistent developer database. Remove all
    // business fixtures created above so a successful test cannot leak a fake
    // benefit into the public catalogue.
    sqlx::query("DELETE FROM benefit_source_candidates WHERE id = $1")
        .bind(candidate_id)
        .execute(&pool)
        .await
        .expect("candidate fixture cleanup");
    sqlx::query("DELETE FROM programs WHERE id = $1")
        .bind(program_id)
        .execute(&pool)
        .await
        .expect("program fixture cleanup");
    sqlx::query("DELETE FROM benefit_source_domains WHERE id = $1")
        .bind(domain_id)
        .execute(&pool)
        .await
        .expect("domain fixture cleanup");
}

#[tokio::test]
async fn monitored_source_change_hides_and_review_restores_program() {
    let _catalog_guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let base = start_test_server().await;
    let client = Client::new();
    let (_, admin_token) = create_test_admin(&base).await;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .expect("test DB connection");
    let unique = Uuid::new_v4().simple().to_string();
    let domain = format!("monitor-{unique}.example.com");
    let source_url = format!("https://{domain}/benefits/student");

    let domain_id = sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO benefit_source_domains (
             organization_name, domain, source_role, crawl_allowed,
             verification_status, verification_reference, verified_at,
             compliance_status, collection_mode, is_active
         ) VALUES (
             'Monitor Fixture', $1, 'brand_official', false,
             'verified', $2, now(), 'pending', 'manual_review', true
         ) RETURNING id",
    )
    .bind(&domain)
    .bind(&source_url)
    .fetch_one(&pool)
    .await
    .expect("source domain fixture");
    let program_id = sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO programs (
             program_type, source_type, source_id, title, summary,
             provider_name, official_url, program_status, is_active,
             provider_kind, company_name, benefit_category,
             source_health, source_verified_at, source_review_due_at
         ) VALUES (
             'corporate_benefit', 'corporate_official', $1,
             '변경 감지 테스트 혜택', '공식 출처 변경 감지 테스트입니다.',
             'Monitor Fixture', $1, 'active', true,
             'corporate', 'Monitor Fixture', 'discount',
             'current', now(), now() + interval '30 days'
         ) RETURNING id",
    )
    .bind(&source_url)
    .fetch_one(&pool)
    .await
    .expect("program fixture");
    let old_hash = "0123456789abcdef0123456789abcdef";
    let new_hash = "fedcba9876543210fedcba9876543210";
    let candidate_id = sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO benefit_source_candidates (
             domain_id, url, canonical_url, discovered_via, status,
             candidate_kind, content_hash, next_check_at,
             recheck_interval_seconds
         ) VALUES (
             $1, $2, $2, 'manual', 'approved', 'detail', $3,
             now(), 2592000
         ) RETURNING id",
    )
    .bind(domain_id)
    .bind(&source_url)
    .bind(old_hash)
    .fetch_one(&pool)
    .await
    .expect("candidate fixture");
    sqlx::query(
        "INSERT INTO program_source_monitors (
             program_id, candidate_id, verification_state,
             verified_content_hash, verified_at, review_due_at,
             recheck_interval_seconds
         ) VALUES ($1, $2, 'current', $3, now(), now() + interval '30 days', 2592000)",
    )
    .bind(program_id)
    .bind(candidate_id)
    .bind(old_hash)
    .execute(&pool)
    .await
    .expect("source monitor fixture");

    let outcome = mazimi_worker::source_monitoring::record_page_check(
        &pool,
        mazimi_worker::source_monitoring::PageCheck {
            candidate_id,
            previous_content_hash: Some(old_hash),
            content_hash: new_hash,
            page_title: Some("학생 혜택 변경"),
            review_text: Some("변경된 공식 혜택 검수용 텍스트"),
            etag: Some("fixture-v2"),
            last_modified: None,
            http_status: 200,
            fetched_bytes: 128,
            recheck_interval_seconds: 2_592_000,
            thin_content: false,
        },
    )
    .await
    .expect("record changed source");
    assert_eq!(
        outcome,
        mazimi_worker::source_monitoring::ContentChange::Changed
    );

    let held: (bool, String, String, i64) = sqlx::query_as(
        "SELECT program.is_active, program.program_status, program.source_health,
                (SELECT count(*) FROM program_source_holds hold
                 WHERE hold.program_id = program.id AND hold.resolved_at IS NULL)
         FROM programs program WHERE program.id = $1",
    )
    .bind(program_id)
    .fetch_one(&pool)
    .await
    .expect("held program query");
    assert!(!held.0, "changed-source benefit must be hidden");
    assert_eq!(held.1, "review_required");
    assert_eq!(held.2, "change_detected");
    assert_eq!(held.3, 1);

    let incident: (String, i32) = sqlx::query_as(
        "SELECT status, occurrence_count FROM operational_events
         WHERE fingerprint = $1",
    )
    .bind(format!("source_review:{candidate_id}"))
    .fetch_one(&pool)
    .await
    .expect("source review incident query");
    assert_eq!(incident.0, "open");
    assert_eq!(incident.1, 1);

    let queue = client
        .get(format!(
            "{base}/api/v1/admin/benefit-candidates?status=change_review"
        ))
        .bearer_auth(&admin_token)
        .send()
        .await
        .expect("source review queue request");
    assert_eq!(queue.status(), 200);
    let queue_body: Value = queue.json().await.expect("source review queue json");
    let queued_item = queue_body["items"]
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .find(|item| item["id"] == candidate_id.to_string())
        })
        .expect("changed candidate must be present in admin source queue");
    assert_eq!(queued_item["previous_content_hash"], old_hash);
    assert_eq!(
        queued_item["monitored_programs"][0]["program_id"],
        program_id.to_string()
    );
    assert_eq!(
        queued_item["monitored_programs"][0]["verification_state"],
        "change_detected"
    );

    let detail = client
        .get(format!(
            "{base}/api/v1/admin/benefit-candidates/{candidate_id}"
        ))
        .bearer_auth(&admin_token)
        .send()
        .await
        .expect("source review detail request");
    assert_eq!(detail.status(), 200);
    let detail_body: Value = detail.json().await.expect("source review detail json");
    assert_eq!(detail_body["review_text"], "변경된 공식 혜택 검수용 텍스트");
    assert_eq!(
        detail_body["monitored_programs"][0]["title"],
        "변경 감지 테스트 혜택"
    );

    let reviewed = client
        .put(format!(
            "{base}/api/v1/admin/programs/{program_id}/source-verification"
        ))
        .bearer_auth(&admin_token)
        .json(&json!({
            "candidate_id": candidate_id,
            "resolution": "confirmed_unchanged",
            "note": "관리자가 변경된 페이지에서 기존 정규화 정보가 유효함을 확인"
        }))
        .send()
        .await
        .expect("source verification request");
    assert_eq!(reviewed.status(), 200);
    let reviewed_body: Value = reviewed.json().await.expect("source review json");
    assert_eq!(reviewed_body["source_health"], "current");
    assert_eq!(reviewed_body["is_active"], true);
    assert_eq!(reviewed_body["candidate_status"], "approved");

    let restored: (bool, String, String, i64) = sqlx::query_as(
        "SELECT program.is_active, program.program_status, program.source_health,
                (SELECT count(*) FROM program_source_holds hold
                 WHERE hold.program_id = program.id AND hold.resolved_at IS NULL)
         FROM programs program WHERE program.id = $1",
    )
    .bind(program_id)
    .fetch_one(&pool)
    .await
    .expect("restored program query");
    assert!(restored.0);
    assert_eq!(restored.1, "active");
    assert_eq!(restored.2, "current");
    assert_eq!(restored.3, 0);

    let incident_status = sqlx::query_scalar::<_, String>(
        "SELECT status FROM operational_events WHERE fingerprint = $1",
    )
    .bind(format!("source_review:{candidate_id}"))
    .fetch_one(&pool)
    .await
    .expect("resolved source incident query");
    assert_eq!(incident_status, "resolved");

    sqlx::query("DELETE FROM operational_events WHERE fingerprint = $1")
        .bind(format!("source_review:{candidate_id}"))
        .execute(&pool)
        .await
        .expect("operational event fixture cleanup");
    sqlx::query("DELETE FROM programs WHERE id = $1")
        .bind(program_id)
        .execute(&pool)
        .await
        .expect("program fixture cleanup");
    sqlx::query("DELETE FROM benefit_source_candidates WHERE id = $1")
        .bind(candidate_id)
        .execute(&pool)
        .await
        .expect("candidate fixture cleanup");
    sqlx::query("DELETE FROM benefit_source_domains WHERE id = $1")
        .bind(domain_id)
        .execute(&pool)
        .await
        .expect("source domain fixture cleanup");
}

/// Non-crawlable verified official domain, as every seeded brand site is.
async fn insert_manual_review_domain(pool: &sqlx::PgPool, domain: &str, reference: &str) -> Uuid {
    sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO benefit_source_domains (
             organization_name, domain, source_role, crawl_allowed,
             verification_status, verification_reference, verified_at,
             compliance_status, collection_mode, is_active
         ) VALUES (
             'Monitor Fixture', $1, 'brand_official', false,
             'verified', $2, now(), 'pending', 'manual_review', true
         ) RETURNING id",
    )
    .bind(domain)
    .bind(reference)
    .fetch_one(pool)
    .await
    .expect("source domain fixture")
}

/// Published official benefit + approved candidate + current monitor. A NULL
/// hash mirrors the seeded rows that were verified by a human, not a fetch.
async fn insert_monitored_program(
    pool: &sqlx::PgPool,
    domain_id: Uuid,
    source_url: &str,
    verified_hash: Option<&str>,
    verified_days_ago: i32,
    review_due_in_days: i32,
) -> (Uuid, Uuid) {
    let program_id = sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO programs (
             program_type, source_type, source_id, title, summary,
             provider_name, official_url, program_status, is_active,
             provider_kind, company_name, benefit_category,
             source_health, source_verified_at, source_review_due_at
         ) VALUES (
             'corporate_benefit', 'corporate_official', $1,
             '모니터 픽스처 혜택', '출처 모니터링 테스트입니다.',
             'Monitor Fixture', $1, 'active', true,
             'corporate', 'Monitor Fixture', 'discount',
             'current', now() - make_interval(days => $2), now() + make_interval(days => $3)
         ) RETURNING id",
    )
    .bind(source_url)
    .bind(verified_days_ago)
    .bind(review_due_in_days)
    .fetch_one(pool)
    .await
    .expect("program fixture");
    let candidate_id = sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO benefit_source_candidates (
             domain_id, url, canonical_url, discovered_via, status,
             candidate_kind, content_hash, next_check_at, recheck_interval_seconds
         ) VALUES ($1, $2, $2, 'manual', 'approved', 'detail', $3, now(), 2592000)
         RETURNING id",
    )
    .bind(domain_id)
    .bind(source_url)
    .bind(verified_hash)
    .fetch_one(pool)
    .await
    .expect("candidate fixture");
    sqlx::query(
        "INSERT INTO program_source_monitors (
             program_id, candidate_id, verification_state, verified_content_hash,
             verified_at, review_due_at, recheck_interval_seconds
         ) VALUES (
             $1, $2, 'current', $3,
             now() - make_interval(days => $4), now() + make_interval(days => $5), 2592000
         )",
    )
    .bind(program_id)
    .bind(candidate_id)
    .bind(verified_hash)
    .bind(verified_days_ago)
    .bind(review_due_in_days)
    .execute(pool)
    .await
    .expect("source monitor fixture");
    (program_id, candidate_id)
}

async fn cleanup_monitored_fixture(
    pool: &sqlx::PgPool,
    program_ids: &[Uuid],
    candidate_ids: &[Uuid],
    domain_id: Uuid,
) {
    for candidate_id in candidate_ids {
        for prefix in ["source_review", "source_review_upcoming"] {
            sqlx::query("DELETE FROM operational_events WHERE fingerprint = $1")
                .bind(format!("{prefix}:{candidate_id}"))
                .execute(pool)
                .await
                .expect("operational event fixture cleanup");
        }
    }
    sqlx::query("DELETE FROM programs WHERE id = ANY($1)")
        .bind(program_ids)
        .execute(pool)
        .await
        .expect("program fixture cleanup");
    sqlx::query("DELETE FROM benefit_source_candidates WHERE id = ANY($1)")
        .bind(candidate_ids)
        .execute(pool)
        .await
        .expect("candidate fixture cleanup");
    sqlx::query("DELETE FROM benefit_source_domains WHERE id = $1")
        .bind(domain_id)
        .execute(pool)
        .await
        .expect("source domain fixture cleanup");
}

async fn candidate_state(pool: &sqlx::PgPool, candidate_id: Uuid) -> (String, i64) {
    sqlx::query_as(
        "SELECT c.status,
                (SELECT count(*) FROM program_source_holds hold
                 WHERE hold.candidate_id = c.id AND hold.resolved_at IS NULL)
         FROM benefit_source_candidates c WHERE c.id = $1",
    )
    .bind(candidate_id)
    .fetch_one(pool)
    .await
    .expect("candidate state query")
}

#[tokio::test]
async fn rediscovery_never_rewinds_a_held_candidate_and_batch_review_restores_it() {
    let _catalog_guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let base = start_test_server().await;
    let client = Client::new();
    let (_, admin_token) = create_test_admin(&base).await;
    let (_, user_token, _) = create_test_user(&base).await;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .expect("test DB connection");
    let unique = Uuid::new_v4().simple().to_string();
    let domain = format!("held-{unique}.example.com");
    let source_url = format!("https://{domain}/benefits/youth");
    let domain_id = insert_manual_review_domain(&pool, &domain, &source_url).await;
    let old_hash = "0123456789abcdef0123456789abcdef";
    let new_hash = "fedcba9876543210fedcba9876543210";
    let (program_id, candidate_id) =
        insert_monitored_program(&pool, domain_id, &source_url, Some(old_hash), 0, 30).await;

    let outcome = mazimi_worker::source_monitoring::record_page_check(
        &pool,
        mazimi_worker::source_monitoring::PageCheck {
            candidate_id,
            previous_content_hash: Some(old_hash),
            content_hash: new_hash,
            page_title: Some("바뀐 페이지"),
            review_text: Some("변경된 공식 혜택 검수용 텍스트"),
            etag: None,
            last_modified: None,
            http_status: 200,
            fetched_bytes: 128,
            recheck_interval_seconds: 2_592_000,
            thin_content: false,
        },
    )
    .await
    .expect("record changed source");
    assert_eq!(
        outcome,
        mazimi_worker::source_monitoring::ContentChange::Changed
    );
    assert_eq!(
        candidate_state(&pool, candidate_id).await,
        ("change_review".to_string(), 1)
    );

    // H1: an admin re-queue of the same URL must not rewind the state machine.
    let requeued = client
        .post(format!("{base}/api/v1/admin/benefit-candidates"))
        .bearer_auth(&admin_token)
        .json(&json!({ "url": source_url, "discovered_via": "manual" }))
        .send()
        .await
        .expect("requeue request");
    assert_eq!(requeued.status(), 200);
    let requeued_body: Value = requeued.json().await.expect("requeue json");
    assert_eq!(requeued_body["id"], candidate_id.to_string());
    assert_eq!(requeued_body["status"], "change_review");

    // H1: nor may the 6-hourly official seed job.
    let target_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO brand_discovery_targets (
             id, target_key, display_name, coverage_category, discovery_priority,
             official_domain, domain_status, policy_status, seed_urls
         ) VALUES ($1, $2, 'Held Fixture', '테스트', 1, $3, 'verified', 'search_only', ARRAY[$4])",
    )
    .bind(target_id)
    .bind(format!("held_fixture_{unique}"))
    .bind(&domain)
    .bind(&source_url)
    .execute(&pool)
    .await
    .expect("seed target fixture");
    mazimi_worker::sources::official_seeds::OfficialSeedDiscoverer::run(&pool)
        .await
        .expect("official seed discovery");
    assert_eq!(
        candidate_state(&pool, candidate_id).await,
        ("change_review".to_string(), 1),
        "seed discovery must preserve the monitoring state"
    );

    // M3: candidate review is refused while a hold is open, in both directions.
    let approve_body = json!({
        "decision": "approve",
        "title": "Held 청년 할인",
        "summary": "요약",
        "benefit_category": "discount",
        "publish": false
    });
    let approve = client
        .put(format!(
            "{base}/api/v1/admin/benefit-candidates/{candidate_id}/review"
        ))
        .bearer_auth(&admin_token)
        .json(&approve_body)
        .send()
        .await
        .expect("approve held candidate");
    assert_eq!(approve.status(), 409);
    let approve_json: Value = approve.json().await.expect("approve json");
    assert_eq!(approve_json["held_program_ids"][0], program_id.to_string());
    assert_eq!(
        approve_json["source_verification_paths"][0],
        format!("/api/v1/admin/programs/{program_id}/source-verification")
    );
    let reject = client
        .put(format!(
            "{base}/api/v1/admin/benefit-candidates/{candidate_id}/review"
        ))
        .bearer_auth(&admin_token)
        .json(&json!({ "decision": "reject", "reason": "테스트 거절" }))
        .send()
        .await
        .expect("reject held candidate");
    assert_eq!(reject.status(), 409);
    assert_eq!(
        candidate_state(&pool, candidate_id).await,
        ("change_review".to_string(), 1)
    );

    // Batch input is validated as a whole before any write.
    let batch_url = format!("{base}/api/v1/admin/programs/source-verification/batch");
    for (label, body, expected) in [
        (
            "missing note",
            json!({ "items": [{ "program_id": program_id, "resolution": "confirmed_unchanged" }] }),
            400,
        ),
        (
            "retire is single-item only",
            json!({ "items": [{ "program_id": program_id, "resolution": "retired", "note": "x" }] }),
            400,
        ),
        (
            "duplicate program",
            json!({ "items": [
                { "program_id": program_id, "resolution": "confirmed_unchanged", "note": "a" },
                { "program_id": program_id, "resolution": "confirmed_unchanged", "note": "b" }
            ] }),
            400,
        ),
        ("empty batch", json!({ "items": [] }), 400),
    ] {
        let response = client
            .put(&batch_url)
            .bearer_auth(&admin_token)
            .json(&body)
            .send()
            .await
            .expect(label);
        assert_eq!(response.status(), expected, "{label}");
    }
    let forbidden = client
        .put(&batch_url)
        .bearer_auth(&user_token)
        .json(&json!({ "items": [{ "program_id": program_id, "resolution": "confirmed_unchanged", "note": "x" }] }))
        .send()
        .await
        .expect("regular user batch");
    assert_eq!(forbidden.status(), 403);
    assert_eq!(
        candidate_state(&pool, candidate_id).await,
        ("change_review".to_string(), 1),
        "rejected batches must not write"
    );

    // H2: bulk confirmation reuses the single-item service and restores the program.
    let missing_program = Uuid::new_v4();
    let batch = client
        .put(&batch_url)
        .bearer_auth(&admin_token)
        .json(&json!({ "items": [
            { "program_id": program_id, "resolution": "confirmed_unchanged",
              "note": "관리자가 변경된 페이지의 기존 사실이 유효함을 확인" },
            { "program_id": missing_program, "resolution": "confirmed_unchanged", "note": "없는 프로그램" }
        ] }))
        .send()
        .await
        .expect("batch confirmation");
    assert_eq!(batch.status(), 200);
    let batch_body: Value = batch.json().await.expect("batch json");
    assert_eq!(batch_body["succeeded"], 1);
    assert_eq!(batch_body["failed"], 1);
    assert_eq!(batch_body["results"][0]["status"], "ok");
    assert_eq!(batch_body["results"][0]["outcome"]["is_active"], true);
    assert_eq!(
        batch_body["results"][0]["outcome"]["source_health"],
        "current"
    );
    assert_eq!(batch_body["results"][1]["status"], "error");
    assert_eq!(batch_body["results"][1]["http_status"], 404);
    let restored: (bool, String) =
        sqlx::query_as("SELECT is_active, source_health FROM programs WHERE id = $1")
            .bind(program_id)
            .fetch_one(&pool)
            .await
            .expect("restored program");
    assert_eq!(restored, (true, "current".to_string()));
    assert_eq!(
        candidate_state(&pool, candidate_id).await,
        ("approved".to_string(), 0)
    );

    // M3: a domain that is no longer verified cannot publish its review candidates.
    sqlx::query("UPDATE benefit_source_domains SET verification_status = 'rejected' WHERE id = $1")
        .bind(domain_id)
        .execute(&pool)
        .await
        .expect("reject domain fixture");
    sqlx::query(
        "UPDATE benefit_source_candidates SET status = 'review', raw_text = '검수 텍스트' WHERE id = $1",
    )
    .bind(candidate_id)
    .execute(&pool)
    .await
    .expect("review state fixture");
    let rejected_domain = client
        .put(format!(
            "{base}/api/v1/admin/benefit-candidates/{candidate_id}/review"
        ))
        .bearer_auth(&admin_token)
        .json(&approve_body)
        .send()
        .await
        .expect("approve on rejected domain");
    assert_eq!(rejected_domain.status(), 409);

    sqlx::query("DELETE FROM brand_discovery_targets WHERE id = $1")
        .bind(target_id)
        .execute(&pool)
        .await
        .expect("target fixture cleanup");
    cleanup_monitored_fixture(&pool, &[program_id], &[candidate_id], domain_id).await;
}

#[tokio::test]
async fn null_hash_monitor_first_fetch_needs_recent_or_human_verification() {
    let _catalog_guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let _base = start_test_server().await;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .expect("test DB connection");
    let unique = Uuid::new_v4().simple().to_string();
    let domain = format!("baseline-{unique}.example.com");
    let domain_id = insert_manual_review_domain(&pool, &domain, "fixture").await;
    let fetched_hash = "abcdefabcdefabcdefabcdefabcdefab";
    let mut programs = Vec::new();
    let mut candidates = Vec::new();

    for (label, verified_days_ago, thin_content, expect_review) in [
        ("stale human verification", 30, false, true),
        ("recent human verification", 0, false, false),
        ("recent but script-shell text", 0, true, true),
    ] {
        let source_url = format!("https://{domain}/benefits/{}", programs.len());
        let (program_id, candidate_id) =
            insert_monitored_program(&pool, domain_id, &source_url, None, verified_days_ago, 30)
                .await;
        programs.push(program_id);
        candidates.push(candidate_id);

        let outcome = mazimi_worker::source_monitoring::record_page_check(
            &pool,
            mazimi_worker::source_monitoring::PageCheck {
                candidate_id,
                previous_content_hash: None,
                content_hash: fetched_hash,
                page_title: Some("첫 자동 수집"),
                review_text: Some("첫 자동 수집 검수용 텍스트"),
                etag: None,
                last_modified: None,
                http_status: 200,
                fetched_bytes: 128,
                recheck_interval_seconds: 2_592_000,
                thin_content,
            },
        )
        .await
        .expect("record first fetch");
        let (is_active, verified_hash): (bool, Option<String>) = sqlx::query_as(
            "SELECT program.is_active, monitor.verified_content_hash
             FROM programs program
             JOIN program_source_monitors monitor ON monitor.program_id = program.id
             WHERE program.id = $1",
        )
        .bind(program_id)
        .fetch_one(&pool)
        .await
        .expect("baseline state query");
        if expect_review {
            assert_eq!(
                outcome,
                mazimi_worker::source_monitoring::ContentChange::Changed,
                "{label}"
            );
            assert!(!is_active, "{label}: program must be held");
            assert!(verified_hash.is_none(), "{label}: no silent baseline");
            assert_eq!(
                candidate_state(&pool, candidate_id).await,
                ("change_review".to_string(), 1),
                "{label}"
            );
            let reason: Option<String> = sqlx::query_scalar(
                "SELECT error_summary FROM benefit_source_checks
                 WHERE candidate_id = $1 ORDER BY detected_at DESC LIMIT 1",
            )
            .bind(candidate_id)
            .fetch_one(&pool)
            .await
            .expect("check reason");
            assert!(
                reason.is_some_and(|reason| reason.contains("baseline")),
                "{label}: the check must say why the baseline was refused"
            );
        } else {
            assert_eq!(
                outcome,
                mazimi_worker::source_monitoring::ContentChange::BaselineEstablished,
                "{label}"
            );
            assert!(is_active, "{label}");
            assert_eq!(verified_hash.as_deref(), Some(fetched_hash), "{label}");
            assert_eq!(
                candidate_state(&pool, candidate_id).await,
                ("approved".to_string(), 0),
                "{label}"
            );
        }
    }

    cleanup_monitored_fixture(&pool, &programs, &candidates, domain_id).await;
}

#[tokio::test]
async fn upcoming_manual_review_is_warned_and_counted_before_programs_are_hidden() {
    let _catalog_guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let base = start_test_server().await;
    let client = Client::new();
    let (_, admin_token) = create_test_admin(&base).await;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .expect("test DB connection");
    let unique = Uuid::new_v4().simple().to_string();
    let domain = format!("upcoming-{unique}.example.com");
    let source_url = format!("https://{domain}/benefits/student");
    let domain_id = insert_manual_review_domain(&pool, &domain, &source_url).await;
    let hash = "0123456789abcdef0123456789abcdef";
    let (program_id, candidate_id) =
        insert_monitored_program(&pool, domain_id, &source_url, Some(hash), 27, 3).await;
    let fingerprint = format!("source_review_upcoming:{candidate_id}");

    // H2(a): the maintenance sweep warns once, 7 days ahead, without flapping.
    let flagged = mazimi_worker::source_monitoring::warn_upcoming_manual_reviews(&pool)
        .await
        .expect("upcoming review sweep");
    assert!(flagged >= 1, "fixture must be flagged, got {flagged}");
    let event: (String, String, i32) = sqlx::query_as(
        "SELECT status, severity, occurrence_count FROM operational_events WHERE fingerprint = $1",
    )
    .bind(&fingerprint)
    .fetch_one(&pool)
    .await
    .expect("upcoming review event");
    assert_eq!(event, ("open".to_string(), "warning".to_string(), 1));
    mazimi_worker::source_monitoring::warn_upcoming_manual_reviews(&pool)
        .await
        .expect("second sweep");
    let occurrences: i32 = sqlx::query_scalar(
        "SELECT occurrence_count FROM operational_events WHERE fingerprint = $1",
    )
    .bind(&fingerprint)
    .fetch_one(&pool)
    .await
    .expect("occurrence query");
    assert_eq!(
        occurrences, 1,
        "an open warning is not re-raised every sweep"
    );
    let still_active: bool = sqlx::query_scalar("SELECT is_active FROM programs WHERE id = $1")
        .bind(program_id)
        .fetch_one(&pool)
        .await
        .expect("active query");
    assert!(still_active, "a warning never hides the benefit");

    // H2(b): the operator queue counts the re-verification deadline as expiring.
    let maintenance = client
        .get(format!("{base}/api/v1/admin/program-maintenance?limit=100"))
        .bearer_auth(&admin_token)
        .send()
        .await
        .expect("maintenance request");
    assert_eq!(maintenance.status(), 200);
    let maintenance_body: Value = maintenance.json().await.expect("maintenance json");
    assert!(maintenance_body["expiring"]["total"].as_i64().unwrap_or(0) >= 1);
    let expiring_item = maintenance_body["expiring"]["items"]
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .find(|item| item["id"] == program_id.to_string())
        })
        .expect("program due for source review must appear in the expiring queue");
    assert_eq!(expiring_item["expiry_kind"], "source_review");
    assert!(expiring_item["source_review_due_at"].is_string());

    // Confirming the source closes the warning; a later sweep leaves it closed.
    let confirmed = client
        .put(format!(
            "{base}/api/v1/admin/programs/{program_id}/source-verification"
        ))
        .bearer_auth(&admin_token)
        .json(&json!({
            "candidate_id": candidate_id,
            "resolution": "confirmed_unchanged",
            "note": "공식 페이지를 직접 확인함"
        }))
        .send()
        .await
        .expect("confirm request");
    assert_eq!(confirmed.status(), 200);
    let resolved: String =
        sqlx::query_scalar("SELECT status FROM operational_events WHERE fingerprint = $1")
            .bind(&fingerprint)
            .fetch_one(&pool)
            .await
            .expect("resolved event query");
    assert_eq!(resolved, "resolved");
    mazimi_worker::source_monitoring::warn_upcoming_manual_reviews(&pool)
        .await
        .expect("post-confirmation sweep");
    let after_sweep: String =
        sqlx::query_scalar("SELECT status FROM operational_events WHERE fingerprint = $1")
            .bind(&fingerprint)
            .fetch_one(&pool)
            .await
            .expect("post-sweep event query");
    assert_eq!(after_sweep, "resolved");

    cleanup_monitored_fixture(&pool, &[program_id], &[candidate_id], domain_id).await;
}

#[tokio::test]
async fn admin_cannot_publish_an_unmonitored_corporate_benefit() {
    let base = start_test_server().await;
    let client = Client::new();
    let (_, admin_token) = create_test_admin(&base).await;

    let published_manual = client
        .post(format!("{base}/api/v1/admin/programs"))
        .bearer_auth(&admin_token)
        .json(&json!({
            "program_type": "corporate_benefit",
            "source_type": "manual",
            "title": "모니터 없는 기업 혜택",
            "is_active": true
        }))
        .send()
        .await
        .expect("publish manual corporate benefit");
    assert_eq!(published_manual.status(), 400);

    let draft = client
        .post(format!("{base}/api/v1/admin/programs"))
        .bearer_auth(&admin_token)
        .json(&json!({
            "program_type": "corporate_benefit",
            "source_type": "manual",
            "title": "모니터 없는 기업 혜택 초안",
            "is_active": false
        }))
        .send()
        .await
        .expect("draft manual corporate benefit");
    assert_eq!(draft.status(), 200);
    let draft_body: Value = draft.json().await.expect("draft json");
    let program_id = Uuid::parse_str(draft_body["id"].as_str().expect("program id")).unwrap();

    let publish_later = client
        .put(format!("{base}/api/v1/admin/programs/{program_id}"))
        .bearer_auth(&admin_token)
        .json(&json!({ "is_active": true }))
        .send()
        .await
        .expect("publish draft");
    assert_eq!(publish_later.status(), 400);

    let edit_draft = client
        .put(format!("{base}/api/v1/admin/programs/{program_id}"))
        .bearer_auth(&admin_token)
        .json(&json!({ "title": "모니터 없는 기업 혜택 초안 v2" }))
        .send()
        .await
        .expect("edit draft");
    assert_eq!(edit_draft.status(), 200);

    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .expect("test DB cleanup connection");
    sqlx::query("DELETE FROM programs WHERE id = $1")
        .bind(program_id)
        .execute(&pool)
        .await
        .expect("program cleanup");
}

#[tokio::test]
async fn rejected_official_domain_cannot_return_through_a_search_suggestion() {
    let base = start_test_server().await;
    let client = Client::new();
    let (_, admin_token) = create_test_admin(&base).await;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .expect("test DB connection");
    let unique = Uuid::new_v4().simple().to_string();
    let brand_id = Uuid::new_v4();
    let target_id = Uuid::new_v4();
    let domain = format!("rejected-{unique}.example.com");
    let homepage = format!("https://{domain}/");

    sqlx::query(
        "INSERT INTO brands (id, name, normalized_name, source_type, source_id, discovery_priority)
         VALUES ($1, '거절 도메인 테스트', $2, 'test', $2, 1)",
    )
    .bind(brand_id)
    .bind(format!("rejecteddomain{unique}"))
    .execute(&pool)
    .await
    .expect("brand fixture");
    sqlx::query(
        "INSERT INTO brand_discovery_targets (
             id, target_key, brand_id, display_name, coverage_category,
             discovery_priority, domain_status, policy_status
         ) VALUES ($1, $2, $3, '거절 도메인 테스트', '테스트', 1, 'unresolved', 'unreviewed')",
    )
    .bind(target_id)
    .bind(format!("rejected_domain_{unique}"))
    .bind(brand_id)
    .execute(&pool)
    .await
    .expect("target fixture");
    let domain_id = sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO benefit_source_domains (
             organization_name, domain, source_role, crawl_allowed,
             verification_status, verification_reference,
             compliance_status, collection_mode, is_active
         ) VALUES (
             '거절 도메인 테스트', $1, 'brand_official', false,
             'rejected', 'manual review: not the official site',
             'rejected', 'manual_review', true
         ) RETURNING id",
    )
    .bind(&domain)
    .fetch_one(&pool)
    .await
    .expect("rejected domain fixture");
    let suggestion_id = sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO brand_source_suggestions (
             target_id, url, canonical_url, domain, page_title,
             discovered_via, suggestion_kind, status
         ) VALUES ($1, $2, $2, $3, '공식 홈페이지?', 'naver_search', 'official_homepage', 'pending')
         RETURNING id",
    )
    .bind(target_id)
    .bind(&homepage)
    .bind(&domain)
    .fetch_one(&pool)
    .await
    .expect("suggestion fixture");

    let review = client
        .put(format!(
            "{base}/api/v1/admin/brand-source-suggestions/{suggestion_id}/review"
        ))
        .bearer_auth(&admin_token)
        .json(&json!({ "decision": "approve" }))
        .send()
        .await
        .expect("suggestion review request");
    assert_eq!(review.status(), 409);
    let state: (String, String, String) = sqlx::query_as(
        "SELECT s.status, t.domain_status, d.verification_status
         FROM brand_source_suggestions s
         JOIN brand_discovery_targets t ON t.id = s.target_id
         JOIN benefit_source_domains d ON d.domain = s.domain
         WHERE s.id = $1",
    )
    .bind(suggestion_id)
    .fetch_one(&pool)
    .await
    .expect("post-review state");
    assert_eq!(
        state,
        (
            "pending".to_string(),
            "unresolved".to_string(),
            "rejected".to_string()
        )
    );

    sqlx::query("DELETE FROM brand_discovery_targets WHERE id = $1")
        .bind(target_id)
        .execute(&pool)
        .await
        .expect("target cleanup");
    sqlx::query("DELETE FROM benefit_source_domains WHERE id = $1")
        .bind(domain_id)
        .execute(&pool)
        .await
        .expect("domain cleanup");
    sqlx::query("DELETE FROM brands WHERE id = $1")
        .bind(brand_id)
        .execute(&pool)
        .await
        .expect("brand cleanup");
}

#[tokio::test]
async fn official_domain_registration_updates_discovery_state_without_enabling_crawl() {
    let base = start_test_server().await;
    let client = Client::new();
    let (_admin_id, admin_token) = create_test_admin(&base).await;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .expect("test DB connection");
    let unique = Uuid::new_v4().simple().to_string();
    let brand_id = Uuid::new_v4();
    let target_id = Uuid::new_v4();
    let domain = format!("official-{unique}.example.com");

    sqlx::query(
        "INSERT INTO brands (
             id, name, normalized_name, source_type, source_id, discovery_priority
         ) VALUES ($1, '공식 도메인 서비스 테스트', $2, 'test', $2, 1)",
    )
    .bind(brand_id)
    .bind(format!("officialdomain{unique}"))
    .execute(&pool)
    .await
    .expect("brand fixture insert");
    sqlx::query(
        "INSERT INTO brand_discovery_targets (
             id, target_key, brand_id, display_name, coverage_category,
             discovery_priority, domain_status, policy_status
         ) VALUES ($1, $2, $3, '공식 도메인 서비스 테스트', '테스트',
                   1, 'unresolved', 'unreviewed')",
    )
    .bind(target_id)
    .bind(format!("official_domain_{unique}"))
    .bind(brand_id)
    .execute(&pool)
    .await
    .expect("target fixture insert");

    let response = client
        .post(format!("{base}/api/v1/admin/benefit-source-domains"))
        .bearer_auth(&admin_token)
        .json(&json!({
            "organization_name": "공식 도메인 서비스 테스트",
            "domain": domain,
            "source_role": "brand_official",
            "brand_id": brand_id,
            "verification_reference": format!("https://{domain}/about"),
            "is_active": true
        }))
        .send()
        .await
        .expect("domain registration request failed");
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.expect("domain response json");
    let domain_id = Uuid::parse_str(body["id"].as_str().unwrap()).unwrap();
    assert_eq!(body["crawl_allowed"], false);
    assert_eq!(body["collection_mode"], "manual_review");

    let state: (String, String, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT t.domain_status, t.policy_status,
                t.official_homepage_url, b.homepage_url
         FROM brand_discovery_targets t
         JOIN brands b ON b.id = t.brand_id
         WHERE t.id = $1",
    )
    .bind(target_id)
    .fetch_one(&pool)
    .await
    .expect("discovery state query");
    assert_eq!(state.0, "verified");
    assert_eq!(state.1, "search_only");
    assert_eq!(
        state.2.as_deref(),
        Some(format!("https://{domain}/").as_str())
    );
    assert_eq!(state.3, state.2);

    let conflicting_domain = format!("other-{unique}.example.com");
    let conflict = client
        .post(format!("{base}/api/v1/admin/benefit-source-domains"))
        .bearer_auth(&admin_token)
        .json(&json!({
            "organization_name": "공식 도메인 서비스 테스트",
            "domain": conflicting_domain,
            "source_role": "brand_official",
            "brand_id": brand_id,
            "verification_reference": format!("https://{conflicting_domain}/"),
            "is_active": true
        }))
        .send()
        .await
        .expect("conflicting domain request failed");
    assert_eq!(conflict.status(), 409);

    sqlx::query("DELETE FROM benefit_source_domains WHERE id = $1")
        .bind(domain_id)
        .execute(&pool)
        .await
        .expect("domain fixture cleanup");
    sqlx::query("DELETE FROM brand_discovery_targets WHERE id = $1")
        .bind(target_id)
        .execute(&pool)
        .await
        .expect("target fixture cleanup");
    sqlx::query("DELETE FROM brands WHERE id = $1")
        .bind(brand_id)
        .execute(&pool)
        .await
        .expect("brand fixture cleanup");
}

#[tokio::test]
async fn broad_brand_registry_is_seeded_but_direct_collection_stays_off() {
    let _base = start_test_server().await;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .expect("test DB connection");

    let (targets, categories, unmapped): (i64, i64, i64) = sqlx::query_as(
        "SELECT COUNT(*), COUNT(DISTINCT coverage_category),
                COUNT(*) FILTER (WHERE brand_id IS NULL AND coverage_category <> 'university')
         FROM brand_discovery_targets
         WHERE is_active = true",
    )
    .fetch_one(&pool)
    .await
    .expect("brand coverage query");
    assert!(
        targets >= 90,
        "expected broad brand coverage, got {targets}"
    );
    assert!(categories >= 9, "expected cross-industry coverage");
    assert_eq!(
        unmapped, 0,
        "every corporate discovery target must map to a brand"
    );

    let anchors: Vec<(String, String, String, String, bool)> = sqlx::query_as(
        "SELECT t.target_key, t.domain_status, t.policy_status,
                d.collection_mode, d.crawl_allowed
         FROM brand_discovery_targets t
         JOIN benefit_source_domains d ON d.domain = t.official_domain
         WHERE t.target_key = ANY($1)
         ORDER BY t.target_key",
    )
    .bind(vec!["starbucks", "oliveyoung", "cgv", "lottecinema"])
    .fetch_all(&pool)
    .await
    .expect("anchor source policy query");
    assert_eq!(anchors.len(), 4);
    for (key, domain_status, policy_status, collection_mode, crawl_allowed) in anchors {
        assert_eq!(domain_status, "verified", "{key}");
        assert_eq!(policy_status, "search_only", "{key}");
        let expected_mode = match key.as_str() {
            "starbucks" | "oliveyoung" | "cgv" => "manual_review",
            _ => "licensed_search",
        };
        assert_eq!(collection_mode, expected_mode, "{key}");
        assert!(!crawl_allowed, "{key} must not be crawl-enabled by seeding");
    }
}

#[tokio::test]
async fn round_two_corporate_benefits_are_published_with_monitored_official_evidence() {
    let _base = start_test_server().await;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .expect("test DB connection");

    let (total, published, linked, monitored): (i64, i64, i64, i64) = sqlx::query_as(
        "SELECT
             COUNT(*),
             COUNT(*) FILTER (
                 WHERE program.is_active = true
                   AND program.source_health = 'current'
             ),
             COUNT(*) FILTER (
                 WHERE EXISTS (
                     SELECT 1
                     FROM program_brands AS link
                     WHERE link.program_id = program.id
                 )
             ),
             COUNT(*) FILTER (
                 WHERE EXISTS (
                     SELECT 1
                     FROM program_source_monitors AS monitor
                     WHERE monitor.program_id = program.id
                       AND monitor.verification_state = 'current'
                 )
             )
         FROM programs AS program
         WHERE program.source_type = 'corporate_official'
           AND program.raw_payload -> 'curation' ->> 'batch' =
               '2026-07-21-expansion-2'",
    )
    .fetch_one(&pool)
    .await
    .expect("round-two benefit integrity query");
    assert_eq!(total, 17);
    assert_eq!(published, total, "every reviewed benefit must be published");
    assert_eq!(linked, total, "every reviewed benefit must map to a brand");
    assert_eq!(
        monitored, total,
        "every reviewed benefit must have a current source monitor"
    );

    let evidence_domains = vec![
        "www.lottecinema.co.kr",
        "gs25.gsretail.com",
        "www.daiso.co.kr",
        "www.coffeebeankorea.com",
        "www.baristapaulbassett.co.kr",
        "www.h-point.co.kr",
        "bluefit.shinsegae.com",
        "www.yes24.com",
        "www.aladin.co.kr",
        "www.okcashbag.com",
        "www.musinsa.com",
        "aws.amazon.com",
        "www.tableau.com",
        "m.lpoint.com",
        "www.lotteeatz.com",
    ];
    let (verified, crawl_enabled): (i64, i64) = sqlx::query_as(
        "SELECT
             COUNT(*) FILTER (WHERE verification_status = 'verified'),
             COUNT(*) FILTER (WHERE crawl_allowed = true)
         FROM benefit_source_domains
         WHERE domain = ANY($1)",
    )
    .bind(&evidence_domains)
    .fetch_one(&pool)
    .await
    .expect("round-two evidence-domain policy query");
    assert_eq!(verified, evidence_domains.len() as i64);
    assert_eq!(
        crawl_enabled, 0,
        "manual verification must never enable crawling implicitly"
    );

    let (new_targets, mapped_targets): (i64, i64) = sqlx::query_as(
        "SELECT COUNT(*), COUNT(*) FILTER (WHERE brand_id IS NOT NULL)
         FROM brand_discovery_targets
         WHERE target_key = ANY($1)",
    )
    .bind(vec!["shinsegae_point", "tableau"])
    .fetch_one(&pool)
    .await
    .expect("round-two discovery-target query");
    assert_eq!(new_targets, 2);
    assert_eq!(mapped_targets, new_targets);
}

#[tokio::test]
async fn source_suggestion_review_is_transactional_idempotent_and_non_crawling() {
    let base = start_test_server().await;
    let client = Client::new();
    let (admin_id, admin_token) = create_test_admin(&base).await;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .expect("test DB connection");
    let unique = Uuid::new_v4().simple().to_string();
    let brand_id = Uuid::new_v4();
    let target_id = Uuid::new_v4();
    let suggestion_id = Uuid::new_v4();
    let target_key = format!("review_{unique}");
    let domain = format!("brand-{unique}.example.com");
    let homepage = format!("https://{domain}/");

    sqlx::query(
        "INSERT INTO brands (
             id, name, normalized_name, source_type, source_id, discovery_priority
         ) VALUES ($1, '검증 테스트 브랜드', $2, 'test', $2, 1)",
    )
    .bind(brand_id)
    .bind(format!("reviewbrand{unique}"))
    .execute(&pool)
    .await
    .expect("brand fixture insert");
    sqlx::query(
        "INSERT INTO brand_discovery_targets (
             id, target_key, brand_id, display_name, coverage_category,
             discovery_priority, domain_status, policy_status
         ) VALUES ($1, $2, $3, '검증 테스트 브랜드', '테스트', 1, 'candidate', 'unreviewed')",
    )
    .bind(target_id)
    .bind(target_key)
    .bind(brand_id)
    .execute(&pool)
    .await
    .expect("target fixture insert");
    sqlx::query(
        "INSERT INTO brand_source_suggestions (
             id, target_id, url, canonical_url, domain, page_title,
             discovered_via, suggestion_kind, status
         ) VALUES ($1, $2, $3, $3, $4, '공식 홈페이지',
                   'naver_search', 'official_homepage', 'pending')",
    )
    .bind(suggestion_id)
    .bind(target_id)
    .bind(&homepage)
    .bind(&domain)
    .execute(&pool)
    .await
    .expect("suggestion fixture insert");

    let reviewed = client
        .put(format!(
            "{base}/api/v1/admin/brand-source-suggestions/{suggestion_id}/review"
        ))
        .bearer_auth(&admin_token)
        .json(&json!({
            "decision": "approve",
            "note": "공식 홈페이지 확인"
        }))
        .send()
        .await
        .expect("suggestion review request");
    assert_eq!(reviewed.status(), 200);
    let body: Value = reviewed.json().await.expect("review response json");
    assert_eq!(body["official_domain"], domain);
    assert_eq!(body["crawl_allowed"], false);

    let state: (String, String, bool, String, String) = sqlx::query_as(
        "SELECT s.status, s.reviewed_by, d.crawl_allowed, d.collection_mode,
                t.policy_status
         FROM brand_source_suggestions s
         JOIN benefit_source_domains d ON d.domain = s.domain
         JOIN brand_discovery_targets t ON t.id = s.target_id
         WHERE s.id = $1",
    )
    .bind(suggestion_id)
    .fetch_one(&pool)
    .await
    .expect("reviewed state query");
    assert_eq!(state.0, "approved");
    assert_eq!(state.1, admin_id.to_string());
    assert!(!state.2);
    assert_eq!(state.3, "licensed_search");
    assert_eq!(state.4, "search_only");

    let duplicate = client
        .put(format!(
            "{base}/api/v1/admin/brand-source-suggestions/{suggestion_id}/review"
        ))
        .bearer_auth(&admin_token)
        .json(&json!({ "decision": "approve" }))
        .send()
        .await
        .expect("duplicate review request");
    assert_eq!(duplicate.status(), 409);

    let second_brand_id = Uuid::new_v4();
    let second_target_id = Uuid::new_v4();
    let second_suggestion_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO brands (
             id, name, normalized_name, source_type, source_id, discovery_priority
         ) VALUES ($1, '공유 도메인 브랜드', $2, 'test', $2, 1)",
    )
    .bind(second_brand_id)
    .bind(format!("sharedbrand{unique}"))
    .execute(&pool)
    .await
    .expect("second brand fixture insert");
    sqlx::query(
        "INSERT INTO brand_discovery_targets (
             id, target_key, brand_id, display_name, coverage_category,
             discovery_priority, domain_status, policy_status
         ) VALUES ($1, $2, $3, '공유 도메인 브랜드', '테스트', 1, 'candidate', 'unreviewed')",
    )
    .bind(second_target_id)
    .bind(format!("shared_{unique}"))
    .bind(second_brand_id)
    .execute(&pool)
    .await
    .expect("second target fixture insert");
    sqlx::query(
        "INSERT INTO brand_source_suggestions (
             id, target_id, url, canonical_url, domain, page_title,
             discovered_via, suggestion_kind, status
         ) VALUES ($1, $2, $3, $3, $4, '공유 공식 홈페이지',
                   'naver_search', 'official_homepage', 'pending')",
    )
    .bind(second_suggestion_id)
    .bind(second_target_id)
    .bind(&homepage)
    .bind(&domain)
    .execute(&pool)
    .await
    .expect("second suggestion fixture insert");

    let implicit_share = client
        .put(format!(
            "{base}/api/v1/admin/brand-source-suggestions/{second_suggestion_id}/review"
        ))
        .bearer_auth(&admin_token)
        .json(&json!({ "decision": "approve" }))
        .send()
        .await
        .expect("implicit shared-domain review request");
    assert_eq!(implicit_share.status(), 409);

    let explicit_share = client
        .put(format!(
            "{base}/api/v1/admin/brand-source-suggestions/{second_suggestion_id}/review"
        ))
        .bearer_auth(&admin_token)
        .json(&json!({
            "decision": "approve",
            "allow_shared_domain": true,
            "note": "동일 운영사 도메인 확인"
        }))
        .send()
        .await
        .expect("explicit shared-domain review request");
    assert_eq!(explicit_share.status(), 200);

    let mapped_brands = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*)
         FROM benefit_source_domain_brands mapping
         JOIN benefit_source_domains domain ON domain.id = mapping.domain_id
         WHERE domain.domain = $1",
    )
    .bind(&domain)
    .fetch_one(&pool)
    .await
    .expect("shared-domain mapping query");
    assert_eq!(mapped_brands, 2);

    // Integration tests may run against a persistent developer database.
    // Remove the complete graph so test-only brands never enter discovery.
    sqlx::query("DELETE FROM brand_source_suggestions WHERE id = ANY($1::uuid[])")
        .bind(vec![suggestion_id, second_suggestion_id])
        .execute(&pool)
        .await
        .expect("suggestion fixtures cleanup");
    sqlx::query("DELETE FROM benefit_source_domains WHERE domain = $1")
        .bind(&domain)
        .execute(&pool)
        .await
        .expect("shared domain fixture cleanup");
    sqlx::query("DELETE FROM brand_discovery_targets WHERE id = ANY($1::uuid[])")
        .bind(vec![target_id, second_target_id])
        .execute(&pool)
        .await
        .expect("target fixtures cleanup");
    sqlx::query("DELETE FROM brands WHERE id = ANY($1::uuid[])")
        .bind(vec![brand_id, second_brand_id])
        .execute(&pool)
        .await
        .expect("brand fixtures cleanup");
}

#[tokio::test]
async fn expired_application_keeps_submission_snapshot_and_owner_privacy() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let base = start_test_server().await;
    let client = Client::new();
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .unwrap();
    let (owner, token, _) = create_test_user(&base).await;
    let (other, other_token, _) = create_test_user(&base).await;
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO programs(id,program_type,source_type,source_id,title,program_status,is_active,official_url) VALUES($1,'scholarship','manual',$2,'신청 당시 내용','active',true,'https://example.com/apply')")
        .bind(id).bind(id.to_string()).execute(&pool).await.unwrap();
    let response = client
        .put(format!("{base}/api/v1/my/applications/{id}"))
        .bearer_auth(&token)
        .json(&json!({"status":"applied","memo":"제출 확인"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
    sqlx::query(
        "UPDATE programs SET title='변경된 내용',deadline_at=now()-interval '1 day' WHERE id=$1",
    )
    .bind(id)
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(
        client
            .get(format!("{base}/api/v1/programs/{id}"))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    let saved: Value = client
        .get(format!("{base}/api/v1/my/applications/{id}"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(saved["program_title"], "신청 당시 내용");
    assert_eq!(saved["snapshot"]["program"]["title"], "신청 당시 내용");
    assert!(saved["applied_at"].is_string());
    assert_eq!(saved["history"].as_array().unwrap().len(), 1);
    assert_eq!(
        client
            .get(format!("{base}/api/v1/my/applications/{id}"))
            .bearer_auth(other_token)
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    let invalid = client
        .put(format!("{base}/api/v1/my/applications/{id}"))
        .bearer_auth(&token)
        .json(&json!({"status":"received","memo":"x".repeat(2001)}))
        .send()
        .await
        .unwrap();
    assert_eq!(invalid.status(), 400);
    let unchanged: String = sqlx::query_scalar(
        "SELECT state FROM user_program_states WHERE user_id=$1 AND program_id=$2",
    )
    .bind(owner)
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(unchanged, "applied");
    sqlx::query("DELETE FROM users WHERE id=ANY($1)")
        .bind(vec![owner, other])
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
async fn oauth_handoff_is_verifier_bound_and_single_use() {
    use sha2::{Digest, Sha256};
    let base = start_test_server().await;
    let client = Client::new();
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .unwrap();
    let (owner, _, _) = create_test_user(&base).await;
    let code = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let verifier = "a".repeat(64);
    let hash = |s: &str| format!("{:x}", Sha256::digest(s.as_bytes()));
    sqlx::query("INSERT INTO oauth_handoffs(state_hash,challenge,client,callback_used,exchange_hash,user_id) VALUES($1,$2,'web',true,$3,$4)")
        .bind(hash(&code)).bind(hash(&verifier)).bind(hash(&code)).bind(owner).execute(&pool).await.unwrap();
    let url = format!("{base}/api/v1/auth/kakao/exchange");
    assert_eq!(
        client
            .post(&url)
            .json(&json!({"code":code,"verifier":"b".repeat(64)}))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let response = client
        .post(&url)
        .json(&json!({"code":code,"verifier":verifier}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let credentials: Value = response.json().await.unwrap();
    assert!(credentials["token"].is_string());
    assert_eq!(
        client
            .post(&url)
            .json(&json!({"code":code,"verifier":verifier}))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    sqlx::query("DELETE FROM users WHERE id=$1")
        .bind(owner)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn campus_catalog_covers_regions_and_respects_leave_status() {
    let base = start_test_server().await;
    let client = Client::new();
    let schools: Value = client
        .get(format!("{base}/api/v1/campus/universities"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let schools = schools["items"].as_array().unwrap();
    assert_eq!(schools.len(), 46);
    assert!(schools
        .iter()
        .all(|s| s["region"] == "busan" || s["region"] == "daegu"));
    let enrolled: Value = client
        .get(format!("{base}/api/v1/campus/programs"))
        .query(&[("school", "pusan"), ("enrollment_status", "enrolled")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let leave: Value = client
        .get(format!("{base}/api/v1/campus/programs"))
        .query(&[("school", "pusan"), ("enrollment_status", "on_leave")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(enrolled["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["title"].as_str().unwrap().contains("Google AI")));
    assert!(!leave["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["title"].as_str().unwrap().contains("Google AI")));
    assert!(leave["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["title"].as_str().unwrap().contains("Microsoft")));
}

#[tokio::test]
async fn campus_common_offers_reuse_corporate_ids_and_exclude_unknown_schools() {
    let base = start_test_server().await;
    let client = Client::new();
    let campus_url = format!("{base}/api/v1/campus/programs");
    let mut common_ids = None;
    for school in ["pusan", "pknu", "dgist", "bdu", "kopo-daegu"] {
        let response = client
            .get(&campus_url)
            .query(&[("school", school), ("enrollment_status", "enrolled")])
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let body: Value = response.json().await.unwrap();
        assert_eq!(
            body["total"].as_u64().unwrap(),
            body["school_total"].as_u64().unwrap() + body["common_total"].as_u64().unwrap()
        );
        let ids: std::collections::BTreeSet<String> = body["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|p| p["program_type"] == "corporate_benefit")
            .map(|p| p["id"].as_str().unwrap().to_string())
            .collect();
        assert!(ids.len() >= 10);
        assert!(!body["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["title"].as_str().unwrap().contains("Canva for Education")));
        if let Some(previous) = &common_ids {
            assert_eq!(&ids, previous);
        } else {
            common_ids = Some(ids);
        }
    }
    for (school, status) in [
        ("not-a-school", "enrolled"),
        ("bdu", "graduated"),
        ("bdu", "on_leave"),
    ] {
        let body: Value = client
            .get(&campus_url)
            .query(&[("school", school), ("enrollment_status", status)])
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(body["total"], 0, "{school}/{status}");
    }
    assert_eq!(
        client
            .get(&campus_url)
            .query(&[("school", "")])
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    let body: Value = client
        .get(&campus_url)
        .query(&[
            ("school", "bdu"),
            ("q", "Tower"),
            ("enrollment_status", "enrolled"),
        ])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["total"], 1);
    let id = body["items"][0]["id"].as_str().unwrap();
    let detail: Value = client
        .get(format!("{base}/api/v1/programs/{id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(detail["program_type"], "corporate_benefit");
}

#[tokio::test]
async fn preparation_uses_resolved_destination_and_resets_after_link_changes() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let base = start_test_server().await;
    let client = Client::new();
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .unwrap();
    let (user, token, _) = create_test_user(&base).await;
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO programs(id,program_type,source_type,title,program_status,is_active,official_url,application_method) VALUES($1,'financial_product','manual','routing fixture','active',true,'https://bank.example.org/apply?id=42','기관 신청서 작성 후 본인인증. 안내 https://bank.example.org/guide, https://user:pw@bank.example.org/x http://bank.example.org/y')")
        .bind(id).execute(&pool).await.unwrap();
    let url = format!("{base}/api/v1/programs/{id}/preparation");
    let plan: Value = client
        .get(&url)
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        plan["policy"]["destination"]["url"],
        "https://bank.example.org/apply?id=42"
    );
    assert_eq!(plan["policy"]["destination"]["evidence"], "official_url");
    assert_eq!(plan["policy"]["destination"]["kind"], "unverified_page");
    assert_eq!(
        plan["policy"]["official_origin"],
        "https://bank.example.org"
    );
    assert_eq!(plan["policy"]["can_submit"], false);
    assert_eq!(plan["policy"]["can_autofill"], false);
    // Only the safe HTTPS link quoted in the method text is offered; it is a
    // pointer, not submit authority.
    assert_eq!(plan["official_links"].as_array().unwrap().len(), 1);
    assert_eq!(
        plan["official_links"][0]["url"],
        "https://bank.example.org/guide"
    );
    assert!(plan.get("materials").is_none());
    assert!(plan["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|task| task["id"] == "financial_terms"));
    assert_eq!(
        client
            .put(&url)
            .bearer_auth(&token)
            .json(&json!({"revision":plan["revision"],"completed_tasks":["eligibility"]}))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    sqlx::query(
        "UPDATE programs SET official_url='https://bank.example.org/apply?id=43' WHERE id=$1",
    )
    .bind(id)
    .execute(&pool)
    .await
    .unwrap();
    let changed: Value = client
        .get(&url)
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(changed["progress_reset"], true);
    assert_ne!(changed["revision"], plan["revision"]);
    assert_eq!(
        client
            .put(&url)
            .bearer_auth(&token)
            .json(&json!({"revision":plan["revision"],"completed_tasks":["eligibility"]}))
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    sqlx::query("DELETE FROM programs WHERE id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM users WHERE id=$1")
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn publisher_feed_replay_conflict_and_revocation_are_enforced() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let base = start_test_server().await;
    let client = Client::new();
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .unwrap();
    let (admin, admin_token) = create_test_admin(&base).await;
    let brand = Uuid::new_v4();
    let domain = Uuid::new_v4();
    let unique = Uuid::new_v4().simple().to_string();
    sqlx::query("INSERT INTO brands(id,name,normalized_name,source_type,source_id) VALUES($1,'Feed Test',$2,'test',$2)").bind(brand).bind(&unique).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO benefit_source_domains(id,brand_id,organization_name,domain,source_role,verification_status,verification_reference) VALUES($1,$2,'Feed Test',$3,'brand_official','verified','integration fixture')").bind(domain).bind(brand).bind(format!("feed-{unique}.example.com")).execute(&pool).await.unwrap();
    let response=client.post(format!("{base}/api/v1/admin/benefit-feeds")).bearer_auth(&admin_token).json(&json!({"name":"Feed Test","domain_id":domain,"brand_id":brand,"authority_reference":"Integration fixture, no actual publication authority","authorized_until":Utc::now()+Duration::days(30)})).send().await.unwrap();
    assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
    // Create a second distinct feed to retain its one-time response token.
    let feed:Value=client.post(format!("{base}/api/v1/admin/benefit-feeds")).bearer_auth(&admin_token).json(&json!({"name":"Feed Test 2","domain_id":domain,"brand_id":brand,"authority_reference":"Integration fixture only","authorized_until":Utc::now()+Duration::days(30)})).send().await.unwrap().json().await.unwrap();
    let feed_id = feed["id"].as_str().unwrap();
    let token = feed["token"].as_str().unwrap();
    let url = format!("{base}/api/v1/benefit-feeds/{feed_id}/batches");
    let mut batch = json!({"schema_version":1,"sequence":1,"items":[{"external_id":"offer","title":"학생 혜택","summary":"테스트 혜택 설명","official_url":format!("https://feed-{unique}.example.com/students"),"category":"software","conditions":"재학생 확인","application_method":"공식 홈페이지","documents":["재학증명서"],"valid_until":Utc::now()+Duration::days(7),"enabled":true}]});
    let first = client
        .put(&url)
        .bearer_auth(token)
        .json(&batch)
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), 200, "{}", first.text().await.unwrap());
    let replay: Value = client
        .put(&url)
        .bearer_auth(token)
        .json(&batch)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(replay["replayed"], true);
    batch["items"][0]["title"] = json!("무효한 재전송");
    assert_eq!(
        client
            .put(&url)
            .bearer_auth(token)
            .json(&batch)
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    let id: Uuid =
        sqlx::query_scalar("SELECT program_id FROM benefit_feed_items WHERE feed_id=$1::uuid")
            .bind(feed_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        client
            .get(format!("{base}/api/v1/programs/{id}"))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        client
            .delete(format!("{base}/api/v1/admin/benefit-feeds/{feed_id}"))
            .bearer_auth(admin_token)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        client
            .get(format!("{base}/api/v1/programs/{id}"))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    assert_eq!(
        client
            .put(&url)
            .bearer_auth(token)
            .json(&batch)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    sqlx::query("DELETE FROM benefit_feed_batches WHERE feed_id=$1::uuid")
        .bind(feed_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM benefit_feed_items WHERE feed_id=$1::uuid")
        .bind(feed_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM benefit_feeds WHERE created_by=$1")
        .bind(admin)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM programs WHERE id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM benefit_source_domains WHERE id=$1")
        .bind(domain)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM brands WHERE id=$1")
        .bind(brand)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM users WHERE id=$1")
        .bind(admin)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn oauth_provider_isolation_cancellation_and_google_handoff() {
    use sha2::{Digest, Sha256};
    let base = start_test_server().await;
    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .unwrap();
    let hash = |s: &str| format!("{:x}", Sha256::digest(s.as_bytes()));
    let nonce = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    sqlx::query("INSERT INTO oauth_handoffs(state_hash,challenge,client,provider) VALUES($1,$2,'mobile','google')")
        .bind(hash(&nonce)).bind("a".repeat(64)).execute(&pool).await.unwrap();
    let wrong = client
        .get(format!(
            "{base}/api/v1/auth/kakao/callback?state={nonce}&error=access_denied"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), 400);
    let cancel = client.get(format!("{base}/api/v1/auth/google/callback?state={nonce}&error=access_denied&redirect_uri=https://attacker.invalid")).send().await.unwrap();
    assert_eq!(cancel.status(), 307);
    assert_eq!(
        cancel.headers()["location"],
        "mazimi://login?provider=google&error=cancelled"
    );
    assert_eq!(
        client
            .get(format!(
                "{base}/api/v1/auth/google/callback?state={nonce}&error=access_denied"
            ))
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    sqlx::query("DELETE FROM oauth_handoffs WHERE state_hash=$1")
        .bind(hash(&nonce))
        .execute(&pool)
        .await
        .unwrap();
    let (owner, _, _) = create_test_user(&base).await;
    let code = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let verifier = "a".repeat(64);
    sqlx::query("INSERT INTO oauth_handoffs(state_hash,challenge,client,provider,callback_used,exchange_hash,user_id) VALUES($1,$2,'web','google',true,$3,$4)")
        .bind(hash(&code)).bind(hash(&verifier)).bind(hash(&code)).bind(owner).execute(&pool).await.unwrap();
    for (provider, status) in [("kakao", 401), ("google", 200), ("google", 401)] {
        // Each test server has an independent IP limiter; split the callback and handoff batches.
        let exchange_base = start_test_server().await;
        let response = client
            .post(format!("{exchange_base}/api/v1/auth/{provider}/exchange"))
            .json(&json!({"code":code,"verifier":verifier}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), status);
        if status == 200 {
            assert!(response.json::<Value>().await.unwrap()["token"].is_string());
        }
    }
    sqlx::query("DELETE FROM users WHERE id=$1")
        .bind(owner)
        .execute(&pool)
        .await
        .unwrap();
}

/// The Next.js site signs in through the same verifier-bound handoff as the
/// apps. The provider is mocked by seeding the row its callback would write.
#[tokio::test]
async fn site_oauth_handoff_lands_on_the_next_server_route_and_issues_working_tokens() {
    use sha2::{Digest, Sha256};
    let hash = |s: &str| format!("{:x}", Sha256::digest(s.as_bytes()));
    let random = || format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .unwrap();

    // Each test server has its own IP limiter (5 auth calls); batch per server.
    let base = start_test_server().await;
    let start = |client_kind: &str| {
        format!(
            "{base}/api/v1/auth/kakao/start?client={client_kind}&challenge={}",
            "a".repeat(64)
        )
    };
    assert_eq!(
        client.get(start("evil")).send().await.unwrap().status(),
        400
    );
    // 307 to Kakao when credentials are configured, 503 otherwise; never 400.
    let site = client.get(start("site")).send().await.unwrap().status();
    assert!(site == 307 || site == 503, "site start returned {site}");
    let nonce = random();
    sqlx::query("INSERT INTO oauth_handoffs(state_hash,challenge,client,provider) VALUES($1,$2,'site','kakao')")
        .bind(hash(&nonce)).bind("a".repeat(64)).execute(&pool).await.unwrap();
    let cancel = client
        .get(format!(
            "{base}/api/v1/auth/kakao/callback?state={nonce}&error=access_denied"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(cancel.status(), 307);
    assert_eq!(
        cancel.headers()["location"],
        "http://localhost:3000/auth/callback?provider=kakao&error=cancelled"
    );

    let (owner, _, _) = create_test_user(&base).await;
    let code = random();
    let verifier = random();
    sqlx::query("INSERT INTO oauth_handoffs(state_hash,challenge,client,provider,callback_used,exchange_hash,user_id) VALUES($1,$2,'site','kakao',true,$3,$4)")
        .bind(hash(&random())).bind(hash(&verifier)).bind(hash(&code)).bind(owner).execute(&pool).await.unwrap();
    let exchange_base = start_test_server().await;
    let exchange = format!("{exchange_base}/api/v1/auth/kakao/exchange");
    let wrong = client
        .post(&exchange)
        .json(&json!({"code":code,"verifier":random()}))
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), 401, "a different browser's verifier");
    let issued: Value = client
        .post(&exchange)
        .json(&json!({"code":code,"verifier":verifier}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let token = issued["token"].as_str().expect("access token").to_owned();
    let refresh = issued["refresh_token"].as_str().expect("refresh token");
    assert_eq!(
        client
            .post(&exchange)
            .json(&json!({"code":code,"verifier":verifier}))
            .send()
            .await
            .unwrap()
            .status(),
        401,
        "the code is single-use"
    );

    let me: Value = client
        .get(format!("{base}/api/v1/auth/me"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(me["id"], owner.to_string());
    let stats = format!("{base}/api/v1/admin/stats");
    assert_eq!(
        client
            .get(&stats)
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    sqlx::query("UPDATE users SET role='admin' WHERE id=$1")
        .bind(owner)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        client
            .get(&stats)
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .status(),
        200,
        "the role is read per request"
    );

    let session_base = start_test_server().await;
    let rotated: Value = client
        .post(format!("{session_base}/api/v1/auth/refresh"))
        .json(&json!({"refresh_token":refresh}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let rotated = rotated["refresh_token"].as_str().expect("rotated refresh");
    assert_eq!(
        client
            .post(format!("{session_base}/api/v1/auth/logout"))
            .json(&json!({"refresh_token":rotated}))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        client
            .post(format!("{session_base}/api/v1/auth/refresh"))
            .json(&json!({"refresh_token":rotated}))
            .send()
            .await
            .unwrap()
            .status(),
        401,
        "logout revokes the refresh token"
    );
    sqlx::query("DELETE FROM oauth_handoffs WHERE state_hash=$1")
        .bind(hash(&nonce))
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM users WHERE id=$1")
        .bind(owner)
        .execute(&pool)
        .await
        .unwrap();
}

/// A shared NAT address (testers on one Wi-Fi) can run several sign-ins in
/// a row; the refresh/logout bucket stays strict.
#[tokio::test]
async fn oauth_handoff_limit_admits_a_shared_address_while_token_routes_stay_strict() {
    let base = start_test_server().await;
    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    // Ten sign-in starts: more than three people's worth of handoff calls.
    for attempt in 0..10 {
        let status = client
            .get(format!(
                "{base}/api/v1/auth/kakao/start?client=site&challenge=bad{attempt}"
            ))
            .send()
            .await
            .unwrap()
            .status();
        assert_eq!(status, 400, "start {attempt} reached the handler");
    }
    let mut statuses = Vec::new();
    for _ in 0..6 {
        statuses.push(
            client
                .post(format!("{base}/api/v1/auth/refresh"))
                .json(&json!({"refresh_token":"not-a-token"}))
                .send()
                .await
                .unwrap()
                .status()
                .as_u16(),
        );
    }
    assert_eq!(statuses, [401, 401, 401, 401, 401, 429]);
}

#[tokio::test]
async fn login_configuration_is_public_only_as_availability_flags() {
    let base = start_test_server().await;
    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let result: Value = client
        .get(format!("{base}/api/v1/auth/providers"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(result.as_object().unwrap().len(), 2);
    assert!(result["kakao"].is_boolean() && result["google"].is_boolean());
    if result["google"] == false {
        let response = client
            .get(format!(
                "{base}/api/v1/auth/google/start?client=web&challenge={}",
                "a".repeat(64)
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 503);
        assert!(response.headers().get("location").is_none());
    }
}

#[tokio::test]
async fn application_journey_is_resumable_private_and_never_submits_on_handoff() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let base = start_test_server().await;
    let client = Client::new();
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&DATABASE_URL)
        .await
        .unwrap();
    let (owner, token, _) = create_test_user(&base).await;
    let (other, other_token, _) = create_test_user(&base).await;
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO programs(id,program_type,source_type,title,program_status,is_active,official_url,application_method) VALUES($1,'scholarship','manual','Journey original','active',true,'https://provider.example.org/notice/42','방문, 인터넷')")
        .bind(id).execute(&pool).await.unwrap();
    let prep = format!("{base}/api/v1/programs/{id}/preparation");
    let handoff = format!("{base}/api/v1/programs/{id}/handoff");
    let journey_url = format!("{base}/api/v1/my/application-journeys/{id}");
    let detail_url = format!("{base}/api/v1/my/applications/{id}");
    let plan: Value = client
        .get(&prep)
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(plan["selected_channel"].is_null());
    assert_eq!(plan["policy"]["can_submit"], false);
    let saved:Value=client.put(&prep).bearer_auth(&token).json(&json!({"revision":plan["revision"],"completed_tasks":["eligibility"],"selected_channel":"visit"})).send().await.unwrap().json().await.unwrap();
    assert_eq!(saved["selected_channel"], "visit");
    let resumed: Value = client
        .get(&prep)
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(resumed["selected_channel"], "visit");
    let body = json!({"revision":plan["revision"],"channel":"visit"});
    let (a, b) = tokio::join!(
        client.post(&handoff).bearer_auth(&token).json(&body).send(),
        client.post(&handoff).bearer_auth(&token).json(&body).send()
    );
    let a = a.unwrap();
    let b = b.unwrap();
    assert_eq!(a.status(), 200);
    assert_eq!(b.status(), 200);
    let first: Value = a.json().await.unwrap();
    let second: Value = b.json().await.unwrap();
    assert_eq!(first["id"], second["id"]);
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM user_program_state_history WHERE user_id=$1 AND program_id=$2",
    )
    .bind(owner)
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, 1);
    let detail: Value = client
        .get(&detail_url)
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(detail["current_status"], "applying");
    assert_eq!(detail["submission_check"], "checking");
    assert_eq!(detail["display_status"], "applying");
    assert_eq!(detail["display_label"], "신청 준비 중");
    assert!(detail.get("provider_submission").is_none());
    assert!(detail["applied_at"].is_null());
    assert_eq!(client.get(&journey_url).send().await.unwrap().status(), 401);
    let other_view: Value = client
        .get(&journey_url)
        .bearer_auth(&other_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(other_view["journey"].is_null());
    assert_eq!(
        client
            .put(&journey_url)
            .bearer_auth(&other_token)
            .json(&json!({"journey_id":first["id"],"outcome":"unknown"}))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    let unknown: Value = client
        .put(&journey_url)
        .bearer_auth(&token)
        .json(&json!({"journey_id":first["id"],"outcome":"unknown"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(unknown["outcome"], "unknown");
    let replay: Value = client
        .post(&handoff)
        .bearer_auth(&token)
        .json(&body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(replay["outcome"], "unknown");
    sqlx::query("UPDATE programs SET title='Changed after handoff',application_method='우편',is_active=false,deadline_at=now()-interval '1 hour' WHERE id=$1").bind(id).execute(&pool).await.unwrap();
    assert_eq!(
        client
            .get(&prep)
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    let expired: Value = client
        .get(&journey_url)
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(expired["journey"]["snapshot"]["title"], "Journey original");
    let report = json!({"journey_id":first["id"],"outcome":"reported_applied","receipt_reference":"LOCAL-TEST-42"});
    assert_eq!(
        client
            .put(&journey_url)
            .bearer_auth(&token)
            .json(&report)
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    let mut report = report;
    report["confirmed"] = json!(true);
    let first_report: Value = client
        .put(&journey_url)
        .bearer_auth(&token)
        .json(&report)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(first_report["outcome"], "reported_applied");
    assert_eq!(
        client
            .put(&journey_url)
            .bearer_auth(&token)
            .json(&report)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let detail: Value = client
        .get(&detail_url)
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(detail["current_status"], "applied");
    assert_eq!(detail["submission_check"], "reported_applied");
    assert_eq!(detail["display_status"], "applied");
    assert_eq!(detail["display_label"], "신청 완료 · 직접 기록");
    assert_eq!(detail["snapshot"]["program"]["title"], "Journey original");
    assert_eq!(
        detail["snapshot"]["program"]["application_method"],
        "방문, 인터넷"
    );
    assert_eq!(detail["snapshot"]["capture_reason"], "handoff_report");
    assert_eq!(detail["history"].as_array().unwrap().len(), 3);
    assert_eq!(
        client
            .put(&journey_url)
            .bearer_auth(&token)
            .json(&json!({"journey_id":first["id"],"outcome":"unknown"}))
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    assert_eq!(
        client
            .put(&detail_url)
            .bearer_auth(&token)
            .json(&json!({"status":"received"}))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    report["receipt_reference"] = json!("LOCAL-TEST-42-corrected");
    assert_eq!(
        client
            .put(&journey_url)
            .bearer_auth(&token)
            .json(&report)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let detail: Value = client
        .get(&detail_url)
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(detail["current_status"], "received");
    sqlx::query("DELETE FROM users WHERE id=ANY($1)")
        .bind(vec![owner, other])
        .execute(&pool)
        .await
        .unwrap();
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM application_journeys WHERE program_id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count, 0);
    sqlx::query("DELETE FROM programs WHERE id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn application_workflow_respects_no_application_and_stale_forms() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let base = start_test_server().await;
    let client = Client::new();
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .unwrap();
    let (user, token, _) = create_test_user(&base).await;
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO programs(id,program_type,source_type,title,program_status,is_active,application_method) VALUES($1,'benefit','manual','No application fixture','active',true,'신청불필요')").bind(id).execute(&pool).await.unwrap();
    let prep = format!("{base}/api/v1/programs/{id}/preparation");
    let handoff = format!("{base}/api/v1/programs/{id}/handoff");
    let url = format!("{base}/api/v1/my/application-journeys/{id}");
    let plan: Value = client
        .get(&prep)
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(plan["policy"]["workflow"]["no_application_required"], true);
    assert_eq!(plan["tasks"].as_array().unwrap().len(), 2);
    assert_eq!(
        client
            .post(&handoff)
            .bearer_auth(&token)
            .json(&json!({"revision":plan["revision"],"channel":"not_required"}))
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    sqlx::query("UPDATE programs SET application_method='방문, 우편' WHERE id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let visit: Value = client
        .get(&prep)
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(visit["policy"]["can_prepare"], true);
    assert!(visit["policy"]["destination"].is_null());
    assert_eq!(
        client
            .post(&handoff)
            .bearer_auth(&token)
            .json(&json!({"revision":plan["revision"],"channel":"visit"}))
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    assert_eq!(
        client
            .post(&handoff)
            .bearer_auth(&token)
            .json(&json!({"revision":visit["revision"],"channel":"online"}))
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    let first: Value = client
        .post(&handoff)
        .bearer_auth(&token)
        .json(&json!({"revision":visit["revision"],"channel":"visit"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let second: Value = client
        .post(&handoff)
        .bearer_auth(&token)
        .json(&json!({"revision":visit["revision"],"channel":"postal"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_ne!(first["id"], second["id"]);
    assert_eq!(
        client
            .put(&url)
            .bearer_auth(&token)
            .json(&json!({"journey_id":first["id"],"outcome":"unknown"}))
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    assert_eq!(client.put(&url).bearer_auth(&token).json(&json!({"journey_id":second["id"],"outcome":"reported_applied","confirmed":true,"receipt_reference":"x".repeat(121)})).send().await.unwrap().status(),400);
    assert_eq!(client.put(&url).bearer_auth(&token).json(&json!({"journey_id":second["id"],"outcome":"verified_by_provider","confirmed":true})).send().await.unwrap().status(),400);
    sqlx::query("DELETE FROM users WHERE id=$1")
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM programs WHERE id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
}
