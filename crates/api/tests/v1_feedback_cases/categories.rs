//! F5.1, F5.2: provider and field categories.
use super::*;

async fn pool() -> sqlx::PgPool {
    PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .unwrap()
}

/// (program_type, source_type, provider_kind, benefit_category, title,
/// summary, raw, expected provider, expected field).
type Case = (
    &'static str,
    &'static str,
    Option<&'static str>,
    Option<&'static str>,
    &'static str,
    Option<&'static str>,
    Value,
    &'static str,
    &'static str,
);

/// Expected provider of a row on no provider tab (NULL, `'{}'`).
const NO_TAB: &str = "(none)";

fn gov(title: &'static str, field_of_service: &str) -> (Value, &'static str) {
    (json!({ "서비스분야": field_of_service }), title)
}

/// Real prod rows (2026-10-23), including the review's misfiled examples,
/// with the columns the functions read.
fn cases() -> Vec<Case> {
    let g = |title: &'static str, service: &str, provider, field| -> Case {
        let (raw, title) = gov(title, service);
        (
            "benefit",
            "gov_benefits",
            Some("government"),
            Some("benefit"),
            title,
            None,
            raw,
            provider,
            field,
        )
    };
    let y = |program_type, title, lclsf: &str, mclsf: &str, field| -> Case {
        (
            program_type,
            "youth_center",
            None,
            Some(program_type),
            title,
            None,
            json!({"lclsfNm": lclsf, "mclsfNm": mclsf}),
            "policy",
            field,
        )
    };
    let w = |program_type, title, themes: &str, field| -> Case {
        (
            program_type,
            "local_welfare",
            Some("municipality"),
            Some(program_type),
            title,
            None,
            json!({"intrsThemaNmArray": themes}),
            "policy",
            field,
        )
    };
    vec![
        g(
            "거창군장학회 장학금",
            "보육·교육",
            "scholarship",
            "education",
        ),
        g(
            "재능키움 장학서비스",
            "보육·교육",
            "scholarship",
            "education",
        ),
        g(
            "대전 전세사기피해자 지원사업 신청",
            "주거·자립",
            "policy",
            "housing",
        ),
        // 주거·자립 without a housing word is not housing.
        g("결혼축하금 지원", "주거·자립", "policy", "living"),
        g("다문화가족 정착지원", "주거·자립", "policy", "welfare"),
        g("주택 태양광시설 설치지원", "주거·자립", "policy", "living"),
        // 보육·교육: care is welfare; teaching is education.
        g("아이돌봄 서비스 지원", "보육·교육", "policy", "welfare"),
        g("아빠 육아휴직 장려금", "보육·교육", "policy", "welfare"),
        g("장애아동수당", "보육·교육", "policy", "welfare"),
        g(
            "고교입학생 등 교복구입비 지원",
            "보육·교육",
            "policy",
            "education",
        ),
        g(
            "사립유치원 장애유아 의무 교육비 지원",
            "보육·교육",
            "policy",
            "education",
        ),
        // Farm operations are everyday life; a new farmer is employment.
        g("가축 살처분 보상금", "농림축산어업", "policy", "living"),
        g("양봉기자재 지원", "농림축산어업", "policy", "living"),
        g(
            "청년농업인 영농정착 지원",
            "농림축산어업",
            "policy",
            "employment",
        ),
        g(
            "대구광역시 남구 미취업청년 자격증 응시료 지원사업",
            "고용·창업",
            "policy",
            "employment",
        ),
        (
            "benefit",
            "gov_benefits",
            Some("government"),
            Some("benefit"),
            "경제단체 우수프로그램 지원",
            Some("경제단체에서 발굴한 기업지원 사업에 대해 심의를 통해 사업비 지원"),
            json!({"서비스분야": "고용·창업"}),
            "policy",
            "living",
        ),
        // Review MEDIUM-A: job rows under 고용·창업 stay employment.
        g("국민내일배움카드", "고용·창업", "policy", "employment"),
        g("자영업자 실업급여", "고용·창업", "policy", "employment"),
        g("원어민 화상영어 교실", "보육·교육", "policy", "education"),
        g("독서실 이용료 감면", "보육·교육", "policy", "education"),
        g("어린이 전집대출 서비스", "보육·교육", "policy", "welfare"),
        g("학자금대출 이자지원", "보육·교육", "policy", "finance"),
        g("1인1나눔 계좌갖기", "생활안정", "policy", "living"),
        g("청춘가 청년주거지 조성", "주거·자립", "policy", "housing"),
        g("부마항쟁 희생자 위로금", "행정·안전", "policy", "welfare"),
        g(
            "체육시설 이용요금 감면(인조잔디구장)",
            "문화·환경",
            "policy",
            "living",
        ),
        g("예방접종관리", "행정·안전", "policy", "welfare"),
        g(
            "산모·신생아 건강관리 지원",
            "임신·출산",
            "policy",
            "welfare",
        ),
        g("국가유공자 및 유족 지원", "생활안정", "policy", "welfare"),
        g("음식물 감량기 설치 지원", "생활안정", "policy", "living"),
        g("어업경영자금 지원", "농림축산어업", "policy", "finance"),
        g(
            "동네서점 바로대출 서비스 운영",
            "문화·환경",
            "policy",
            "living",
        ),
        (
            "financial_product",
            "gov_benefits",
            Some("government"),
            Some("financial_product"),
            "내집마련 디딤돌 대출",
            None,
            json!({"서비스분야": "주거·자립"}),
            "policy",
            "finance",
        ),
        // A summary may name housing; "주거" alone in a summary does not.
        (
            "benefit",
            "gov_benefits",
            Some("government"),
            Some("benefit"),
            "청년 정착 지원",
            Some("월세 20만원 지원"),
            json!({"서비스분야": "주거·자립"}),
            "policy",
            "housing",
        ),
        (
            "benefit",
            "gov_benefits",
            Some("government"),
            Some("benefit"),
            "청소년회복지원시설운영",
            Some("상담·주거·학업 지원"),
            json!({"서비스분야": "주거·자립"}),
            "policy",
            "welfare",
        ),
        y(
            "employment",
            "대구 청년 맞춤형 고용지원서비스",
            "일자리",
            "취업",
            "employment",
        ),
        y(
            "policy",
            "반도체 특성화 대학",
            "교육･직업훈련",
            "미래역량강화",
            "education",
        ),
        y(
            "policy",
            "청년 참여 포인트 제도 (청포도)",
            "참여･기반",
            "청년참여",
            "living",
        ),
        y(
            "welfare",
            "청년 마음건강 지원",
            "금융･복지･문화",
            "건강",
            "welfare",
        ),
        y(
            "welfare",
            "금왕산단 청년문화센터 건립사업",
            "금융･복지･문화",
            "문화활동 및 생활지원",
            "living",
        ),
        // 취약계층 및 금융지원 is finance only for a financial product.
        y(
            "welfare",
            "청년 학자금 대출 이자 지원",
            "금융･복지･문화",
            "취약계층 및 금융지원",
            "finance",
        ),
        y(
            "welfare",
            "전북 청년 1인 소상공인 출산육아수당",
            "금융･복지･문화",
            "취약계층 및 금융지원",
            "welfare",
        ),
        y(
            "welfare",
            "자립준비청년 자립정착금",
            "금융･복지･문화",
            "취약계층 및 금융지원",
            "welfare",
        ),
        y(
            "housing",
            "청년 창업 공간",
            "일자리,일자리",
            "창업,취업",
            "employment",
        ),
        w("welfare", "청년 이사비용 지원사업", "서민금융", "housing"),
        w("welfare", "청년 월세 지원", "서민금융, 주거", "housing"),
        w(
            "welfare",
            "청년 구직 활동비",
            "서민금융, 일자리",
            "employment",
        ),
        w("welfare", "결혼장려금 지원", "서민금융", "welfare"),
        w(
            "welfare",
            "청년 자격증 응시료 지원",
            "서민금융, 일자리",
            "employment",
        ),
        (
            "welfare",
            "national_welfare",
            Some("government"),
            Some("welfare"),
            "아동 교육 지원",
            None,
            json!({"intrsThemaArray": "생활지원,교육"}),
            "policy",
            "education",
        ),
        (
            "welfare",
            "local_welfare",
            Some("municipality"),
            Some("welfare"),
            "전입학생 생활안정 장학금",
            None,
            json!({"intrsThemaNmArray": "교육"}),
            "scholarship",
            "education",
        ),
        (
            "scholarship",
            "scholarship",
            Some("municipality"),
            Some("scholarship"),
            "저소득장학생",
            None,
            json!({}),
            "scholarship",
            "education",
        ),
        (
            "corporate_benefit",
            "corporate_official",
            Some("corporate"),
            Some("software"),
            "GitHub Student Developer Pack",
            None,
            json!({}),
            "corporate",
            "living",
        ),
        (
            "corporate_benefit",
            "corporate_official",
            Some("corporate"),
            Some("education"),
            "Coursera 학생 할인",
            None,
            json!({}),
            "corporate",
            "education",
        ),
        (
            "corporate_benefit",
            "corporate_official",
            Some("corporate"),
            Some("software"),
            "삼성 드림 장학금",
            None,
            json!({}),
            "scholarship",
            "education",
        ),
        (
            "campus_benefit",
            "university_official",
            Some("university"),
            None,
            "도서관 무료 사물함",
            None,
            json!({}),
            // A school's own offer: only its school's 대학 tab, no provider tab.
            NO_TAB,
            "living",
        ),
        (
            "financial_product",
            "fss_financial",
            Some("corporate"),
            Some("finance"),
            "청년 우대 적금",
            None,
            json!({}),
            // A bank's deposit: 금융, not a company offer.
            NO_TAB,
            "finance",
        ),
        (
            "benefit",
            "gov_benefits",
            Some("government"),
            Some("benefit"),
            "서비스분야 없는 행",
            None,
            json!({}),
            "policy",
            "living",
        ),
        (
            "housing",
            "manual",
            Some("government"),
            Some("housing"),
            "주거 키워드 행",
            None,
            json!({}),
            "policy",
            "welfare",
        ),
    ]
}

