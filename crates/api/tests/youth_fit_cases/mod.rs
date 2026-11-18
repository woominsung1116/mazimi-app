//! youth_fit in the catalogue and the recommendations: demoted rows leave
//! the default list but stay searchable (after the rest) and reachable by
//! link; recommendations drop them unless the profile fits their tier.

use super::*;

async fn pool() -> sqlx::PgPool {
    PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .unwrap()
}

/// A busan, active, undated row in the throwaway `category`, titled with a
/// search token, and its stored verdict (`None` = not computed yet).
async fn program(
    pool: &sqlx::PgPool,
    category: &str,
    title: &str,
    verdict: Option<(&str, Option<&str>)>,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO programs(id,program_type,source_type,source_id,title,program_status,regions,is_active) VALUES($1,$2,'manual',$1::text,$3,'active',ARRAY['busan'],true)")
        .bind(id).bind(category).bind(title).execute(pool).await.unwrap();
    if let Some((verdict, target)) = verdict {
        // Stored as a reviewed verdict: a concurrent worker refresh of the
        // shared test database rewrites computed verdicts, never reviews.
        sqlx::query("INSERT INTO program_youth_fit(program_id,verdict,target,rule,classifier_version,input_hash,reviewed_verdict,reviewed_target,reviewed_by,reviewed_at) VALUES($1,$2,$3,'S2','youth_fit_v1',repeat('a',64),$2,$3,'test',now())")
            .bind(id).bind(verdict).bind(target).execute(pool).await.unwrap();
    }
    id
}

fn ids(body: &Value, key: &str) -> Vec<String> {
    body[key]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["id"].as_str().unwrap().to_owned())
        .collect()
}

struct Catalogue {
    category: String,
    token: String,
    fit: Uuid,
    unknown: Uuid,
    off: Uuid,
    child: Uuid,
    disability: Uuid,
}

async fn catalogue(pool: &sqlx::PgPool) -> Catalogue {
    let tag = Uuid::new_v4().simple().to_string()[..10].to_owned();
    let category = format!("yfit_{tag}");
    let token = format!("yf{tag}");
    let title = |name: &str| format!("{token} {name}");
    Catalogue {
        fit: program(pool, &category, &title("월세 지원"), Some(("fit", None))).await,
        unknown: program(pool, &category, &title("교통비 지원"), None).await,
        off: program(
            pool,
            &category,
            &title("사료 지원"),
            Some(("off_target", Some("farm_fishery"))),
        )
        .await,
        child: program(
            pool,
            &category,
            &title("교복 지원"),
            Some(("child_only", Some("child"))),
        )
        .await,
        disability: program(
            pool,
            &category,
            &title("보조기기 지원"),
            Some(("disability_only", Some("disability"))),
        )
        .await,
        category,
        token,
    }
}

