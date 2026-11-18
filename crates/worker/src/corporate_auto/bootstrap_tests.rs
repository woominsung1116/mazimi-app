use super::*;
use crate::corporate_auto::evidence::compact;
use crate::corporate_auto::http::LoopbackHttp;
use std::time::Duration;

fn today() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 10, 18).unwrap()
}

fn row(url: &str, domain: &str, brand: Option<&str>) -> CandidateRow {
    CandidateRow {
        id: "c1".into(),
        url: url.into(),
        domain_id: None,
        domain: domain.into(),
        organization_name: brand.map(str::to_string),
        brand_id: None,
        brand_name: brand.map(str::to_string),
        compliance_status: Some("pending".into()),
        policy_notes: None,
        allow_subdomains: Some(false),
        verification_status: Some("verified".into()),
        domain_active: Some(true),
        page_title: None,
        campus_programme: None,
        university_domain: None,
        scope: Default::default(),
    }
}

/// Pads a fixture past the JS-shell threshold with neutral visible text.
fn page(body: &str) -> String {
    let filler =
        "<p>매장 안내와 고객센터 운영 시간, 개인정보 처리방침 및 이용 안내 문구가 이어집니다.</p>"
            .repeat(12);
    format!("<html><head><title>T</title></head><body>{body}{filler}</body></html>")
}

// ── policy before any request ────────────────────────────────────────────

#[test]
fn precheck_rejects_out_of_scope_and_non_first_party_urls() {
    let ok = row(
        "https://www.nike.com/kr/student",
        "www.nike.com",
        Some("나이키"),
    );
    assert!(precheck(&ok).is_ok());

    let campus = row("https://www.pusan.ac.kr/notice/1", "pusan.ac.kr", None);
    assert_eq!(precheck(&campus).unwrap_err(), "campus_out_of_scope");

    let mut rejected = ok.clone();
    rejected.compliance_status = Some("rejected".into());
    assert_eq!(
        precheck(&rejected).unwrap_err(),
        "domain_compliance_rejected"
    );

    let mut terms = ok.clone();
    terms.policy_notes =
        Some("robots allows indexing but service terms restrict commercial activity".into());
    assert_eq!(precheck(&terms).unwrap_err(), "prior_review_terms_restrict");

    let off = row("https://shop.example.com/x", "www.nike.com", Some("나이키"));
    assert_eq!(precheck(&off).unwrap_err(), "off_brand_domain");

    let mut sub = row("https://m.nike.com/kr/student", "nike.com", Some("나이키"));
    assert_eq!(precheck(&sub).unwrap_err(), "off_brand_domain");
    sub.allow_subdomains = Some(true);
    assert!(precheck(&sub).is_ok());

    let plain = row("http://www.nike.com/kr", "www.nike.com", Some("나이키"));
    assert_eq!(precheck(&plain).unwrap_err(), "not_https");

    let archive = row(
        "https://web.archive.org/web/2024/https://www.cjone.com/",
        "web.archive.org",
        Some("CJ ONE"),
    );
    assert_eq!(precheck(&archive).unwrap_err(), "archive_or_aggregator");
}

#[test]
fn user_repositories_on_a_brand_host_are_not_the_brand_page() {
    let repo = row(
        "https://github.com/someone/everytime_clone",
        "github.com",
        Some("GitHub"),
    );
    assert_eq!(precheck(&repo).unwrap_err(), "third_party_content");
    let edu = row(
        "https://github.com/education/students",
        "github.com",
        Some("GitHub"),
    );
    assert!(precheck(&edu).is_ok());
}

#[test]
fn bootstrap_identity_is_the_honest_bot() {
    assert_eq!(
        bootstrap_user_agent("https://mazimi.example/crawler-policy").unwrap(),
        "MazimiBenefitBot/1.0 (+https://mazimi.example/crawler-policy)"
    );
    assert!(bootstrap_user_agent("http://insecure.example/bot").is_err());
}

// ── visible lines ────────────────────────────────────────────────────────

