//! F6.2a: required documents split from the notice's own documents field.
use super::*;

#[tokio::test]
async fn detail_and_checklist_list_the_notice_documents_when_none_are_curated() {
    let base = start_test_server().await;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .unwrap();
    let id = Uuid::new_v4();
    let text = "○ 장학생 신청서○ 재학증명서○ 통장 사본(선택)※ 자세한 사항은 홈페이지 참고";
    sqlx::query("INSERT INTO programs(id,program_type,source_type,source_id,title,program_status,regions,is_active,submission_documents) VALUES($1,'scholarship','manual',$2,'서류 분리 테스트','active','{}',true,$3)")
        .bind(id).bind(id.to_string()).bind(text)
        .execute(&pool).await.unwrap();
    let client = Client::new();
    let detail: Value = client
        .get(format!("{base}/api/v1/programs/{id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let docs = detail["required_documents"].as_array().unwrap();
    let names: Vec<&str> = docs
        .iter()
        .map(|d| d["document_name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["장학생 신청서", "재학증명서", "통장 사본(선택)"]);
    assert!(docs.iter().all(|d| d["source"] == "official_text"));
    assert_eq!(docs[2]["is_required"], false);
    assert_eq!(docs[0]["is_required"], Value::Null);
    assert_eq!(
        detail["required_document_notes"],
        json!(["자세한 사항은 홈페이지 참고"])
    );
    assert_eq!(detail["submission_documents"], text);
    // Stable ids across reads.
    let again: Value = client
        .get(format!("{base}/api/v1/programs/{id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(again["required_documents"][0]["id"], docs[0]["id"]);

    // The checklist has one task per required (not 선택) item.
    let (_, token, _) = create_test_user(&base).await;
    let plan: Value = client
        .get(format!("{base}/api/v1/programs/{id}/preparation"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let tasks: Vec<&str> = plan["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["id"].as_str().unwrap())
        .collect();
    assert!(tasks.contains(&"document:장학생 신청서"), "{tasks:?}");
    assert!(tasks.contains(&"document:재학증명서"));
    assert!(!tasks.contains(&"document:통장 사본(선택)"));
    assert!(!tasks.contains(&"documents:confirm"));

    // A curated row wins over the split text.
    sqlx::query("INSERT INTO program_documents(program_id,document_name,is_required,sort_order) VALUES($1,'주민등록등본',true,1)")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let curated: Value = client
        .get(format!("{base}/api/v1/programs/{id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(curated["required_documents"].as_array().unwrap().len(), 1);
    assert_eq!(curated["required_documents"][0]["source"], "curated");
    // Leave no active fixture behind: other tests rank the shared catalogue.
    sqlx::query("UPDATE programs SET is_active = false WHERE id = ANY($1)")
        .bind(vec![id])
        .execute(&pool)
        .await
        .unwrap();
}

/// 보조금24 rows carry 구비서류 from the serviceDetail row. A list is split
/// and cited; "해당없음" is the explicit none-required state (not missing
/// data); no text keeps today's unknown copy.
#[tokio::test]
async fn gov24_documents_list_none_required_and_unknown() {
    let base = start_test_server().await;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .unwrap();
    let client = Client::new();
    let listed_text =
        "○ 신청인 제출서류\n- 신분증\n- 지원신청서\n○ 공무원 확인 서류\n- 주민등록등본";
    let mut ids = Vec::new();
    for (text, curated) in [
        (Some(listed_text), false),
        (Some("해당없음"), false),
        (None, false),
        (Some("해당없음"), true),
    ] {
        let id = Uuid::new_v4();
        let raw = json!({"서비스ID": id.to_string(), "서비스명": "구비서류 테스트", "detail": {"구비서류": text}});
        sqlx::query("INSERT INTO programs(id,program_type,source_type,source_id,title,program_status,regions,is_active,submission_documents,raw_payload) VALUES($1,'welfare','gov_benefits',$2,'구비서류 테스트','active','{}',true,$3,$4)")
            .bind(id).bind(id.to_string()).bind(text).bind(raw)
            .execute(&pool).await.unwrap();
        if curated {
            sqlx::query("INSERT INTO program_documents(program_id,document_name,is_required,sort_order) VALUES($1,'주민등록등본',true,1)")
                .bind(id).execute(&pool).await.unwrap();
        }
        ids.push(id);
    }
    let detail = |id: Uuid| {
        let client = client.clone();
        let base = base.clone();
        async move {
            client
                .get(format!("{base}/api/v1/programs/{id}"))
                .send()
                .await
                .unwrap()
                .json::<Value>()
                .await
                .unwrap()
        }
    };

    let listed = detail(ids[0]).await;
    let names: Vec<&str> = listed["required_documents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["document_name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["신분증", "지원신청서", "주민등록등본"]);
    assert_eq!(listed["no_documents_required"], false);
    let cited = listed["evidence"]
        .as_array()
        .is_some_and(|e| e.iter().any(|e| e["check"] == "documents"));
    assert!(cited, "the detail row's text is the citation: {listed}");

    let none = detail(ids[1]).await;
    assert_eq!(none["required_documents"], json!([]));
    assert_eq!(none["no_documents_required"], true);

    let unknown = detail(ids[2]).await;
    assert_eq!(unknown["required_documents"], json!([]));
    assert_eq!(unknown["no_documents_required"], false);

    // A curated row wins: the flag only speaks for an empty checklist.
    let curated = detail(ids[3]).await;
    assert_eq!(curated["required_documents"].as_array().unwrap().len(), 1);
    assert_eq!(curated["no_documents_required"], false);

    // The preparation checklist says the same, not "확인된 서류 목록이 아직 없어요".
    let (_, token, _) = create_test_user(&base).await;
    let plan: Value = client
        .get(format!("{base}/api/v1/programs/{}/preparation", ids[1]))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let confirm = plan["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == "documents:confirm")
        .expect("documents task");
    assert!(
        confirm["detail"]
            .as_str()
            .unwrap()
            .starts_with("공고상 별도 제출 서류 없음"),
        "{confirm}"
    );
    assert_eq!(confirm["kind"], "information_check");
    sqlx::query("UPDATE programs SET is_active = false WHERE id = ANY($1)")
        .bind(&ids)
        .execute(&pool)
        .await
        .unwrap();
}
