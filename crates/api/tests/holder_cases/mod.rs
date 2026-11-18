//! Holder condition through the recommendation list (review C2): an offer
//! a telco pays for is hidden from someone who answered another carrier,
//! capped at 확인 필요 with 통신사 as the missing check for someone who did
//! not answer, and listed as today for a holder. The two SK텔레콤 청년
//! seeds (20261107000002) are held the same way as a fixture offer.

use super::*;

/// The listed item of `program` in a preview for `body`, if any.
async fn listed(base: &str, body: &Value, program: Uuid) -> Option<Value> {
    let response = Client::new()
        .post(format!("{base}/api/v1/recommend/preview?scope=all"))
        .json(body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let reply: Value = response.json().await.unwrap();
    reply["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["program_id"] == json!(program))
        .cloned()
}

#[tokio::test]
async fn a_telco_offer_is_three_state_on_the_carrier_answer() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let base = start_test_server().await;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .unwrap();
    let skt: Uuid = sqlx::query_scalar(
        "SELECT id FROM brands WHERE holder_field = 'telecom_carrier' AND holder_value = 'skt'
         ORDER BY discovery_priority DESC LIMIT 1",
    )
    .fetch_one(&pool)
    .await
    .expect("the registry migration gives SK텔레콤 its holder key");
    let fixture = Uuid::new_v4();
    let mut tx = pool.begin().await.unwrap();
    sqlx::query(
        "INSERT INTO programs (id, program_type, source_type, source_id, title, summary, provider_name,
                               company_name, program_status, provider_kind, eligibility_condition,
                               regions, is_active, source_health)
         VALUES ($1, 'corporate_benefit', 'corporate_official', $2, 'SK텔레콤 · 홀더 회귀 테스트 혜택',
                 '홀더 회귀 테스트', 'SK텔레콤', 'SK텔레콤', 'always_open', 'corporate',
                 'SK텔레콤 휴대폰 이용 고객', ARRAY[]::text[], true, 'current')",
    )
    .bind(fixture)
    .bind(format!("holder-test:{fixture}"))
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO program_brands (program_id, brand_id, relationship_type, is_primary)
         VALUES ($1, $2, 'sponsor', true)",
    )
    .bind(fixture)
    .bind(skt)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    // The seeds are held until evidence confirms them; listed here for the
    // test only and put back as they were.
    let seeds: Vec<(Uuid, bool, String, String)> = sqlx::query_as(
        "SELECT id, is_active, program_status, source_health FROM programs
         WHERE source_id LIKE 'issuer:20261106:skt:%'",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(seeds.len(), 2);
    sqlx::query(
        "UPDATE programs SET is_active = true, program_status = 'always_open', source_health = 'current'
         WHERE source_id LIKE 'issuer:20261106:skt:%'",
    )
    .execute(&pool)
    .await
    .unwrap();

    let base_profile = json!({"birth_year": 2001, "region_code": "busan"});
    let mut other = base_profile.clone();
    other["telecom_carrier"] = json!("kt");
    let mut holder = base_profile.clone();
    holder["telecom_carrier"] = json!("skt");
    let mut failures = Vec::new();
    for program in std::iter::once(fixture).chain(seeds.iter().map(|(id, ..)| *id)) {
        match listed(&base, &base_profile, program).await {
            Some(item) => {
                if item["eligibility_status"] != "unknown" {
                    failures.push(format!("{program}: unanswered must be 확인 필요: {item}"));
                }
                if !item["missing_checks"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|c| c.as_str().is_some_and(|c| c.contains("통신사")))
                {
                    failures.push(format!("{program}: 통신사 is the missing check: {item}"));
                }
            }
            None => failures.push(format!("{program}: unanswered must stay listed")),
        }
        if listed(&base, &other, program).await.is_some() {
            failures.push(format!("{program}: another carrier must hide it"));
        }
        match listed(&base, &holder, program).await {
            Some(item) => {
                if item["eligibility_status"] == "pass" {
                    failures.push(format!("{program}: a corporate offer is never pass"));
                }
                if item["missing_checks"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|c| c.as_str().is_some_and(|c| c.contains("통신사")))
                {
                    failures.push(format!("{program}: a holder is not asked again: {item}"));
                }
            }
            None => failures.push(format!("{program}: a holder must see it")),
        }
    }
    for (id, active, status, health) in &seeds {
        sqlx::query(
            "UPDATE programs SET is_active = $2, program_status = $3, source_health = $4 WHERE id = $1",
        )
        .bind(id)
        .bind(active)
        .bind(status)
        .bind(health)
            .execute(&pool)
            .await
            .unwrap();
    }
    sqlx::query("DELETE FROM programs WHERE id = $1")
        .bind(fixture)
        .execute(&pool)
        .await
        .unwrap();
    assert!(failures.is_empty(), "{failures:#?}");
}