#[test]
fn lines_are_contiguous_runs_of_the_visible_text_and_skip_hidden_text() {
    let html = r#"<html><body><h2>학생 할인</h2><p>학생 인증 후 <b>10% 할인</b> 혜택을 드립니다.</p>
        <div style="display:none">숨은 학생 90% 할인</div><p aria-hidden="true">가려진 문구</p>
        <script>var x = "학생 50% 할인";</script><ul><li>30일마다 인증 갱신</li></ul></body></html>"#;
    let lines = visible_lines(html);
    assert_eq!(
        lines,
        vec![
            "학생 할인".to_string(),
            "학생 인증 후 10% 할인 혜택을 드립니다.".to_string(),
            "30일마다 인증 갱신".to_string(),
        ]
    );
    assert_eq!(compact(&lines.join(" ")), compact(&visible_text(html)));
}

#[test]
fn quotes_never_exceed_the_cap_and_keep_the_fact() {
    let long = format!(
        "{}학생은 모든 앱 65% 할인{}",
        "가".repeat(150),
        "나".repeat(150)
    );
    let start = long.find("65%").unwrap();
    let quote = quote_around(&long, start, start + "65% 할인".len());
    assert!(quote.chars().count() <= MAX_QUOTE_CHARS);
    assert!(quote.contains("65% 할인"));
}

// ── offer reading ────────────────────────────────────────────────────────

#[test]
fn a_student_discount_page_yields_one_offer_with_verbatim_quotes() {
    let html = page(
        "<h1>학생 할인 프로그램</h1><p>학생 인증 후 10% 할인 혜택을 제공해 드립니다.</p>\
         <p>SheerID를 통해 학생 자격을 확인합니다.</p><p>30일마다 인증 갱신 시, 할인 쿠폰이 재발급됩니다.</p>",
    );
    let offer = extract_offer(&html, today()).unwrap();
    let percent = offer
        .facts
        .iter()
        .find(|f| f.kind == "discount_percent")
        .unwrap();
    assert_eq!(percent.value, json!(10));
    assert!(percent.quote.contains("10% 할인"));
    assert!(percent.quote.chars().count() <= MAX_QUOTE_CHARS);
    assert_eq!(offer.verification.as_ref().unwrap().value, json!("sheerid"));
    assert!(offer.audience.contains(&"student"));
    let evidence = PageEvidence::from_html(&html);
    for fact in offer.facts.iter().chain(offer.verification.iter()) {
        assert!(evidence.contains(&fact.quote), "{}", fact.quote);
    }
}

#[test]
fn free_plans_free_periods_and_youth_age_bands_are_read() {
    let html = page("<p>Verified students get 6 months free access to all courses.</p>");
    let offer = extract_offer(&html, today()).unwrap();
    assert!(offer
        .facts
        .iter()
        .any(|f| f.kind == "free_months" && f.value == json!(6)));
    // Two free periods for students are two offers (rule 4): `two years`
    // was not read as a period before the shared value reader read words.
    let two = page("<p>Students get two years free. Verified students get 6 months free access to all courses.</p>");
    assert_eq!(
        extract_offer(&two, today()).unwrap_err(),
        "multi_offer_page"
    );

    let youth = page("<p>만 34세 이하 고객 중 아래 요금제 가입 고객은 청년 혜택으로 데이터를 2배 제공합니다.</p>");
    let offer = extract_offer(&youth, today()).unwrap();
    let age = offer.facts.iter().find(|f| f.kind == "age_band").unwrap();
    assert_eq!(age.value, json!({"min": null, "max": 34}));
    assert_eq!(offer.age, Some((None, Some(34))));
    assert_eq!(age.evidence_type, "age");

    let free =
        page("<p>Figma for Education provides free Figma access to students and educators.</p>");
    let offer = extract_offer(&free, today()).unwrap();
    assert!(offer.facts.iter().any(|f| f.kind == "free_access"));
}

#[test]
fn a_book_title_with_a_student_word_and_a_price_is_not_an_offer() {
    // aladin product page: the audience word belongs to a book title.
    let html = page(
        "<h1>대학생을 위한 한자의 이해</h1><p>정가 15,000원 판매가 13,500원 (10%, 1,500원 할인)</p>\
         <p>마일리지 750원(5%) 적립</p>",
    );
    assert_eq!(
        extract_offer(&html, today()).unwrap_err(),
        "product_listing"
    );
    // yes24 related-items rail (seen live): a discount next to another
    // book's title is not a student offer, even without a list price.
    let rail = page(
        "<li>18,000 원 (10% 할인) AI를 활용한 대학생 기초 글쓰기</li>\
         <li>13,500 원 (0% 할인) 중고등학생을 위한 표준 한국어 의사소통 2</li>",
    );
    assert_eq!(
        extract_offer(&rail, today()).unwrap_err(),
        "no_student_youth_offer"
    );
    assert_eq!(find_target("대학생을 위한 실용한자 10% 할인"), None);
    assert!(find_target("할인 대상: 대학생").is_some());
}