#[tokio::test]
async fn default_list_leaves_demoted_rows_out_and_search_lists_them_last() {
    let base = start_test_server().await;
    let pool = pool().await;
    let c = catalogue(&pool).await;
    // Verify gap 2: the disability row closes first, so by deadline alone it
    // would lead; only the tier sort key can put it last (not the id order).
    sqlx::query("UPDATE programs SET deadline_at = now() + interval '2 days' WHERE id = $1")
        .bind(c.disability)
        .execute(&pool)
        .await
        .unwrap();
    let client = Client::new();
    let get = |url: String| {
        let client = client.clone();
        async move {
            let response = client.get(url).send().await.unwrap();
            assert_eq!(response.status(), 200);
            response.json::<Value>().await.unwrap()
        }
    };

    let list = get(format!(
        "{base}/api/v1/programs?category={}&per_page=100",
        c.category
    ))
    .await;
    let listed = ids(&list, "items");
    // Audit M5: an anonymous viewer has answered no flag, so the disability
    // tier is not demoted for them.
    assert_eq!(list["total"], 3, "{list}");
    for shown in [c.fit, c.unknown, c.disability] {
        assert!(listed.contains(&shown.to_string()), "{list}");
    }
    // Review R-M5: a tier shown only because its flag is unanswered sorts
    // after the other rows of the default list.
    assert_eq!(listed.last(), Some(&c.disability.to_string()), "{list}");
    let item = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == c.fit.to_string())
        .unwrap();
    assert_eq!(item["youth_fit"], "fit");
    assert!(item["youth_fit_label"].is_null());

    // An explicit search keeps every row; demoted ones come after the rest.
    let search = get(format!(
        "{base}/api/v1/programs?q={}&category={}&per_page=100",
        c.token, c.category
    ))
    .await;
    let found = ids(&search, "items");
    assert_eq!(found.len(), 5, "{search}");
    let first_demoted = found
        .iter()
        .position(|id| *id == c.off.to_string() || *id == c.child.to_string())
        .unwrap();
    assert!(found[..first_demoted].contains(&c.fit.to_string()));
    assert!(found[..first_demoted].contains(&c.unknown.to_string()));
    assert!(found[..first_demoted].contains(&c.disability.to_string()));
    assert_eq!(first_demoted, 3, "{search}");
    let off = search["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == c.off.to_string())
        .unwrap();
    assert_eq!(
        (off["youth_fit"].as_str(), off["youth_fit_label"].as_str()),
        (Some("off_target"), Some("청년 대상이 아닐 수 있음"))
    );

    // A direct link still works and carries the label.
    let detail = get(format!("{base}/api/v1/programs/{}", c.child)).await;
    assert_eq!(detail["youth_fit"], "child_only");
    assert_eq!(detail["youth_fit_label"], "아동·청소년 대상일 수 있음");

    // A signed-in user with the disability flag sees disability rows in the
    // default list; so do an anonymous viewer and an invalid token, who
    // answered no flag (audit M5).
    let (user, token, _) = create_test_user(&base).await;
    // 21 years old, so child rows (shown to likely parents) stay out.
    sqlx::query(
        "UPDATE user_profiles SET has_disability_answer = true, has_disability = true, birth_year = $2 WHERE user_id = $1",
    )
    .bind(user)
    .bind(chrono::Datelike::year(&Utc::now()) - 21)
    .execute(&pool)
    .await
    .unwrap();
    let url = format!(
        "{base}/api/v1/programs?category={}&per_page=100",
        c.category
    );
    let response = client.get(&url).bearer_auth(&token).send().await.unwrap();
    assert_eq!(response.headers()["cache-control"], "private, no-store");
    assert_eq!(response.headers()["vary"], "Authorization");
    let mine: Value = response.json().await.unwrap();
    assert_eq!(mine["total"], 3, "{mine}");
    assert!(ids(&mine, "items").contains(&c.disability.to_string()));
    let forged: Value = client
        .get(&url)
        .bearer_auth("not-a-token")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(forged["total"], 3, "{forged}");

    // A reviewed verdict wins over the computed one.
    sqlx::query("UPDATE program_youth_fit SET reviewed_verdict='fit', reviewed_target=NULL, reviewed_by='test', reviewed_at=now() WHERE program_id=$1")
        .bind(c.off).execute(&pool).await.unwrap();
    let list = get(url).await;
    assert!(ids(&list, "items").contains(&c.off.to_string()), "{list}");

    sqlx::query("DELETE FROM programs WHERE program_type = $1")
        .bind(&c.category)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn recommendations_drop_demoted_rows_unless_the_profile_fits_the_tier() {
    let base = start_test_server().await;
    let pool = pool().await;
    let c = catalogue(&pool).await;
    let client = Client::new();
    let year = chrono::Datelike::year(&Utc::now());
    let listed = |birth_year: i32, disability: bool| {
        let client = client.clone();
        let base = base.clone();
        async move {
            let body: Value = client
                .post(format!("{base}/api/v1/recommend/preview?scope=all"))
                .json(&json!({"birth_year": birth_year, "region_code": "busan", "has_disability": disability}))
                .send().await.unwrap().json().await.unwrap();
            body["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|i| i["program_id"].as_str().unwrap().to_owned())
                .collect::<Vec<_>>()
        }
    };
    let young = listed(year - 21, false).await;
    for shown in [c.fit, c.unknown] {
        assert!(young.contains(&shown.to_string()));
    }
    for hidden in [c.off, c.child, c.disability] {
        assert!(
            !young.contains(&hidden.to_string()),
            "{hidden} listed for a 21-year-old"
        );
    }
    // 25+ may be a parent; the disability flag lifts the disability tier.
    let parent = listed(year - 31, false).await;
    assert!(parent.contains(&c.child.to_string()));
    assert!(!parent.contains(&c.off.to_string()));
    let disabled = listed(year - 21, true).await;
    assert!(disabled.contains(&c.disability.to_string()));
    assert!(!disabled.contains(&c.child.to_string()));

    // Verifier gaps 9, 10 (audit M5 and its twin, end to end): with the flag
    // unanswered the disability tier is listed, never pass or verified, and
    // asks for the flag; an answered "no" (above) leaves it out.
    let body: Value = client
        .post(format!("{base}/api/v1/recommend/preview?scope=all"))
        .json(&json!({"birth_year": year - 21, "region_code": "busan"}))
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
        .find(|i| i["program_id"] == c.disability.to_string())
        .unwrap_or_else(|| panic!("disability tier not listed: {body}"));
    assert_eq!(item["eligibility_status"], "unknown", "{item}");
    assert_eq!(item["verified"], false, "{item}");
    assert!(
        item["missing_checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c == "장애 여부 확인 필요"),
        "{item}"
    );

    sqlx::query("DELETE FROM programs WHERE program_type = $1")
        .bind(&c.category)
        .execute(&pool)
        .await
        .unwrap();
}

/// The default list exempts exactly what the recommendations exempt for
/// the signed-in viewer: business rows for the self-employed, child rows
/// for a likely parent (household of 3+), veteran rows for a veteran family.
#[tokio::test]
async fn default_list_keeps_the_viewers_own_tiers() {
    let base = start_test_server().await;
    let pool = pool().await;
    let c = catalogue(&pool).await;
    let business = program(
        &pool,
        &c.category,
        &format!("{} 경영 지원", c.token),
        Some(("business_only", Some("business"))),
    )
    .await;
    let veteran = program(
        &pool,
        &c.category,
        &format!("{} 명예수당", c.token),
        Some(("veteran_only", Some("veteran"))),
    )
    .await;
    let (user, token, _) = create_test_user(&base).await;
    sqlx::query("UPDATE user_profiles SET birth_year = $2, household_size = 3, employment_status = 'self_employed', veteran_family_answer = true, veteran_family = true WHERE user_id = $1")
        .bind(user)
        .bind(chrono::Datelike::year(&Utc::now()) - 21)
        .execute(&pool).await.unwrap();
    let url = format!(
        "{base}/api/v1/programs?category={}&per_page=100",
        c.category
    );
    let client = Client::new();
    let mine: Value = client
        .get(&url)
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let listed = ids(&mine, "items");
    // The saved has_disability = false is an unanswered flag (audit M5).
    for shown in [c.fit, c.unknown, c.child, business, veteran, c.disability] {
        assert!(
            listed.contains(&shown.to_string()),
            "{shown} missing: {mine}"
        );
    }
    assert!(
        !listed.contains(&c.off.to_string()),
        "{} listed: {mine}",
        c.off
    );
    let anonymous: Value = client.get(&url).send().await.unwrap().json().await.unwrap();
    assert_eq!(anonymous["total"], 4, "{anonymous}");
    sqlx::query("DELETE FROM programs WHERE program_type = $1")
        .bind(&c.category)
        .execute(&pool)
        .await
        .unwrap();
}

/// Assistant search labels a demoted row only when it is listed because the
/// question asked for its target, not when the user's own profile keeps it.
#[tokio::test]
async fn assistant_search_labels_only_rows_the_question_asked_for() {
    let base = start_test_server().await;
    let pool = pool().await;
    let marker = format!("검증{}", &Uuid::new_v4().simple().to_string()[..6]);
    let category = format!("yfit_{}", &Uuid::new_v4().simple().to_string()[..8]);
    let child = program(
        &pool,
        &category,
        &format!("{marker} 교복 지원"),
        Some(("child_only", Some("child"))),
    )
    .await;
    // More fit rows than MAX_RESULTS that match one question word only: the
    // asked-for row matches more words and must not be cut behind them.
    for i in 0..6 {
        program(
            &pool,
            &category,
            &format!("자녀 장학 지원 {category}{i}"),
            None,
        )
        .await;
    }
    let client = Client::new();
    let search = |token: String, question: String| {
        let client = client.clone();
        let base = base.clone();
        async move {
            let body: Value = client
                .post(format!("{base}/api/v1/assistant/search"))
                .bearer_auth(token)
                .json(&json!({ "question": question }))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            body["results"]
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["program_id"] == child.to_string())
                .map(|r| r.get("youth_fit_label").cloned().unwrap_or(Value::Null))
        }
    };
    // A 21-year-old asking for children's benefits: listed, labelled.
    let (young, young_token, _) = create_test_user(&base).await;
    sqlx::query("UPDATE user_profiles SET birth_year = $2 WHERE user_id = $1")
        .bind(young)
        .bind(chrono::Datelike::year(&Utc::now()) - 21)
        .execute(&pool)
        .await
        .unwrap();
    let asked = search(young_token.clone(), format!("부산 {marker} 자녀 교복")).await;
    assert_eq!(asked, Some(json!("아동·청소년 대상일 수 있음")));
    // Without the request the row is not listed at all.
    assert_eq!(search(young_token, format!("부산 {marker}")).await, None);
    // A likely parent (household of 3+) gets it as a normal result.
    let (parent, parent_token, _) = create_test_user(&base).await;
    sqlx::query("UPDATE user_profiles SET household_size = 3 WHERE user_id = $1")
        .bind(parent)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        search(parent_token, format!("부산 {marker}")).await,
        Some(Value::Null)
    );
    sqlx::query("DELETE FROM programs WHERE program_type = $1")
        .bind(&category)
        .execute(&pool)
        .await
        .unwrap();
}
