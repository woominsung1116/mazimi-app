//! 기업혜택 browsing chips: `corporate_category` on the list,
//! `GET /api/v1/programs/corporate-categories`, the stored chip
//! (`programs.browse_category`) and the admin self-metric.
use super::*;
use mazimi_core::corporate_categories;

async fn pool() -> sqlx::PgPool {
    PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .unwrap()
}

async fn stored(pool: &sqlx::PgPool, id: Uuid) -> (Option<String>, Option<String>) {
    sqlx::query_as("SELECT browse_category, browse_category_by FROM programs WHERE id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Store the chips of `ids`, as a write path does.
async fn refresh(pool: &sqlx::PgPool, ids: &[Uuid]) -> corporate_categories::Refresh {
    let mut connection = pool.acquire().await.unwrap();
    corporate_categories::refresh(&mut connection, Some(ids), 1)
        .await
        .unwrap()
}

/// The worker's sweep fills rows written without a chip (the post-migration
/// backfill), recomputes a row whose text changed, leaves an unchanged row
/// alone and clears a row that left the 기업혜택 tab. Rows are written
/// directly, as a path that does not store the chip would.
#[tokio::test]
async fn the_sweep_fills_recomputes_and_clears_stored_chips() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let _ = start_test_server().await;
    let pool = pool().await;
    let tag = format!("sweep{}", Uuid::new_v4().simple());
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO programs(program_type,source_type,source_id,title,provider_name,program_status,provider_kind,benefit_category,is_active,regions)
         VALUES('corporate_benefit','manual',$1,'Figma 학생 무료','Figma','always_open','corporate','software',false,ARRAY[]::text[]) RETURNING id",
    )
    .bind(&tag)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(stored(&pool, id).await, (None, None), "no trigger fills it");
    let sweep = || async {
        let mut connection = pool.acquire().await.unwrap();
        let mut last = corporate_categories::Refresh::default();
        // The shared test database may hold other pending rows; sweep until none.
        for _ in 0..100 {
            last = corporate_categories::refresh(&mut connection, None, 4)
                .await
                .unwrap();
            if last.remaining == 0 {
                break;
            }
        }
        last
    };
    let run = sweep().await;
    assert_eq!(run.remaining, 0);
    assert_eq!(
        stored(&pool, id).await,
        (Some("design".into()), Some("keyword".into()))
    );
    // Unchanged: nothing to do for this row.
    assert_eq!(
        refresh(&pool, &[id]).await,
        corporate_categories::Refresh::default()
    );
    // A changed title (no updated_at bump) is recomputed.
    sqlx::query("UPDATE programs SET title = '스타벅스 리워드', provider_name = '스타벅스 코리아' WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(refresh(&pool, &[id]).await.updated, 1);
    assert_eq!(stored(&pool, id).await.0.as_deref(), Some("food"));
    // Off the tab: the chip is cleared.
    sqlx::query("UPDATE programs SET program_type = 'policy', provider_kind = 'government', benefit_category = 'education' WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let off: Vec<String> =
        sqlx::query_scalar("SELECT provider_categories FROM programs WHERE id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(!off.contains(&"corporate".to_string()), "{off:?}");
    assert_eq!(refresh(&pool, &[id]).await.cleared, 1);
    assert_eq!(stored(&pool, id).await, (None, None));
    sqlx::query("DELETE FROM programs WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
}

/// The admin write paths store the chip with the write.
#[tokio::test]
async fn admin_create_and_update_store_the_chip() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let base = start_test_server().await;
    let pool = pool().await;
    let client = Client::new();
    let (_, admin) = create_test_admin(&base).await;
    let created: Value = client
        .post(format!("{base}/api/v1/admin/programs"))
        .bearer_auth(&admin)
        .json(&json!({
            "program_type": "corporate_benefit", "title": "Figma 학생 무료 (관리자 테스트)",
            "provider_name": "Figma", "company_name": "Figma", "provider_kind": "corporate",
            "benefit_category": "software", "is_active": false
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id: Uuid = created["id"]
        .as_str()
        .unwrap_or_else(|| panic!("{created}"))
        .parse()
        .unwrap();
    assert_eq!(
        stored(&pool, id).await,
        (Some("design".into()), Some("keyword".into()))
    );
    let status = client
        .put(format!("{base}/api/v1/admin/programs/{id}"))
        .bearer_auth(&admin)
        .json(
            &json!({"title": "토익 응시료 할인 (관리자 테스트)", "benefit_category": "education"}),
        )
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(status, 200);
    assert_eq!(
        stored(&pool, id).await,
        (Some("study".into()), Some("keyword".into()))
    );
    sqlx::query("DELETE FROM programs WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
}

/// (program_type, provider_kind, title, benefit_category, active, status, regions)
type SeedRow = (
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    bool,
    &'static str,
    &'static [&'static str],
);

struct Fixture {
    tag: String,
    ids: Vec<Uuid>,
    brand: Uuid,
}

/// Visible on the tab: Figma (디자인, nationwide, always open, linked to a
/// brand), Adobe (디자인, Busan only, `active`), 카페 (외식), 새 적금 (기타).
/// Hidden: Sketch (inactive) and Framer (`review_required`). Off the tab: a
/// policy row whose title names Figma, 부산대 and 경성대 campus offers and a
/// bank's deposit (`fss_financial`).
async fn seed(pool: &sqlx::PgPool) -> Fixture {
    let tag = format!("ccat{}", Uuid::new_v4().simple());
    let mut ids = Vec::new();
    // (program_type, provider_kind, title, benefit_category, active, status, regions)
    let rows: [SeedRow; 7] = [
        (
            "corporate_benefit",
            "corporate",
            "Figma 학생 무료",
            "software",
            true,
            "always_open",
            &[],
        ),
        (
            "corporate_benefit",
            "corporate",
            "Adobe 학생 할인",
            "student_discount",
            true,
            "active",
            &["busan"],
        ),
        (
            "corporate_benefit",
            "corporate",
            "카페 스탬프 커피 적립",
            "membership",
            true,
            "always_open",
            &[],
        ),
        (
            "corporate_benefit",
            "corporate",
            "새 적금 상품",
            "finance",
            true,
            "always_open",
            &[],
        ),
        (
            "corporate_benefit",
            "corporate",
            "Sketch 학생 무료",
            "software",
            false,
            "always_open",
            &[],
        ),
        (
            "corporate_benefit",
            "corporate",
            "Framer 학생 무료",
            "software",
            true,
            "review_required",
            &[],
        ),
        (
            "policy",
            "government",
            "청년 Figma 교육 지원",
            "education",
            true,
            "always_open",
            &[],
        ),
    ];
    for (i, (program_type, kind, title, category, active, status, regions)) in
        rows.into_iter().enumerate()
    {
        let title = format!("{title} {tag}");
        let regions: Vec<String> = regions.iter().map(|r| r.to_string()).collect();
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO programs(program_type,source_type,source_id,title,summary,provider_name,program_status,provider_kind,benefit_category,is_active,source_health,regions,search_tsv)
             VALUES($1,'corporate_official',$2,$3,'fixture','픽스처',$7,$4,$5,$6,'current',$8,to_tsvector('simple',$3 || ' 픽스처')) RETURNING id",
        )
        .bind(program_type)
        .bind(format!("{tag}:{i}"))
        .bind(&title)
        .bind(kind)
        .bind(category)
        .bind(active)
        .bind(status)
        .bind(regions)
        .fetch_one(pool)
        .await
        .unwrap();
        ids.push(id);
    }
    // Never on the tab: two schools' own offers (listed only in each school's
    // 대학 tab) and a bank's deposit (listed under 금융). Before the split all
    // three were counted under 기업혜택 for every viewer.
    for (i, (program_type, source_type, kind, title, category, school)) in [
        (
            "campus_benefit",
            "university_official",
            "university",
            "부산대 Microsoft 365",
            "software",
            Some("pusan"),
        ),
        (
            "campus_benefit",
            "university_official",
            "university",
            "경성대 학내 AI Chat",
            "software",
            Some("ks"),
        ),
        (
            "financial_product",
            "fss_financial",
            "corporate",
            "농협 청년 적금",
            "finance",
            None,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let title = format!("{title} {tag}");
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO programs(program_type,source_type,source_id,title,summary,provider_name,program_status,provider_kind,benefit_category,is_active,source_health,regions,university_id,search_tsv)
             VALUES($1,$2,$3,$4,'fixture','픽스처','always_open',$5,$6,true,'current',ARRAY[]::text[],$7,to_tsvector('simple',$4 || ' 픽스처')) RETURNING id",
        )
        .bind(program_type)
        .bind(source_type)
        .bind(format!("{tag}:off{i}"))
        .bind(&title)
        .bind(kind)
        .bind(category)
        .bind(school)
        .fetch_one(pool)
        .await
        .unwrap();
        ids.push(id);
    }
    let brand: Uuid = sqlx::query_scalar(
        "INSERT INTO brands(name,normalized_name,source_type,source_id) VALUES($1,$1,'manual',$1) RETURNING id",
    )
    .bind(format!("brand-{tag}"))
    .fetch_one(pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO program_brands(program_id,brand_id,relationship_type,is_primary) VALUES($1,$2,'redeemable_at',true)")
        .bind(ids[0])
        .bind(brand)
        .execute(pool)
        .await
        .unwrap();
    // Stored as a write path stores it (the policy row gets none).
    refresh(pool, &ids).await;
    Fixture { tag, ids, brand }
}

async fn cleanup(pool: &sqlx::PgPool, f: &Fixture) {
    sqlx::query("DELETE FROM program_brands WHERE brand_id = $1")
        .bind(f.brand)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM brands WHERE id = $1")
        .bind(f.brand)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM programs WHERE id = ANY($1)")
        .bind(&f.ids)
        .execute(pool)
        .await
        .unwrap();
}

async fn get(
    client: &Client,
    url: &str,
    query: &[(&str, &str)],
    token: Option<&str>,
) -> (u16, reqwest::header::HeaderMap, Value) {
    let mut request = client.get(url).query(query);
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    let response = request.send().await.unwrap();
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    (status, headers, response.json().await.unwrap())
}

async fn list(client: &Client, base: &str, query: &[(&str, &str)]) -> (u16, Value) {
    let (status, _, body) = get(client, &format!("{base}/api/v1/programs"), query, None).await;
    (status, body)
}

fn titles_of(body: &Value) -> Vec<String> {
    let mut t: Vec<String> = body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            i["title"]
                .as_str()
                .unwrap()
                .split(' ')
                .next()
                .unwrap()
                .to_owned()
        })
        .collect();
    t.sort();
    t
}

fn chips(body: &Value) -> Vec<(String, i64)> {
    body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["code"].as_str().unwrap().to_owned(),
                c["count"].as_i64().unwrap(),
            )
        })
        .collect()
}

fn chip(code: &str, count: i64) -> (String, i64) {
    (code.to_owned(), count)
}

/// Each list item and the detail page carry the stored chip as
/// `corporate_category` (the app's category glyph and hero); rows off the
/// 기업혜택 tab (a policy, a bank deposit) carry null.
#[tokio::test]
async fn list_items_carry_the_corporate_chip_only_on_the_corporate_tab() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let base = start_test_server().await;
    let pool = pool().await;
    let client = Client::new();
    let f = seed(&pool).await;
    let (status, body) = list(&client, &base, &[("q", f.tag.as_str())]).await;
    assert_eq!(status, 200);
    let mut served: Vec<(String, Value)> = body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            assert!(
                i.as_object().unwrap().contains_key("corporate_category"),
                "{i}"
            );
            (
                i["title"]
                    .as_str()
                    .unwrap()
                    .split(' ')
                    .next()
                    .unwrap()
                    .to_owned(),
                i["corporate_category"].clone(),
            )
        })
        .collect();
    served.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        served,
        [
            ("Adobe".to_owned(), json!("design")),
            ("Figma".to_owned(), json!("design")),
            ("농협".to_owned(), Value::Null),
            ("새".to_owned(), json!("other")),
            ("청년".to_owned(), Value::Null),
            ("카페".to_owned(), json!("food")),
        ]
    );
    // The detail page serves the same chip.
    let id_of = |prefix: &str| {
        body["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["title"].as_str().unwrap().starts_with(prefix))
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    for (prefix, chip) in [("카페", json!("food")), ("청년", Value::Null)] {
        let (status, _, detail) = get(
            &client,
            &format!("{base}/api/v1/programs/{}", id_of(prefix)),
            &[],
            None,
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(detail["corporate_category"], chip, "{prefix}: {detail}");
    }
    // A row that leaves the tab keeps its stored chip until the sweep, but is never served it.
    sqlx::query(
        "UPDATE programs SET program_type = 'policy', provider_kind = 'government' WHERE id = $1",
    )
    .bind(f.ids[0])
    .execute(&pool)
    .await
    .unwrap();
    let (_, body) = list(&client, &base, &[("q", f.tag.as_str())]).await;
    let moved = body["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == json!(f.ids[0]))
        .unwrap();
    assert_eq!(
        stored(&pool, f.ids[0]).await.0.as_deref(),
        Some("design"),
        "still stored"
    );
    assert_eq!(moved["corporate_category"], Value::Null, "{moved}");
    cleanup(&pool, &f).await;
}

#[tokio::test]
async fn corporate_category_filters_the_list_and_keeps_search_and_brand() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let base = start_test_server().await;
    let pool = pool().await;
    let client = Client::new();
    let f = seed(&pool).await;
    let tag = f.tag.as_str();

    // Code or Korean label; only visible corporate rows of that chip.
    for value in ["design", "디자인·창작", " DESIGN "] {
        let (status, body) =
            list(&client, &base, &[("q", tag), ("corporate_category", value)]).await;
        assert_eq!(status, 200, "{value}");
        assert_eq!(titles_of(&body), ["Adobe", "Figma"], "{value}");
        assert_eq!(body["total"], 2);
    }
    let (_, body) = list(
        &client,
        &base,
        &[("q", tag), ("corporate_category", "food")],
    )
    .await;
    assert_eq!(titles_of(&body), ["카페"]);
    let (_, body) = list(
        &client,
        &base,
        &[("q", tag), ("corporate_category", "other")],
    )
    .await;
    assert_eq!(titles_of(&body), ["새"]);
    // With the provider tab, as the app sends it.
    let (_, body) = list(
        &client,
        &base,
        &[
            ("q", tag),
            ("provider", "corporate"),
            ("corporate_category", "design"),
        ],
    )
    .await;
    assert_eq!(titles_of(&body), ["Adobe", "Figma"]);
    // A policy row naming Figma is never under a corporate chip. 전체 holds
    // the bank's deposit but no school's own offer.
    let (_, body) = list(&client, &base, &[("q", tag)]).await;
    assert_eq!(
        titles_of(&body),
        ["Adobe", "Figma", "농협", "새", "청년", "카페"]
    );
    // Company-name search inside a chip (search_tsv holds the provider name).
    let search = format!("픽스처 {tag}");
    let (_, body) = list(
        &client,
        &base,
        &[("q", &search), ("corporate_category", "design")],
    )
    .await;
    assert_eq!(titles_of(&body), ["Adobe", "Figma"]);
    // brand_id still works, alone or with a chip.
    let brand = f.brand.to_string();
    let (_, body) = list(&client, &base, &[("q", tag), ("brand_id", &brand)]).await;
    assert_eq!(titles_of(&body), ["Figma"]);
    let (_, body) = list(
        &client,
        &base,
        &[
            ("q", tag),
            ("brand_id", &brand),
            ("corporate_category", "food"),
        ],
    )
    .await;
    assert_eq!(titles_of(&body), Vec::<String>::new());

    // Unknown chips, old benefit_category values and injection attempts are a
    // generic 400 that echoes nothing back.
    for bad in [
        "membership",
        "other' OR '1'='1",
        "",
        "x".repeat(500).as_str(),
    ] {
        let (status, body) = list(&client, &base, &[("corporate_category", bad)]).await;
        assert_eq!(status, 400, "{bad:?}");
        assert!(!body.to_string().contains("OR '1'"), "{body}");
        assert_eq!(body["field"], "corporate_category", "{body}");
    }
    cleanup(&pool, &f).await;
}

#[tokio::test]
async fn chip_counts_equal_their_lists_under_every_filter() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let base = start_test_server().await;
    let pool = pool().await;
    let client = Client::new();
    let f = seed(&pool).await;
    let tag = f.tag.as_str();
    let url = format!("{base}/api/v1/programs/corporate-categories");
    let counts = |query: Vec<(&'static str, String)>| {
        let (client, url) = (&client, &url);
        async move {
            let query: Vec<(&str, &str)> = query.iter().map(|(k, v)| (*k, v.as_str())).collect();
            let (status, headers, body) = get(client, url, &query, None).await;
            assert_eq!(status, 200, "{body}");
            assert_eq!(headers["vary"], "Authorization");
            assert!(
                headers.get("cache-control").is_none(),
                "anonymous counts are not no-store"
            );
            body
        }
    };

    // Inactive, review_required and policy rows are not counted; empty chips are absent.
    let body = counts(vec![("q", tag.to_owned())]).await;
    assert_eq!(
        chips(&body),
        [chip("design", 2), chip("food", 1), chip("other", 1)]
    );
    assert_eq!(body["total"], 4);
    assert_eq!(body["items"][0]["label"], "디자인·창작");
    // status and region narrow the counts as they narrow the list.
    let body = counts(vec![("q", tag.to_owned()), ("status", "active".into())]).await;
    assert_eq!(chips(&body), [chip("design", 1)]);
    let body = counts(vec![("q", tag.to_owned()), ("region", "seoul".into())]).await;
    assert_eq!(
        chips(&body),
        [chip("design", 1), chip("food", 1), chip("other", 1)]
    );
    // corporate_category itself is ignored by the counts (the chips stay put).
    let body = counts(vec![
        ("q", tag.to_owned()),
        ("corporate_category", "food".into()),
    ])
    .await;
    assert_eq!(body["total"], 4);
    // Bad filters are the list's own 400s.
    let (status, _, _) = get(&client, &url, &[("region", "mars")], None).await;
    assert_eq!(status, 400);

    // A chip's count is the total its list answers, under each filter set.
    let filters: [&[(&str, &str)]; 8] = [
        &[],
        &[("status", "open")],
        &[("status", "active")],
        &[("region", "busan")],
        &[("region", "seoul")],
        &[("q", tag)],
        &[("field", "living")],
        &[("field", "finance")],
    ];
    for extra in filters {
        let mut query: Vec<(&'static str, String)> = vec![("provider", "corporate".into())];
        query.extend(extra.iter().map(|(k, v)| (*k, v.to_string())));
        let body = counts(query.clone()).await;
        let base_query: Vec<(&str, &str)> = query.iter().map(|(k, v)| (*k, v.as_str())).collect();
        for (code, count) in chips(&body) {
            let mut q = base_query.clone();
            q.extend([("corporate_category", code.as_str()), ("per_page", "1")]);
            let (_, list_body) = list(&client, &base, &q).await;
            assert_eq!(
                list_body["total"].as_i64().unwrap(),
                count,
                "{code} {extra:?}"
            );
        }
        let mut q = base_query.clone();
        q.push(("per_page", "1"));
        let (_, list_body) = list(&client, &base, &q).await;
        assert_eq!(list_body["total"], body["total"], "전체 {extra:?}");
    }

    // A signed-in viewer's counts are private.
    let (_, token, _) = create_test_user(&base).await;
    let (status, headers, _) = get(&client, &url, &[], Some(&token)).await;
    assert_eq!(status, 200);
    assert_eq!(headers["vary"], "Authorization");
    assert_eq!(headers["cache-control"], "private, no-store");
    // A bad token is served the anonymous counts.
    let (status, headers, _) = get(&client, &url, &[], Some("not-a-token")).await;
    assert_eq!(status, 200);
    assert!(headers.get("cache-control").is_none());
    cleanup(&pool, &f).await;
}

/// Rows no keyword recognised are counted for the operator, and the 기타
/// titles listed.
#[tokio::test]
async fn admin_health_counts_rows_no_keyword_recognised() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let base = start_test_server().await;
    let pool = pool().await;
    let client = Client::new();
    let (_, admin) = create_test_admin(&base).await;
    let url = format!("{base}/api/v1/admin/corporate-pipeline/health");
    let read = || async {
        let (status, _, body) = get(&client, &url, &[], Some(&admin)).await;
        assert_eq!(status, 200, "{body}");
        body["browse_categories"].clone()
    };
    let before = read().await;
    let f = seed(&pool).await;
    let after = read().await;
    let delta = |key: &str| after[key].as_i64().unwrap() - before[key].as_i64().unwrap();
    assert_eq!(delta("visible"), 4);
    assert_eq!(delta("decided_by_keyword"), 3, "Figma, Adobe, 카페");
    assert_eq!(delta("in_other"), 1);
    assert_eq!(delta("unmatched"), 1);
    let other = after["other_titles"].as_array().unwrap();
    let expected = format!("새 적금 상품 {}", f.tag);
    assert!(
        other.iter().any(|t| t.as_str() == Some(expected.as_str())),
        "{after}"
    );
    assert_eq!(delta("pending"), 0);
    // A visible row no write path stored is `pending`, counted in visible.
    let unstored: Uuid = sqlx::query_scalar(
        "INSERT INTO programs(program_type,source_type,source_id,title,provider_name,program_status,provider_kind,benefit_category,is_active,source_health,regions)
         VALUES('corporate_benefit','corporate_official',$1,$2,'픽스처','always_open','corporate','software',true,'current',ARRAY[]::text[]) RETURNING id",
    )
    .bind(format!("{}:pending", f.tag))
    .bind(format!("미저장 {}", f.tag))
    .fetch_one(&pool)
    .await
    .unwrap();
    let pending = read().await;
    assert_eq!(
        pending["pending"].as_i64().unwrap() - before["pending"].as_i64().unwrap(),
        1
    );
    assert_eq!(
        pending["visible"].as_i64().unwrap() - before["visible"].as_i64().unwrap(),
        5
    );
    sqlx::query("DELETE FROM programs WHERE id = $1")
        .bind(unstored)
        .execute(&pool)
        .await
        .unwrap();
    // Not for members.
    let (_, member, _) = create_test_user(&base).await;
    let (status, _, _) = get(&client, &url, &[], Some(&member)).await;
    assert_eq!(status, 403);
    cleanup(&pool, &f).await;
}

/// 기업혜택 is company offers only: no school's own offer and no bank
/// deposit, on the list, under any chip, in the counts or on the detail page,
/// across the whole catalogue (the migrations' seeded campus offers included).
#[tokio::test]
async fn the_corporate_tab_holds_no_campus_offer_or_bank_product() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let base = start_test_server().await;
    let pool = pool().await;
    let client = Client::new();
    let f = seed(&pool).await;
    let tag = f.tag.as_str();

    let (_, body) = list(&client, &base, &[("q", tag), ("provider", "corporate")]).await;
    assert_eq!(titles_of(&body), ["Adobe", "Figma", "새", "카페"]);
    let (_, body) = list(&client, &base, &[("q", tag), ("corporate_category", "dev")]).await;
    assert_eq!(
        titles_of(&body),
        Vec::<String>::new(),
        "no campus row in 개발"
    );

    // Every page of the whole tab.
    let mut page = 1;
    loop {
        let p = page.to_string();
        let (status, body) = list(
            &client,
            &base,
            &[("provider", "corporate"), ("per_page", "100"), ("page", &p)],
        )
        .await;
        assert_eq!(status, 200, "{body}");
        for item in body["items"].as_array().unwrap() {
            assert_eq!(item["program_type"], "corporate_benefit", "{item}");
        }
        if body["has_next"] != true {
            break;
        }
        page += 1;
    }

    // The stored columns say so: no provider tab, no chip.
    for id in &f.ids[7..] {
        let (provider, providers, chip): (Option<String>, Vec<String>, Option<String>) =
            sqlx::query_as(
                "SELECT provider_category, provider_categories, browse_category FROM programs WHERE id = $1",
            )
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!((provider, providers, chip), (None, vec![], None), "{id}");
        let (status, _, detail) =
            get(&client, &format!("{base}/api/v1/programs/{id}"), &[], None).await;
        assert_eq!(status, 200, "{detail}");
        assert_eq!(detail["provider_label"], Value::Null, "{detail}");
        assert_eq!(detail["provider_categories"], json!([]), "{detail}");
    }
    cleanup(&pool, &f).await;
}