#[test]
fn generic_sales_and_free_templates_are_not_student_offers() {
    let sale = page("<p>전 상품 30% 할인! 이번 주말 한정 특가.</p>");
    assert_eq!(
        extract_offer(&sale, today()).unwrap_err(),
        "no_student_youth_offer"
    );
    let templates = page("<p>대학생 PPT 무료 템플릿 - 저작권 걱정 없는 디자인</p>");
    assert_eq!(
        extract_offer(&templates, today()).unwrap_err(),
        "no_student_youth_offer"
    );
}

#[test]
fn a_page_listing_many_offers_is_rejected() {
    let html = page(
        "<li>청년 요금제 10% 할인</li><li>학생 전용 20% 할인</li><li>대학생 30% 할인</li><li>청소년 40% 할인</li>",
    );
    assert_eq!(
        extract_offer(&html, today()).unwrap_err(),
        "multi_offer_page"
    );
}

/// A discount percent is read by the shared value reader with its claim
/// word anywhere in the same clause, before or after the number
/// (Ableton, 2026-10-31: `Students and teachers: save 50% on Live`). A
/// percent in another clause of the line is not a discount.
#[test]
fn a_discount_is_read_with_its_claim_on_either_side() {
    let ableton = page("<p>Students and teachers: save 50% on Live</p>");
    let offer = extract_offer(&ableton, today()).expect("Ableton's student discount");
    assert!(
        offer
            .facts
            .iter()
            .any(|f| f.kind == "discount_percent" && f.value == json!(50)),
        "{:?}",
        offer.facts
    );
    let after = page("<p>대학생 최대 30% 할인 혜택</p>");
    assert!(extract_offer(&after, today())
        .unwrap()
        .facts
        .iter()
        .any(|f| f.value == json!(30)));
    let other_clause = page("<p>대학생 10% 할인, 출석률 80% 이상 유지 시</p>");
    let facts = extract_offer(&other_clause, today()).unwrap().facts;
    assert!(facts.iter().all(|f| f.value != json!(80)), "{facts:?}");
    let no_claim = page("<p>대학생 응답자의 90%가 만족했습니다</p>");
    assert_eq!(
        extract_offer(&no_claim, today()).unwrap_err(),
        "no_student_youth_offer"
    );
}

/// A free period is read only where the free claim and the period share a
/// clause (notion.com, 2026-10-31: `There is no set end date. To stay on the
/// free Education Plan, verify your status once a year` is free with no
/// period, not a free year).
#[test]
fn a_free_period_shares_a_clause_with_the_free_claim() {
    let notion = page(
        "<p>Students get the Education Plan free. There is no set end date. To stay on the free Education Plan, verify your status once a year with your school email.</p>",
    );
    let facts = extract_offer(&notion, today()).expect("a free plan").facts;
    assert!(facts.iter().all(|f| f.kind != "free_months"), "{facts:?}");
    assert!(facts.iter().any(|f| f.kind == "free_access"), "{facts:?}");
    let gemini = page("<p>1년간 학생 요금제를 무료로 이용하세요</p>");
    assert!(extract_offer(&gemini, today())
        .unwrap()
        .facts
        .iter()
        .any(|f| f.kind == "free_months" && f.value == json!(12)));
}

/// One offer restated on many lines (gemini.google/students, 2026-10-31:
/// a free year of the student plan, said in 14 lines) is one offer: several
/// offers means several values. A page whose lines state no value at all
/// (only "free") cannot tell a restatement from a list, so the line cap
/// still holds there.
#[test]
fn an_offer_restated_on_many_lines_is_one_offer() {
    let mut body = String::new();
    for n in 0..5 {
        body.push_str(&format!(
            "<p>무료로 학생 요금제 신청하기 {}</p>",
            "!".repeat(n)
        ));
        body.push_str("<p>학생 요금제를 1년간 무료로 이용하세요</p>");
    }
    let restated = page(&body);
    let offer = extract_offer(&restated, today()).expect("one free year");
    assert!(offer
        .facts
        .iter()
        .any(|f| f.kind == "free_months" && f.value == json!(12)));
    let two_periods = page("<p>대학생은 6개월 무료</p><p>대학원생은 1년 무료</p>");
    assert_eq!(
        extract_offer(&two_periods, today()).unwrap_err(),
        "multi_offer_page"
    );
    let list = page(
        &(1..=9)
            .map(|n| format!("<p>학생 전용 쿠폰 {n}종 무료 제공</p>"))
            .collect::<String>(),
    );
    assert_eq!(
        extract_offer(&list, today()).unwrap_err(),
        "multi_offer_page"
    );
}

