//! F4.5, F5.6: cleaned summary and condition text next to the originals.
use super::*;

#[tokio::test]
async fn list_and_detail_serve_cleaned_text_and_keep_the_stored_text() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let base = start_test_server().await;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .unwrap();
    let term = format!("display{}", Uuid::new_v4().simple());
    let id = Uuid::new_v4();
    let summary = format!("○ {term} 생활 지원\n  - 월 20만원 (최대 12개월)");
    sqlx::query("INSERT INTO programs(id,program_type,source_type,source_id,title,summary,program_status,regions,is_active,normalized_payload) VALUES($1,'policy','manual',$2,$3,$4,'active','{}',true,$5)")
        .bind(id).bind(id.to_string()).bind(format!("{term} 청년 정책")).bind(&summary)
        .bind(json!({"conditions": "ㅇ 만 19~34세\nㅇ 무주택"}))
        .execute(&pool).await.unwrap();
    let client = Client::new();
    let list: Value = client
        .get(format!("{base}/api/v1/programs"))
        .query(&[("q", term.as_str())])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let item = &list["items"][0];
    assert_eq!(
        item["summary"],
        summary.as_str(),
        "stored text is served unchanged"
    );
    assert_eq!(
        item["summary_lines"],
        json!([format!("{term} 생활 지원"), "월 20만원 (최대 12개월)"])
    );
    assert_eq!(
        item["summary_display"],
        format!("{term} 생활 지원\n월 20만원 (최대 12개월)")
    );
    let detail: Value = client
        .get(format!("{base}/api/v1/programs/{id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(detail["summary_lines"], item["summary_lines"]);
    assert_eq!(detail["benefit_conditions"], "ㅇ 만 19~34세\nㅇ 무주택");
    assert_eq!(
        detail["benefit_conditions_lines"],
        json!(["만 19~34세", "무주택"])
    );
    assert_eq!(detail["benefit_conditions_display"], "만 19~34세\n무주택");
    let stored: String = sqlx::query_scalar("SELECT summary FROM programs WHERE id=$1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(stored, summary);
    // Leave no active fixture behind: other tests rank the shared catalogue.
    sqlx::query("UPDATE programs SET is_active = false WHERE id = ANY($1)")
        .bind(vec![id])
        .execute(&pool)
        .await
        .unwrap();
}
