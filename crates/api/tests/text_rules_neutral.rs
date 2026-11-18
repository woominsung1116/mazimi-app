//! rule_v1 extractions are measurement only: with the "조건 확인됨" section
//! flag off and on, the recommendation list is byte-identical with and
//! without stored extractions. Its own test binary, so toggling
//! `VERIFIED_SECTION_ENABLED` and the catalogue never race other tests.

use reqwest::Client;
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use std::net::SocketAddr;
use tokio::net::TcpListener;
use uuid::Uuid;

fn database_url() -> String {
    std::env::var("TEST_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://wello:wello@localhost:5432/wello".into())
}

async fn start_server(pool: sqlx::PgPool) -> String {
    sqlx::migrate!("../../infra/migrations")
        .run(&pool)
        .await
        .expect("migrations");
    let app = api::build_app_with_env(pool, "test-secret".into(), "local".into());
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
    format!("http://{addr}")
}

/// The preview body of every profile and scope, as bytes.
async fn lists(client: &Client, base: &str, profiles: &[Value]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    for profile in profiles {
        for scope in ["youth", "all"] {
            let response = client
                .post(format!("{base}/api/v1/recommend/preview?scope={scope}"))
                .json(profile)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), 200);
            out.push(response.bytes().await.unwrap().to_vec());
        }
    }
    out
}

#[tokio::test]
async fn stored_extractions_change_no_byte_of_the_list_with_the_flag_off_or_on() {
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&database_url())
        .await
        .unwrap();
    let base = start_server(pool.clone()).await;
    let client = Client::new();

    // A programme rule_v1 reads as fully structured and evaluable, and one it
    // reads as partial, both with a source-code rule row.
    let mut ids = Vec::new();
    for (text, evaluable) in [
        ("기준 중위소득 100% 이하인 자", true),
        ("관내 거주 기준 중위소득 100% 이하 무주택세대구성원", false),
    ] {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO programs (id, program_type, source_type, source_id, title, program_status,
                                   is_active, raw_payload, regions)
             VALUES ($1, 'benefit', 'gov_benefits', $2, $3, 'active', true, $4, '{}')",
        )
        .bind(id)
        .bind(id.to_string())
        .bind(format!("측정 전용 {}", id.simple()))
        .bind(json!({"지원대상": text, "사용자구분": "개인"}))
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO eligibility_rules (program_id, rule_json, hard_filter_json, version, created_by)
             VALUES ($1, $2, $2, 1, 'ingestion:source_codes')",
        )
        .bind(id)
        .bind(json!({"all": [{"field": "region_code", "op": "in",
            "value": ["busan", "seoul"], "origin": "source_codes"}]}))
        .execute(&pool)
        .await
        .unwrap();
        ids.push((id, evaluable));
    }

    let profiles = [
        json!({"birth_year": 2000, "region_code": "busan"}),
        json!({"birth_year": 2000, "region_code": "busan", "income_median_band": "le60",
               "homeless_household": true, "marital_status": "married", "newlywed": true}),
        json!({"birth_year": 2000, "region_code": "busan", "income_median_band": "gt180",
               "homeless_household": false}),
    ];
    for flag in ["false", "true"] {
        std::env::set_var("VERIFIED_SECTION_ENABLED", flag);
        sqlx::query("DELETE FROM program_rule_extractions WHERE program_id = ANY($1)")
            .bind(ids.iter().map(|(id, _)| *id).collect::<Vec<_>>())
            .execute(&pool)
            .await
            .unwrap();
        let without = lists(&client, &base, &profiles).await;
        for (id, evaluable) in &ids {
            let leaves = json!([{"field": "income_median_pct", "op": "lte", "value": 100,
                "weight": 1.0, "origin": "rule_v1", "kind": "income_median",
                "quote": "기준 중위소득 100% 이하", "extractor_version": 1}]);
            sqlx::query(&format!(
                "INSERT INTO program_rule_extractions (program_id, extractor, extractor_version,
                     input_hash, has_text, fully_structured, evaluable, facts, leaves)
                 SELECT p.id, 'rule_v1', $2, {}, true, $3, $3, '[]', $4 FROM programs p WHERE p.id = $1",
                mazimi_core::text_rules::input_hash_sql("p.")
            ))
            .bind(id)
            .bind(mazimi_core::text_rules::EXTRACTOR_VERSION)
            .bind(evaluable)
            .bind(&leaves)
            .execute(&pool)
            .await
            .unwrap();
        }
        let with = lists(&client, &base, &profiles).await;
        assert_eq!(without.len(), with.len());
        for (a, b) in without.iter().zip(&with) {
            assert_eq!(
                a, b,
                "flag {flag}: the list changed with extractions present"
            );
        }
        // The programmes are listed, so the comparison covered them.
        let listed: Value = serde_json::from_slice(&with[1]).unwrap();
        for (id, _) in &ids {
            assert!(
                listed["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|i| i["program_id"] == id.to_string()),
                "flag {flag}: {id} listed"
            );
        }
    }
    std::env::remove_var("VERIFIED_SECTION_ENABLED");
    sqlx::query("DELETE FROM programs WHERE id = ANY($1)")
        .bind(ids.iter().map(|(id, _)| *id).collect::<Vec<_>>())
        .execute(&pool)
        .await
        .unwrap();
}