#[tokio::test]
async fn category_functions_classify_real_rows() {
    let _ = start_test_server().await; // runs the migrations
    let pool = pool().await;
    let mut wrong = Vec::new();
    for (
        program_type,
        source_type,
        provider_kind,
        benefit_category,
        title,
        summary,
        raw,
        provider,
        field,
    ) in cases()
    {
        let (got_provider, got_field, got_tabs): (Option<String>, String, Vec<String>) = sqlx::query_as(
            "SELECT program_provider_category($1,$2,$3,$4), program_field_category($1,$5,$2,$3,$4,$6,$7),
                    program_provider_categories($1,$2,$3,$4)",
        )
        .bind(program_type)
        .bind(provider_kind)
        .bind(benefit_category)
        .bind(title)
        .bind(source_type)
        .bind(summary)
        .bind(&raw)
        .fetch_one(&pool)
        .await
        .unwrap();
        let got_provider = got_provider.unwrap_or_else(|| NO_TAB.to_owned());
        if (got_provider.as_str(), got_field.as_str()) != (provider, field) {
            wrong.push(format!(
                "{title}: {got_provider}/{got_field} (want {provider}/{field})"
            ));
        }
        let both = title == "삼성 드림 장학금";
        let expected: Vec<&str> = if both {
            vec!["scholarship", "corporate"]
        } else if provider == NO_TAB {
            vec![]
        } else {
            vec![provider]
        };
        assert_eq!(got_tabs, expected, "{title}");
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// The categories are stored per row (the trigger) and follow an update of
/// their inputs; the filter indexes exist.
#[tokio::test]
async fn categories_are_stored_and_follow_updates() {
    let _ = start_test_server().await;
    let pool = pool().await;
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO programs(id,program_type,source_type,source_id,title,program_status,regions,is_active,raw_payload) VALUES($1,'benefit','gov_benefits',$2,'결혼축하금','active','{}',false,$3)")
        .bind(id).bind(id.to_string()).bind(json!({"서비스분야": "주거·자립"}))
        .execute(&pool).await.unwrap();
    let read = || async {
        sqlx::query_as::<_, (String, Vec<String>, String)>(
            "SELECT provider_category, provider_categories, field_category FROM programs WHERE id = $1",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap()
    };
    assert_eq!(
        read().await,
        ("policy".into(), vec!["policy".into()], "living".into())
    );
    sqlx::query("UPDATE programs SET title = '청년 월세 지원' WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(read().await.2, "housing");
    sqlx::query("UPDATE programs SET title = '청년 월세 장학금' WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        read().await,
        (
            "scholarship".into(),
            vec!["scholarship".into()],
            "education".into()
        )
    );
    // Filters read indexes.
    let indexes: Vec<String> = sqlx::query_scalar(
        "SELECT indexname FROM pg_indexes WHERE tablename = 'programs' AND indexname IN ('programs_field_category','programs_provider_categories') ORDER BY 1",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        indexes,
        ["programs_field_category", "programs_provider_categories"]
    );
}

#[tokio::test]
async fn list_and_detail_serve_and_filter_categories() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let base = start_test_server().await;
    let pool = pool().await;
    let client = Client::new();
    let term = format!("cattabs{}", Uuid::new_v4().simple());
    let insert = |program_type: &'static str,
                  source_type: &'static str,
                  kind: Option<&'static str>,
                  category: Option<&'static str>,
                  title: String,
                  raw: Value| {
        let pool = pool.clone();
        async move {
            let id = Uuid::new_v4();
            sqlx::query("INSERT INTO programs(id,program_type,source_type,source_id,title,program_status,regions,is_active,provider_kind,benefit_category,raw_payload) VALUES($1,$2,$3,$4,$5,'active','{}',true,$6,$7,$8)")
                .bind(id).bind(program_type).bind(source_type).bind(id.to_string()).bind(title)
                .bind(kind).bind(category).bind(raw)
                .execute(&pool).await.unwrap();
            id
        }
    };
    let gov = insert(
        "benefit",
        "manual",
        Some("government"),
        None,
        format!("{term} 청년 정책"),
        json!({}),
    )
    .await;
    let corp_scholarship = insert(
        "corporate_benefit",
        "manual",
        Some("corporate"),
        Some("software"),
        format!("{term} 드림 장학금"),
        json!({}),
    )
    .await;
    let corp = insert(
        "corporate_benefit",
        "manual",
        Some("corporate"),
        Some("education"),
        format!("{term} 강의 할인"),
        json!({}),
    )
    .await;

    let list = |params: Vec<(&'static str, &'static str)>| {
        let client = client.clone();
        let base = base.clone();
        let term = term.clone();
        async move {
            let mut query = vec![("q", term.clone())];
            query.extend(params.into_iter().map(|(k, v)| (k, v.to_owned())));
            let response = client
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
        let mut ids: Vec<String> = body["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["id"].as_str().unwrap().to_owned())
            .collect();
        ids.sort();
        ids
    };
    let sorted = |mut v: Vec<Uuid>| {
        v.sort();
        v.into_iter().map(|id| id.to_string()).collect::<Vec<_>>()
    };

    let (status, all) = list(vec![]).await;
    assert_eq!(status, 200);
    let item = all["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == corp_scholarship.to_string())
        .unwrap();
    assert_eq!(item["provider_category"], "scholarship");
    assert_eq!(item["provider_label"], "장학금");
    assert_eq!(
        item["provider_categories"],
        json!(["scholarship", "corporate"])
    );
    assert_eq!(item["field_category"], "education");
    assert_eq!(item["field_label"], "교육");

    let (_, scholarships) = list(vec![("provider", "장학금")]).await;
    assert_eq!(ids(&scholarships), sorted(vec![corp_scholarship]));
    let (_, corporate) = list(vec![("provider", "corporate")]).await;
    assert_eq!(ids(&corporate), sorted(vec![corp_scholarship, corp]));
    let (_, policy) = list(vec![("provider", "policy")]).await;
    assert_eq!(ids(&policy), sorted(vec![gov]));
    let (_, education) = list(vec![("field", "education"), ("provider", "corporate")]).await;
    assert_eq!(education["total"], 2);
    let (_, living) = list(vec![("field", "생활")]).await;
    assert_eq!(ids(&living), sorted(vec![gov]));
    // Existing params still apply alongside.
    let (_, typed) = list(vec![
        ("category", "corporate_benefit"),
        ("field", "education"),
    ])
    .await;
    assert_eq!(typed["total"], 2);
    for bad in [("provider", "government"), ("field", "benefit")] {
        let (status, body) = list(vec![bad]).await;
        assert_eq!(status, 400, "{bad:?}");
        assert_eq!(body["code"], "invalid_category");
        assert_eq!(body["field"], bad.0);
    }

    let detail: Value = client
        .get(format!("{base}/api/v1/programs/{corp}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(detail["provider_category"], "corporate");
    assert_eq!(detail["field_category"], "education");
    assert_eq!(detail["provider_categories"], json!(["corporate"]));
    // Leave no active fixture behind: other tests rank the shared catalogue.
    sqlx::query("UPDATE programs SET is_active = false WHERE id = ANY($1)")
        .bind(vec![gov, corp_scholarship, corp])
        .execute(&pool)
        .await
        .unwrap();
}

/// Until the worker fills a row its categories are NULL: it is listed (and
/// served with null categories) only without a category filter.
#[tokio::test]
async fn rows_without_categories_are_listed_only_under_all() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let base = start_test_server().await;
    let pool = pool().await;
    let term = format!("nullcat{}", Uuid::new_v4().simple());
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO programs(id,program_type,source_type,source_id,title,program_status,regions,is_active) VALUES($1,'policy','manual',$2,$3,'active','{}',true)")
        .bind(id).bind(id.to_string()).bind(format!("{term} 정책"))
        .execute(&pool).await.unwrap();
    // As a row written before the columns existed (no trigger fires).
    sqlx::query("UPDATE programs SET provider_category = NULL, provider_categories = NULL, field_category = NULL, category_version = NULL WHERE id = $1")
        .bind(id).execute(&pool).await.unwrap();
    let get = |params: Vec<(&'static str, &'static str)>| {
        let base = base.clone();
        let term = term.clone();
        async move {
            let mut query = vec![("q", term)];
            query.extend(params.into_iter().map(|(k, v)| (k, v.to_owned())));
            Client::new()
                .get(format!("{base}/api/v1/programs"))
                .query(&query)
                .send()
                .await
                .unwrap()
                .json::<Value>()
                .await
                .unwrap()
        }
    };
    let all = get(vec![]).await;
    assert_eq!(all["total"], 1);
    assert_eq!(all["items"][0]["provider_category"], Value::Null);
    assert_eq!(all["items"][0]["field_category"], Value::Null);
    assert_eq!(all["items"][0]["provider_categories"], json!([]));
    assert_eq!(get(vec![("field", "living")]).await["total"], 0);
    assert_eq!(get(vec![("provider", "policy")]).await["total"], 0);
    let detail: Value = Client::new()
        .get(format!("{base}/api/v1/programs/{id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(detail["field_category"], Value::Null);
    sqlx::query("UPDATE programs SET is_active = false WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
}
