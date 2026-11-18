use super::*;
use crate::corporate_auto::http::LoopbackHttp;
use crate::corporate_auto::VerifyMode;
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const UA: &str = "MazimiBenefitBot/1.0 (+https://mazimi.example/crawler-policy)";
const NIKE_ID: &str = "5fa3af29-0c99-489b-8ebd-1ef063bf25b4";

type Route = (u16, &'static str, String);

#[derive(Clone, Default)]
struct Mock {
    routes: Arc<Mutex<HashMap<String, Route>>>,
    locations: Arc<Mutex<HashMap<String, String>>>,
    log: Arc<Mutex<Vec<(String, String, Instant)>>>,
}

impl Mock {
    fn redirect(&self, path: &str, status: u16, location: &str) {
        self.route(path, status, "text/html", "moved");
        self.locations
            .lock()
            .unwrap()
            .insert(path.into(), location.into());
    }
    fn route(&self, path: &str, status: u16, content_type: &'static str, body: &str) {
        self.routes
            .lock()
            .unwrap()
            .insert(path.into(), (status, content_type, body.into()));
    }
    fn hits(&self, path: &str) -> usize {
        self.log
            .lock()
            .unwrap()
            .iter()
            .filter(|(p, _, _)| p == path)
            .count()
    }
}

async fn start(mock: Mock) -> String {
    use axum::{extract::Request, response::IntoResponse, Router};
    let app = Router::new().fallback(move |request: Request| {
        let mock = mock.clone();
        async move {
            let path = request
                .uri()
                .path_and_query()
                .map(|p| p.to_string())
                .unwrap_or_default();
            let ua = request
                .headers()
                .get("user-agent")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();
            mock.log
                .lock()
                .unwrap()
                .push((path.clone(), ua, Instant::now()));
            let route = mock.routes.lock().unwrap().get(&path).cloned();
            let location = mock.locations.lock().unwrap().get(&path).cloned();
            match route {
                Some((status, content_type, body)) => {
                    let mut response = (
                        axum::http::StatusCode::from_u16(status).unwrap(),
                        [("content-type", content_type), ("retry-after", "120")],
                        body,
                    )
                        .into_response();
                    if let Some(location) = location {
                        response
                            .headers_mut()
                            .insert("location", location.parse().unwrap());
                    }
                    response
                }
                None => (axum::http::StatusCode::NOT_FOUND, "missing").into_response(),
            }
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{address}")
}

fn flags(gray_generic_tos: bool) -> Flags {
    Flags {
        census_enabled: true,
        verify_enabled: false,
        verify_mode: VerifyMode::Shadow,
        publish_enabled: false,
        weekly_audit_enabled: false,
        gray_generic_tos,
        contact_url: Some("https://mazimi.example/crawler-policy".into()),
    }
}

fn domain(gray_robots_missing: bool) -> DomainRow {
    DomainRow {
        domain_id: Uuid::new_v4(),
        domain: "127.0.0.1".into(),
        gray_generic_tos: true,
        gray_robots_missing,
        gray_crawl_delay_min: true,
        min_request_interval_ms: 10_000,
        max_pages_per_day: 200,
        feed_active: false,
    }
}

fn row(domain: &DomainRow, url: String, program: Option<Uuid>) -> UrlRow {
    UrlRow {
        domain_id: domain.domain_id,
        candidate_id: Uuid::new_v4(),
        url,
        program_id: program,
        program_title: None,
        official_url: None,
        min_age: None,
        max_age: None,
    }
}

fn long_page(body: &str) -> String {
    format!(
        "<html><body><main>{body}<p>{}</p></main><footer><a href=\"/terms\">이용약관</a></footer></body></html>",
        "학생 혜택 안내 문서입니다. ".repeat(60)
    )
}

async fn census(
    mock: &Mock,
    base: &str,
    domain: &DomainRow,
    paths: &[(&str, bool)],
    gray_tos: bool,
) -> Vec<Entry> {
    let rows: Vec<UrlRow> = paths
        .iter()
        .map(|(path, monitored)| {
            row(
                domain,
                format!("{base}{path}"),
                monitored.then(Uuid::new_v4),
            )
        })
        .collect();
    let http = LoopbackHttp::new(UA);
    let mut session = DomainSession::new(&http, None, None, true, 10_000, 200)
        .with_test_spacing(Duration::from_millis(5));
    let entries = probe_domain(
        &mut session,
        domain,
        &rows,
        &flags(gray_tos),
        &CensusSettings::default(),
        &|_: &Url| false,
    )
    .await;
    assert!(
        mock.log.lock().unwrap().iter().all(|(_, ua, _)| ua == UA),
        "every request is the honest bot"
    );
    entries
}

fn lane_of<'a>(entries: &'a [Entry], suffix: &str) -> (&'a str, &'static str) {
    let entry = entries
        .iter()
        .find(|e| e.url.as_deref().is_some_and(|u| u.ends_with(suffix)))
        .unwrap();
    (entry.lane.as_str(), entry.reason)
}

#[tokio::test]
async fn robots_served_as_html_fails_closed_and_fetches_nothing_else() {
    let mock = Mock::default();
    mock.route(
        "/robots.txt",
        200,
        "text/html",
        "<!doctype html><html><body>app shell</body></html>",
    );
    mock.route(
        "/benefit",
        200,
        "text/html",
        &long_page("<p>학생 10% 할인</p>"),
    );
    let base = start(mock.clone()).await;
    let entries = census(&mock, &base, &domain(true), &[("/benefit", true)], false).await;
    assert_eq!(
        lane_of(&entries, "/benefit"),
        ("secondary", "robots_bot_wall")
    );
    assert_eq!(mock.hits("/benefit"), 0);
    assert_eq!(entries[0].robots_state, "robots_html");
}

#[tokio::test]
async fn missing_robots_is_allowed_only_while_g2_is_on() {
    let mock = Mock::default();
    mock.route(
        "/benefit",
        200,
        "text/html",
        &long_page("<p>학생 10% 할인</p>"),
    );
    mock.route(
        "/terms",
        200,
        "text/html",
        "<p>회원은 약관을 지켜야 합니다.</p>",
    );
    let base = start(mock.clone()).await;
    let on = census(&mock, &base, &domain(true), &[("/benefit", true)], false).await;
    assert_eq!(lane_of(&on, "/benefit"), ("direct", "direct_html"));
    assert_eq!(on[0].robots_state, "missing");
    assert_eq!(on[0].tos_signal, TosSignal::None);
    let before = mock.hits("/benefit");
    let off = census(&mock, &base, &domain(false), &[("/benefit", true)], false).await;
    assert_eq!(
        lane_of(&off, "/benefit"),
        ("impossible", "robots_missing_gray_off")
    );
    assert_eq!(
        mock.hits("/benefit"),
        before,
        "a disallowed page is never requested"
    );
}

#[tokio::test]
async fn path_disallow_is_per_path_and_crawl_delay_spaces_requests() {
    let mock = Mock::default();
    mock.route(
        "/robots.txt",
        200,
        "text/plain",
        "User-agent: *\nCrawl-delay: 30\nDisallow: /members\n",
    );
    mock.route(
        "/members/offer",
        200,
        "text/html",
        &long_page("<p>회원 전용</p>"),
    );
    mock.route(
        "/benefit",
        200,
        "text/html",
        &long_page("<p>학생 10% 할인</p>"),
    );
    mock.route("/terms", 200, "text/html", "<p>약관</p>");
    let base = start(mock.clone()).await;
    let entries = census(
        &mock,
        &base,
        &domain(true),
        &[("/members/offer", true), ("/benefit", false)],
        false,
    )
    .await;
    assert_eq!(
        lane_of(&entries, "/members/offer"),
        ("snippet", "robots_disallow")
    );
    assert_eq!(lane_of(&entries, "/benefit"), ("direct", "direct_html"));
    assert_eq!(mock.hits("/members/offer"), 0);
    let log = mock.log.lock().unwrap().clone();
    for pair in log.windows(2) {
        assert!(
            pair[1].2 - pair[0].2 >= Duration::from_millis(290),
            "Crawl-delay ordering not respected: {pair:?}"
        );
    }
    assert_eq!(entries[0].crawl_delay_ms, Some(30_000));
}

#[tokio::test]
async fn challenge_page_stops_the_domain() {
    let mock = Mock::default();
    mock.route(
        "/robots.txt",
        200,
        "text/plain",
        "User-agent: *\nAllow: /\n",
    );
    mock.route(
        "/a",
        403,
        "text/html",
        "<title>Just a moment...</title><div id=\"cf-chl-widget\"></div>",
    );
    mock.route("/b", 200, "text/html", &long_page("<p>학생 할인</p>"));
    let base = start(mock.clone()).await;
    let entries = census(
        &mock,
        &base,
        &domain(true),
        &[("/a", true), ("/b", true)],
        false,
    )
    .await;
    assert_eq!(lane_of(&entries, "/a"), ("secondary", "page_bot_wall"));
    assert_eq!(lane_of(&entries, "/b"), ("secondary", "page_bot_wall"));
    assert_eq!(mock.hits("/b"), 0, "no request after a challenge");
}

#[tokio::test]
async fn rate_limit_stops_the_domain_with_retry_after() {
    let mock = Mock::default();
    mock.route(
        "/robots.txt",
        200,
        "text/plain",
        "User-agent: *\nAllow: /\n",
    );
    mock.route("/a", 429, "text/html", "slow down");
    mock.route("/b", 200, "text/html", &long_page("<p>학생 할인</p>"));
    let base = start(mock.clone()).await;
    let entries = census(
        &mock,
        &base,
        &domain(true),
        &[("/a", true), ("/b", true)],
        false,
    )
    .await;
    assert_eq!(lane_of(&entries, "/a"), ("impossible", "rate_limited"));
    assert_eq!(lane_of(&entries, "/b"), ("impossible", "rate_limited"));
    assert_eq!(mock.hits("/b"), 0);
}

#[tokio::test]
async fn robots_403_is_a_bot_wall() {
    let mock = Mock::default();
    mock.route("/robots.txt", 403, "text/html", "<h1>Access Denied</h1>");
    mock.route("/a", 200, "text/html", &long_page(""));
    let base = start(mock.clone()).await;
    let entries = census(&mock, &base, &domain(true), &[("/a", true)], false).await;
    assert_eq!(lane_of(&entries, "/a"), ("secondary", "robots_bot_wall"));
    assert_eq!(mock.hits("/a"), 0);
}

/// The evidence job follows same-host redirects (3 hops) but only fetches a
/// URL the census rated `direct_html`. The census rated a redirecting URL
/// `redirect_same_host` without looking further, so such a monitor was never
/// verified (production: m.tworld SKT, aws.amazon.com).
#[tokio::test]
async fn a_same_host_redirect_is_followed_and_rated_on_the_final_page() {
    let mock = Mock::default();
    mock.route(
        "/robots.txt",
        200,
        "text/plain",
        "User-agent: *\nDisallow: /private\n",
    );
    mock.redirect("/old", 301, "/new");
    mock.route("/new", 200, "text/html", &long_page("<p>학생 10% 할인</p>"));
    mock.redirect("/to-private", 302, "/private/offer");
    mock.route("/private/offer", 200, "text/html", &long_page(""));
    mock.redirect("/away", 302, "https://elsewhere.example/offer");
    mock.redirect("/loop", 302, "/loop");
    mock.route("/terms", 200, "text/html", "<p>약관</p>");
    let base = start(mock.clone()).await;
    let entries = census(
        &mock,
        &base,
        &domain(true),
        &[
            ("/old", true),
            ("/to-private", true),
            ("/away", true),
            ("/loop", true),
        ],
        false,
    )
    .await;
    assert_eq!(lane_of(&entries, "/old"), ("direct", "direct_html"));
    assert_eq!(mock.hits("/new"), 1);
    let old = entries
        .iter()
        .find(|e| e.url.as_deref().is_some_and(|u| u.ends_with("/old")))
        .unwrap();
    assert!(old.text_chars.unwrap() >= 500, "rated on the final page");
    assert_eq!(
        lane_of(&entries, "/to-private"),
        ("snippet", "robots_disallow")
    );
    assert_eq!(mock.hits("/private/offer"), 0, "robots holds on every hop");
    assert_eq!(
        lane_of(&entries, "/away"),
        ("impossible", "redirect_off_host")
    );
    assert_eq!(lane_of(&entries, "/loop"), ("impossible", "redirect_loop"));
    assert!(
        mock.hits("/loop") <= 1,
        "a self-redirect is not re-requested"
    );
}

/// RFC 9309 §2.3.1.2: crawlers follow robots.txt redirects. A same-host
/// redirect used to read as `robots_http_error` (production: kscms.ks.ac.kr
/// → /resources/robots.txt). Off-host redirects stay unavailable.
#[tokio::test]
async fn robots_redirects_on_the_same_host_are_followed() {
    let mock = Mock::default();
    mock.redirect("/robots.txt", 302, "/resources/robots.txt");
    mock.route(
        "/resources/robots.txt",
        200,
        "text/plain",
        "User-agent: *\nDisallow: /members\n",
    );
    mock.route(
        "/benefit",
        200,
        "text/html",
        &long_page("<p>학생 10% 할인</p>"),
    );
    mock.route("/members/x", 200, "text/html", &long_page(""));
    mock.route("/terms", 200, "text/html", "<p>약관</p>");
    let base = start(mock.clone()).await;
    let entries = census(
        &mock,
        &base,
        &domain(true),
        &[("/benefit", true), ("/members/x", true)],
        false,
    )
    .await;
    assert_eq!(lane_of(&entries, "/benefit"), ("direct", "direct_html"));
    assert_eq!(
        lane_of(&entries, "/members/x"),
        ("snippet", "robots_disallow")
    );
    assert_eq!(mock.hits("/members/x"), 0);

    let away = Mock::default();
    away.redirect("/robots.txt", 301, "https://elsewhere.example/robots.txt");
    away.route("/benefit", 200, "text/html", &long_page(""));
    let base = start(away.clone()).await;
    let entries = census(&away, &base, &domain(true), &[("/benefit", true)], false).await;
    assert_eq!(
        lane_of(&entries, "/benefit"),
        ("impossible", "robots_unavailable")
    );
    assert_eq!(entries[1].robots_state, "robots_redirect_off_host");
    assert_eq!(away.hits("/benefit"), 0);
}

#[tokio::test]
async fn js_shell_goes_to_the_render_lane() {
    let mock = Mock::default();
    mock.route(
        "/robots.txt",
        200,
        "text/plain",
        "User-agent: *\nAllow: /\n",
    );
    mock.route(
        "/app",
        200,
        "text/html",
        "<html><body><div id=root></div><noscript>JavaScript를 켜 주세요</noscript></body></html>",
    );
    let base = start(mock.clone()).await;
    let entries = census(&mock, &base, &domain(true), &[("/app", true)], false).await;
    assert_eq!(lane_of(&entries, "/app"), ("render", "js_shell"));
}

#[tokio::test]
async fn generic_tos_follows_g1() {
    let mock = Mock::default();
    mock.route(
        "/robots.txt",
        200,
        "text/plain",
        "User-agent: *\nAllow: /\n",
    );
    mock.route(
        "/benefit",
        200,
        "text/html",
        &long_page("<p>학생 10% 할인</p>"),
    );
    mock.route(
        "/terms",
        200,
        "text/html",
        "<p>로봇, 스파이더 등 자동화된 수단으로 서비스에 접근하는 행위를 금지합니다.</p>",
    );
    let base = start(mock.clone()).await;
    let off = census(&mock, &base, &domain(true), &[("/benefit", true)], false).await;
    assert_eq!(off[0].tos_signal, TosSignal::GenericAutomated);
    assert_eq!(
        lane_of(&off, "/benefit"),
        ("impossible", "tos_generic_gray_off")
    );
    let on = census(&mock, &base, &domain(true), &[("/benefit", true)], true).await;
    assert_eq!(lane_of(&on, "/benefit"), ("direct", "direct_html"));
    mock.route("/terms", 200, "text/html", "<p>크롤링 등 자동화된 방법으로 사이트의 가격 및 이벤트 정보를 수집하여 제3자에게 제공하는 행위를 금지합니다.</p>");
    let specific = census(&mock, &base, &domain(true), &[("/benefit", true)], true).await;
    assert_eq!(
        lane_of(&specific, "/benefit"),
        ("impossible", "tos_specific_collection")
    );
}

#[tokio::test]
async fn shadow_extraction_checks_the_audit_quote_on_the_live_page() {
    let mock = Mock::default();
    mock.route(
        "/robots.txt",
        200,
        "text/plain",
        "User-agent: *\nAllow: /\n",
    );
    mock.route(
        "/nike",
        200,
        "text/html; charset=utf-8",
        &long_page("<p>학생 인증 후 10% 할인 혜택을 제공해 드립니다.</p><p>30일마다 인증 갱신 시, 할인 쿠폰이 재발급됩니다.</p><p>SheerID 대학생 인증</p>"),
    );
    let base = start(mock.clone()).await;
    let d = domain(true);
    let mut nike = row(&d, format!("{base}/nike"), Some(NIKE_ID.parse().unwrap()));
    nike.program_title = Some("나이키 학생 10% 할인".into());
    let http = LoopbackHttp::new(UA);
    let mut session = DomainSession::new(&http, None, None, true, 10_000, 200)
        .with_test_spacing(Duration::from_millis(5));
    let entries = probe_domain(
        &mut session,
        &d,
        &[nike],
        &flags(false),
        &CensusSettings::default(),
        &|_: &Url| false,
    )
    .await;
    let shadow = entries[1].shadow.clone().unwrap();
    assert_eq!(shadow["audit"]["verdict"], "CORRECT");
    assert_eq!(shadow["audit"]["evidence_present"], json!([true, true]));
    assert_eq!(shadow["audit"]["would_adopt"], "confirmed");
    let facts = shadow["facts"].as_array().unwrap();
    assert!(facts.iter().any(|f| f["type"] == "discount_percent"
        && f["value"] == "10%"
        && f["audit_corroborated"] == true));
    assert!(facts
        .iter()
        .any(|f| f["type"] == "verification" && f["value"] == "sheerid"));
    let summary = summarize(&entries);
    assert_eq!(summary["p1_adoption_preview"]["CORRECT:confirmed"], 1);
    assert_eq!(
        summary["shadow_fact_types"]["discount_percent"]["support"],
        1
    );
    assert!(summary_text(&summary).contains("p1_adoption_preview"));
}

#[test]
fn shadow_parsers_read_offer_facts() {
    let facts = shadow_facts("학생 인증 시 40% 할인. 월 6,600원 요금. 6개월 무료 이용. 만 19세 이상 34세 이하 고객. SheerID 인증");
    let kinds: Vec<(&str, String)> = facts
        .iter()
        .map(|f| (f["type"].as_str().unwrap(), f["value"].to_string()))
        .collect();
    assert!(kinds.contains(&("discount_percent", "\"40%\"".into())));
    assert!(kinds.contains(&("price_krw", "\"6600\"".into())));
    assert!(kinds.contains(&("free_period", "\"6개월\"".into())));
    assert!(kinds
        .iter()
        .any(|(k, v)| *k == "age" && v.contains("19") && v.contains("34")));
    assert!(kinds.contains(&("verification", "\"sheerid\"".into())));
    assert!(corroborated(
        &json!({"type": "price_krw", "value": "6600"}),
        "월 ₩6,600이 청구됩니다"
    ));
}

#[tokio::test]
async fn census_run_is_stored_and_readable_by_the_gate() {
    let Ok(database_url) = std::env::var("TEST_DATABASE_URL") else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&database_url)
        .await
        .unwrap();
    let run_id: Uuid =
        sqlx::query_scalar("INSERT INTO corporate_census_runs DEFAULT VALUES RETURNING id")
            .fetch_one(&pool)
            .await
            .unwrap();
    let d: Uuid = sqlx::query_scalar("SELECT id FROM benefit_source_domains LIMIT 1")
        .fetch_one(&pool)
        .await
        .unwrap();
    let entry = Entry {
        subject_kind: "domain",
        domain_id: d,
        candidate_id: None,
        program_id: None,
        url: None,
        host: Some("example.com".into()),
        lane: Lane::Impossible,
        reason: "domain_summary",
        probed: false,
        robots_state: "ok",
        path_allowed: None,
        search_engine_allowlist: true,
        crawl_delay_ms: Some(10_000),
        tos_signal: TosSignal::NotFound,
        http_status: None,
        text_chars: None,
        shadow: None,
    };
    store_entries(&pool, run_id, &[entry]).await.unwrap();
    let stored: (String, bool) = sqlx::query_as(
        "SELECT tos_signal, search_engine_allowlist FROM corporate_census_entries WHERE run_id=$1",
    )
    .bind(run_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(stored, ("not_found".into(), true));
    sqlx::query("DELETE FROM corporate_census_runs WHERE id=$1")
        .bind(run_id)
        .execute(&pool)
        .await
        .unwrap();
}

/// M1: the opt-out check runs per URL right before each fetch, so an
/// opt-out that arrives in the middle of a domain's probe stops the next
/// request (a snapshot taken at the start of the run would not).
#[tokio::test]
async fn optout_arriving_mid_run_stops_the_next_fetch() {
    let mock = Mock::default();
    mock.route(
        "/robots.txt",
        200,
        "text/plain",
        "User-agent: *\nAllow: /\n",
    );
    mock.route("/a", 200, "text/html", &long_page("<p>학생 10% 할인</p>"));
    mock.route("/b", 200, "text/html", &long_page("<p>학생 20% 할인</p>"));
    let base = start(mock.clone()).await;
    let d = domain(true);
    let rows = vec![
        row(&d, format!("{base}/a"), Some(Uuid::new_v4())),
        row(&d, format!("{base}/b"), Some(Uuid::new_v4())),
    ];
    let http = LoopbackHttp::new(UA);
    let mut session = DomainSession::new(&http, None, None, true, 10_000, 200)
        .with_test_spacing(Duration::from_millis(5));
    let seen = mock.clone();
    // The publisher opts out once our first page request has been served.
    let check = move |_: &Url| seen.hits("/a") > 0;
    let entries = probe_domain(
        &mut session,
        &d,
        &rows,
        &flags(false),
        &CensusSettings::default(),
        &check,
    )
    .await;
    assert_eq!(mock.hits("/a"), 1);
    assert_eq!(mock.hits("/b"), 0, "no request after the opt-out");
    assert_eq!(lane_of(&entries, "/b"), ("impossible", "opt_out"));
}

/// Review M11: the shadow parser reads a percentage whole, never "1.5%"
/// as 5% or "100%" as 0%.
#[test]
fn shadow_percentages_are_read_whole() {
    let percents = |text: &str| -> Vec<String> {
        shadow_facts(text)
            .iter()
            .filter(|f| f["type"] == "discount_percent")
            .map(|f| f["value"].as_str().unwrap().to_string())
            .collect()
    };
    for text in ["학생 1.5% 할인", "학생 100% 할인", "학생 12,5% 적립"] {
        assert!(percents(text).is_empty(), "{text}: {:?}", percents(text));
    }
    assert_eq!(percents("학생 15% 할인"), vec!["15%".to_string()]);
    assert_eq!(percents("학생(10%할인)"), vec!["10%".to_string()]);
}

/// The student-offer seed migration (2026-10-26) enters through the normal
/// intake path: the official-seed discoverer makes each seed a branded
/// `review` candidate on an eligible, not-crawl-approved domain, and nothing
/// becomes a programme, monitor or fact.
#[tokio::test]
async fn student_offer_seeds_enter_the_census_as_review_candidates() {
    let Ok(database_url) = std::env::var("TEST_DATABASE_URL") else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&database_url)
        .await
        .unwrap();
    crate::sources::official_seeds::OfficialSeedDiscoverer::run(&pool)
        .await
        .unwrap();
    let seeds = [
        "https://www.ableton.com/en/shop/education/",
        "https://lucid.co/lucidchart/use-cases/education",
        "https://gemini.google/students/",
        "https://www.wolframalpha.com/pro-for-students",
        "https://www.overleaf.com/user/subscription/plans",
        "https://master.dev/welcome/github-student-developers/",
        // 20261031000001: passed the whole intake chain in projection.
        "https://nc.me/",
        // 20261101000101: passed the precision-fixed chain, checked by hand.
        "https://configcat.com/student/",
        "https://deepscan.io/github-student-pack",
        "https://imgbot.net/github-students",
        "https://popsql.com/github-students",
        "https://paperpile.com/pricing/",
        "https://sentry.io/for/education/",
        "https://todiagram.com/education",
        "https://workingcopy.app/education/",
        "https://www.captureone.com/en/education",
        "https://www.cambercloud.com/github-student-pack",
        "https://www.dashlane.com/github-students",
        "https://www.honeybadger.io/github-students/",
        "https://www.native-instruments.com/en/company/education-program/",
        "https://www.reasonstudios.com/school",
        "https://zeplin.io/students/",
    ];
    for seed in seeds {
        let row: Option<(String, bool, bool, i64, i64, bool)> = sqlx::query_as(&format!(
            "SELECT c.status, d.crawl_allowed, {eligible},
                    (SELECT count(*) FROM benefit_candidate_brands b WHERE b.candidate_id = c.id),
                    (SELECT count(*) FROM program_source_monitors m WHERE m.candidate_id = c.id),
                    c.program_id IS NOT NULL
             FROM benefit_source_candidates c
             JOIN benefit_source_domains d ON d.id = c.domain_id
             WHERE c.url = $1",
            eligible = policy::ELIGIBLE_DOMAIN_SQL
        ))
        .bind(seed)
        .fetch_optional(&pool)
        .await
        .unwrap();
        let (status, crawl_allowed, eligible, brands, monitors, programme) =
            row.unwrap_or_else(|| panic!("{seed}: no candidate"));
        assert_eq!(status, "review", "{seed}");
        assert!(!crawl_allowed, "{seed}: no crawl approval is granted");
        assert!(eligible, "{seed}: the census may rate it");
        assert_eq!(brands, 1, "{seed}");
        assert_eq!(monitors, 0, "{seed}: nothing is monitored or shown");
        assert!(!programme, "{seed}");
    }
    let facts: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM corporate_offer_facts f JOIN programs p ON p.id = f.program_id
         WHERE p.official_url = ANY($1)",
    )
    .bind(&seeds[..])
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(facts, 0);
}
