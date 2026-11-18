//! F4.4: home "마감 임박" (D-0 … D-7, KST) and the deadline sort.
use super::*;

#[tokio::test]
async fn closing_within_counts_korean_days_and_the_deadline_sort_puts_rolling_rows_last() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let base = start_test_server().await;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .unwrap();
    let term = format!("closing{}", Uuid::new_v4().simple());
    // An end at 24:00 KST of today + n: the last open second is on day n.
    let end_of_day = |n: i64| {
        let day = mazimi_core::seoul_date(Utc::now()) + Duration::days(n + 1);
        day.and_hms_opt(0, 0, 0).unwrap().and_utc() - Duration::hours(9)
    };
    let insert = |title: String, status: &'static str, end: Option<chrono::DateTime<Utc>>| {
        let pool = pool.clone();
        async move {
            let id = Uuid::new_v4();
            sqlx::query("INSERT INTO programs(id,program_type,source_type,source_id,title,program_status,regions,is_active,application_end_at) VALUES($1,'policy','manual',$2,$3,$4,'{}',true,$5)")
                .bind(id).bind(id.to_string()).bind(title).bind(status).bind(end)
                .execute(&pool).await.unwrap();
            id
        }
    };
    let d3 = insert(format!("{term} D-3"), "active", Some(end_of_day(3))).await;
    let d0 = insert(format!("{term} D-0"), "active", Some(end_of_day(0))).await;
    let d7 = insert(format!("{term} D-7"), "active", Some(end_of_day(7))).await;
    let d8 = insert(format!("{term} D-8"), "active", Some(end_of_day(8))).await;
    let rolling = insert(format!("{term} 상시"), "always_open", Some(end_of_day(1))).await;
    let undated = insert(format!("{term} 기한 없음"), "active", None).await;

    let get = |params: Vec<(&'static str, String)>| {
        let base = base.clone();
        let term = term.clone();
        async move {
            let mut query = vec![("q", term)];
            query.extend(params);
            let response = Client::new()
                .get(format!("{base}/api/v1/programs"))
                .query(&query)
                .send()
                .await
                .unwrap();
            let status = response.status().as_u16();
            (status, response.json::<Value>().await.unwrap())
        }
    };
    let ids = |body: &Value| -> Vec<String> {
        body["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["id"].as_str().unwrap().to_owned())
            .collect()
    };
    let s = |ids: &[Uuid]| ids.iter().map(Uuid::to_string).collect::<Vec<_>>();

    let (status, home) = get(vec![
        ("closing_within", "7".into()),
        ("sort", "deadline".into()),
    ])
    .await;
    assert_eq!(status, 200, "{home}");
    // The always-open row has a real deadline tomorrow: it is closing too,
    // but the deadline sort lists rolling rows after dated ones.
    assert_eq!(ids(&home), s(&[d0, d3, d7, rolling]));
    let first = &home["items"][0];
    assert_eq!(
        first["deadline_date"],
        mazimi_core::seoul_date(Utc::now()).to_string()
    );
    assert!(first["effective_deadline"].is_string());

    let (_, all) = get(vec![("sort", "deadline".into())]).await;
    assert_eq!(ids(&all), s(&[d0, d3, d7, d8, rolling, undated]));
    assert_eq!(all["items"][5]["deadline_date"], Value::Null);

    let (_, today) = get(vec![("closing_within", "0".into())]).await;
    assert_eq!(ids(&today), s(&[d0]));

    for bad in ["-1", "32", "x"] {
        let (status, _) = get(vec![("closing_within", bad.into())]).await;
        assert_eq!(status, 400, "{bad}");
    }
    // The default sort is unchanged.
    let (_, default) = get(vec![]).await;
    assert_eq!(ids(&default), s(&[d0, rolling, d3, d7, d8, undated]));
    // Leave no active fixture behind: other tests rank the shared catalogue.
    sqlx::query("UPDATE programs SET is_active = false WHERE id = ANY($1)")
        .bind(vec![d0, d3, d7, d8, rolling, undated])
        .execute(&pool)
        .await
        .unwrap();
}
