//! 서류 작성 도우미 field definitions: admin-only writes bound to the verified
//! template hash; public reads only for a published, unchanged PDF template.
use super::*;

pub(super) async fn verified_template(
    pool: &sqlx::PgPool,
    base: &str,
    token: &str,
    format: &str,
    hash: &str,
) -> (Uuid, Uuid) {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO programs(id,program_type,source_type,title,program_status,is_active,official_url,application_method) VALUES($1,'policy','manual','2026년 서류 작성 도우미 fixture','active',true,'https://official.example.org/notice?intake=2026','방문신청')")
        .bind(id).execute(pool).await.unwrap();
    let client = Client::new();
    let row: Value = client.post(format!("{base}/api/v1/admin/programs/{id}/application-resources")).bearer_auth(token)
        .json(&json!({"kind":"template","url":format!("https://official.example.org/download?file={format}"),"source_url":"https://official.example.org/notice?intake=2026","label":format!("신청서.{format}")}))
        .send().await.unwrap().json().await.unwrap();
    let rid = Uuid::parse_str(row["id"].as_str().unwrap()).unwrap();
    // Simulated bounded worker observation; no external site called by tests.
    sqlx::query("UPDATE program_application_resources SET content_hash=$2,source_hash=$3,file_format=$4,byte_size=1024,checked_at=now() WHERE id=$1")
        .bind(rid).bind(hash).bind("b".repeat(64)).bind(format).execute(pool).await.unwrap();
    let listing: Value = client
        .get(format!(
            "{base}/api/v1/admin/programs/{id}/application-resources"
        ))
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let row = listing["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == json!(rid))
        .unwrap()
        .clone();
    let approved = client.put(format!("{base}/api/v1/admin/application-resources/{rid}")).bearer_auth(token)
        .json(&json!({"decision":"verified","expected_revision":row["revision"],"program_fingerprint":listing["program_fingerprint"],
            "access":"direct","same_program_confirmed":true,"current_intake_confirmed":true,"destination_confirmed":true,
            "note":"공식 공고의 2026년 모집 회차 신청서 양식 파일을 대조한 테스트 근거입니다.","validity_hours":24}))
        .send().await.unwrap();
    assert_eq!(approved.status(), 200, "{}", approved.text().await.unwrap());
    (id, rid)
}

fn definition(hash: &str) -> Value {
    json!({"version":1,"content_hash":hash,"page_count":1,"source":"admin","fields":[
        {"id":"name","page":0,"rect":{"x":0.2,"y":0.2,"w":0.3,"h":0.03},"type":"text","label":"성명","mapping":"name","required":true},
        {"id":"rrn","page":0,"rect":{"x":0.2,"y":0.25,"w":0.3,"h":0.03},"type":"text","label":"주민등록번호","mapping":"rrn","required":true},
        {"id":"sign","page":0,"rect":{"x":0.6,"y":0.8,"w":0.2,"h":0.05},"type":"signature","label":"서명","mapping":"signature","required":true}]})
}

#[tokio::test]
async fn form_templates_are_admin_only_hash_bound_and_hidden_when_the_form_changes() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let base = start_test_server().await;
    let client = Client::new();
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .unwrap();
    let (admin, admin_token) = create_test_admin(&base).await;
    let (user, token, _) = create_test_user(&base).await;
    let hash = "c".repeat(64);
    let (id, rid) = verified_template(&pool, &base, &admin_token, "pdf", &hash).await;
    let admin_url = format!("{base}/api/v1/admin/application-resources/{rid}/form-template");
    let public_url =
        format!("{base}/api/v1/programs/{id}/application-resources/{rid}/form-template");

    // Admin-only writes and reads.
    assert_eq!(client.get(&admin_url).send().await.unwrap().status(), 401);
    let put = |t: &str, body: Value| client.put(&admin_url).bearer_auth(t).json(&body);
    assert_eq!(
        put(
            &token,
            json!({"template":definition(&hash),"expected_revision":null})
        )
        .send()
        .await
        .unwrap()
        .status(),
        403
    );
    assert_eq!(
        client
            .delete(&admin_url)
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    // No definition yet: the app falls back to AcroForm/guided mode.
    assert_eq!(client.get(&public_url).send().await.unwrap().status(), 404);

    // Boxes drawn on other bytes than the verified template are refused.
    let other = put(
        &admin_token,
        json!({"template":definition(&"d".repeat(64)),"expected_revision":null}),
    )
    .send()
    .await
    .unwrap();
    assert_eq!(other.status(), 409);
    let mut bad = definition(&hash);
    bad["fields"][0]["rect"]["x"] = json!(0.95);
    assert_eq!(
        put(
            &admin_token,
            json!({"template":bad,"expected_revision":null})
        )
        .send()
        .await
        .unwrap()
        .status(),
        400
    );
    // A definition can never carry a user's answer.
    let mut with_value = definition(&hash);
    with_value["fields"][0]["value"] = json!("홍길동");
    assert!(put(
        &admin_token,
        json!({"template":with_value,"expected_revision":null})
    )
    .send()
    .await
    .unwrap()
    .status()
    .is_client_error());

    let saved = put(
        &admin_token,
        json!({"template":definition(&hash),"expected_revision":null}),
    )
    .send()
    .await
    .unwrap();
    assert_eq!(saved.status(), 200, "{}", saved.text().await.unwrap());
    // A second editor working from a stale revision loses, not overwrites.
    assert_eq!(
        put(
            &admin_token,
            json!({"template":definition(&hash),"expected_revision":null})
        )
        .send()
        .await
        .unwrap()
        .status(),
        409
    );
    let resaved: Value = put(
        &admin_token,
        json!({"template":definition(&hash),"expected_revision":1}),
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(resaved["revision"], 2);

    let public = client.get(&public_url).send().await.unwrap();
    assert_eq!(public.status(), 200);
    let body: Value = public.json().await.unwrap();
    assert_eq!(body["content_hash"], hash);
    assert_eq!(body["fields"].as_array().unwrap().len(), 3);
    assert!(body.get("updated_by").is_none() && body.get("revision").is_none());
    // Wrong program/resource pairing reveals nothing.
    assert_eq!(
        client
            .get(format!(
                "{base}/api/v1/programs/{}/application-resources/{rid}/form-template",
                Uuid::new_v4()
            ))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );

    // Financial-product applications are excluded on the server as well.
    sqlx::query("UPDATE programs SET program_type='financial_product' WHERE id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(client.get(&public_url).send().await.unwrap().status(), 404);
    assert_eq!(
        put(
            &admin_token,
            json!({"template":definition(&hash),"expected_revision":2})
        )
        .send()
        .await
        .unwrap()
        .status(),
        400
    );
    sqlx::query("UPDATE programs SET program_type='policy' WHERE id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(client.get(&public_url).send().await.unwrap().status(), 200);

    // The official file changes: stale boxes are no longer served.
    sqlx::query("UPDATE program_application_resources SET content_hash=$2 WHERE id=$1")
        .bind(rid)
        .bind("e".repeat(64))
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(client.get(&public_url).send().await.unwrap().status(), 404);
    let admin_view: Value = client
        .get(&admin_url)
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(admin_view["stale"], true);
    assert_eq!(admin_view["template"]["content_hash"], hash);

    // An HWP template takes boxes only through a converted PDF (see the derived test).
    let (hwp_program, hwp) =
        verified_template(&pool, &base, &admin_token, "hwp", &"f".repeat(64)).await;
    let hwp_put = client
        .put(format!(
            "{base}/api/v1/admin/application-resources/{hwp}/form-template"
        ))
        .bearer_auth(&admin_token)
        .json(&json!({"template":definition(&"f".repeat(64)),"expected_revision":null}))
        .send()
        .await
        .unwrap();
    assert_eq!(hwp_put.status(), 400);
    // A PDF template cannot claim to be derived from something else.
    let mut claims_derived = definition(&hash);
    claims_derived["derived"] = json!({"source_sha256":"f".repeat(64),"converter":"rhwp 0.8.6"});
    sqlx::query("UPDATE program_application_resources SET content_hash=$2 WHERE id=$1")
        .bind(rid)
        .bind(&hash)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        put(
            &admin_token,
            json!({"template":claims_derived,"expected_revision":2})
        )
        .send()
        .await
        .unwrap()
        .status(),
        400
    );

    assert_eq!(
        client
            .delete(&admin_url)
            .bearer_auth(&admin_token)
            .send()
            .await
            .unwrap()
            .status(),
        204
    );
    let rls:(bool,bool)=sqlx::query_as("SELECT relrowsecurity,relforcerowsecurity FROM pg_class WHERE oid='application_form_templates'::regclass").fetch_one(&pool).await.unwrap();
    assert_eq!(rls, (true, true));
    for program in [id, hwp_program] {
        sqlx::query("DELETE FROM programs WHERE id=$1")
            .bind(program)
            .execute(&pool)
            .await
            .unwrap();
    }
    for account in [admin, user] {
        sqlx::query("DELETE FROM users WHERE id=$1")
            .bind(account)
            .execute(&pool)
            .await
            .unwrap();
    }
}

/// HWP/HWPX: Mazimi serves a PDF converted from the verified official file.
/// The boxes are bound to that PDF and, through it, to the official hash.
#[tokio::test]
async fn derived_pdf_templates_are_bound_to_the_official_hwp_and_served_by_mazimi() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let base = start_test_server().await;
    let client = Client::new();
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .unwrap();
    let (admin, admin_token) = create_test_admin(&base).await;
    let (user, token, _) = create_test_user(&base).await;
    let source = "1".repeat(64);
    let (id, rid) = verified_template(&pool, &base, &admin_token, "hwp", &source).await;
    let pdf_url = format!("{base}/api/v1/admin/application-resources/{rid}/form-pdf");
    let admin_url = format!("{base}/api/v1/admin/application-resources/{rid}/form-template");
    let public_url =
        format!("{base}/api/v1/programs/{id}/application-resources/{rid}/form-template");
    let public_pdf = format!("{base}/api/v1/programs/{id}/application-resources/{rid}/form-pdf");
    let converter = "rhwp 0.8.6 export-pdf (svg); image sha256:95b7e1d8";
    let pdf: Vec<u8> = b"%PDF-1.7\n% blank official form, converted\n%%EOF\n".to_vec();
    let pdf_hash = mazimi_core::application_resources::digest(&pdf);
    let upload = |t: &str, src: &str, body: Vec<u8>, ctype: &str| {
        client
            .put(&pdf_url)
            .query(&[("source_sha256", src), ("converter", converter)])
            .bearer_auth(t)
            .header("content-type", ctype)
            .body(body)
    };

    // Admin-only, PDF-only, and only for the official bytes the worker verified.
    assert_eq!(
        upload(&token, &source, pdf.clone(), "application/pdf")
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        upload(
            &admin_token,
            &source,
            b"<html>login</html>".to_vec(),
            "application/pdf"
        )
        .send()
        .await
        .unwrap()
        .status(),
        400
    );
    assert_eq!(
        upload(&admin_token, &source, pdf.clone(), "text/plain")
            .send()
            .await
            .unwrap()
            .status(),
        415
    );
    assert_eq!(
        upload(
            &admin_token,
            &"2".repeat(64),
            pdf.clone(),
            "application/pdf"
        )
        .send()
        .await
        .unwrap()
        .status(),
        409,
        "converted from other bytes than the verified official file"
    );
    let big = [
        b"%PDF-1.7\n".as_slice(),
        &vec![b' '; 8 * 1024 * 1024],
        b"%%EOF",
    ]
    .concat();
    assert_eq!(
        upload(&admin_token, &source, big, "application/pdf")
            .send()
            .await
            .unwrap()
            .status(),
        413
    );
    let saved = upload(&admin_token, &source, pdf.clone(), "application/pdf")
        .send()
        .await
        .unwrap();
    assert_eq!(saved.status(), 200, "{}", saved.text().await.unwrap());
    let saved: Value = saved.json().await.unwrap();
    assert_eq!(saved["content_hash"], pdf_hash);

    // A PDF with no boxes is not served: nothing to fill on it.
    assert_eq!(client.get(&public_pdf).send().await.unwrap().status(), 404);

    let mut derived = definition(&pdf_hash);
    derived["derived"] = json!({"source_sha256": source, "converter": converter});
    let put = |body: Value| client.put(&admin_url).bearer_auth(&admin_token).json(&body);
    // Boxes must sit on the uploaded PDF, from the current official file, by the recorded converter.
    for (field, value) in [
        ("content_hash", json!("3".repeat(64))),
        (
            "derived",
            json!({"source_sha256": "2".repeat(64), "converter": converter}),
        ),
        (
            "derived",
            json!({"source_sha256": source, "converter": "other 1.0"}),
        ),
    ] {
        let mut wrong = derived.clone();
        wrong[field] = value;
        assert_eq!(
            put(json!({"template": wrong, "expected_revision": null}))
                .send()
                .await
                .unwrap()
                .status(),
            409,
            "{field}"
        );
    }
    let ok = put(json!({"template": derived, "expected_revision": null}))
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), 200, "{}", ok.text().await.unwrap());

    let public: Value = client
        .get(&public_url)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(public["content_hash"], pdf_hash);
    assert_eq!(public["derived"]["source_sha256"], source);
    assert_eq!(public["derived"]["converter"], converter);
    let served = client.get(&public_pdf).send().await.unwrap();
    assert_eq!(served.status(), 200);
    assert_eq!(served.headers()["content-type"], "application/pdf");
    assert_eq!(served.headers()["x-content-type-options"], "nosniff");
    assert_eq!(
        served.headers()["cache-control"],
        "public, max-age=86400, immutable"
    );
    let etag = served.headers()["etag"].to_str().unwrap().to_owned();
    assert_eq!(etag, format!("\"{pdf_hash}\""));
    assert_eq!(served.bytes().await.unwrap().as_ref(), pdf.as_slice());
    // A conditional GET with the current hash is answered without the bytes.
    let revalidated = client
        .get(&public_pdf)
        .header("if-none-match", &etag)
        .send()
        .await
        .unwrap();
    assert_eq!(revalidated.status(), 304);
    assert_eq!(revalidated.headers()["etag"], etag.as_str());
    assert!(revalidated.bytes().await.unwrap().is_empty());
    let other = client
        .get(&public_pdf)
        .header("if-none-match", "\"0000\"")
        .send()
        .await
        .unwrap();
    assert_eq!(other.status(), 200);
    // Wrong program/resource pairing reveals nothing.
    assert_eq!(
        client
            .get(format!(
                "{base}/api/v1/programs/{}/application-resources/{rid}/form-pdf",
                Uuid::new_v4()
            ))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    let admin_view: Value = client
        .get(&admin_url)
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(admin_view["stale"], false);
    assert_eq!(admin_view["derived_pdf"]["content_hash"], pdf_hash);
    assert!(admin_view["derived_pdf"].get("pdf").is_none());

    // Replacing the PDF under live boxes needs the hash the admin saw; then it
    // orphans the boxes drawn on the old one.
    let other_pdf = b"%PDF-1.7\n% reconverted\n%%EOF\n".to_vec();
    assert_eq!(
        upload(&admin_token, &source, other_pdf.clone(), "application/pdf")
            .send()
            .await
            .unwrap()
            .status(),
        409,
        "silent replacement would hide published boxes"
    );
    assert_eq!(
        upload(&admin_token, &source, other_pdf.clone(), "application/pdf")
            .query(&[("expected_content_hash", "9".repeat(64))])
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    // Re-uploading the very same PDF and converter changes nothing and is allowed.
    assert_eq!(
        upload(&admin_token, &source, pdf.clone(), "application/pdf")
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(client.get(&public_pdf).send().await.unwrap().status(), 200);
    // Same bytes recorded under another converter also unbinds the boxes.
    let reconverted = client
        .put(&pdf_url)
        .query(&[
            ("source_sha256", source.as_str()),
            ("converter", "rhwp 0.9.0 other"),
        ])
        .bearer_auth(&admin_token)
        .header("content-type", "application/pdf")
        .body(pdf.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(reconverted.status(), 409);
    assert_eq!(
        upload(&admin_token, &source, other_pdf, "application/pdf")
            .query(&[("expected_content_hash", pdf_hash.as_str())])
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(client.get(&public_url).send().await.unwrap().status(), 404);
    assert_eq!(client.get(&public_pdf).send().await.unwrap().status(), 404);
    assert_eq!(
        upload(&admin_token, &source, pdf.clone(), "application/pdf")
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(client.get(&public_pdf).send().await.unwrap().status(), 200);

    // The agency replaces the official HWP: derived boxes and PDF stop being served.
    sqlx::query("UPDATE program_application_resources SET content_hash=$2 WHERE id=$1")
        .bind(rid)
        .bind("4".repeat(64))
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(client.get(&public_url).send().await.unwrap().status(), 404);
    assert_eq!(client.get(&public_pdf).send().await.unwrap().status(), 404);
    let admin_view: Value = client
        .get(&admin_url)
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(admin_view["stale"], true);
    assert_eq!(admin_view["derived_pdf"]["stale"], true);
    // And a PDF converted from the old file can no longer be uploaded.
    assert_eq!(
        upload(&admin_token, &source, pdf.clone(), "application/pdf")
            .send()
            .await
            .unwrap()
            .status(),
        409
    );

    // Financial products stay out, as for PDF templates.
    sqlx::query("UPDATE program_application_resources SET content_hash=$2 WHERE id=$1")
        .bind(rid)
        .bind(&source)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(client.get(&public_pdf).send().await.unwrap().status(), 200);
    sqlx::query("UPDATE programs SET program_type='financial_product' WHERE id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(client.get(&public_pdf).send().await.unwrap().status(), 404);
    assert_eq!(
        upload(&admin_token, &source, pdf.clone(), "application/pdf")
            .send()
            .await
            .unwrap()
            .status(),
        400
    );

    let rls: (bool, bool) = sqlx::query_as("SELECT relrowsecurity,relforcerowsecurity FROM pg_class WHERE oid='application_form_derived_pdfs'::regclass")
        .fetch_one(&pool).await.unwrap();
    assert_eq!(rls, (true, true));
    sqlx::query("DELETE FROM programs WHERE id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let left: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM application_form_derived_pdfs WHERE resource_id=$1",
    )
    .bind(rid)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(left, 0, "derived PDFs go with their resource");
    for account in [admin, user] {
        sqlx::query("DELETE FROM users WHERE id=$1")
            .bind(account)
            .execute(&pool)
            .await
            .unwrap();
    }
}

/// Checkbox groups (single/multi) survive the admin PUT and the public GET
/// unchanged; an invalid group is a 400, never a silently dropped key.
#[tokio::test]
async fn checkbox_groups_round_trip_and_invalid_groups_are_refused() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let base = start_test_server().await;
    let client = Client::new();
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .unwrap();
    let (_admin, admin_token) = create_test_admin(&base).await;
    let hash = "e".repeat(64);
    let (id, rid) = verified_template(&pool, &base, &admin_token, "pdf", &hash).await;
    let admin_url = format!("{base}/api/v1/admin/application-resources/{rid}/form-template");
    let public_url =
        format!("{base}/api/v1/programs/{id}/application-resources/{rid}/form-template");
    let mut grouped = definition(&hash);
    let fields = grouped["fields"].as_array_mut().unwrap();
    for (n, (label, group)) in [
        ("구분: 대학생", "g1"),
        ("구분: 졸업생", "g1"),
        ("관심: 주거", "g2"),
        ("관심: 일자리", "g2"),
    ]
    .into_iter()
    .enumerate()
    {
        fields.push(json!({"id":format!("c{n}"),"page":0,"rect":{"x":0.1 + 0.1 * n as f64,"y":0.5,"w":0.02,"h":0.02},
            "type":"checkbox","label":label,"mapping":null,"required":false,"group":group}));
    }
    grouped["groups"] = json!([{"id":"g1","mode":"single","label":"구분"},{"id":"g2","mode":"multi","label":"관심 분야"}]);

    for broken in [
        // A group with one option.
        {
            let mut t = grouped.clone();
            t["fields"][4]["group"] = Value::Null;
            t
        },
        // A reference to a group that does not exist.
        {
            let mut t = grouped.clone();
            t["fields"][3]["group"] = json!("g9");
            t
        },
        // A text box in a group.
        {
            let mut t = grouped.clone();
            t["fields"][0]["group"] = json!("g1");
            t
        },
        // An unknown mode.
        {
            let mut t = grouped.clone();
            t["groups"][0]["mode"] = json!("radio");
            t
        },
    ] {
        let response = client
            .put(&admin_url)
            .bearer_auth(&admin_token)
            .json(&json!({"template":broken,"expected_revision":null}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400);
    }
    let saved = client
        .put(&admin_url)
        .bearer_auth(&admin_token)
        .json(&json!({"template":grouped,"expected_revision":null}))
        .send()
        .await
        .unwrap();
    assert_eq!(saved.status(), 200, "{}", saved.text().await.unwrap());
    let public: Value = client
        .get(&public_url)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let served = &public;
    assert_eq!(served["groups"], grouped["groups"]);
    assert_eq!(served["fields"][3]["group"], json!("g1"));
    assert_eq!(served["fields"][5]["group"], json!("g2"));
    assert!(served["fields"][0].get("group").is_none());
    sqlx::query("DELETE FROM programs WHERE id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
}

/// One stored revision serves both kinds of client: a build that sends
/// `X-Mazimi-Client-Caps: form-date-parts-v1` gets the date-part boxes with
/// their part and application_date link; any other build gets them as plain
/// boxes (no part, no mapping), which it parsed before parts existed. The two
/// answers carry different ETags and vary on the header, so no cache serves
/// one client's copy to the other.
#[tokio::test]
async fn date_parts_go_only_to_clients_that_read_them() {
    let _guard = PROGRAM_CATALOG_TEST_LOCK.lock().await;
    let base = start_test_server().await;
    let client = Client::new();
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&DATABASE_URL)
        .await
        .unwrap();
    let (_admin, admin_token) = create_test_admin(&base).await;
    let hash = "f".repeat(64);
    let (id, rid) = verified_template(&pool, &base, &admin_token, "pdf", &hash).await;
    let admin_url = format!("{base}/api/v1/admin/application-resources/{rid}/form-template");
    let public_url =
        format!("{base}/api/v1/programs/{id}/application-resources/{rid}/form-template");
    let mut dated = definition(&hash);
    let fields = dated["fields"].as_array_mut().unwrap();
    for (n, (label, part)) in [
        ("작성일 연도", "year4"),
        ("작성일 월", "month"),
        ("작성일 일", "day"),
    ]
    .into_iter()
    .enumerate()
    {
        fields.push(json!({"id":format!("d{n}"),"page":0,"rect":{"x":0.3 + 0.1 * n as f64,"y":0.7,"w":0.05,"h":0.02},
            "type":"text","label":label,"mapping":"application_date","required":false,"part":part}));
    }
    let saved = client
        .put(&admin_url)
        .bearer_auth(&admin_token)
        .json(&json!({"template":dated,"expected_revision":null}))
        .send()
        .await
        .unwrap();
    assert_eq!(saved.status(), 200, "{}", saved.text().await.unwrap());

    let get = |caps: Option<&'static str>, etag: Option<String>| {
        let mut req = client.get(&public_url);
        if let Some(caps) = caps {
            req = req.header("X-Mazimi-Client-Caps", caps);
        }
        if let Some(etag) = etag {
            req = req.header("If-None-Match", etag);
        }
        req.send()
    };
    let new = get(Some("journey-v1, form-date-parts-v1"), None)
        .await
        .unwrap();
    assert_eq!(new.status(), 200);
    let varies = new
        .headers()
        .get_all("vary")
        .iter()
        .any(|v| v.to_str().unwrap().contains("X-Mazimi-Client-Caps"));
    let new_etag = new.headers()["etag"].to_str().unwrap().to_string();
    let new: Value = new.json().await.unwrap();
    assert_eq!(
        (
            new["fields"][3]["mapping"].clone(),
            new["fields"][3]["part"].clone()
        ),
        (json!("application_date"), json!("year4"))
    );
    assert_eq!(new["fields"][5]["part"], json!("day"));
    assert!(varies, "the response varies on the caps header");

    for caps in [None, Some("journey-v1")] {
        let old = get(caps, None).await.unwrap();
        assert_eq!(old.status(), 200);
        let old_etag = old.headers()["etag"].to_str().unwrap().to_string();
        assert_ne!(
            old_etag, new_etag,
            "a cached copy never crosses between the two clients"
        );
        let old: Value = old.json().await.unwrap();
        for f in &old["fields"].as_array().unwrap()[3..] {
            assert!(f.get("part").is_none(), "{f}");
            assert_eq!(f["mapping"], Value::Null, "{f}");
        }
        assert_eq!(
            old["fields"][0]["mapping"],
            json!("name"),
            "other boxes unchanged"
        );
        // Revalidation answers 304 only for that client's own copy.
        assert_eq!(
            get(caps, Some(old_etag.clone())).await.unwrap().status(),
            304
        );
        assert_eq!(
            get(Some("form-date-parts-v1"), Some(old_etag))
                .await
                .unwrap()
                .status(),
            200
        );
    }
    assert_eq!(
        get(Some("form-date-parts-v1"), Some(new_etag.clone()))
            .await
            .unwrap()
            .status(),
        304
    );
    assert_eq!(get(None, Some(new_etag)).await.unwrap().status(), 200);
    sqlx::query("DELETE FROM programs WHERE id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
}