#[test]
fn ended_offers_are_rejected_by_wording_or_date() {
    let dated = page("<p>대학생 15% 할인 쿠폰</p><p>이벤트 기간: 2025.09.01 ~ 2025.09.30</p>");
    assert_eq!(extract_offer(&dated, today()).unwrap_err(), "offer_ended");
    let worded = page("<p>대학생 15% 할인 쿠폰</p><p>본 이벤트가 종료되었습니다.</p>");
    assert_eq!(extract_offer(&worded, today()).unwrap_err(), "offer_ended");
    let open = page("<p>대학생 15% 할인 쿠폰</p><p>이벤트 기간: 2026.10.01 ~ 2026.12.31</p>");
    let offer = extract_offer(&open, today()).unwrap();
    assert_eq!(offer.end_date, NaiveDate::from_ymd_opt(2026, 12, 31));
}

#[test]
fn js_shells_are_not_read() {
    let shell = "<html><body><div id=\"root\"></div><p>학생 10% 할인</p></body></html>";
    assert_eq!(extract_offer(shell, today()).unwrap_err(), "js_shell");
}

#[test]
fn conflicting_ages_across_quotes_are_rejected() {
    let html = page("<p>만 34세 이하 청년 요금제 가입 시 할인 혜택</p><p>만 29세 이하 청년 고객 전용 무료 쿠폰 혜택</p>");
    let result = extract_offer(&html, today());
    assert!(
        matches!(result, Err("multi_offer_page") | Err("conflicting_ages")),
        "{result:?}"
    );
}

// ── end to end against a loopback site ──────────────────────────────────