/// A school's own offer reaches only that school's students: the 대학 tab
/// lists it for its school alone, and no catalogue list (전체, a field tab, a
/// provider tab) shows it to anyone.
#[tokio::test]
async fn campus_offers_reach_only_their_own_school() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let base = start_test_server().await;
    let pool = pool().await;
    let client = Client::new();
    let f = seed(&pool).await;
    let tag = f.tag.as_str();
    let campus = |school: &'static str| {
        let (client, base) = (&client, &base);
        async move {
            let (status, _, body) = get(
                client,
                &format!("{base}/api/v1/campus/programs"),
                &[("school", school), ("q", tag)],
                None,
            )
            .await;
            assert_eq!(status, 200, "{body}");
            let mut titles: Vec<String> = body["items"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|i| i["program_type"] == "campus_benefit")
                .map(|i| {
                    i["title"]
                        .as_str()
                        .unwrap()
                        .split(' ')
                        .next()
                        .unwrap()
                        .to_owned()
                })
                .collect();
            titles.sort();
            assert_eq!(body["school_total"].as_i64().unwrap(), titles.len() as i64);
            titles
        }
    };
    // A guest names the school picked at onboarding.
    assert_eq!(campus("부산대학교").await, ["부산대"]);
    assert_eq!(campus("부산대").await, ["부산대"], "an alias");
    assert_eq!(campus("경성대학교").await, ["경성대"]);
    assert_eq!(campus("동명대학교").await, Vec::<String>::new());

    // A signed-in member gets the school saved in their profile, whatever
    // school the request names; a member with no school gets no school's offers.
    let member_campus = |school: &'static str, token: String| {
        let (client, base) = (&client, &base);
        async move {
            let (status, headers, body) = get(
                client,
                &format!("{base}/api/v1/campus/programs"),
                &[("school", school), ("q", tag)],
                Some(&token),
            )
            .await;
            assert_eq!(status, 200, "{body}");
            let private = headers
                .get("cache-control")
                .map(|v| v.to_str().unwrap().to_owned());
            let mut titles: Vec<String> = body["items"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|i| i["program_type"] == "campus_benefit")
                .map(|i| {
                    i["title"]
                        .as_str()
                        .unwrap()
                        .split(' ')
                        .next()
                        .unwrap()
                        .to_owned()
                })
                .collect();
            titles.sort();
            (titles, private)
        }
    };
    let (pnu_student, pnu_token, _) = create_test_user(&base).await;
    sqlx::query("UPDATE user_profiles SET school_name = '부산대학교' WHERE user_id = $1")
        .bind(pnu_student)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        member_campus("경성대학교", pnu_token.clone()).await.0,
        ["부산대"],
        "another school's name is ignored"
    );
    assert_eq!(
        member_campus("부산대학교", pnu_token.clone()).await.0,
        ["부산대"]
    );
    let (_, no_school_token, _) = create_test_user(&base).await;
    assert_eq!(
        member_campus("경성대학교", no_school_token).await.0,
        Vec::<String>::new(),
        "a member without a saved school sees no school's offers"
    );
    // The answer depends on the token: never shared by a cache.
    assert_eq!(
        member_campus("부산대학교", pnu_token).await.1.as_deref(),
        Some("private, no-store")
    );
    // An invalid token is a guest.
    let (status, _, body) = get(
        &client,
        &format!("{base}/api/v1/campus/programs"),
        &[("school", "경성대학교"), ("q", tag)],
        Some("not-a-token"),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["school_total"], 1, "{body}");

    for query in [
        vec![("q", tag)],
        vec![("q", tag), ("field", "living")],
        vec![("q", tag), ("provider", "corporate")],
        vec![("q", tag), ("provider", "policy")],
    ] {
        let (_, body) = list(&client, &base, &query).await;
        let titles = titles_of(&body);
        assert!(
            !titles.iter().any(|t| t == "부산대" || t == "경성대"),
            "{query:?}: {titles:?}"
        );
    }
    // A signed-in student of 부산대 is not shown it in 전체 either: the
    // 대학 tab is its one place.
    let (_, token, _) = create_test_user(&base).await;
    let (_, _, body) = get(
        &client,
        &format!("{base}/api/v1/programs"),
        &[("q", tag)],
        Some(&token),
    )
    .await;
    assert!(!titles_of(&body)
        .iter()
        .any(|t| t == "부산대" || t == "경성대"));
    cleanup(&pool, &f).await;
}