async fn serve(routes: Vec<(&'static str, u16, &'static str, String)>) -> String {
    use axum::{http::StatusCode, response::IntoResponse, routing::get, Router};
    let mut app = Router::new();
    for (path, status, content_type, body) in routes {
        app = app.route(
            path,
            get(move || {
                let body = body.clone();
                async move {
                    (
                        StatusCode::from_u16(status).unwrap(),
                        [("content-type", content_type)],
                        body,
                    )
                        .into_response()
                }
            }),
        );
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("127.0.0.1:{}", addr.port())
}

fn loopback_row(id: &str, host: &str, path: &str) -> CandidateRow {
    let mut r = row(&format!("http://{host}{path}"), host, Some("브랜드"));
    r.id = id.into();
    r
}

/// The loopback transport speaks plain http; precheck requires https, so
/// the e2e test drives `run_domain` with a precheck-free copy of the rows.
async fn run_loopback(rows: Vec<CandidateRow>) -> (DomainReport, Vec<Decision>) {
    let http = LoopbackHttp::new("MazimiBenefitBot/1.0 (+https://mazimi.kr/crawler-policy)");
    let mut session = DomainSession::new(&http, None, None, true, 0, 50)
        .with_test_spacing(Duration::from_millis(5));
    let settings = Settings {
        test_spacing: Some(Duration::from_millis(5)),
        ..Settings::default()
    };
    run_domain_unchecked(&mut session, &rows, &settings, today()).await
}

#[tokio::test]
async fn robots_disallow_challenges_and_specific_terms_stop_collection() {
    let offer = page(
        "<p>학생 인증 후 10% 할인 혜택을 제공해 드립니다.</p><footer><a href=\"/terms\">이용약관</a></footer>",
    );
    let host =
        serve(vec![
        (
            "/robots.txt",
            200,
            "text/plain",
            "User-agent: *\nDisallow: /private\n".into(),
        ),
        ("/student", 200, "text/html; charset=utf-8", offer.clone()),
        ("/private/student", 200, "text/html; charset=utf-8", offer.clone()),
        (
            "/terms",
            200,
            "text/html; charset=utf-8",
            page("<p>로봇, 스파이더 등 자동화된 수단으로 서비스에 접근하는 행위를 금지합니다.</p>"),
        ),
    ])
        .await;
    let (report, decisions) = run_loopback(vec![
        loopback_row("a", &host, "/student"),
        loopback_row("b", &host, "/private/student"),
    ])
    .await;
    assert_eq!(report.tos, "generic_automated");
    let by_id = |id: &str| {
        decisions
            .iter()
            .find(|d| d.candidate_ids.contains(&id.to_string()))
            .unwrap()
            .outcome
            .clone()
    };
    assert_eq!(by_id("a"), "proposal");
    assert_eq!(by_id("b"), "robots_disallow");

    // A terms clause that names collecting prices/events for third parties
    // stops the whole domain, and the first page is not proposed.
    let specific = serve(vec![
        ("/robots.txt", 404, "text/plain", String::new()),
        ("/student", 200, "text/html; charset=utf-8", offer.clone()),
        ("/student2", 200, "text/html; charset=utf-8", offer.clone()),
        (
            "/terms",
            200,
            "text/html; charset=utf-8",
            page("<p>크롤링 등 자동화된 방법으로 사이트의 가격 및 이벤트 정보를 수집하여 제3자에게 제공하는 행위를 금지합니다.</p>"),
        ),
    ])
    .await;
    let (report, decisions) = run_loopback(vec![
        loopback_row("a", &specific, "/student"),
        loopback_row("b", &specific, "/student2"),
    ])
    .await;
    assert_eq!(report.tos, "specific_collection");
    assert!(decisions
        .iter()
        .all(|d| d.outcome == "tos_specific_collection"));
    assert_eq!(
        report.requests, 3,
        "robots, first page, terms; nothing after"
    );

    // A challenge page stops the domain for the rest of the run (A1).
    let walled = serve(vec![
        (
            "/robots.txt",
            200,
            "text/plain",
            "User-agent: *\nAllow: /\n".into(),
        ),
        (
            "/student",
            403,
            "text/html",
            "<title>Just a moment...</title>".into(),
        ),
        ("/student2", 200, "text/html; charset=utf-8", offer),
    ])
    .await;
    let (report, decisions) = run_loopback(vec![
        loopback_row("a", &walled, "/student"),
        loopback_row("b", &walled, "/student2"),
    ])
    .await;
    assert_eq!(report.stopped, Some("page_bot_wall"));
    assert_eq!(decisions[0].outcome, "bot_wall");
    assert_eq!(decisions[1].outcome, "domain_stopped_bot_wall");
    assert_eq!(report.requests, 2);
}

#[tokio::test]
async fn run_refuses_a_disguised_user_agent() {
    let http = LoopbackHttp::new("Mozilla/5.0");
    let result = run(&http, vec![], Settings::default(), today()).await;
    assert!(result.is_err());
}

/// Campus scope is refused even under a name: university hosts, the
/// university list and campus programmes.
#[test]
fn campus_scope_is_refused_even_with_a_name() {
    let branded_campus = row(
        "https://www.pusan.ac.kr/notice/1",
        "pusan.ac.kr",
        Some("부산대학교"),
    );
    assert_eq!(
        precheck(&branded_campus).unwrap_err(),
        "campus_out_of_scope"
    );
    let edu = row("https://www.harvard.edu/x", "harvard.edu", Some("Harvard"));
    assert_eq!(precheck(&edu).unwrap_err(), "campus_out_of_scope");
    let mut listed = row(
        "https://www.example-univ.kr/x",
        "example-univ.kr",
        Some("대학"),
    );
    listed.university_domain = Some(true);
    assert_eq!(precheck(&listed).unwrap_err(), "campus_out_of_scope");
    let mut programme = row("https://www.example.com/x", "example.com", Some("예시"));
    programme.campus_programme = Some(true);
    assert_eq!(precheck(&programme).unwrap_err(), "campus_out_of_scope");
    let company = row("https://example.com/x", "example.com", Some("예시"));
    assert!(precheck(&company).is_ok(), "{:?}", precheck(&company));
    assert!(!campus_host("www.acme.kr") && campus_host("kaist.ac.kr"));
}

/// The facts one line states on its own.
fn facts_of(text: &str) -> Vec<Fact> {
    line_facts(&Line {
        text: text.into(),
        ..Line::default()
    })
    .facts
}

/// Review M11: a percentage is read only as a whole number. "1.5%" is not
/// 5%, "100%" is not 0% and "12,5%" is not 5%; such lines yield no percent.
#[test]
fn a_percentage_is_read_whole_never_from_its_last_digits() {
    let percents = |text: &str| -> Vec<Value> {
        facts_of(text)
            .into_iter()
            .filter(|f| f.kind == "discount_percent")
            .map(|f| f.value)
            .collect()
    };
    for text in [
        "학생 인증 시 1.5% 할인",
        "학생 인증 시 0.5 % 적립",
        "학생 회원 100% 할인 쿠폰",
        "학생 회원 12,5% 할인",
    ] {
        assert!(percents(text).is_empty(), "{text}: {:?}", percents(text));
    }
    assert_eq!(percents("학생 인증 시 15% 할인"), vec![json!(15)]);
    assert_eq!(percents("학생 인증 후 5% 할인 혜택"), vec![json!(5)]);
    assert_eq!(percents("학생 인증 시(10%할인)"), vec![json!(10)]);
    let fact = facts_of("학생 인증 시 15% 할인")
        .into_iter()
        .find(|f| f.kind == "discount_percent")
        .unwrap();
    assert!(fact.quote.contains("15% 할인"), "{}", fact.quote);
}

/// Review M12: the CLI fails closed. An export row without the policy
/// fields `precheck` reads (missing would read as verified and allowed) is
/// refused before any request; a run with a page it could not read writes
/// no report and leaves an earlier one untouched; a clean run writes it.
#[tokio::test]
async fn the_cli_refuses_incomplete_input_and_never_writes_a_partial_report() {
    let ok = row(
        "https://shop.example.com/student",
        "shop.example.com",
        Some("샵"),
    );
    assert!(check_input(std::slice::from_ref(&ok)).is_ok());
    for missing in [
        CandidateRow {
            verification_status: None,
            ..ok.clone()
        },
        CandidateRow {
            compliance_status: None,
            ..ok.clone()
        },
        CandidateRow {
            domain_active: None,
            ..ok.clone()
        },
    ] {
        assert!(check_input(&[ok.clone(), missing]).is_err());
    }

    let http = LoopbackHttp::new("MazimiBenefitBot/1.0 (+https://mazimi.example/crawler-policy)");
    let mut report = run(
        &http,
        vec![row("https://example.com/x", "example.com", None)],
        Settings::default(),
        today(),
    )
    .await
    .unwrap();
    assert_eq!(report.decisions[0].outcome, "campus_out_of_scope");
    let dir = std::env::temp_dir().join(format!("bootstrap-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join("proposals.json");
    write_report(&report, &out).unwrap();
    let clean = std::fs::read_to_string(&out).unwrap();
    assert!(clean.contains("campus_out_of_scope"));

    for failure in [
        "page_unreachable",
        "robots_unavailable",
        "rate_limited",
        "domain_stopped_rate_limited",
        "unexpected_reply",
        "an_outcome_added_later",
    ] {
        report.decisions[0].outcome = failure.into();
        assert!(write_report(&report, &out).is_err(), "{failure}");
        assert_eq!(std::fs::read_to_string(&out).unwrap(), clean, "{failure}");
    }
    let left: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert_eq!(left.len(), 1, "no temporary file left: {left:?}");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A link label (inline `<a>` text) names another page and is no fact
/// (kotlinlang.org: `Free educational licenses for students and teachers`
/// links to JetBrains); an `<a>` that wraps blocks is an event card, the
/// page's own content (pagoda21.com: `<a><dl><dt>카카오 톡학생증 인증 최대
/// 25% 할인</dt></dl></a>`).
#[test]
fn link_labels_are_no_fact_but_link_cards_are_content() {
    let label = page("<ul><li><a href=\"https://www.jetbrains.com/community/education/\">Free educational licenses for students and teachers</a></li></ul>");
    assert_eq!(
        extract_offer(&label, today()).unwrap_err(),
        "no_student_youth_offer"
    );
    let card = page("<a href=\"/event/1\"><dl><dt>카카오 톡학생증 인증 최대 25% 할인</dt><dd>파고다어학원 전 과목</dd></dl></a>");
    let offer = extract_offer(&card, today()).expect("the card's own offer");
    assert!(offer
        .facts
        .iter()
        .any(|f| f.kind == "discount_percent" && f.value == json!(25)));
}