/// The 대학 tab lists the companies' offers for every university student to
/// any viewer, plus the viewer's own school's offers when there is a school.
/// A member without a recognised school in their profile gets the common
/// offers and `school_required` (not an empty tab); a member's school comes
/// only from their profile, never from the `school` parameter.
#[tokio::test]
async fn the_campus_tab_shows_common_offers_to_a_member_without_a_school() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let base = start_test_server().await;
    let pool = pool().await;
    let client = Client::new();
    let f = seed(&pool).await;
    let tag = f.tag.as_str();
    // Figma is a company offer for every university student.
    sqlx::query(
        "UPDATE programs SET normalized_payload = COALESCE(normalized_payload,'{}'::jsonb) || '{\"audience_scope\":\"higher_education\"}'::jsonb WHERE id = $1",
    )
    .bind(f.ids[0])
    .execute(&pool)
    .await
    .unwrap();
    let url = format!("{base}/api/v1/campus/programs");
    let campus = |school: &'static str, token: Option<String>| {
        let (client, url) = (&client, &url);
        async move {
            let (status, headers, body) = get(
                client,
                url,
                &[("school", school), ("q", tag)],
                token.as_deref(),
            )
            .await;
            assert_eq!(status, 200, "{body}");
            assert_eq!(
                headers.get("cache-control").unwrap().to_str().unwrap(),
                "private, no-store"
            );
            assert!(
                headers
                    .get_all("vary")
                    .iter()
                    .any(|v| v.to_str().unwrap().contains("Authorization")),
                "{headers:?}"
            );
            assert_eq!(
                body["total"].as_i64().unwrap(),
                body["school_total"].as_i64().unwrap() + body["common_total"].as_i64().unwrap(),
                "{body}"
            );
            body
        }
    };
    let summary = |body: &Value| {
        (
            titles_of(body),
            body["school_total"].as_i64().unwrap(),
            body["common_total"].as_i64().unwrap(),
            body["school_required"].clone(),
        )
    };

    // Guest: the school named at onboarding plus the common offers.
    assert_eq!(
        summary(&campus("부산대학교", None).await),
        (
            vec!["Figma".to_owned(), "부산대".to_owned()],
            1,
            1,
            json!(false)
        )
    );
    // Guest with a school that is no university: nothing, as before.
    let body = campus("없는대학교", None).await;
    assert_eq!(
        (
            titles_of(&body),
            body["total"].as_i64(),
            body["school_required"].clone()
        ),
        (vec![], Some(0), json!(true))
    );

    // Member without a school: the common offers only, whatever school the
    // request names, and the flag that a school unlocks school-only offers.
    let (_, no_school, _) = create_test_user(&base).await;
    for named in ["부산대학교", "경성대학교", ""] {
        assert_eq!(
            summary(&campus(named, Some(no_school.clone())).await),
            (vec!["Figma".to_owned()], 0, 1, json!(true)),
            "named {named:?}"
        );
    }
    // A member need not send the parameter at all.
    let (status, _, body) = get(&client, &url, &[("q", tag)], Some(&no_school)).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        summary(&body),
        (vec!["Figma".to_owned()], 0, 1, json!(true))
    );
    // Member whose saved school is no known university: the same.
    let (unknown, unknown_token, _) = create_test_user(&base).await;
    sqlx::query("UPDATE user_profiles SET school_name = '없는대학교' WHERE user_id = $1")
        .bind(unknown)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        summary(&campus("부산대학교", Some(unknown_token)).await),
        (vec!["Figma".to_owned()], 0, 1, json!(true))
    );

    // Member with a school: their own school plus the common offers, and
    // never another school's even when the request names it.
    let (pnu, pnu_token, _) = create_test_user(&base).await;
    sqlx::query("UPDATE user_profiles SET school_name = '부산대학교' WHERE user_id = $1")
        .bind(pnu)
        .execute(&pool)
        .await
        .unwrap();
    for named in ["부산대학교", "경성대학교", ""] {
        assert_eq!(
            summary(&campus(named, Some(pnu_token.clone())).await),
            (
                vec!["Figma".to_owned(), "부산대".to_owned()],
                1,
                1,
                json!(false)
            ),
            "named {named:?}"
        );
    }
    cleanup(&pool, &f).await;
}
